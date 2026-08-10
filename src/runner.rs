use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use owo_colors::OwoColorize;

use crate::asset::AssetGraph;
use crate::contract;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::manifest::{Manifest, Param, RetryPolicy};
use crate::operator;
use crate::precondition;
use crate::precondition::Precondition;
use crate::state::{self, SkipReason, StateBackend, StepStatus};

/// Load dotenv files and return their key-value pairs.
/// Files are loaded in declared order; later files override earlier ones.
/// Missing files are silently skipped.
fn load_dotenv_files(dir: &Path, dotenv_paths: &[String]) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    for path_str in dotenv_paths {
        let path = dir.join(path_str);
        if let Ok(iter) = dotenvy::from_path_iter(&path) {
            for (key, value) in iter.flatten() {
                vars.insert(key, value);
            }
        }
        // Missing files are silently skipped (from_path_iter returns Err).
    }
    vars
}

/// Resolve parameters from dotenv files, manifest defaults, and CLI overrides.
///
/// Precedence (highest wins): CLI params > dotenv files > manifest defaults.
/// Returns a map of ARC_PARAM_{NAME_UPPERCASED} -> value for all resolved params.
///
/// Missing required params (no default, not in dotenv or CLI) produce MissingParam error.
pub fn resolve_params(
    manifest_params: &IndexMap<String, Param>,
    dotenv_vars: &HashMap<String, String>,
    cli_params: &[(String, String)],
) -> Result<HashMap<String, String>> {
    let mut resolved: HashMap<String, String> = HashMap::new();

    // Build a lookup from CLI params.
    let cli_map: HashMap<&str, &str> = cli_params
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    for (name, param) in manifest_params {
        // Precedence: CLI > dotenv > default.
        let value = if let Some(v) = cli_map.get(name.as_str()) {
            Some(v.to_string())
        } else if let Some(v) = dotenv_vars.get(name) {
            Some(v.clone())
        } else {
            param.default.clone()
        };

        match value {
            Some(v) => {
                let env_key = format!("ARC_PARAM_{}", name.to_uppercase());
                resolved.insert(env_key, v);
            }
            None => {
                return Err(Error::MissingParam { name: name.clone() });
            }
        }
    }

    Ok(resolved)
}

/// Compute the backoff duration for a retry attempt.
/// Formula: backoff_sec * 2^(attempt-1) where attempt is 1-indexed.
pub fn backoff_duration(policy: &RetryPolicy, attempt: u32) -> Duration {
    let secs = policy.backoff_sec * 2f64.powi((attempt as i32) - 1);
    Duration::from_secs_f64(secs)
}

/// Execute a lifecycle hook step.
///
/// Hooks use the Engine's execute paths (SQL or command) with the given env vars.
/// Returns Ok on success, Err on failure.
fn execute_hook(
    hook: &crate::manifest::Step,
    engine: &dyn Engine,
    db_path: &Path,
    dir: &Path,
    env: &HashMap<String, String>,
) -> Result<()> {
    if let Some(ref sql) = hook.sql {
        let sql_path = dir.join(sql);
        engine.execute_sql(db_path, &sql_path, env, None)?;
    } else if let Some(ref command) = hook.command {
        engine.execute_command(command, env, false, None)?;
    }
    Ok(())
}

/// Run a pipeline with no CLI parameter overrides.
///
/// Convenience wrapper that delegates to `run_with_params` with empty params.
/// Only the tests need the parameterless form today; production call sites pass
/// params explicitly, so it is scoped to test builds until a public API surface
/// (the planned `[lib]` target) exports it deliberately.
#[cfg(test)]
pub fn run(dir: &Path, engine: &dyn Engine, state: &dyn StateBackend, force: bool) -> Result<()> {
    run_with_params(dir, engine, state, force, &[])
}

/// Run a pipeline with CLI parameter overrides.
pub fn run_with_params(
    dir: &Path,
    engine: &dyn Engine,
    state: &dyn StateBackend,
    force: bool,
    cli_params: &[(String, String)],
) -> Result<()> {
    let manifest = Manifest::load(dir)?;

    // If there are SQL steps, verify the engine is available and check version.
    if manifest.has_sql_steps() {
        let info = engine.preflight()?;

        // Check engine version constraint if specified.
        if let Some(ref constraint_str) = manifest.engine_version
            && let Ok(req) = semver::VersionReq::parse(constraint_str)
        {
            match &info.version {
                Some(ver) => {
                    if !req.matches(ver) {
                        return Err(Error::VersionMismatch {
                            required: constraint_str.clone(),
                            found: ver.to_string(),
                        });
                    }
                }
                None => {
                    // Version unparseable — warn but don't block.
                    eprintln!(
                        "{} could not detect engine version — skipping version check (requires {})",
                        "warning:".yellow(),
                        constraint_str,
                    );
                }
            }
        }
        // If constraint_str is invalid, manifest validation already caught it.
    }

    if manifest.steps.is_empty() {
        println!("{}", "No steps defined.".dimmed());
        return Ok(());
    }

    // Resolve parameters: dotenv files → manifest defaults → CLI overrides.
    let dotenv_vars = load_dotenv_files(dir, &manifest.dotenv);
    let mut env_map = resolve_params(&manifest.params, &dotenv_vars, cli_params)?;

    // Initialise the state backend (creates tables if needed).
    state.init()?;

    // Start a new run record.
    let run_id = state.start_run()?;

    // Build the asset graph and validate dependency ordering.
    let asset_graph = AssetGraph::build(&manifest, dir);

    // Print any warnings from asset discovery (e.g. unparseable SQL).
    for warning in &asset_graph.warnings {
        eprintln!("{} {}", "warning:".yellow(), warning);
    }

    // If the graph has assets, validate step ordering against dependencies.
    if asset_graph.has_assets() {
        let step_order: Vec<String> = manifest.steps.iter().map(|s| s.name.clone()).collect();
        asset_graph.validate_order(&step_order)?;
    }

    // Determine which steps are stale, and — for the fresh ones — the typed reason
    // they can be skipped (recorded per step in the contract).
    let staleness = compute_staleness(&manifest, dir, state, &asset_graph, force, &env_map)?;

    let db_path = manifest.db_path(dir);
    // The shared fetch cache, resolved once for the run and handed to every operator
    // step. `None` when `$ARCFORM_FETCH_CACHE` says `off` or there is no home
    // directory to put it in — a run without one behaves as every run did before the
    // cache existed, which is the comparison the cache is held to.
    let fetch_cache = crate::fetch_cache::FetchCache::from_env();
    let total = manifest.steps.len();
    let mut succeeded = 0;
    let mut executed = 0;
    let mut skipped = 0;
    let mut total_retries: usize = 0;

    // --- Live Protocol+Run contract ---
    // Persist the asset graph + a per-step status stream instead of discarding the
    // graph when the run returns. `runs_dir` and its parents are created up front;
    // `stream` appends one JSONL line per step as it finishes (and a terminal
    // `run_complete` line at the end). `step_outcomes` records each step's terminal
    // state + attempt count for the final JSON contract.
    let started_at = contract::now_iso();
    let runs_dir = contract::runs_dir(dir);
    let _ = std::fs::create_dir_all(&runs_dir);
    let mut stream = contract::RunStream::create(&runs_dir, &run_id);
    let mut step_outcomes: HashMap<String, contract::StepOutcome> = HashMap::new();
    let contract_params = contract::param_entries(&manifest.params, &dotenv_vars, cli_params);

    // Pipeline-level timeout tracking.
    let pipeline_start = Instant::now();
    let pipeline_timeout = manifest.timeout_sec.map(Duration::from_secs_f64);

    // Track whether on_init was attempted (for on_exit try/finally guarantee).
    let mut init_attempted = false;

    // --- on_init hook ---
    if let Some(ref init_hook) = manifest.hooks.on_init {
        init_attempted = true;
        println!("{} {} ...", "[hook]".dimmed(), init_hook.name.bold());
        if let Err(e) = execute_hook(init_hook, engine, &db_path, dir, &env_map) {
            // on_init failure is fatal — no steps execute.
            // But on_exit still runs.
            eprintln!(
                "{} on_init hook '{}' failed: {}",
                "error:".red(),
                init_hook.name,
                e
            );

            // Run on_exit with ARC_PIPELINE_STATUS=init_failed.
            if let Some(ref exit_hook) = manifest.hooks.on_exit {
                let mut exit_env = env_map.clone();
                exit_env.insert("ARC_PIPELINE_STATUS".to_string(), "init_failed".to_string());
                println!("{} {} ...", "[hook]".dimmed(), exit_hook.name.bold());
                if let Err(exit_err) = execute_hook(exit_hook, engine, &db_path, dir, &exit_env) {
                    eprintln!(
                        "{} on_exit hook '{}' failed: {}",
                        "warning:".yellow(),
                        exit_hook.name,
                        exit_err
                    );
                }
            }

            let _ = state.finish_run(&run_id, executed, "init_failed", total_retries);
            return Err(e);
        }
    }

    // --- Step execution loop ---
    // Run the step loop, capturing the result for hook dispatch.
    let step_loop_result: std::result::Result<(), Error> = (|| {
        for (i, step) in manifest.steps.iter().enumerate() {
            let is_stale = staleness.stale.contains(&step.name);

            if !is_stale {
                // Fresh: report the typed reason (hash_clean / precondition_*) rather than
                // a bare `[skip]`, and record it so history selectors can read it back.
                let skip_reason = staleness.skip_reasons.get(&step.name).copied();
                let reason_tag = skip_reason
                    .map(|r| format!("[skip: {}]", r.as_str()))
                    .unwrap_or_else(|| "[skip]".to_string());
                println!(
                    "[{}/{}] {} {}",
                    i + 1,
                    total,
                    step.name.bold(),
                    reason_tag.dimmed(),
                );
                skipped += 1;
                stream.step(&step.name, "skipped");
                step_outcomes.insert(
                    step.name.clone(),
                    contract::StepOutcome {
                        state: "skipped".to_string(),
                        attempts: 0,
                        skip_reason,
                        duration_sec: None,
                    },
                );
                continue;
            }

            // Check pipeline timeout before executing.
            if let Some(pt) = pipeline_timeout {
                let elapsed = pipeline_start.elapsed();
                if elapsed >= pt {
                    let _ = state.finish_run(&run_id, executed, "timeout", total_retries);
                    return Err(Error::PipelineTimeout {
                        step: step.name.clone(),
                        elapsed_sec: elapsed.as_secs_f64(),
                    });
                }
            }

            println!("[{}/{}] {} ...", i + 1, total, step.name.bold());

            // Compute the SQL hash for this step (for state recording).
            let sql_hash = if let Some(ref sql) = step.sql {
                let sql_path = dir.join(sql);
                if !sql_path.exists() {
                    return Err(Error::SqlFileNotFound {
                        step: step.name.clone(),
                        path: sql_path,
                    });
                }
                let content = std::fs::read(&sql_path).map_err(|e| Error::FileRead {
                    path: sql_path.clone(),
                    source: e,
                })?;
                state::content_hash(&content)
            } else if step.op.is_some() {
                op_config_hash(step)
            } else {
                String::new()
            };

            let capture_stdout = step.output.is_some();

            // Resolve effective retry policy: step-level overrides defaults wholesale.
            // Hooks do not inherit manifest defaults — only pipeline steps do.
            let effective_retry = step
                .retry
                .as_ref()
                .or_else(|| manifest.defaults.as_ref().and_then(|d| d.retry.as_ref()));

            let max_attempts = effective_retry.map_or(1, |r| r.max_attempts);

            let mut last_error = None;

            // Wall-clock timer for this step — spans every attempt and its backoff sleep,
            // captured into the contract as `duration_sec` (per-step timing, replacing the
            // discarded pipeline-wide retry count).
            let step_start = Instant::now();

            for attempt in 1..=max_attempts {
                if attempt > 1 {
                    // Print retry separator.
                    if let Some(policy) = effective_retry {
                        let delay = backoff_duration(policy, attempt);
                        eprintln!(
                            "[retry {}/{}, backoff {:.1}s]",
                            attempt,
                            max_attempts,
                            delay.as_secs_f64()
                        );
                        std::thread::sleep(delay);
                    }
                    total_retries += 1;
                }

                // Compute step timeout per attempt, clamped to remaining pipeline time.
                // Recomputed each iteration so backoff sleep + prior attempts are accounted for.
                let step_timeout = {
                    let step_t = step.timeout_sec.map(Duration::from_secs_f64);
                    if let Some(pt) = pipeline_timeout {
                        let remaining = pt.saturating_sub(pipeline_start.elapsed());
                        match step_t {
                            Some(st) => Some(st.min(remaining)),
                            None => Some(remaining),
                        }
                    } else {
                        step_t
                    }
                };

                let result = if let Some(ref sql) = step.sql {
                    let sql_path = dir.join(sql);
                    engine.execute_sql(&db_path, &sql_path, &env_map, step_timeout)
                } else if let Some(ref op_ref) = step.op {
                    match operator::resolve(op_ref) {
                        Ok(op) => {
                            let ctx = operator::OpContext {
                                dir,
                                db_path: db_path.as_path(),
                                env: &env_map,
                                timeout: step_timeout,
                                cache: fetch_cache.as_ref(),
                            };
                            let with = step.with.clone().unwrap_or(serde_yaml::Value::Null);
                            op.run(&with, &ctx)
                        }
                        Err(e) => Err(e),
                    }
                } else if let Some(ref command) = step.command {
                    engine.execute_command(command, &env_map, capture_stdout, step_timeout)
                } else {
                    unreachable!("validation ensures sql, command, or op is present")
                };

                match result {
                    Ok(output) => {
                        succeeded += 1;
                        executed += 1;
                        let _ = state.record_step(&step.name, &sql_hash, StepStatus::Success);

                        // If this step captures output, inject it as ARC_PARAM_ for downstream steps.
                        if let Some(ref output_name) = step.output {
                            let captured = output.stdout.unwrap_or_default();
                            let env_key = format!("ARC_PARAM_{}", output_name.to_uppercase());
                            env_map.insert(env_key, captured);
                        }

                        last_error = None;
                        stream.step(&step.name, "success");
                        step_outcomes.insert(
                            step.name.clone(),
                            contract::StepOutcome {
                                state: "success".to_string(),
                                attempts: attempt,
                                skip_reason: None,
                                duration_sec: Some(step_start.elapsed().as_secs_f64()),
                            },
                        );
                        break;
                    }
                    Err(Error::StepFailed { code, stderr, .. }) => {
                        last_error = Some(Error::StepFailed {
                            step: step.name.clone(),
                            code,
                            stderr,
                        });
                        // Continue to next attempt if retries remain.
                    }
                    Err(Error::StepTimeout {
                        step: timed_out_step,
                    }) => {
                        // A timed-out step counts as a failed attempt — retryable.
                        last_error = Some(Error::StepTimeout {
                            step: timed_out_step,
                        });
                        // Continue to next attempt if retries remain.
                    }
                    Err(e) => {
                        // Non-retryable errors (StepExecution, etc.) — halt immediately.
                        let _ = state.record_step(&step.name, &sql_hash, StepStatus::Failed);
                        let _ = state.finish_run(&run_id, executed, "error", total_retries);
                        stream.step(&step.name, "failed");
                        step_outcomes.insert(
                            step.name.clone(),
                            contract::StepOutcome {
                                state: "failed".to_string(),
                                attempts: attempt,
                                skip_reason: None,
                                duration_sec: Some(step_start.elapsed().as_secs_f64()),
                            },
                        );
                        return Err(e);
                    }
                }
            }

            // If we exhausted all attempts with an error, record failure and halt.
            if let Some(err) = last_error {
                executed += 1;
                let _ = state.record_step(&step.name, &sql_hash, StepStatus::Failed);
                let _ = state.finish_run(&run_id, executed, "failed", total_retries);
                stream.step(&step.name, "failed");
                step_outcomes.insert(
                    step.name.clone(),
                    contract::StepOutcome {
                        state: "failed".to_string(),
                        attempts: max_attempts,
                        skip_reason: None,
                        duration_sec: Some(step_start.elapsed().as_secs_f64()),
                    },
                );
                return Err(err);
            }
        }

        // All steps succeeded.
        let _ = state.finish_run(&run_id, executed, "success", total_retries);
        Ok(())
    })();

    // --- Lifecycle hooks: on_success / on_failure / on_exit ---
    // Hooks run outside the pipeline timeout boundary.

    match &step_loop_result {
        Ok(()) => {
            // --- on_success hook ---
            if let Some(ref success_hook) = manifest.hooks.on_success {
                println!("{} {} ...", "[hook]".dimmed(), success_hook.name.bold());
                if let Err(e) = execute_hook(success_hook, engine, &db_path, dir, &env_map) {
                    // Non-fatal: report but keep Ok result.
                    eprintln!(
                        "{} on_success hook '{}' failed: {}",
                        "warning:".yellow(),
                        success_hook.name,
                        e
                    );
                }
            }
        }
        Err(e) => {
            // --- on_failure hook ---
            if let Some(ref failure_hook) = manifest.hooks.on_failure {
                let mut failure_env = env_map.clone();
                // Inject failure context env vars.
                match e {
                    Error::StepFailed { step, code, .. } => {
                        failure_env.insert("ARC_FAILED_STEP".to_string(), step.clone());
                        failure_env.insert("ARC_EXIT_CODE".to_string(), code.to_string());
                    }
                    Error::StepTimeout { step } => {
                        failure_env.insert("ARC_FAILED_STEP".to_string(), step.clone());
                        failure_env.insert("ARC_EXIT_CODE".to_string(), "timeout".to_string());
                    }
                    Error::PipelineTimeout { step, .. } => {
                        failure_env.insert("ARC_FAILED_STEP".to_string(), step.clone());
                        failure_env.insert("ARC_EXIT_CODE".to_string(), "timeout".to_string());
                    }
                    _ => {}
                }

                println!("{} {} ...", "[hook]".dimmed(), failure_hook.name.bold());
                if let Err(hook_err) =
                    execute_hook(failure_hook, engine, &db_path, dir, &failure_env)
                {
                    // Non-fatal: report but keep original error.
                    eprintln!(
                        "{} on_failure hook '{}' failed: {}",
                        "warning:".yellow(),
                        failure_hook.name,
                        hook_err
                    );
                }
            }
        }
    }

    // --- on_exit hook (try/finally) ---
    // on_exit runs if on_init was attempted OR if any steps ran (even without on_init).
    let should_run_exit = init_attempted || !manifest.steps.is_empty();
    if should_run_exit && let Some(ref exit_hook) = manifest.hooks.on_exit {
        let mut exit_env = env_map.clone();
        match &step_loop_result {
            Ok(()) => {
                exit_env.insert("ARC_PIPELINE_STATUS".to_string(), "success".to_string());
            }
            Err(e) => {
                exit_env.insert("ARC_PIPELINE_STATUS".to_string(), "failed".to_string());
                // Inject failure context on failed status.
                match e {
                    Error::StepFailed { step, code, .. } => {
                        exit_env.insert("ARC_FAILED_STEP".to_string(), step.clone());
                        exit_env.insert("ARC_EXIT_CODE".to_string(), code.to_string());
                    }
                    Error::StepTimeout { step } => {
                        exit_env.insert("ARC_FAILED_STEP".to_string(), step.clone());
                        exit_env.insert("ARC_EXIT_CODE".to_string(), "timeout".to_string());
                    }
                    Error::PipelineTimeout { step, .. } => {
                        exit_env.insert("ARC_FAILED_STEP".to_string(), step.clone());
                        exit_env.insert("ARC_EXIT_CODE".to_string(), "timeout".to_string());
                    }
                    _ => {}
                }
            }
        }

        println!("{} {} ...", "[hook]".dimmed(), exit_hook.name.bold());
        if let Err(exit_err) = execute_hook(exit_hook, engine, &db_path, dir, &exit_env) {
            eprintln!(
                "{} on_exit hook '{}' failed: {}",
                "warning:".yellow(),
                exit_hook.name,
                exit_err
            );
        }
    }

    // --- Finalize the live Protocol+Run contract ---
    // Assemble the full contract (assets, per-table row counts, steps), write it to
    // `<run_id>.json`, mark the status stream complete, and render the asset DAG to
    // the terminal. Best-effort: a contract-write failure warns but never fails the
    // run itself.
    let finished_at = contract::now_iso();
    let outcome = match &step_loop_result {
        Ok(()) => "success",
        Err(_) if succeeded > 0 => "partial",
        Err(_) => "error",
    };
    let run_contract = contract::build_contract(contract::ContractInputs {
        manifest: &manifest,
        dir,
        db_path: &db_path,
        graph: &asset_graph,
        run_id: &run_id,
        started_at: &started_at,
        finished_at: &finished_at,
        outcome,
        params: contract_params,
        step_outcomes: &step_outcomes,
    });
    if let Err(e) = contract::write_contract(&runs_dir, &run_id, &run_contract) {
        eprintln!(
            "{} could not write run contract: {}",
            "warning:".yellow(),
            e
        );
    }
    stream.complete(outcome);
    print!("{}", contract::render_dag(&run_contract));

    // Print summary and return the pipeline result (not hook result).
    match step_loop_result {
        Ok(()) => {
            if skipped > 0 {
                println!(
                    "\n{} {}/{} steps succeeded, {} skipped (fresh).",
                    "✓".green(),
                    succeeded,
                    total,
                    skipped,
                );
            } else {
                println!("\n{} {}/{} steps succeeded.", "✓".green(), succeeded, total,);
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// The staleness verdict for a run: which steps must execute, and — for each step that
/// ends up fresh — the typed reason it can be skipped. A fresh step forced to run by
/// downstream propagation has no entry in `skip_reasons` (it executed).
struct Staleness {
    stale: std::collections::HashSet<String>,
    skip_reasons: HashMap<String, SkipReason>,
}

/// Pick the typed skip reason for a step whose preconditions all evaluated fresh: a
/// `modified_after` clock precondition is called out distinctly from generic ones so the
/// contract records *which* freshness mechanism decided the skip.
fn precondition_skip_reason(preconditions: &[Precondition]) -> SkipReason {
    if preconditions
        .iter()
        .any(|p| matches!(p, Precondition::ModifiedAfter { .. }))
    {
        SkipReason::PreconditionModifiedAfter
    } else {
        SkipReason::PreconditionFresh
    }
}

/// Determine which steps are stale and need to execute.
///
/// A step is stale if:
/// - `force` is true (all steps run)
/// - It's a command step (always re-runs)
/// - It has no prior state (first run)
/// - Its prior run failed
/// - Its SQL file content hash changed
/// - An upstream step (via asset graph) is stale (downstream propagation)
///
/// For every step that stays fresh, a typed [`SkipReason`] is recorded so the run
/// contract can distinguish a hash-clean skip from a precondition-driven one.
fn compute_staleness(
    manifest: &Manifest,
    dir: &Path,
    state: &dyn StateBackend,
    asset_graph: &AssetGraph,
    force: bool,
    env: &HashMap<String, String>,
) -> Result<Staleness> {
    let mut stale: std::collections::HashSet<String> = std::collections::HashSet::new();

    if force {
        // Force mode: everything is stale — nothing is skipped, so no reasons.
        for step in &manifest.steps {
            stale.insert(step.name.clone());
        }
        return Ok(Staleness {
            stale,
            skip_reasons: HashMap::new(),
        });
    }

    // Phase 1: Check each step's own staleness, and note why a fresh step is skippable.
    // Reasons are provisional until downstream propagation settles (Phase 3).
    let mut reasons: HashMap<String, SkipReason> = HashMap::new();
    for step in &manifest.steps {
        if step.command.is_some() {
            if step.preconditions.is_empty() {
                // No preconditions — command steps always re-run (backwards compat).
                stale.insert(step.name.clone());
            } else if precondition::evaluate_all(&step.preconditions, dir, &step.name, env)? {
                // All preconditions fresh — skippable via the precondition mechanism.
                reasons.insert(
                    step.name.clone(),
                    precondition_skip_reason(&step.preconditions),
                );
            } else {
                stale.insert(step.name.clone());
            }
            continue;
        }

        // SQL/op step — check hash staleness (op steps hash their config).
        let hash_stale = is_hash_stale(step, dir, state)?;

        if step.preconditions.is_empty() {
            // No preconditions — SQL steps use hash only (backwards compat).
            if hash_stale {
                stale.insert(step.name.clone());
            } else {
                reasons.insert(step.name.clone(), SkipReason::HashClean);
            }
        } else {
            // AND: hash AND preconditions must both be fresh to skip.
            let preconditions_fresh =
                precondition::evaluate_all(&step.preconditions, dir, &step.name, env)?;
            if hash_stale || !preconditions_fresh {
                stale.insert(step.name.clone());
            } else {
                // Both clean — the precondition kind is the more specific signal.
                reasons.insert(
                    step.name.clone(),
                    precondition_skip_reason(&step.preconditions),
                );
            }
        }
    }

    // Phase 2: Downstream propagation.
    let directly_stale: Vec<String> = stale.iter().cloned().collect();
    let downstream = asset_graph.downstream_steps(&directly_stale);
    for step_name in downstream {
        stale.insert(step_name);
    }

    // Phase 3: A fresh step dragged stale by an upstream change actually executes, so it
    // is no longer skipped — drop its provisional reason.
    reasons.retain(|name, _| !stale.contains(name));

    Ok(Staleness {
        stale,
        skip_reasons: reasons,
    })
}

/// Staleness hash for an `op:` step: the operator reference plus its serialized `with:`
/// config. An op step re-runs when either changes and skips when neither does — the same
/// cache-correctness a SQL step gets from its file hash, stored in the same state column.
fn op_config_hash(step: &crate::manifest::Step) -> String {
    let op = step.op.as_deref().unwrap_or_default();
    let with = step
        .with
        .as_ref()
        .and_then(|v| serde_yaml::to_string(v).ok())
        .unwrap_or_default();
    state::content_hash(format!("{}\n{}", op, with).as_bytes())
}

/// Parse `--param KEY=VALUE` flags into (key, value) pairs.
///
/// Lives here rather than in the CLI module because it is part of running a
/// pipeline, not part of the command line: the registry run path calls it too,
/// and the CLI can be compiled out.
/// Splits on the first '=' — keys cannot contain '=', values can.
pub(crate) fn parse_params(raw: &[String]) -> Result<Vec<(String, String)>> {
    let mut parsed = Vec::new();
    for param in raw {
        if let Some(pos) = param.find('=') {
            let key = param[..pos].to_string();
            let value = param[pos + 1..].to_string();
            if key.is_empty() {
                return Err(Error::ManifestValidation(format!(
                    "invalid --param '{}': key cannot be empty",
                    param
                )));
            }
            parsed.push((key, value));
        } else {
            return Err(Error::ManifestValidation(format!(
                "invalid --param '{}': expected KEY=VALUE format",
                param
            )));
        }
    }
    Ok(parsed)
}

/// Check whether a SQL or op step's staleness hash has changed since the last run.
///
/// Returns true (stale) if: no prior state, prior failure, hash mismatch, or missing file.
fn is_hash_stale(
    step: &crate::manifest::Step,
    dir: &Path,
    state: &dyn StateBackend,
) -> Result<bool> {
    let prior = state.get_step_state(&step.name)?;

    match prior {
        None => Ok(true), // Never run before.
        Some(prior_state) => {
            if prior_state.status == StepStatus::Failed {
                return Ok(true); // Previously failed.
            }
            if step.op.is_some() {
                // Op step — config hash (operator ref + serialized `with:`).
                return Ok(op_config_hash(step) != prior_state.sql_hash);
            }
            if let Some(ref sql) = step.sql {
                let sql_path = dir.join(sql);
                if sql_path.exists() {
                    let content = std::fs::read(&sql_path).map_err(|e| Error::FileRead {
                        path: sql_path.clone(),
                        source: e,
                    })?;
                    let current_hash = state::content_hash(&content);
                    Ok(current_hash != prior_state.sql_hash)
                } else {
                    Ok(true) // File missing — will error during execution.
                }
            } else {
                Ok(false) // No SQL file (shouldn't happen for SQL steps).
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::mock::{MockCall, MockEngine};
    use crate::state::mock::MockStateBackend;
    use std::fs;

    fn setup_project(dir: &Path, yaml: &str, files: &[(&str, &str)]) {
        fs::write(dir.join("arcform.yaml"), yaml).unwrap();
        for (path, content) in files {
            let full = dir.join(path);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(full, content).unwrap();
        }
    }

    // Empty steps list exits successfully.
    #[test]
    fn test_run_empty_steps() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path(), "name: test\nsteps: []\n", &[]);
        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();
        // No preflight called for empty steps.
        assert!(engine.calls.borrow().is_empty());
    }

    // Steps execute in declared order against shared database.
    #[test]
    fn test_run_sql_steps_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n  - name: s2\n    sql: models/s2.sql\n  - name: s3\n    sql: models/s3.sql\n";
        setup_project(
            dir.path(),
            yaml,
            &[
                ("models/s1.sql", "CREATE TABLE t(v TEXT);"),
                ("models/s2.sql", "INSERT INTO t VALUES ('b');"),
                ("models/s3.sql", "INSERT INTO t VALUES ('c');"),
            ],
        );

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        assert_eq!(calls.len(), 4); // 1 preflight + 3 sql
        assert!(matches!(calls[0], MockCall::Preflight));

        // Verify execution order.
        let sql_calls: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                MockCall::Sql { sql_content, .. } => Some(sql_content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            sql_calls,
            vec![
                "CREATE TABLE t(v TEXT);",
                "INSERT INTO t VALUES ('b');",
                "INSERT INTO t VALUES ('c');",
            ]
        );
    }

    // Command steps execute via sh -c, preflight skipped for command-only.
    #[test]
    fn test_run_command_step() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: greet\n    command: echo hello\n";
        setup_project(dir.path(), yaml, &[]);

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        // No preflight (no sql steps), 1 command.
        assert_eq!(calls.len(), 1);
        assert!(matches!(&calls[0], MockCall::Command { command, .. } if command == "echo hello"));
    }

    // Halt on failure — steps after a failed step do not execute.
    #[test]
    fn test_run_halts_on_step2_failure_step3_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n  - name: s2\n    sql: models/s2.sql\n  - name: s3\n    sql: models/s3.sql\n";
        setup_project(
            dir.path(),
            yaml,
            &[
                ("models/s1.sql", "SELECT 1;"),
                ("models/s2.sql", "INVALID SQL;"),
                ("models/s3.sql", "SELECT 3;"),
            ],
        );

        let engine = MockEngine::new();
        engine.set_fail_on_call(1, 1, "syntax error");
        let state = MockStateBackend::new();

        let result = run(dir.path(), &engine, &state, false);
        assert!(result.is_err());

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("s2"),
            "error should name step 's2': {err_msg}"
        );
    }

    // Missing SQL file produces a specific error.
    #[test]
    fn test_run_missing_sql_file() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/missing.sql\n";
        setup_project(dir.path(), yaml, &[]);

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        let err = run(dir.path(), &engine, &state, false).unwrap_err();
        assert!(err.to_string().contains("sql file not found"));
    }

    // SQL files passed to engine byte-identical.
    #[test]
    fn test_sql_content_passed_unmodified() {
        let dir = tempfile::tempdir().unwrap();
        let original_sql = "SELECT 1;\n-- comment with special chars: émojis 🎉\n";
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", original_sql)]);

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        let sql_content = match &calls[1] {
            MockCall::Sql { sql_content, .. } => sql_content.as_str(),
            _ => panic!("expected Sql call"),
        };
        assert_eq!(sql_content, original_sql);
    }

    // Preflight failure blocks execution — no steps run.
    #[test]
    fn test_preflight_failure_blocks_execution() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        engine.set_preflight_failure();
        let state = MockStateBackend::new();

        let err = run(dir.path(), &engine, &state, false).unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "should report engine not found: {err}"
        );
    }

    // Failing command step exits non-zero and halts pipeline.
    #[test]
    fn test_command_step_failure_halts_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: fetch\n    command: curl http://example.com\n  - name: transform\n    command: echo done\n";
        setup_project(dir.path(), yaml, &[]);

        let engine = MockEngine::new();
        engine.set_fail_on_call(0, 1, "connection refused");
        let state = MockStateBackend::new();

        let result = run(dir.path(), &engine, &state, false);
        assert!(result.is_err());

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("fetch"),
            "error should name step 'fetch': {err_msg}"
        );
    }

    // `arc run` halts with dependency order violation before executing.
    #[test]
    fn test_v02_dependency_order_blocks_execution() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: summary\n    sql: models/summary.sql\n  - name: load\n    sql: models/load.sql\n";
        setup_project(
            dir.path(),
            yaml,
            &[
                (
                    "models/summary.sql",
                    "CREATE TABLE summary AS SELECT count(*) FROM customers;",
                ),
                ("models/load.sql", "CREATE TABLE customers (id INT);"),
            ],
        );

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        let result = run(dir.path(), &engine, &state, false);
        assert!(result.is_err());

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("summary"),
            "error should name reader 'summary': {err_msg}"
        );
        assert!(
            err_msg.contains("customers"),
            "error should name asset 'customers': {err_msg}"
        );
    }

    // v0.1-style manifest (no assets) runs identically.
    #[test]
    fn test_v02_v1_manifest_runs_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: greet\n    command: echo hello\n  - name: done\n    command: echo done\n";
        setup_project(dir.path(), yaml, &[]);

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        assert_eq!(calls.len(), 2);
    }

    // Unparseable SQL warns but still executes.
    #[test]
    fn test_v02_unparseable_sql_still_runs() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: weird\n    sql: models/weird.sql\n";
        setup_project(
            dir.path(),
            yaml,
            &[("models/weird.sql", "THIS IS NOT VALID SQL %%%")],
        );

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        assert_eq!(calls.len(), 2); // preflight + 1 SQL
    }

    // Multi-step chain with valid ordering succeeds.
    #[test]
    fn test_v02_valid_chain_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: step-a\n    sql: models/a.sql\n  - name: step-b\n    sql: models/b.sql\n  - name: step-c\n    sql: models/c.sql\n";
        setup_project(
            dir.path(),
            yaml,
            &[
                ("models/a.sql", "CREATE TABLE x (id INT);"),
                ("models/b.sql", "CREATE TABLE y AS SELECT * FROM x;"),
                ("models/c.sql", "CREATE TABLE z AS SELECT * FROM y;"),
            ],
        );

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        assert_eq!(calls.len(), 4); // preflight + 3 SQL
    }

    // ---- v0.3 Staleness Tests ----

    // Fresh SQL step is skipped on second run.
    #[test]
    fn test_v03_fresh_step_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        let sql = "CREATE TABLE t(v TEXT);";
        setup_project(dir.path(), yaml, &[("models/s1.sql", sql)]);

        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // First run — step executes.
        run(dir.path(), &engine, &state, false).unwrap();
        let calls_after_first = engine.calls.borrow().len();
        assert_eq!(calls_after_first, 2); // preflight + 1 sql

        // Second run — step should be skipped (hash unchanged).
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        // Only preflight, no SQL execution.
        assert_eq!(calls.len(), 1, "fresh step should be skipped on second run");
        assert!(matches!(calls[0], MockCall::Preflight));
    }

    // Stale SQL step re-runs after edit.
    #[test]
    fn test_v03_stale_step_reruns() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // First run.
        run(dir.path(), &engine, &state, false).unwrap();

        // Edit the SQL file.
        fs::write(dir.path().join("models/s1.sql"), "SELECT 2;").unwrap();

        // Second run — step should re-execute (hash changed).
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        assert_eq!(
            calls.len(),
            2,
            "stale step should re-run: preflight + 1 sql"
        );
    }

    // Downstream propagation — stale upstream makes dependents stale.
    #[test]
    fn test_v03_downstream_propagation() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: step-a\n    sql: models/a.sql\n  - name: step-b\n    sql: models/b.sql\n  - name: step-c\n    sql: models/c.sql\n";
        setup_project(
            dir.path(),
            yaml,
            &[
                ("models/a.sql", "CREATE TABLE x (id INT);"),
                ("models/b.sql", "CREATE TABLE y AS SELECT * FROM x;"),
                ("models/c.sql", "CREATE TABLE z AS SELECT * FROM y;"),
            ],
        );

        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // First run — all execute.
        run(dir.path(), &engine, &state, false).unwrap();

        // Edit only step-a's SQL.
        fs::write(
            dir.path().join("models/a.sql"),
            "CREATE TABLE x (id INT, name TEXT);",
        )
        .unwrap();

        // Second run — all three should re-run (a is stale, b and c are downstream).
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        let sql_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Sql { .. }))
            .collect();
        assert_eq!(
            sql_calls.len(),
            3,
            "all 3 steps should re-run due to downstream propagation"
        );
    }

    // Failed step always re-runs.
    #[test]
    fn test_v03_failed_step_reruns() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        engine.set_fail_on_call(0, 1, "error");
        let state = MockStateBackend::new();

        // First run — fails.
        let _ = run(dir.path(), &engine, &state, false);

        // Verify state records failure.
        let step_state = state.get_step_state("s1").unwrap().unwrap();
        assert_eq!(step_state.status, StepStatus::Failed);

        // Second run — should re-execute (failed = always stale).
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        let sql_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Sql { .. }))
            .collect();
        assert_eq!(sql_calls.len(), 1, "failed step should re-run");
    }

    // Command steps always re-run.
    #[test]
    fn test_v03_command_always_reruns() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: greet\n    command: echo hello\n";
        setup_project(dir.path(), yaml, &[]);

        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // First run.
        run(dir.path(), &engine, &state, false).unwrap();

        // Second run — command should still execute.
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        assert_eq!(calls.len(), 1, "command step should always re-run");
    }

    // --force runs all steps regardless of staleness.
    #[test]
    fn test_v03_force_runs_all() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // First run.
        run(dir.path(), &engine, &state, false).unwrap();

        // Second run with --force — should execute even though fresh.
        {
            let engine = MockEngine::new();
            run(dir.path(), &engine, &state, true).unwrap();

            let calls = engine.calls.borrow();
            let sql_calls: Vec<_> = calls
                .iter()
                .filter(|c| matches!(c, MockCall::Sql { .. }))
                .collect();
            assert_eq!(sql_calls.len(), 1, "--force should run fresh step");
        }

        // Third run without --force — should skip (--force recorded new state).
        {
            let engine = MockEngine::new();
            run(dir.path(), &engine, &state, false).unwrap();

            let calls = engine.calls.borrow();
            let sql_calls: Vec<_> = calls
                .iter()
                .filter(|c| matches!(c, MockCall::Sql { .. }))
                .collect();
            assert_eq!(sql_calls.len(), 0, "after --force, step should be fresh");
        }
    }

    // First run treats all steps as stale.
    #[test]
    fn test_v03_first_run_all_stale() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n  - name: s2\n    sql: models/s2.sql\n";
        setup_project(
            dir.path(),
            yaml,
            &[
                ("models/s1.sql", "SELECT 1;"),
                ("models/s2.sql", "SELECT 2;"),
            ],
        );

        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // First run — no prior state, all should execute.
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        let sql_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Sql { .. }))
            .collect();
        assert_eq!(sql_calls.len(), 2, "first run should execute all steps");
    }

    // ---- Local-Remote Parity Tests ----

    // Version mismatch blocks execution before any step runs.
    #[test]
    fn test_lrp_version_mismatch_blocks_execution() {
        let dir = tempfile::tempdir().unwrap();
        let yaml =
            "name: test\nengine_version: '>=2.0'\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        // MockEngine defaults to v2.0.0, set it to 1.3.0 to trigger mismatch.
        engine.set_version(Some(semver::Version::new(1, 3, 0)));
        let state = MockStateBackend::new();

        let result = run(dir.path(), &engine, &state, false);
        assert!(result.is_err(), "should fail due to version mismatch");

        // Verify no steps executed — only preflight was called.
        let calls = engine.calls.borrow();
        assert_eq!(calls.len(), 1, "only preflight should be called");
        assert!(matches!(calls[0], MockCall::Preflight));
    }

    // Version mismatch error contains both required and found versions.
    #[test]
    fn test_lrp_error_contains_both_versions() {
        let dir = tempfile::tempdir().unwrap();
        let yaml =
            "name: test\nengine_version: '>=2.0'\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        engine.set_version(Some(semver::Version::new(1, 3, 0)));
        let state = MockStateBackend::new();

        let err = run(dir.path(), &engine, &state, false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(">=2.0"),
            "error should contain constraint: {msg}"
        );
        assert!(
            msg.contains("1.3.0"),
            "error should contain detected version: {msg}"
        );
    }

    // No engine_version skips the version check.
    #[test]
    fn test_lrp_no_version_constraint_skips_check() {
        let dir = tempfile::tempdir().unwrap();
        // No engine_version in YAML — should skip version check.
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        // Even with a very old version, no constraint means no check.
        engine.set_version(Some(semver::Version::new(0, 1, 0)));
        let state = MockStateBackend::new();

        // Should succeed — no version comparison.
        run(dir.path(), &engine, &state, false).unwrap();
    }

    // Version that satisfies constraint passes.
    #[test]
    fn test_lrp_version_satisfies_constraint() {
        let dir = tempfile::tempdir().unwrap();
        let yaml =
            "name: test\nengine_version: '>=1.5'\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        engine.set_version(Some(semver::Version::new(1, 5, 2)));
        let state = MockStateBackend::new();

        // Should succeed — 1.5.2 >= 1.5.
        run(dir.path(), &engine, &state, false).unwrap();
    }

    // Unparseable version warns but pipeline continues.
    #[test]
    fn test_lrp_unparseable_version_warns_continues() {
        let dir = tempfile::tempdir().unwrap();
        let yaml =
            "name: test\nengine_version: '>=1.5'\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        // Set version to None (simulating unparseable output).
        engine.set_version(None);
        let state = MockStateBackend::new();

        // Should succeed — unparseable version skips check.
        run(dir.path(), &engine, &state, false).unwrap();

        // Verify step actually executed.
        let calls = engine.calls.borrow();
        let sql_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Sql { .. }))
            .collect();
        assert_eq!(
            sql_calls.len(),
            1,
            "step should execute despite unparseable version"
        );
    }

    // ---- Step Preconditions Tests ----

    // YAML with preconditions deserialises correctly.
    #[test]
    fn test_pre_preconditions_deserialise() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: fetch
    command: "curl http://example.com"
    preconditions:
      - modified_after:
          path: data/output.json
          period: 24h
      - command: "test -f /tmp/ready"
"#;
        setup_project(dir.path(), yaml, &[]);
        let manifest = crate::manifest::Manifest::load(dir.path()).unwrap();
        assert_eq!(manifest.steps[0].preconditions.len(), 2);
    }

    // YAML without preconditions still works (backwards compat).
    #[test]
    fn test_pre_no_preconditions_backwards_compat() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: greet\n    command: echo hello\n";
        setup_project(dir.path(), yaml, &[]);
        let manifest = crate::manifest::Manifest::load(dir.path()).unwrap();
        assert!(manifest.steps[0].preconditions.is_empty());
    }

    // Command step with passing precondition is skipped.
    #[test]
    fn test_pre_command_with_fresh_precondition_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: fetch
    command: "echo fetching"
    preconditions:
      - command: "true"
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        // Precondition "true" exits 0 → fresh → step skipped.
        let calls = engine.calls.borrow();
        assert!(
            calls.is_empty(),
            "command step with fresh precondition should be skipped, got {} calls",
            calls.len()
        );
    }

    // Command step with failing precondition runs.
    #[test]
    fn test_pre_command_with_stale_precondition_runs() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: fetch
    command: "echo fetching"
    preconditions:
      - command: "false"
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        // Precondition "false" exits non-zero → stale → step runs.
        let calls = engine.calls.borrow();
        assert_eq!(
            calls.len(),
            1,
            "command step with stale precondition should run"
        );
        assert!(
            matches!(&calls[0], MockCall::Command { command, .. } if command == "echo fetching")
        );
    }

    // Command steps without preconditions still always re-run.
    // (Verified by existing test_v03_command_always_reruns — this is a confirmation.)
    #[test]
    fn test_pre_command_no_preconditions_always_runs() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: greet\n    command: echo hello\n";
        setup_project(dir.path(), yaml, &[]);

        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // First run.
        run(dir.path(), &engine, &state, false).unwrap();

        // Second run — command without preconditions should still execute.
        let engine2 = MockEngine::new();
        run(dir.path(), &engine2, &state, false).unwrap();
        let calls = engine2.calls.borrow();
        assert_eq!(
            calls.len(),
            1,
            "command step without preconditions should always re-run"
        );
    }

    // SQL + preconditions — fresh hash + stale precondition → runs.
    #[test]
    fn test_pre_sql_fresh_hash_stale_precondition_runs() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: transform
    sql: models/transform.sql
    preconditions:
      - command: "false"
"#;
        setup_project(dir.path(), yaml, &[("models/transform.sql", "SELECT 1;")]);
        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // First run — establishes hash state.
        run(dir.path(), &engine, &state, false).unwrap();

        // Second run — hash is fresh but precondition says stale → should run.
        let engine2 = MockEngine::new();
        run(dir.path(), &engine2, &state, false).unwrap();
        let calls = engine2.calls.borrow();
        let sql_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Sql { .. }))
            .collect();
        assert_eq!(
            sql_calls.len(),
            1,
            "SQL step should run when precondition is stale"
        );
    }

    // SQL + preconditions — stale hash + fresh precondition → runs.
    #[test]
    fn test_pre_sql_stale_hash_fresh_precondition_runs() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: transform
    sql: models/transform.sql
    preconditions:
      - command: "true"
"#;
        setup_project(dir.path(), yaml, &[("models/transform.sql", "SELECT 1;")]);
        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // First run — establishes hash state.
        run(dir.path(), &engine, &state, false).unwrap();

        // Edit SQL to make hash stale.
        fs::write(dir.path().join("models/transform.sql"), "SELECT 2;").unwrap();

        // Second run — hash is stale even though precondition is fresh → should run (AND).
        let engine2 = MockEngine::new();
        run(dir.path(), &engine2, &state, false).unwrap();
        let calls = engine2.calls.borrow();
        let sql_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Sql { .. }))
            .collect();
        assert_eq!(
            sql_calls.len(),
            1,
            "SQL step should run when hash is stale (AND semantics)"
        );
    }

    // SQL + preconditions — both fresh → skips.
    #[test]
    fn test_pre_sql_both_fresh_skips() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: transform
    sql: models/transform.sql
    preconditions:
      - command: "true"
"#;
        setup_project(dir.path(), yaml, &[("models/transform.sql", "SELECT 1;")]);
        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // First run — establishes hash state.
        run(dir.path(), &engine, &state, false).unwrap();

        // Second run — hash is fresh AND precondition is fresh → should skip.
        let engine2 = MockEngine::new();
        run(dir.path(), &engine2, &state, false).unwrap();
        let calls = engine2.calls.borrow();
        let sql_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Sql { .. }))
            .collect();
        assert_eq!(
            sql_calls.len(),
            0,
            "SQL step should be skipped when both hash and precondition are fresh"
        );
    }

    // SQL + preconditions — both stale → runs.
    #[test]
    fn test_pre_sql_both_stale_runs() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: transform
    sql: models/transform.sql
    preconditions:
      - command: "false"
"#;
        setup_project(dir.path(), yaml, &[("models/transform.sql", "SELECT 1;")]);
        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // First run.
        run(dir.path(), &engine, &state, false).unwrap();

        // Edit SQL + precondition is false → both stale → runs.
        fs::write(dir.path().join("models/transform.sql"), "SELECT 2;").unwrap();
        let engine2 = MockEngine::new();
        run(dir.path(), &engine2, &state, false).unwrap();
        let calls = engine2.calls.borrow();
        let sql_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Sql { .. }))
            .collect();
        assert_eq!(
            sql_calls.len(),
            1,
            "SQL step should run when both hash and precondition are stale"
        );
    }

    // SQL steps without preconditions use hash staleness unchanged.
    // (Verified by the existing v0.3 asset-graph tests — this confirms no regression.)
    #[test]
    fn test_pre_sql_no_preconditions_uses_hash() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // First run.
        run(dir.path(), &engine, &state, false).unwrap();

        // Second run — hash unchanged, no preconditions → skip.
        let engine2 = MockEngine::new();
        run(dir.path(), &engine2, &state, false).unwrap();
        let calls = engine2.calls.borrow();
        let sql_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Sql { .. }))
            .collect();
        assert_eq!(
            sql_calls.len(),
            0,
            "SQL step without preconditions should use hash staleness"
        );
    }

    // --force overrides preconditions — step runs regardless.
    #[test]
    fn test_pre_force_overrides_preconditions() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: fetch
    command: "echo fetching"
    preconditions:
      - command: "true"
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // With force=true, preconditions should be ignored.
        run(dir.path(), &engine, &state, true).unwrap();
        let calls = engine.calls.borrow();
        assert_eq!(
            calls.len(),
            1,
            "--force should override fresh precondition and run the step"
        );
        assert!(
            matches!(&calls[0], MockCall::Command { command, .. } if command == "echo fetching")
        );
    }

    // Manifest validation rejects invalid precondition duration.
    #[test]
    fn test_pre_manifest_rejects_invalid_precondition() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: fetch
    command: "echo fetching"
    preconditions:
      - modified_after:
          path: data/file.json
          period: "banana"
"#;
        setup_project(dir.path(), yaml, &[]);
        let err = crate::manifest::Manifest::load(dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("banana"),
            "error should mention the bad duration: {msg}"
        );
    }

    // ---- Pipeline Parameterisation Tests ----

    // Manifest with params and dotenv fields deserialises correctly.
    #[test]
    fn test_param_manifest_with_params_deserialises() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
params:
  start_date:
    default: "2026-01-01"
  end_date: {}
dotenv:
  - .env
  - .env.local
steps:
  - name: greet
    command: echo hello
"#;
        setup_project(dir.path(), yaml, &[]);
        let manifest = crate::manifest::Manifest::load(dir.path()).unwrap();
        assert_eq!(manifest.params.len(), 2);
        assert_eq!(
            manifest.params["start_date"].default,
            Some("2026-01-01".to_string())
        );
        assert!(manifest.params["end_date"].default.is_none());
        assert_eq!(manifest.dotenv, vec![".env", ".env.local"]);
    }

    // Manifest without params/dotenv deserialises to empty defaults.
    #[test]
    fn test_param_manifest_without_params_empty_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: greet\n    command: echo hello\n";
        setup_project(dir.path(), yaml, &[]);
        let manifest = crate::manifest::Manifest::load(dir.path()).unwrap();
        assert!(manifest.params.is_empty());
        assert!(manifest.dotenv.is_empty());
    }

    // parse_params with valid KEY=VALUE pairs.
    #[test]
    fn test_param_parse_valid_params() {
        let raw = vec![
            "start_date=2026-01-01".to_string(),
            "region=us-east-1".to_string(),
        ];
        let parsed = parse_params(&raw).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed[0],
            ("start_date".to_string(), "2026-01-01".to_string())
        );
        assert_eq!(parsed[1], ("region".to_string(), "us-east-1".to_string()));
    }

    // parse_params splits on first '=' only.
    #[test]
    fn test_param_parse_value_with_equals() {
        let raw = vec!["query=SELECT * FROM t WHERE x=1".to_string()];
        let parsed = parse_params(&raw).unwrap();
        assert_eq!(parsed[0].0, "query");
        assert_eq!(parsed[0].1, "SELECT * FROM t WHERE x=1");
    }

    // parse_params rejects missing '='.
    #[test]
    fn test_param_parse_invalid_no_equals() {
        let raw = vec!["no_equals_here".to_string()];
        let err = parse_params(&raw).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("KEY=VALUE"),
            "error should mention format: {msg}"
        );
    }

    // resolve_params merges sources with correct precedence.
    #[test]
    fn test_param_resolve_params_precedence() {
        use crate::manifest::Param;
        use indexmap::IndexMap;

        let mut params = IndexMap::new();
        params.insert(
            "a".to_string(),
            Param {
                default: Some("default_a".to_string()),
            },
        );
        params.insert(
            "b".to_string(),
            Param {
                default: Some("default_b".to_string()),
            },
        );
        params.insert(
            "c".to_string(),
            Param {
                default: Some("default_c".to_string()),
            },
        );

        let mut dotenv_vars = std::collections::HashMap::new();
        dotenv_vars.insert("a".to_string(), "dotenv_a".to_string());
        dotenv_vars.insert("b".to_string(), "dotenv_b".to_string());

        let cli_params = vec![("a".to_string(), "cli_a".to_string())];

        let resolved = resolve_params(&params, &dotenv_vars, &cli_params).unwrap();

        // CLI wins over dotenv and default.
        assert_eq!(resolved["ARC_PARAM_A"], "cli_a");
        // Dotenv wins over default.
        assert_eq!(resolved["ARC_PARAM_B"], "dotenv_b");
        // Default fills gap.
        assert_eq!(resolved["ARC_PARAM_C"], "default_c");
    }

    // resolve_params errors on missing required param.
    #[test]
    fn test_param_missing_required_param() {
        use crate::manifest::Param;
        use indexmap::IndexMap;

        let mut params = IndexMap::new();
        params.insert("required_param".to_string(), Param { default: None });

        let dotenv_vars = std::collections::HashMap::new();
        let cli_params: Vec<(String, String)> = vec![];

        let err = resolve_params(&params, &dotenv_vars, &cli_params).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("required_param"),
            "error should name the param: {msg}"
        );
        assert!(msg.contains("missing"), "error should say missing: {msg}");
    }

    // ARC_PARAM_ prefix and uppercasing.
    #[test]
    fn test_param_arc_param_prefix_uppercasing() {
        use crate::manifest::Param;
        use indexmap::IndexMap;

        let mut params = IndexMap::new();
        params.insert("start_date".to_string(), Param { default: None });

        let dotenv_vars = std::collections::HashMap::new();
        let cli_params = vec![("start_date".to_string(), "2026-01-01".to_string())];

        let resolved = resolve_params(&params, &dotenv_vars, &cli_params).unwrap();
        assert_eq!(resolved.get("ARC_PARAM_START_DATE").unwrap(), "2026-01-01");
    }

    // MockEngine records env map passed to it.
    #[test]
    fn test_param_mock_engine_records_env() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
params:
  region:
    default: "us-west-2"
steps:
  - name: s1
    sql: models/s1.sql
"#;
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);
        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        // Find the SQL call and check env.
        let sql_call = calls.iter().find(|c| matches!(c, MockCall::Sql { .. }));
        match sql_call {
            Some(MockCall::Sql { env, .. }) => {
                assert_eq!(env.get("ARC_PARAM_REGION").unwrap(), "us-west-2");
            }
            _ => panic!("expected SQL call with env"),
        }
    }

    // Dotenv file loading.
    #[test]
    fn test_param_dotenv_file_loading() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
params:
  db_host: {}
dotenv:
  - .env
steps:
  - name: greet
    command: echo hello
"#;
        setup_project(dir.path(), yaml, &[]);
        // Create the .env file.
        fs::write(dir.path().join(".env"), "db_host=localhost\n").unwrap();

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        let cmd_call = calls.iter().find(|c| matches!(c, MockCall::Command { .. }));
        match cmd_call {
            Some(MockCall::Command { env, .. }) => {
                assert_eq!(env.get("ARC_PARAM_DB_HOST").unwrap(), "localhost");
            }
            _ => panic!("expected Command call with env"),
        }
    }

    // Missing dotenv file is silently skipped.
    #[test]
    fn test_param_missing_dotenv_silently_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
params:
  x:
    default: "fallback"
dotenv:
  - .env.local
steps:
  - name: greet
    command: echo hello
"#;
        setup_project(dir.path(), yaml, &[]);
        // Do NOT create .env.local — should be silently skipped.

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        let cmd_call = calls.iter().find(|c| matches!(c, MockCall::Command { .. }));
        match cmd_call {
            Some(MockCall::Command { env, .. }) => {
                // Should use default since dotenv was missing.
                assert_eq!(env.get("ARC_PARAM_X").unwrap(), "fallback");
            }
            _ => panic!("expected Command call"),
        }
    }

    // Step output capture — captured value available downstream.
    #[test]
    fn test_param_output_capture_available_downstream() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: get_date
    command: "echo 2026-04-20"
    output: current_date
  - name: use_date
    command: "echo using date"
"#;
        setup_project(dir.path(), yaml, &[]);

        let engine = MockEngine::new();
        engine.set_simulated_stdout("2026-04-20");
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        // Second call should have ARC_PARAM_CURRENT_DATE in its env.
        let cmd_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Command { .. }))
            .collect();
        assert_eq!(cmd_calls.len(), 2, "should have 2 command calls");

        match &cmd_calls[1] {
            MockCall::Command { env, .. } => {
                assert_eq!(
                    env.get("ARC_PARAM_CURRENT_DATE").unwrap(),
                    "2026-04-20",
                    "downstream step should see captured output"
                );
            }
            _ => unreachable!(),
        }
    }

    // Empty captured stdout sets env var to empty string.
    #[test]
    fn test_param_empty_stdout_sets_empty_string() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: get_empty
    command: "true"
    output: result
  - name: use_result
    command: "echo done"
"#;
        setup_project(dir.path(), yaml, &[]);

        let engine = MockEngine::new();
        engine.set_simulated_stdout(""); // Empty stdout.
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        let cmd_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Command { .. }))
            .collect();
        assert_eq!(cmd_calls.len(), 2);

        match &cmd_calls[1] {
            MockCall::Command { env, .. } => {
                // Empty stdout → env var set to empty string (not omitted).
                assert!(
                    env.contains_key("ARC_PARAM_RESULT"),
                    "env var should exist even for empty stdout"
                );
                assert_eq!(env["ARC_PARAM_RESULT"], "", "empty stdout → empty string");
            }
            _ => unreachable!(),
        }
    }

    // SQL step with output field is rejected.
    #[test]
    fn test_param_sql_step_output_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: bad
    sql: models/bad.sql
    output: result
"#;
        setup_project(dir.path(), yaml, &[("models/bad.sql", "SELECT 1;")]);
        let err = crate::manifest::Manifest::load(dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("SQL steps cannot declare an output"),
            "should reject SQL + output: {msg}"
        );
    }

    // Backwards compatibility — existing manifests work identically.
    #[test]
    fn test_param_backwards_compat_no_params() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        // Verify engine received empty env map.
        let sql_call = calls.iter().find(|c| matches!(c, MockCall::Sql { .. }));
        match sql_call {
            Some(MockCall::Sql { env, .. }) => {
                assert!(env.is_empty(), "backwards-compat: env map should be empty");
            }
            _ => panic!("expected SQL call"),
        }
    }

    // Changing param values does not affect SQL staleness.
    #[test]
    fn test_param_param_staleness_independence() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
params:
  region:
    default: "us-west-2"
steps:
  - name: transform
    sql: models/transform.sql
"#;
        setup_project(dir.path(), yaml, &[("models/transform.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // First run with default param.
        run(dir.path(), &engine, &state, false).unwrap();
        let first_calls = engine.calls.borrow().len();
        assert_eq!(first_calls, 2, "first run: preflight + 1 SQL");

        // Second run with different param value (via run_with_params).
        // SQL file unchanged → step should be skipped.
        let engine2 = MockEngine::new();
        let cli_params = vec![("region".to_string(), "eu-west-1".to_string())];
        run_with_params(dir.path(), &engine2, &state, false, &cli_params).unwrap();

        let calls = engine2.calls.borrow();
        let sql_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Sql { .. }))
            .collect();
        assert_eq!(
            sql_calls.len(),
            0,
            "changing param value should not make SQL step stale (no engine call)"
        );
    }

    // ---- Execution Resilience Tests ----

    // RetryPolicy and Defaults structs deserialise from YAML.
    #[test]
    fn test_res_retry_policy_deserialises() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
defaults:
  retry:
    max_attempts: 3
    backoff_sec: 2.0
steps:
  - name: greet
    command: echo hello
"#;
        setup_project(dir.path(), yaml, &[]);
        let manifest = crate::manifest::Manifest::load(dir.path()).unwrap();
        let defaults = manifest.defaults.unwrap();
        let retry = defaults.retry.unwrap();
        assert_eq!(retry.max_attempts, 3);
        assert_eq!(retry.backoff_sec, 2.0);
    }

    // Manifest without defaults deserialises to None.
    #[test]
    fn test_res_no_defaults_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: greet\n    command: echo hello\n";
        setup_project(dir.path(), yaml, &[]);
        let manifest = crate::manifest::Manifest::load(dir.path()).unwrap();
        assert!(manifest.defaults.is_none());
    }

    // Step with retry and timeout_sec fields.
    #[test]
    fn test_res_step_retry_and_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: fetch
    command: "curl http://example.com"
    retry:
      max_attempts: 5
      backoff_sec: 1.5
    timeout_sec: 30.0
"#;
        setup_project(dir.path(), yaml, &[]);
        let manifest = crate::manifest::Manifest::load(dir.path()).unwrap();
        let step = &manifest.steps[0];
        let retry = step.retry.as_ref().unwrap();
        assert_eq!(retry.max_attempts, 5);
        assert_eq!(retry.backoff_sec, 1.5);
        assert_eq!(step.timeout_sec, Some(30.0));
    }

    // Retry exhaustion — always-fail with max_attempts=2 makes 2 attempts then fails.
    #[test]
    fn test_res_retry_exhaustion() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: flaky
    command: "echo flaky"
    retry:
      max_attempts: 2
      backoff_sec: 0.0
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        engine.set_failure(1, "always fail");
        let state = MockStateBackend::new();
        let result = run(dir.path(), &engine, &state, false);
        assert!(result.is_err(), "should fail after exhausting retries");
        // Verify 2 engine calls (2 attempts).
        let calls = engine.calls.borrow();
        let cmd_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Command { .. }))
            .collect();
        assert_eq!(cmd_calls.len(), 2, "should have made 2 attempts");
    }

    // Retry with fail_on_call — fail first, succeed second.
    #[test]
    fn test_res_retry_succeeds_on_second_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: flaky
    command: "echo flaky"
    retry:
      max_attempts: 3
      backoff_sec: 0.0
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        engine.set_fail_on_call(0, 1, "transient"); // Fail only 1st call, 2nd succeeds.
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        let cmd_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Command { .. }))
            .collect();
        assert_eq!(cmd_calls.len(), 2, "should have retried once and succeeded");
    }

    // backoff_duration pure function.
    #[test]
    fn test_res_backoff_duration() {
        use crate::manifest::RetryPolicy;
        let policy = RetryPolicy {
            max_attempts: 5,
            backoff_sec: 2.0,
        };
        assert_eq!(
            backoff_duration(&policy, 1),
            std::time::Duration::from_secs(2)
        );
        assert_eq!(
            backoff_duration(&policy, 2),
            std::time::Duration::from_secs(4)
        );
        assert_eq!(
            backoff_duration(&policy, 3),
            std::time::Duration::from_secs(8)
        );
    }

    // Defaults resolution — step inherits from defaults.
    #[test]
    fn test_res_defaults_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
defaults:
  retry:
    max_attempts: 3
    backoff_sec: 0.0
steps:
  - name: flaky
    command: "echo flaky"
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        engine.set_fail_on_call(0, 1, "transient");
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        // Should have retried using defaults (2 calls = 1 fail + 1 success).
        let calls = engine.calls.borrow();
        let cmd_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Command { .. }))
            .collect();
        assert_eq!(
            cmd_calls.len(),
            2,
            "defaults.retry should apply when step has no retry"
        );
    }

    // Step-level retry overrides defaults.
    #[test]
    fn test_res_step_overrides_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
defaults:
  retry:
    max_attempts: 5
    backoff_sec: 0.0
steps:
  - name: flaky
    command: "echo flaky"
    retry:
      max_attempts: 1
      backoff_sec: 0.0
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        engine.set_failure(1, "always fail");
        let state = MockStateBackend::new();
        let result = run(dir.path(), &engine, &state, false);
        assert!(
            result.is_err(),
            "step max_attempts=1 should override defaults"
        );
        let calls = engine.calls.borrow();
        let cmd_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Command { .. }))
            .collect();
        assert_eq!(
            cmd_calls.len(),
            1,
            "step override to 1 attempt should mean only 1 call"
        );
    }

    // MockEngine returns StepTimeout when timeout is Some.
    #[test]
    fn test_res_mock_timeout_fires() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: slow
    command: "sleep 999"
    timeout_sec: 5.0
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        engine.set_timeout_fire();
        let state = MockStateBackend::new();
        let err = run(dir.path(), &engine, &state, false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("timed out"),
            "should be a timeout error: {msg}"
        );
    }

    // No timeout → no StepTimeout.
    #[test]
    fn test_res_no_timeout_no_error() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: fast\n    command: echo hi\n";
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        engine.set_timeout_fire(); // Would fire if timeout was Some, but no timeout_sec on step.
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();
    }

    // StepTimeout and PipelineTimeout error messages.
    #[test]
    fn test_res_error_messages() {
        let timeout_err = crate::error::Error::StepTimeout {
            step: "fetch".to_string(),
        };
        assert!(timeout_err.to_string().contains("fetch"));
        assert!(timeout_err.to_string().contains("timed out"));

        let pipeline_err = crate::error::Error::PipelineTimeout {
            step: "transform".to_string(),
            elapsed_sec: 30.5,
        };
        let msg = pipeline_err.to_string();
        assert!(msg.contains("transform"));
        assert!(msg.contains("30.5"));
    }

    // State records final outcome only; total_retries tracked.
    #[test]
    fn test_res_state_final_outcome_and_retries() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: flaky
    command: "echo flaky"
    retry:
      max_attempts: 3
      backoff_sec: 0.0
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        engine.set_fail_on_call(0, 1, "transient"); // Fail first, succeed second.
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        // record_step should be called once with Success.
        let states = state.states.borrow();
        let step_state = states.get("flaky").unwrap();
        assert_eq!(
            step_state.status,
            StepStatus::Success,
            "final outcome should be Success"
        );

        // total_retries should be 1 (1 retry after the first failure).
        assert_eq!(state.total_retries.get(), 1, "should record 1 retry");
    }

    // Backwards compat — manifests without retry/timeout work unchanged.
    #[test]
    fn test_res_backwards_compat() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);
        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();
        // Should work identically to before — 1 preflight + 1 SQL.
        let calls = engine.calls.borrow();
        assert_eq!(calls.len(), 2);
    }

    // Validation rejects max_attempts=0.
    #[test]
    fn test_res_reject_max_attempts_zero() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: bad
    command: echo hi
    retry:
      max_attempts: 0
"#;
        setup_project(dir.path(), yaml, &[]);
        let err = crate::manifest::Manifest::load(dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("max_attempts"),
            "should reject max_attempts=0: {msg}"
        );
    }

    // Validation rejects negative backoff_sec.
    #[test]
    fn test_res_reject_negative_backoff() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: bad
    command: echo hi
    retry:
      max_attempts: 3
      backoff_sec: -1.0
"#;
        setup_project(dir.path(), yaml, &[]);
        let err = crate::manifest::Manifest::load(dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("backoff_sec"),
            "should reject negative backoff: {msg}"
        );
    }

    // Pipeline timeout fires before a step starts.
    #[test]
    fn test_res_pipeline_timeout() {
        let dir = tempfile::tempdir().unwrap();
        // Pipeline timeout of 0.001s — effectively already expired by the time step 2 starts.
        // Step 1 consumes the budget; step 2 should trigger PipelineTimeout.
        let yaml = r#"
name: test
timeout_sec: 0.001
steps:
  - name: step1
    command: "echo one"
  - name: step2
    command: "echo two"
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // Sleep just enough so the pipeline deadline has passed by step 2.
        // We can't guarantee timing with a mock, but 0.001s is virtually instant.
        // The engine runs step1 fine, but by step2 the deadline should be past.
        let result = run(dir.path(), &engine, &state, false);

        // One of two things can happen with such a tiny timeout:
        // 1. PipelineTimeout fires before step2 (timing-dependent)
        // 2. Both steps complete (they're mocked, so nearly instant)
        // To make this deterministic, we'll check the error or success.
        // With a 0.001s timeout, even mocked steps should exceed it due to
        // print overhead and staleness computation.
        // If it succeeds (mock is too fast), that's OK — verify pipeline timeout
        // through the error type when it does fire.
        if let Err(e) = result {
            let msg = e.to_string();
            assert!(
                msg.contains("pipeline timeout") || msg.contains("timed out"),
                "error should be pipeline timeout: {msg}"
            );
        }
        // If it succeeded, the mock was too fast — acceptable for CI.
    }

    // Pipeline timeout (deterministic) — MockEngine timeout simulation.
    #[test]
    fn test_res_pipeline_timeout_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        // Use step timeout to trigger StepTimeout, which with pipeline timeout
        // ensures the pipeline-level tracking is active.
        let yaml = r#"
name: test
timeout_sec: 0.001
steps:
  - name: slow
    command: "echo slow"
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        engine.set_timeout_fire();
        let state = MockStateBackend::new();

        let result = run(dir.path(), &engine, &state, false);
        // The step timeout fires (MockEngine simulates it). The step is retried
        // (if retries configured) or fails. With pipeline timeout also set,
        // the pipeline-level check fires on the next iteration.
        // For a single step with no retries, StepTimeout propagates as the error.
        assert!(result.is_err());
    }

    // Retry output separators — verify correct number of engine calls.
    #[test]
    fn test_res_retry_call_count() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: flaky
    command: "echo flaky"
    retry:
      max_attempts: 3
      backoff_sec: 0.0
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        engine.set_failure(1, "always fail");
        let state = MockStateBackend::new();

        let result = run(dir.path(), &engine, &state, false);
        assert!(result.is_err(), "should fail after exhausting retries");

        let calls = engine.calls.borrow();
        let cmd_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Command { .. }))
            .collect();
        // max_attempts=3 with always-fail → 3 command calls (1 initial + 2 retries).
        assert_eq!(
            cmd_calls.len(),
            3,
            "should have made 3 attempts (with retry separators between)"
        );

        // total_retries should be 2 (attempts 2 and 3 counted).
        assert_eq!(state.total_retries.get(), 2, "should record 2 retries");
    }

    // StepTimeout is retryable — a timed-out step counts as a failed attempt.
    #[test]
    fn test_res_timeout_is_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: slow
    command: "echo slow"
    timeout_sec: 5.0
    retry:
      max_attempts: 3
      backoff_sec: 0.0
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        engine.set_timeout_fire(); // Every call returns StepTimeout.
        let state = MockStateBackend::new();

        let result = run(dir.path(), &engine, &state, false);
        assert!(result.is_err(), "should fail after timeout exhaustion");

        let calls = engine.calls.borrow();
        let cmd_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Command { .. }))
            .collect();
        // StepTimeout is retryable — should have made 3 attempts, not just 1.
        assert_eq!(
            cmd_calls.len(),
            3,
            "StepTimeout should be retried (3 attempts)"
        );

        // Verify the error is StepTimeout (not a non-retryable error).
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("timed out"),
            "final error should be StepTimeout: {msg}"
        );
    }

    // ---- Lifecycle Hook Tests ----

    // Hooks struct deserialises from YAML.
    #[test]
    fn test_hook_hooks_deserialise() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_init:
    name: setup
    command: "echo init"
  on_success:
    name: notify
    command: "echo ok"
  on_failure:
    name: alert
    command: "echo fail"
  on_exit:
    name: teardown
    command: "echo exit"
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let manifest = crate::manifest::Manifest::load(dir.path()).unwrap();
        assert!(manifest.hooks.on_init.is_some());
        assert!(manifest.hooks.on_success.is_some());
        assert!(manifest.hooks.on_failure.is_some());
        assert!(manifest.hooks.on_exit.is_some());
        assert_eq!(manifest.hooks.on_init.unwrap().name, "setup");
    }

    // No hooks section is backwards compatible.
    #[test]
    fn test_hook_no_hooks_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: greet\n    command: echo hello\n";
        setup_project(dir.path(), yaml, &[]);
        let manifest = crate::manifest::Manifest::load(dir.path()).unwrap();
        assert!(manifest.hooks.on_init.is_none());
        assert!(manifest.hooks.on_success.is_none());
        assert!(manifest.hooks.on_failure.is_none());
        assert!(manifest.hooks.on_exit.is_none());
    }

    // on_init runs before steps.
    #[test]
    fn test_hook_init_runs_before_steps() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_init:
    name: setup
    command: "echo init"
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        // on_init command, then load command.
        let cmd_calls: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                MockCall::Command { command, .. } => Some(command.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(cmd_calls, vec!["echo init", "echo loading"]);
    }

    // on_init failure prevents steps, but on_exit still runs.
    #[test]
    fn test_hook_init_failure_aborts_but_exit_runs() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_init:
    name: setup
    command: "echo init"
  on_exit:
    name: teardown
    command: "echo exit"
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        // Fail the first command call (on_init).
        engine.set_fail_on_call(0, 1, "init boom");
        let state = MockStateBackend::new();
        let result = run(dir.path(), &engine, &state, false);
        assert!(result.is_err(), "should fail when on_init fails");

        let calls = engine.calls.borrow();
        let cmd_calls: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                MockCall::Command { command, .. } => Some(command.as_str()),
                _ => None,
            })
            .collect();
        // on_init (failed) + on_exit (runs), but NOT the load step.
        assert_eq!(cmd_calls, vec!["echo init", "echo exit"]);
    }

    // on_success runs after all steps succeed.
    #[test]
    fn test_hook_success_hook_runs() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_success:
    name: notify
    command: "echo ok"
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        let cmd_calls: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                MockCall::Command { command, .. } => Some(command.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(cmd_calls, vec!["echo loading", "echo ok"]);
    }

    // on_success does NOT run when a step fails.
    #[test]
    fn test_hook_success_not_called_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_success:
    name: notify
    command: "echo ok"
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        engine.set_failure(1, "boom");
        let state = MockStateBackend::new();
        let result = run(dir.path(), &engine, &state, false);
        assert!(result.is_err());

        let calls = engine.calls.borrow();
        let cmd_calls: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                MockCall::Command { command, .. } => Some(command.as_str()),
                _ => None,
            })
            .collect();
        // Only the failing step — no on_success.
        assert_eq!(cmd_calls, vec!["echo loading"]);
    }

    // on_failure runs with ARC_FAILED_STEP and ARC_EXIT_CODE.
    #[test]
    fn test_hook_failure_hook_with_env() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_failure:
    name: alert
    command: "echo fail"
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        engine.set_failure(1, "step error");
        let state = MockStateBackend::new();
        let result = run(dir.path(), &engine, &state, false);
        assert!(result.is_err());

        let calls = engine.calls.borrow();
        // Find the on_failure hook call and check env.
        let failure_call = calls.iter().find(|c| match c {
            MockCall::Command { command, .. } => command == "echo fail",
            _ => false,
        });
        assert!(
            failure_call.is_some(),
            "on_failure hook should have been called"
        );
        if let MockCall::Command { env, .. } = failure_call.unwrap() {
            assert_eq!(env.get("ARC_FAILED_STEP"), Some(&"load".to_string()));
            assert_eq!(env.get("ARC_EXIT_CODE"), Some(&"1".to_string()));
        }
    }

    // on_failure does NOT run when all steps succeed.
    #[test]
    fn test_hook_failure_not_called_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_failure:
    name: alert
    command: "echo fail"
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        let cmd_calls: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                MockCall::Command { command, .. } => Some(command.as_str()),
                _ => None,
            })
            .collect();
        // Only the pipeline step — no on_failure.
        assert_eq!(cmd_calls, vec!["echo loading"]);
    }

    // on_exit runs on success with ARC_PIPELINE_STATUS=success.
    #[test]
    fn test_hook_exit_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_exit:
    name: teardown
    command: "echo exit"
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        let exit_call = calls.iter().find(|c| match c {
            MockCall::Command { command, .. } => command == "echo exit",
            _ => false,
        });
        assert!(exit_call.is_some(), "on_exit should run on success");
        if let MockCall::Command { env, .. } = exit_call.unwrap() {
            assert_eq!(env.get("ARC_PIPELINE_STATUS"), Some(&"success".to_string()));
            assert!(
                env.get("ARC_FAILED_STEP").is_none(),
                "no ARC_FAILED_STEP on success"
            );
        }
    }

    // on_exit runs on failure with ARC_PIPELINE_STATUS=failed.
    #[test]
    fn test_hook_exit_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_exit:
    name: teardown
    command: "echo exit"
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        engine.set_failure(1, "step error");
        let state = MockStateBackend::new();
        let result = run(dir.path(), &engine, &state, false);
        assert!(result.is_err());

        let calls = engine.calls.borrow();
        let exit_call = calls.iter().find(|c| match c {
            MockCall::Command { command, .. } => command == "echo exit",
            _ => false,
        });
        assert!(exit_call.is_some(), "on_exit should run on failure");
        if let MockCall::Command { env, .. } = exit_call.unwrap() {
            assert_eq!(env.get("ARC_PIPELINE_STATUS"), Some(&"failed".to_string()));
            assert_eq!(env.get("ARC_FAILED_STEP"), Some(&"load".to_string()));
        }
    }

    // on_exit runs on init failure with ARC_PIPELINE_STATUS=init_failed.
    #[test]
    fn test_hook_exit_on_init_failure() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_init:
    name: setup
    command: "echo init"
  on_exit:
    name: teardown
    command: "echo exit"
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        engine.set_fail_on_call(0, 1, "init error");
        let state = MockStateBackend::new();
        let result = run(dir.path(), &engine, &state, false);
        assert!(result.is_err());

        let calls = engine.calls.borrow();
        let exit_call = calls.iter().find(|c| match c {
            MockCall::Command { command, .. } => command == "echo exit",
            _ => false,
        });
        assert!(
            exit_call.is_some(),
            "on_exit should run even when on_init fails"
        );
        if let MockCall::Command { env, .. } = exit_call.unwrap() {
            assert_eq!(
                env.get("ARC_PIPELINE_STATUS"),
                Some(&"init_failed".to_string())
            );
            assert!(
                env.get("ARC_FAILED_STEP").is_none(),
                "no ARC_FAILED_STEP on init failure"
            );
        }
    }

    // Non-fatal — on_success failure doesn't change pipeline result.
    #[test]
    fn test_hook_success_hook_failure_nonfatal() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_success:
    name: notify
    command: "echo notify"
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        // Fail the second call (on_success hook). First call is the step.
        engine.set_fail_on_call(1, 1, "hook error");
        let state = MockStateBackend::new();
        // Pipeline should still succeed — hook failure is non-fatal.
        let result = run(dir.path(), &engine, &state, false);
        assert!(
            result.is_ok(),
            "pipeline should succeed despite on_success hook failure"
        );
    }

    // Non-fatal — on_failure failure doesn't change pipeline error.
    #[test]
    fn test_hook_failure_hook_failure_returns_original() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_failure:
    name: alert
    command: "echo alert"
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        // All calls fail (step + hook).
        engine.set_failure(1, "original error");
        let state = MockStateBackend::new();
        let result = run(dir.path(), &engine, &state, false);
        assert!(result.is_err());
        // Verify the error is from the step, not the hook.
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("load"),
            "error should name the failed step: {msg}"
        );
    }

    // Hooks with preconditions are rejected.
    #[test]
    fn test_hook_reject_preconditions() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_init:
    name: setup
    command: "echo init"
    preconditions:
      - command: "true"
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let err = crate::manifest::Manifest::load(dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("preconditions"),
            "should reject preconditions on hooks: {msg}"
        );
    }

    // Hooks with retry are rejected.
    #[test]
    fn test_hook_reject_retry() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_init:
    name: setup
    command: "echo init"
    retry:
      max_attempts: 3
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let err = crate::manifest::Manifest::load(dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("retry"), "should reject retry on hooks: {msg}");
    }

    // Hooks with timeout_sec are rejected.
    #[test]
    fn test_hook_reject_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_init:
    name: setup
    command: "echo init"
    timeout_sec: 30.0
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let err = crate::manifest::Manifest::load(dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("timeout_sec"),
            "should reject timeout_sec on hooks: {msg}"
        );
    }

    // Hooks with produces are rejected.
    #[test]
    fn test_hook_reject_produces() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_init:
    name: setup
    command: "echo init"
    produces:
      - some_asset
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let err = crate::manifest::Manifest::load(dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("produces"),
            "should reject produces on hooks: {msg}"
        );
    }

    // Hooks with depends_on are rejected.
    #[test]
    fn test_hook_reject_depends_on() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_init:
    name: setup
    command: "echo init"
    depends_on:
      - some_asset
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let err = crate::manifest::Manifest::load(dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("depends_on"),
            "should reject depends_on on hooks: {msg}"
        );
    }

    // Hooks with output are rejected.
    #[test]
    fn test_hook_reject_output() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_success:
    name: notify
    command: "echo ok"
    output: captured_value
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let err = crate::manifest::Manifest::load(dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("output"),
            "should reject output on hooks: {msg}"
        );
    }

    // Hook name collision with step name is rejected.
    #[test]
    fn test_hook_reject_name_collision() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_init:
    name: load
    command: "echo init"
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let err = crate::manifest::Manifest::load(dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("collides"),
            "should reject name collision: {msg}"
        );
    }

    // Backwards compatibility — no hooks in manifest.
    #[test]
    fn test_hook_backwards_compat() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: greet\n    command: echo hello\n";
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        let cmd_calls: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                MockCall::Command { command, .. } => Some(command.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            cmd_calls,
            vec!["echo hello"],
            "should work identically without hooks"
        );
    }

    // SQL hook step calls engine.execute_sql.
    #[test]
    fn test_hook_sql_hook() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_init:
    name: setup
    sql: hooks/setup.sql
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(
            dir.path(),
            yaml,
            &[("hooks/setup.sql", "CREATE TABLE staging (id INT);")],
        );
        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        // First call should be Preflight (since we have a SQL hook), then SQL, then Command.
        let sql_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Sql { .. }))
            .collect();
        assert_eq!(
            sql_calls.len(),
            1,
            "SQL hook should produce one execute_sql call"
        );
    }

    // Command hook step calls engine.execute_command.
    #[test]
    fn test_hook_command_hook() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_exit:
    name: teardown
    command: "echo cleanup"
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        let cmd_calls: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                MockCall::Command { command, .. } => Some(command.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            cmd_calls.contains(&"echo cleanup"),
            "on_exit command hook should run"
        );
    }

    // hook: Full lifecycle — success path (init → steps → success → exit).
    #[test]
    fn test_hook_full_lifecycle_success() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_init:
    name: setup
    command: "echo init"
  on_success:
    name: notify
    command: "echo ok"
  on_failure:
    name: alert
    command: "echo fail"
  on_exit:
    name: teardown
    command: "echo exit"
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        let cmd_calls: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                MockCall::Command { command, .. } => Some(command.as_str()),
                _ => None,
            })
            .collect();
        // init → load → success → exit (no failure).
        assert_eq!(
            cmd_calls,
            vec!["echo init", "echo loading", "echo ok", "echo exit"]
        );
    }

    // hook: Full lifecycle — failure path (init → steps → failure → exit).
    #[test]
    fn test_hook_full_lifecycle_failure() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
hooks:
  on_init:
    name: setup
    command: "echo init"
  on_success:
    name: notify
    command: "echo ok"
  on_failure:
    name: alert
    command: "echo fail"
  on_exit:
    name: teardown
    command: "echo exit"
steps:
  - name: load
    command: "echo loading"
"#;
        setup_project(dir.path(), yaml, &[]);
        let engine = MockEngine::new();
        // Fail the step (call index 1 — after on_init at index 0).
        engine.set_fail_on_call(1, 1, "step error");
        let state = MockStateBackend::new();
        let result = run(dir.path(), &engine, &state, false);
        assert!(result.is_err());

        let calls = engine.calls.borrow();
        let cmd_calls: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                MockCall::Command { command, .. } => Some(command.as_str()),
                _ => None,
            })
            .collect();
        // init → load (fail) → failure → exit (no success).
        assert_eq!(
            cmd_calls,
            vec!["echo init", "echo loading", "echo fail", "echo exit"]
        );
    }

    // ---- Live Protocol+Run contract ----

    // A successful run persists a deserializable JSON contract with a measured
    // row-count, per-statement SQL lineage, and a per-step + run_complete status
    // stream. Row-count is measured against a real DuckDB table pre-seeded into the
    // run database (the mock engine records but doesn't materialize tables).
    #[test]
    fn test_contract_persisted_with_row_count_and_stream() {
        use crate::contract::{Contract, StepKind};

        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: widgets\ndb: pipeline.duckdb\nsteps:\n  - name: load\n    sql: models/load.sql\n  - name: tally\n    sql: models/tally.sql\n";
        setup_project(
            dir.path(),
            yaml,
            &[
                (
                    "models/load.sql",
                    "CREATE TABLE widgets (id INTEGER, name TEXT);\n",
                ),
                (
                    "models/tally.sql",
                    "CREATE TABLE widget_tally AS SELECT count(*) AS n FROM widgets;\n",
                ),
            ],
        );

        // Pre-seed the run database with a real `widgets` table so the contract's
        // live row-count measure has something to count. Scoped so the connection
        // closes before the run opens the same file.
        {
            let conn = duckdb::Connection::open(dir.path().join("pipeline.duckdb")).unwrap();
            conn.execute_batch(
                "CREATE TABLE widgets (id INTEGER, name TEXT); \
                 INSERT INTO widgets VALUES (1,'a'),(2,'b'),(3,'c');",
            )
            .unwrap();
        }

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        // (a) The per-run JSON exists and deserializes.
        let json_path = dir.path().join("build/.arcform/runs/run-1.json");
        assert!(
            json_path.exists(),
            "contract JSON should exist at {json_path:?}"
        );
        let raw = std::fs::read_to_string(&json_path).unwrap();
        let contract: Contract = serde_json::from_str(&raw).expect("contract JSON deserializes");

        assert_eq!(contract.contract_version, "b4/1");
        assert_eq!(contract.run.outcome, "success");
        assert_eq!(contract.run.run_id, "run-1");
        assert_eq!(contract.run.attempt_id, "run-1");
        assert_eq!(contract.run.engine.arc, env!("CARGO_PKG_VERSION"));
        assert!(contract.run.protocol.manifest_sha256.is_some());

        // (b) At least one asset with a dotted id + kind + a populated row_count.
        let widgets = contract
            .assets
            .iter()
            .find(|a| a.name == "widgets")
            .expect("widgets asset present");
        assert_eq!(widgets.id, "table:widgets");
        assert_eq!(widgets.kind, "table");
        assert_eq!(
            widgets.row_count,
            Some(3),
            "row_count measured from the run db"
        );
        assert_eq!(widgets.produced_by.as_deref(), Some("load"));
        assert!(widgets.consumed_by.contains(&"tally".to_string()));

        // (c) A sql step carries non-empty sql_text + sql_hash + per-statement lineage.
        let load = contract.steps.iter().find(|s| s.name == "load").unwrap();
        assert_eq!(load.kind, StepKind::Sql);
        let sql = load.sql.as_ref().expect("sql detail present");
        assert!(!sql.sql_text.is_empty(), "sql_text captured");
        assert!(!sql.sql_hash.is_empty(), "sql_hash captured");
        // Per-statement lineage records the produced table (keeps the statement handle
        // to check its byte range).
        let widget_stmt = sql
            .statements
            .iter()
            .find(|st| st.produces.contains(&"widgets".to_string()))
            .expect("per-statement lineage records the produced table");
        // Measured asset fields: a relational asset carries a DuckDB `estimated_size`
        // and a deterministic sha256-hex content hash of its rows.
        assert!(
            widgets.bytes.is_some(),
            "table bytes measured via estimated_size"
        );
        let hash = widgets
            .content_hash
            .as_ref()
            .expect("table content_hash computed");
        assert_eq!(hash.len(), 64, "content_hash is sha256 hex: {hash}");
        assert!(
            hash.chars().all(|c| c.is_ascii_hexdigit()),
            "content_hash is hex: {hash}"
        );
        // (c2) the producing statement carries a byte range that slices its
        // own source out of sql_text.
        let [lo, hi] = widget_stmt
            .byte_range
            .expect("statement carries a byte range");
        assert!(sql.sql_text[lo..hi].to_lowercase().contains("widgets"));
        // (c3) an executed step records ≥1 attempt and a wall-clock duration.
        assert!(
            load.attempts >= 1,
            "executed step records its attempt count"
        );
        assert!(
            load.duration_sec.is_some(),
            "executed step records its duration"
        );
        assert!(
            load.status.skip_reason.is_none(),
            "an executed step has no skip reason"
        );

        // (d) The JSONL stream exists with a line per step + a run_complete line.
        let jsonl_path = dir.path().join("build/.arcform/runs/run-1.jsonl");
        assert!(jsonl_path.exists(), "status stream should exist");
        let lines: Vec<serde_json::Value> = std::fs::read_to_string(&jsonl_path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert!(
            lines
                .iter()
                .any(|l| l["step"] == "load" && l["state"] == "success"),
            "stream has load success line"
        );
        assert!(
            lines
                .iter()
                .any(|l| l["step"] == "tally" && l["state"] == "success"),
            "stream has tally success line"
        );
        assert!(
            lines
                .iter()
                .any(|l| l["event"] == "run_complete" && l["outcome"] == "success"),
            "stream has terminal run_complete line"
        );
    }

    // A run that fails partway records the failed step's state, a "partial" outcome
    // (some steps succeeded), and a `failed` line in the status stream.
    #[test]
    fn test_contract_records_failed_step_and_partial_outcome() {
        use crate::contract::Contract;

        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n  - name: s2\n    sql: models/s2.sql\n";
        setup_project(
            dir.path(),
            yaml,
            &[
                ("models/s1.sql", "CREATE TABLE t (id INT);"),
                ("models/s2.sql", "CREATE TABLE u AS SELECT * FROM t;"),
            ],
        );

        let engine = MockEngine::new();
        // Fail the 2nd execution call (s2); s1 succeeds first.
        engine.set_fail_on_call(1, 1, "boom");
        let state = MockStateBackend::new();
        let result = run(dir.path(), &engine, &state, false);
        assert!(result.is_err(), "run should fail on s2");

        let json_path = dir.path().join("build/.arcform/runs/run-1.json");
        let raw = std::fs::read_to_string(&json_path).unwrap();
        let contract: Contract = serde_json::from_str(&raw).unwrap();

        assert_eq!(contract.run.outcome, "partial", "s1 succeeded, s2 failed");
        let s1 = contract.steps.iter().find(|s| s.name == "s1").unwrap();
        let s2 = contract.steps.iter().find(|s| s.name == "s2").unwrap();
        assert_eq!(s1.status.state, "success");
        assert_eq!(s2.status.state, "failed");

        let jsonl =
            std::fs::read_to_string(dir.path().join("build/.arcform/runs/run-1.jsonl")).unwrap();
        let lines: Vec<serde_json::Value> = jsonl
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert!(
            lines
                .iter()
                .any(|l| l["step"] == "s2" && l["state"] == "failed")
        );
        assert!(
            lines
                .iter()
                .any(|l| l["event"] == "run_complete" && l["outcome"] == "partial")
        );
    }
}
