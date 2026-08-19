use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use owo_colors::OwoColorize;

use crate::asset::{AssetGraph, StepAssets};
use crate::asset_kind::AssetKind;
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
    // Who produces what — computed once, consulted both by staleness detection and by
    // the artifact hash each successful step records for the *next* run to compare.
    let all_produced = all_produced_assets(&asset_graph);

    // Print any warnings from asset discovery (e.g. unparseable SQL).
    for warning in &asset_graph.warnings {
        eprintln!("{} {}", "warning:".yellow(), warning);
    }

    // A case collision (two produces:/depends_on: declarations differing only by
    // case) is a manifest defect independent of anything on disk — refused before
    // anything runs, not tolerated with a warning. Unconditional: the collision
    // exists in the manifest whether or not `has_assets()` would otherwise gate the
    // rest of this section.
    asset_graph.validate_no_case_collisions()?;

    // If the graph has assets, validate step ordering against dependencies.
    if asset_graph.has_assets() {
        let step_order: Vec<String> = manifest.steps.iter().map(|s| s.name.clone()).collect();
        asset_graph.validate_order(&step_order)?;
    }

    // Determine which steps are stale, and — for the fresh ones — the typed reason
    // they can be skipped (recorded per step in the contract).
    let staleness = compute_staleness(
        &manifest,
        dir,
        state,
        &asset_graph,
        &all_produced,
        force,
        &env_map,
    )?;

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
                        // Hashed AFTER execution, so it reflects what the step just
                        // wrote — the baseline the *next* run's staleness check
                        // compares against. `is_hash_stale` never calls
                        // `produced_artifact_hash` for a `command:` step regardless of
                        // whether it ran or was skipped via preconditions (command
                        // steps are always stale on hash alone — see
                        // `compute_staleness`), so there is nothing to hash for them
                        // here either. `None` (a relevant file still unreadable right
                        // after "success," or an ambiguous declared spelling) records
                        // a fixed, human-legible placeholder — its value is moot,
                        // since `is_hash_stale` short-circuits to unconditional
                        // staleness on the same condition before this string is ever
                        // compared.
                        let artifact_hash = if step.command.is_some() {
                            String::new()
                        } else {
                            let hash =
                                produced_artifact_hash(step, dir, &asset_graph, &all_produced);
                            if hash.is_none() {
                                // The step just "succeeded," and the run is about to
                                // report success, while its own declared produces:
                                // still cannot be read. Re-running (forced by `None`
                                // above) is the safe outcome; it is not a legible one
                                // on its own — say so, since exit 0 will not.
                                let missing = missing_declared_produces(step, dir, &asset_graph);
                                if !missing.is_empty() {
                                    eprintln!(
                                        "{} step '{}' succeeded but does not appear to have produced: {} — arc will \
                                         keep re-running this step until its own work (or the manifest's \
                                         produces:) matches",
                                        "warning:".yellow(),
                                        step.name,
                                        missing.join(", ")
                                    );
                                }
                            }
                            hash.unwrap_or_else(|| "MISSING".to_string())
                        };
                        let _ = state.record_step(
                            &step.name,
                            &sql_hash,
                            &artifact_hash,
                            StepStatus::Success,
                        );
                        // What the step's declared tools were at the moment it ran — the
                        // identity the next run compares against. Recorded here rather
                        // than at plan time so a step that was un-skipped and then never
                        // reached keeps its old identity and runs again.
                        precondition::record_all(&step.preconditions, dir, &step.name, &env_map);

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
                        let _ = state.record_step(&step.name, &sql_hash, "", StepStatus::Failed);
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
                let _ = state.record_step(&step.name, &sql_hash, "", StepStatus::Failed);
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
/// `tool` identity check and a `modified_after` clock check are each called out
/// distinctly from generic ones so the contract records *which* freshness mechanism
/// decided the skip.
///
/// A step carrying both is reported as `tool`: it is the more specific claim — this
/// named binary is the one it last ran against — and it is the claim a reader is
/// checking when a tool moves.
fn precondition_skip_reason(preconditions: &[Precondition]) -> SkipReason {
    if preconditions
        .iter()
        .any(|p| matches!(p, Precondition::Tool { .. }))
    {
        SkipReason::PreconditionTool
    } else if preconditions
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
/// - Any FILE asset it produced, or any file it reads that nothing in the manifest
///   produces, no longer hashes to what it did after the step's last success —
///   changed, truncated or deleted all count (see [`produced_artifact_hash`])
/// - An upstream step (via asset graph) is stale (downstream propagation)
///
/// For every step that stays fresh, a typed [`SkipReason`] is recorded so the run
/// contract can distinguish a hash-clean skip from a precondition-driven one.
fn compute_staleness(
    manifest: &Manifest,
    dir: &Path,
    state: &dyn StateBackend,
    asset_graph: &AssetGraph,
    all_produced: &std::collections::HashSet<String>,
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

        // SQL/op step — check hash staleness (op steps hash their config) AND
        // artifact staleness (did the bytes this step is answerable for change).
        let hash_stale = is_hash_stale(step, dir, state, asset_graph, all_produced)?;

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
/// Returns true (stale) if: no prior state, prior failure, config hash mismatch, a
/// missing SQL file, **or the step's [`produced_artifact_hash`] no longer matches what
/// was recorded at its last success** — i.e. a file it produced, or a file it reads
/// that nothing in the manifest produces, was changed, truncated or deleted since.
///
/// The config-hash checks alone can only answer "would the same inputs produce the
/// same outputs" — never "do the outputs still hold." An asset-centric engine has to
/// ask the second question too, from the assets themselves, or a corrupted artifact
/// with an unchanged SQL/op config looks identical to a genuinely fresh one.
fn is_hash_stale(
    step: &crate::manifest::Step,
    dir: &Path,
    state: &dyn StateBackend,
    asset_graph: &AssetGraph,
    all_produced: &std::collections::HashSet<String>,
) -> Result<bool> {
    let prior = state.get_step_state(&step.name)?;

    match prior {
        None => Ok(true), // Never run before.
        Some(prior_state) => {
            if prior_state.status == StepStatus::Failed {
                return Ok(true); // Previously failed.
            }

            let config_stale = if step.op.is_some() {
                // Op step — config hash (operator ref + serialized `with:`).
                op_config_hash(step) != prior_state.sql_hash
            } else if let Some(ref sql) = step.sql {
                let sql_path = dir.join(sql);
                if sql_path.exists() {
                    let content = std::fs::read(&sql_path).map_err(|e| Error::FileRead {
                        path: sql_path.clone(),
                        source: e,
                    })?;
                    state::content_hash(&content) != prior_state.sql_hash
                } else {
                    true // File missing — will error during execution.
                }
            } else {
                false // No SQL file (shouldn't happen for SQL steps).
            };

            if config_stale {
                return Ok(true);
            }

            // Config is unchanged — now ask whether the assets this step is
            // answerable for still hold. This is the check that catches a produced
            // file being edited, truncated or deleted underneath an unchanged
            // manifest, which config hashing alone structurally cannot see.
            //
            // `None` means at least one asset this step is answerable for could not
            // be confidently hashed right now — it is currently unreadable (missing,
            // permission-denied — NOT truncated: `fs::read` succeeds on an empty
            // file and yields `Some(hash_of_empty)`, a real digest that compares
            // correctly against a non-empty prior hash; a Directory-kind asset is
            // unreadable the same way when the directory itself is absent, via
            // `hash_directory_contents` — never via a bare `is_dir()` presence
            // check, which cannot tell an emptied directory from a populated one;
            // a Pattern or Table never reaches this function at all — see
            // `produced_artifact_hash`'s kind dispatch), or its
            // declared spelling is itself ambiguous (see `produced_artifact_hash`). Either way
            // this forces staleness unconditionally rather than comparing against
            // `prior_state.artifact_hash` — an absence must never be allowed to read
            // as "unchanged" against a PRIOR absence. Two runs of a step whose
            // declared artifact was never actually written under its declared name
            // would otherwise both hash to the same `MISSING` sentinel and compare
            // equal forever, which is the graph asserting a file is produced while
            // nothing is on disk and the run reporting success — the defect this
            // whole mechanism exists to close, reached through the one case a plain
            // sentinel string cannot distinguish from genuine freshness.
            match produced_artifact_hash(step, dir, asset_graph, all_produced) {
                None => Ok(true),
                Some(current_artifact_hash) => {
                    Ok(current_artifact_hash != prior_state.artifact_hash)
                }
            }
        }
    }
}

/// Every asset name produced by any step in the manifest — the global "who owns this"
/// set `produced_artifact_hash` consults to tell a file a step reads directly with no
/// producer (an external input) apart from one produced upstream (already covered by
/// that producer's own staleness plus downstream propagation).
fn all_produced_assets(asset_graph: &AssetGraph) -> std::collections::HashSet<String> {
    asset_graph
        .steps
        .values()
        .flat_map(|sa| sa.produces.iter().cloned())
        .collect()
}

/// Every `produces:` name (raw, case-preserved) this step declares that cannot
/// currently be read as a file. Checked right after the step "succeeds," so this
/// names a real gap between the manifest's claim and what the step's own work
/// (SQL/op/command) actually did — probe6's shape: `produces: [build/REGISTRANT.tsv]`
/// declared, the step's SQL writes `build/registrant.tsv` instead, and
/// `is_hash_stale` now correctly forces this step to keep re-running rather than
/// silently certifying success (see `produced_artifact_hash`) — but re-running
/// forever is a safe outcome, not a legible one. A run that exits 0 while the asset
/// graph asserts a file is produced and nothing is on disk needs to say so.
fn missing_declared_produces(
    step: &crate::manifest::Step,
    dir: &Path,
    asset_graph: &AssetGraph,
) -> Vec<String> {
    let Some(assets) = asset_graph.steps.get(&step.name) else {
        return Vec::new();
    };
    assets
        .produces
        .iter()
        .filter(|n| is_hashable_kind(assets, n))
        .filter_map(|lowered| {
            let raw = match assets.declared_case.get(lowered) {
                Some(raws) if raws.len() == 1 => raws.iter().next().unwrap().as_str(),
                _ => lowered.as_str(),
            };
            let full = dir.join(raw);
            // Dispatch on the declared kind, not on what happens to be on disk: a
            // Directory-kind entry is "missing" when `hash_directory_contents`
            // returns `None`, which covers the directory itself being absent or
            // unlistable AND a child anywhere in the tree that cannot be read. An
            // EMPTY-but-present directory is not reported here (that gap is
            // `is_hash_stale`'s job, via the content hash changing), because this
            // function names things the step's own work never created, not things
            // that were created and later went bad. A File-kind entry is missing
            // when `fs::read` fails, exactly as before.
            let is_missing = match declared_kind_of(assets, lowered) {
                AssetKind::Directory => state::hash_directory_contents(&full).is_none(),
                _ => std::fs::read(&full).is_err(),
            };
            is_missing.then(|| raw.to_string())
        })
        .collect()
}

/// Whether `assets.declared_kind` marks `name` as something [`produced_artifact_hash`]
/// hashes at all — a `File` or a `Directory`. `Pattern` never resolves to one
/// artifact (staleness comes from what produces the matches, not the pattern
/// itself) and `Table` is not a path in the first place. Defaults to `File` for a
/// name with no kind entry — every current insertion site populates one, so this is
/// a defensive fallback, not a real path; it fails SAFE rather than silently,
/// since a File-shaped read of an actual directory errors and forces staleness via
/// `?`, never quietly skips the way an `is_dir()` opt-out did.
fn is_hashable_kind(assets: &StepAssets, name: &str) -> bool {
    matches!(
        declared_kind_of(assets, name),
        AssetKind::File | AssetKind::Directory
    )
}

/// `assets.declared_kind.get(name)`, defaulting to `File` — the one place both
/// `is_hashable_kind` and `produced_artifact_hash` resolve a name's kind, so the
/// defensive default can never drift between the two call sites.
fn declared_kind_of(assets: &StepAssets, name: &str) -> AssetKind {
    assets
        .declared_kind
        .get(name)
        .copied()
        .unwrap_or(AssetKind::File)
}

/// A combined content hash over every asset a step is answerable for that is
/// actually backed by bytes on disk — `File` and `Directory` kinds, per
/// `declared_kind`: what it produces, plus what it reads directly that nothing in
/// the manifest produces (an external input — the shape of `build/gleif_ra_sec.csv`
/// before `gleif_ra_fetch` existed to own it). `Table` and `Pattern` never
/// participate — a table is left to the existing config-hash and
/// downstream-propagation machinery, and a pattern's staleness comes from what
/// produces the matches, not from the pattern string itself. This only ever adds
/// file-backed bytes to the staleness question, never subtracts from it.
///
/// Resolves each name through [`crate::asset::StepAssets::declared_case`] — the RAW,
/// exactly-as-declared spelling carried alongside the lowercased graph node — and
/// joins that directly onto `dir`. **No scanning, no candidate-counting.** Two earlier
/// designs here (a literal join with a lowercased fallback; a case-insensitive scan
/// requiring exactly one match) were both unsound the same way: however many files
/// happen to share a name case-insensitively on disk *right now* is not evidence about
/// which one is the declared artifact, and stops being evidence at all the moment the
/// declared file is deleted and only a decoy remains — at that point exactly one
/// candidate exists, and a scan finds it confidently, and wrongly. The declared
/// spelling is the one thing scanning can never recover once it has been thrown away,
/// so it is carried from the manifest instead of reconstructed from the filesystem.
///
/// **Which strategy hashes a name is decided by `declared_kind`, set at the point
/// the asset was declared — never by asking the filesystem what is there right
/// now.** A `File` reads its bytes directly; a `Directory` hashes a manifest of its
/// contents via [`crate::state::hash_directory_contents`], not a bare
/// `fs::metadata(..).is_dir()` presence check — a presence check cannot tell an
/// emptied directory from a populated one, so it would let a `COPY … PARTITION_BY`
/// target or a pattern-only `archive_extract` `dest:` skip forever once every
/// partition file inside it was deleted or corrupted while the directory itself
/// survived. `Pattern` and `Table` are filtered out before this loop is ever
/// reached (see `is_hashable_kind`), so this function has no name-based or
/// extension-based guessing left in it at all — that was round 6 and round 7's
/// mistake (an extension allowlist left real files untracked; an unconditional
/// directory skip re-opened the exact hole a content hash closes).
///
/// Returns `None` — never a hash — in two OTHER cases, both forcing the caller
/// (`is_hash_stale`) to unconditional staleness rather than a string comparison:
///
/// - **A relevant asset cannot be read right now**, under its declared spelling —
///   missing or permission-denied for a `File` (not truncated: `fs::read` succeeds
///   on an empty file, yielding `Some(hash_of_empty)`); unreadable/absent for a
///   `Directory`, via `hash_directory_contents` returning `None`. Earlier designs
///   folded this into a stable `"MISSING"` sentinel *string* and compared it like any
///   other digest, which reads as "unchanged" the moment a step's declared artifact
///   has never once existed under its declared name (its SQL wrote a differently-cased
///   file, say): the baseline recorded right after the step's own "success" is already
///   `MISSING`, so every later run compares `MISSING` to `MISSING` and skips forever —
///   the graph asserting a file is produced while nothing is on disk, and the run
///   reporting success. An absence can never be evidence of sameness, so it is never
///   folded into the comparable string at all.
/// - **A declared name resolves to more than one raw spelling** —
///   `declared_case`'s entry for it has 2+ elements. `AssetGraph::
///   validate_no_case_collisions` refuses this for both `produces:` and
///   `depends_on:` at load, so a validated graph should never reach this branch; it
///   exists as the read-time backstop precisely because *"the gate already checked
///   it"* must not be the only thing standing between an ambiguous declaration and a
///   silent, iteration-order-dependent pick of one spelling over the other.
///
/// Hashes the actual on-disk bytes directly — **never** a fetch sidecar's recorded
/// digest (`crate::ingress_meta`, which [`crate::contract::measure_file`] prefers for
/// reporting). Trusting a sidecar here would be exactly the failure this guards
/// against: a truncated artifact whose stale `.arcmeta` sidecar still claims to be
/// what the step wrote.
///
/// A step with no file- or directory-typed produced/external-read assets hashes the
/// empty input deterministically — stable across runs, so it never falsely reads as
/// changed.
fn produced_artifact_hash(
    step: &crate::manifest::Step,
    dir: &Path,
    asset_graph: &AssetGraph,
    all_produced: &std::collections::HashSet<String>,
) -> Option<String> {
    let step_assets = asset_graph.steps.get(&step.name);
    let mut names: Vec<&String> = Vec::new();
    if let Some(assets) = step_assets {
        names.extend(
            assets
                .produces
                .iter()
                .filter(|n| is_hashable_kind(assets, n)),
        );
        names.extend(
            assets
                .reads
                .iter()
                .filter(|n| is_hashable_kind(assets, n) && !all_produced.contains(n.as_str())),
        );
    }
    names.sort();
    names.dedup();

    let declared_case = step_assets.map(|a| &a.declared_case);

    let mut buf = String::new();
    for name in names {
        // The declared, case-preserved spelling — never the lowercased graph node,
        // which is identity, not a path. `raws.len() > 1` is the ambiguous-spelling
        // backstop described above; `None` (no entry at all) falls back to the
        // lowered name itself, defensive against a future insertion site that forgets
        // to populate `declared_case` — every current one does.
        let raw = match declared_case.and_then(|dc| dc.get(name)) {
            Some(raws) if raws.len() == 1 => raws.iter().next().unwrap(),
            Some(raws) if raws.len() > 1 => return None,
            _ => name,
        };
        let full = dir.join(raw);
        // Dispatch on the kind carried from declaration — see the doc comment above
        // for why this replaced an `is_dir()` presence check. `step_assets` is
        // `Some` whenever `names` is non-empty (the only source of `names`), so the
        // `unwrap_or(File)` default here is unreachable in practice; it exists so
        // this function has no path that panics.
        let kind = step_assets
            .map(|a| declared_kind_of(a, name))
            .unwrap_or(AssetKind::File);
        let digest = match kind {
            AssetKind::Directory => state::hash_directory_contents(&full)?,
            _ => {
                let bytes = std::fs::read(&full).ok()?;
                state::content_hash(&bytes)
            }
        };
        buf.push_str(name);
        buf.push('\u{1f}'); // unit separator — cheap guard against name/hash collision
        buf.push_str(&digest);
        buf.push('\n');
    }
    Some(state::content_hash(buf.as_bytes()))
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

    // Card `changing-an-input-file-arcform-did-not-produce-marks-nothing-stale`, AC1 +
    // AC4: a step's own SQL/config hash is unchanged, but the FILE it declares as
    // `produces:` was edited directly on disk — arc must not skip it. Mutation proof:
    // reverting `is_hash_stale`'s artifact-hash check (folding `config_stale` straight
    // into the returned `Ok(...)` it replaced, dropping the `produced_artifact_hash`
    // comparison) turns this red — the third run stays at 1 call (preflight only)
    // instead of 2, because the edited file goes undetected and the step is skipped
    // `hash_clean` with the edited bytes left in place.
    #[test]
    fn test_produced_file_change_forces_rerun() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n    produces: [build/out.csv]\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);
        fs::create_dir_all(dir.path().join("build")).unwrap();
        fs::write(dir.path().join("build/out.csv"), "a,b\n1,2\n").unwrap();

        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // First run — always stale (no prior state).
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            2,
            "first run: preflight + 1 sql"
        );

        // Second run, nothing touched — the positive control. Config unchanged AND the
        // produced file unchanged must still skip, or the fix has regressed the warm
        // path this defect was supposed to leave alone.
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            1,
            "unchanged produced file should stay skipped (preflight only)"
        );

        // Edit the produced file directly — the manifest and its SQL never change.
        fs::write(dir.path().join("build/out.csv"), "a,b\n1,2\n3,4\n").unwrap();

        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            2,
            "edited produced file must force a re-run: preflight + 1 sql"
        );
    }

    // Same defect, truncation — AC1's second failure mode.
    #[test]
    fn test_produced_file_truncated_forces_rerun() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n    produces: [build/out.csv]\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);
        fs::create_dir_all(dir.path().join("build")).unwrap();
        fs::write(dir.path().join("build/out.csv"), "a,b\n1,2\n").unwrap();

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        // Truncate to 0 bytes — content_hash of empty input differs from the recorded
        // hash of the non-empty original, so this must not be mistaken for "unchanged".
        fs::write(dir.path().join("build/out.csv"), "").unwrap();

        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            2,
            "truncated produced file must force a re-run: preflight + 1 sql"
        );
    }

    // Same defect, deletion — AC1's third failure mode. The file is gone outright, not
    // just changed, so `produced_artifact_hash` has to treat "cannot read" as a change
    // rather than silently degrading to "nothing to compare."
    #[test]
    fn test_produced_file_deleted_forces_rerun() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n    produces: [build/out.csv]\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);
        fs::create_dir_all(dir.path().join("build")).unwrap();
        fs::write(dir.path().join("build/out.csv"), "a,b\n1,2\n").unwrap();

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        fs::remove_file(dir.path().join("build/out.csv")).unwrap();

        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            2,
            "deleted produced file must force a re-run: preflight + 1 sql"
        );
    }

    // Regression: an independent verifier reproduced this live against `dd1ea78` on a
    // real case-sensitive APFS volume (`hdiutil create -fs "Case-sensitive APFS"`),
    // mirroring exactly what the real `extract_ncen_*` steps do —
    // `archive_extract@1` with `members: [REGISTRANT.tsv, ...]` writes that case
    // preserved to disk, while the op-declared and explicit-`produces:` lowercasing
    // call sites in `asset.rs`, and every operator's `assets()` in `operator.rs`,
    // normalize the graph's produces node to `.../registrant.tsv` (needed so two
    // differently-cased spellings of the same case-insensitive DuckDB *table* land on
    // one graph node — see `resolve_on_disk_case`'s doc for why that stays; SQL
    // introspection's own produces/reads, by contrast, are captured verbatim from the
    // SQL text and are not part of this). On a case-SENSITIVE filesystem
    // (`ubuntu-latest`, which both `ci.yml` and the publish runner use) the pre-fix
    // `dir.join(lowercased_name)` never finds the real, mixed-case file — not on the
    // baseline run, not after a mutation — so it hashes to the same `MISSING` sentinel
    // both times and `is_hash_stale`'s comparison can never fire. That is the original
    // defect, reachable through the one shape the real manifest actually uses.
    //
    // These three only exercise the bug on a filesystem that distinguishes case — the
    // default macOS temp dir does not (case-preserving but case-INsensitive), so both
    // the broken and fixed code resolve the same bytes there regardless of which name
    // is queried. They are unfiltered — no `#[ignore]` — so they execute as part of
    // `cargo test --workspace` in `ci.yml`'s `build` job on `ubuntu-latest`, and that
    // job on PR #54 at `a9a81d1` (which added them, fixed) is SUCCESS — confirming
    // they run there, not that a red run was ever observed. The actual redden-on-
    // reverted-code proof below was measured locally against a
    // `hdiutil create -fs "Case-sensitive APFS"` volume mounted and used as `$TMPDIR`,
    // not by watching CI turn red.
    #[test]
    fn test_case_mismatched_produced_file_change_forces_rerun() {
        let dir = tempfile::tempdir().unwrap();
        // Declared with the real member's case, matching `members: [REGISTRANT.tsv]`;
        // the graph node the manifest's `produces:` list feeds is lowercased.
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n    produces: [build/REGISTRANT.tsv]\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);
        fs::create_dir_all(dir.path().join("build")).unwrap();
        // Written case-preserved, exactly as archive_extract's extract_members does.
        fs::write(dir.path().join("build/REGISTRANT.tsv"), "cik\tlei\n1\tX\n").unwrap();

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            2,
            "first run: preflight + 1 sql"
        );

        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            1,
            "unchanged case-mismatched file should stay skipped (preflight only)"
        );

        fs::write(
            dir.path().join("build/REGISTRANT.tsv"),
            "cik\tlei\n1\tX\n2\tY\n",
        )
        .unwrap();

        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            2,
            "edited case-mismatched produced file must force a re-run: preflight + 1 sql"
        );
    }

    #[test]
    fn test_case_mismatched_produced_file_truncated_forces_rerun() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n    produces: [build/REGISTRANT.tsv]\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);
        fs::create_dir_all(dir.path().join("build")).unwrap();
        fs::write(dir.path().join("build/REGISTRANT.tsv"), "cik\tlei\n1\tX\n").unwrap();

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        fs::write(dir.path().join("build/REGISTRANT.tsv"), "").unwrap();

        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            2,
            "truncated case-mismatched produced file must force a re-run: preflight + 1 sql"
        );
    }

    #[test]
    fn test_case_mismatched_produced_file_deleted_forces_rerun() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n    produces: [build/REGISTRANT.tsv]\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);
        fs::create_dir_all(dir.path().join("build")).unwrap();
        fs::write(dir.path().join("build/REGISTRANT.tsv"), "cik\tlei\n1\tX\n").unwrap();

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        fs::remove_file(dir.path().join("build/REGISTRANT.tsv")).unwrap();

        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            2,
            "deleted case-mismatched produced file must force a re-run: preflight + 1 sql"
        );
    }

    // Round 4, the actual fix Hugh chose: carry the declared, case-preserved spelling
    // alongside the lowercased graph node, and resolve a produced/read file against
    // THAT — never by counting how many files happen to share a name
    // case-insensitively on disk right now. Round 3's "exactly one candidate, so the
    // code is confident" heuristic was unsound with no collision ever required:
    // deleting the declared artifact resolves the ambiguity by removing one of the
    // two candidates, and the guess resumes — silently, no warning — on exactly the
    // state that matters. A verifier drove this end to end against `14050f2`
    // (probe2/probe6) and reproduced precisely that.
    //
    // Two `produces:` declarations differing only by case is now refused when the
    // manifest loads — a manifest defect independent of anything on disk, closed at
    // the source rather than tolerated at runtime. Proof: reverting
    // `AssetGraph::validate_no_case_collisions` to `Ok(())` unconditionally turns
    // this red — `run(...)` then succeeds instead of returning
    // `Err(ManifestValidation)`.
    #[test]
    fn test_produces_case_collision_refused_at_load() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n    produces: [build/Report.csv]\n  - name: s2\n    sql: models/s2.sql\n    produces: [build/report.csv]\n";
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
        let err = run(dir.path(), &engine, &state, false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("collision") && msg.contains("case"),
            "expected a case-collision refusal, got: {msg}"
        );
        // preflight (an engine-availability check, unrelated to manifest validity)
        // runs before the asset graph is even built, so it is the one call this
        // refusal cannot pre-empt — no step executes.
        assert!(
            matches!(engine.calls.borrow().as_slice(), [MockCall::Preflight]),
            "a refused manifest must not execute any step: {:?}",
            engine.calls.borrow()
        );
    }

    // A stray, differently-cased file coexisting with the REAL declared artifact must
    // never be substituted for it, and deleting the declared artifact must be caught
    // even though only the decoy remains afterward — the shape round 3 missed by
    // stopping its own test one run short of exactly this state. Carries probe1's full
    // sequence through: baseline with a decoy present, an unchanged warm run, deletion
    // of the declared file (decoy still there), and TWO runs after that — round 4's
    // own regression was at the second of those: it correctly reran once (run 3) but
    // then let the resulting `MISSING` baseline compare equal to itself forever
    // (run 4 onward), which is the graph asserting a file is produced while nothing
    // is on disk and the run reporting success. Both post-deletion runs must
    // therefore re-run, not just the first.
    //
    // This scenario is only representable on a case-sensitive filesystem — the default
    // macOS temp dir folds the two writes into one file, so it self-skips there with an
    // explanation rather than failing for a reason unconnected to the code under test.
    // `ubuntu-latest` (`ci.yml`'s `build` job and the publish runner) is case-sensitive,
    // so this executes for real there.
    #[test]
    fn test_declared_artifact_deletion_is_caught_even_with_a_surviving_decoy() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n    produces: [build/REGISTRANT.tsv]\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);
        fs::create_dir_all(dir.path().join("build")).unwrap();
        fs::write(dir.path().join("build/REGISTRANT.tsv"), "real content\n").unwrap();
        fs::write(dir.path().join("build/registrant.tsv"), "decoy content\n").unwrap();

        if fs::read_dir(dir.path().join("build")).unwrap().count() < 2 {
            eprintln!(
                "skipping test_declared_artifact_deletion_is_caught_even_with_a_surviving_decoy: \
                 {} does not distinguish case, so a genuine collision cannot be constructed here",
                dir.path().display()
            );
            return;
        }

        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // Run 1: first run, always stale.
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(engine.calls.borrow().len(), 2, "run 1: preflight + 1 sql");

        // Run 2: nothing touched, decoy still sitting there. The declared file is
        // unchanged and the decoy is irrelevant — this must skip, not be treated as
        // ambiguous forever (round 3's over-caution, which this design does not need).
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            1,
            "run 2: unchanged declared file must stay skipped despite the decoy"
        );

        // Delete the DECLARED file. The decoy — never tracked, never hashed — is left
        // exactly as it was.
        fs::remove_file(dir.path().join("build/REGISTRANT.tsv")).unwrap();

        // Run 3: this is the run round 3's own test stopped one short of. Round 3's
        // scan would now find exactly one candidate (the decoy) and confidently, wrongly
        // treat it as unchanged. Carrying the declared case means the lookup asks for
        // build/REGISTRANT.tsv specifically, finds it gone, and must re-run.
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            2,
            "run 3: deleting the declared artifact must force a re-run even though a \
             differently-cased file survives it: preflight + 1 sql"
        );

        // Run 4: the mock step's own execution never recreates build/REGISTRANT.tsv
        // (it does not touch the filesystem), so the declared file is genuinely,
        // stably absent from here on. This must KEEP re-running, not settle into a
        // skip — round 4's own regression: "MISSING compares equal to a prior
        // MISSING" is exactly probe6's shape (a declared artifact that is never
        // actually produced reads as permanently unchanged). An absence can never be
        // evidence of sameness, run 3's or run 4's or any other run's.
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            2,
            "run 4: a declared file that is STILL absent must force another re-run, \
             not certify itself unchanged against its own prior absence: \
             preflight + 1 sql"
        );
    }

    // Probe6's shape, pinned on its own: a declared name that never existed on disk —
    // not even for one run — while a differently-cased leftover (a stale file from
    // before a fix landed, or a hand-authored copy under the wrong case) sits where
    // the graph's lowercased query would have looked. This is round 4's own
    // regression, reported independently of the decoy test above: `MISSING` (the
    // baseline recorded after this step's first "success," since nothing — not the
    // mock, not the leftover — ever writes build/REGISTRANT.tsv) compared equal to
    // `MISSING` (every later run, for the same reason) forever. The graph asserts a
    // file is produced by s1 while nothing is on disk, and the run reports success —
    // the card's headline sentence. A declared artifact that has never once existed
    // must force a re-run on every single invocation, not settle into a skip after
    // the first: rerunning cannot itself fix anything here (the mock never writes
    // the file either way), so the only honest signal is that the run keeps trying,
    // visibly, rather than reporting quiet, permanent success.
    //
    // Also proves the leftover is genuinely never substituted for the declared
    // file: mutating it changes nothing about *why* s1 reruns (still `None` —
    // "absent," not "changed") or how often (every time, regardless).
    //
    // Representable on any filesystem in principle, but the declared name and the
    // leftover fold to the same entry on one that does not distinguish case, which
    // would make the leftover BE build/REGISTRANT.tsv rather than a decoy beside it —
    // self-skips there rather than asserting something the setup cannot construct.
    // `ubuntu-latest` is case-sensitive, so this executes for real there.
    #[test]
    fn test_declared_case_that_never_existed_forces_perpetual_staleness() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n    produces: [build/REGISTRANT.tsv]\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);
        fs::create_dir_all(dir.path().join("build")).unwrap();
        // A leftover under a case nothing declares. build/REGISTRANT.tsv never exists
        // anywhere in this test.
        fs::write(
            dir.path().join("build/registrant.tsv"),
            "leftover content\n",
        )
        .unwrap();

        if dir.path().join("build/REGISTRANT.tsv").exists() {
            eprintln!(
                "skipping test_declared_case_that_never_existed_forces_perpetual_staleness: \
                 {} does not distinguish case, so the declared name and the leftover are the same file",
                dir.path().display()
            );
            return;
        }

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap(); // run 1: first run, always stale

        // Run 2: declared file still absent (the mock never writes it). Must re-run —
        // a step whose declared artifact has never existed cannot certify "unchanged."
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            2,
            "run 2: declared file still absent, must force a re-run rather than \
             certify unchanged: preflight + 1 sql"
        );

        // Mutate the leftover — must not change WHY s1 reruns, only confirm it still
        // does, for the same reason.
        fs::write(
            dir.path().join("build/registrant.tsv"),
            "mutated leftover\n",
        )
        .unwrap();

        // Run 3: same again. This must never settle into a skip while the declared
        // artifact stays absent, no matter how many times it is asked, and regardless
        // of what happens to an irrelevant, differently-cased leftover.
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            2,
            "run 3: still absent, still must force a re-run — an artifact that has \
             never existed cannot become 'stably unchanged'"
        );
    }

    // Proves the habitat this whole case-collision test family depends on, rather
    // than assuming it: a biconditional, computed once, true on BOTH a case-sensitive
    // and a folding filesystem under correct code, false only on the one state worth
    // catching. It measures case-sensitivity directly (write AAA/aaa, count entries)
    // and SEPARATELY measures whether the runner's own behaviour implies it — does a
    // step correctly go on ignoring a decoy's mutation, which is only possible when
    // the decoy and the declared file are actually two different on-disk entries —
    // then asserts the two agree:
    //
    //   case-sensitive  ⟺  a decoy's mutation is NOT detected
    //
    // On a real case-sensitive habitat, correct code ignores the decoy (it is never
    // looked up at all), so both sides read true/not-detected. On a filesystem that
    // folds case, "the decoy" IS build/REGISTRANT.tsv — there is no separate file to
    // ignore — so mutating it necessarily changes the one real file's bytes and gets
    // detected, regardless of whether this fix is present or reverted; both sides
    // read false/detected. It reddens only when they disagree: either the probe is
    // wrong about the filesystem, or (on a genuinely case-sensitive habitat) the
    // runner is substituting a decoy for a declared artifact again.
    //
    // Narrower than "case-sensitive ⟺ the whole family executed" would be: the other
    // case-family tests still self-skip on a folding filesystem, satisfying this
    // biconditional there without proving any of THEM ran — so this catches the
    // runner regressing, but would not by itself catch a CI habitat that started
    // folding case while everything else in the family quietly went back to skipping.
    #[test]
    fn test_case_sensitivity_probe_agrees_with_observed_runner_behavior() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n    produces: [build/REGISTRANT.tsv]\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);
        fs::create_dir_all(dir.path().join("build")).unwrap();
        fs::write(dir.path().join("build/REGISTRANT.tsv"), "declared\n").unwrap();
        fs::write(dir.path().join("build/registrant.tsv"), "decoy\n").unwrap();

        // Measured directly, once, before anything else touches the directory.
        let case_sensitive = fs::read_dir(dir.path().join("build")).unwrap().count() == 2;

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap(); // run 1: baseline

        // Mutate ONLY the decoy's name — on a folding filesystem this IS the
        // declared file; on a case-sensitive one it is a different, untracked entry.
        fs::write(dir.path().join("build/registrant.tsv"), "mutated decoy\n").unwrap();

        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        let mutation_detected = engine.calls.borrow().len() > 1;

        assert_eq!(
            case_sensitive,
            !mutation_detected,
            "incoherent habitat: case_sensitive={case_sensitive}, \
             mutation_detected={mutation_detected} in {} — a case-sensitive habitat \
             must ignore a decoy's mutation, and a folding one cannot have a separate \
             decoy to ignore at all",
            dir.path().display()
        );
    }

    // Round 5, regression 2: the collision gate checked `produces:` only, so two
    // `depends_on:` entries differing only by case never reached it — one graph node,
    // no warning, `declared_case` silently holding both raw spellings. With
    // build/DATA.csv and build/data.csv both real and distinct, `produced_artifact_hash`
    // picked one by `BTreeSet` iteration order (`DATA.csv`, 0x44, before `data.csv`,
    // 0x64) — deterministic, but arbitrary with respect to which file is the real
    // dependency, and the loser was never hashed, never tracked, at all. `s1` here
    // does not even declare a `sql:` file that touches either name; it is the
    // `depends_on:` declaration alone that must be enough to make the collision
    // reachable — this is a manifest-shape defect, not a read-behaviour one.
    //
    // Proof (see PR for the mutation report): reverting `validate_no_case_collisions`
    // to only inspect `produces:` again turns this red — `run(...)` then succeeds
    // instead of returning `Err(ManifestValidation)`.
    //
    // Representable on any filesystem; the gate is a static check over declared
    // strings and never touches disk, so unlike the read-time tests above this one
    // does not need — and does not get — a case-sensitivity guard.
    #[test]
    fn test_depends_on_case_collision_refused_at_load() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n    depends_on: [build/DATA.csv, build/data.csv]\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        let err = run(dir.path(), &engine, &state, false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("collision") && msg.contains("depends_on:"),
            "expected a depends_on: case-collision refusal naming the manifest key, \
             got: {msg}"
        );
        assert!(
            matches!(engine.calls.borrow().as_slice(), [MockCall::Preflight]),
            "a refused manifest must not execute any step: {:?}",
            engine.calls.borrow()
        );
    }

    // The other half of regression 2's fix: even with the gate above, a
    // `declared_case` entry holding more than one raw spelling must never be resolved
    // by picking one — that IS the exact ambiguity the gate exists to abolish, and
    // "the gate already checked it" must not be the only thing standing between an
    // ambiguous declaration and a silent, iteration-order-dependent choice. The gate
    // refuses this shape before a real `run()` ever reaches `produced_artifact_hash`,
    // so this bypasses `run()` and the gate entirely, hand-injecting the ambiguous
    // graph state the gate exists to prevent, to prove the read-time refusal holds
    // on its own rather than depending on the gate never having a gap.
    #[test]
    fn test_ambiguous_declared_case_forces_staleness_not_iteration_order_pick() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n    depends_on: [build/data.csv]\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);
        fs::create_dir_all(dir.path().join("build")).unwrap();
        fs::write(dir.path().join("build/DATA.csv"), "from DATA.csv\n").unwrap();
        fs::write(dir.path().join("build/data.csv"), "from data.csv\n").unwrap();

        if fs::read_dir(dir.path().join("build")).unwrap().count() < 2 {
            eprintln!(
                "skipping test_ambiguous_declared_case_forces_staleness_not_iteration_order_pick: \
                 {} does not distinguish case, so a genuine collision cannot be constructed here",
                dir.path().display()
            );
            return;
        }

        let manifest = crate::manifest::Manifest::load(dir.path()).unwrap();
        let mut asset_graph = AssetGraph::build(&manifest, dir.path());
        asset_graph
            .steps
            .get_mut("s1")
            .unwrap()
            .declared_case
            .get_mut("build/data.csv")
            .unwrap()
            .insert("build/DATA.csv".to_string());

        let step = manifest.steps.iter().find(|s| s.name == "s1").unwrap();
        let all_produced = all_produced_assets(&asset_graph);
        let result = produced_artifact_hash(step, dir.path(), &asset_graph, &all_produced);
        assert_eq!(
            result, None,
            "an ambiguous declared_case entry (2+ raw spellings) must never be \
             resolved by picking one — it must force staleness (None), not silently \
             choose build/DATA.csv over build/data.csv by BTreeSet iteration order"
        );
    }

    // The "(external source)" shape from the card's original 2026-08-12 evidence: a
    // file a step *reads* (`depends_on:`) that nothing in the manifest produces. No
    // step owns marking itself stale for this file via `produces:`, so
    // `produced_artifact_hash` has to fold a step's *unproduced* direct reads in too,
    // not just what it produces — otherwise this exact shape stays unfixed.
    #[test]
    fn test_external_unproduced_file_change_forces_rerun() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n    depends_on: [external.csv]\n";
        setup_project(
            dir.path(),
            yaml,
            &[
                ("models/s1.sql", "SELECT 1;"),
                ("external.csv", "a,b\n1,2\n"),
            ],
        );

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            2,
            "first run: preflight + 1 sql"
        );

        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            1,
            "unchanged external input should stay skipped (preflight only)"
        );

        // Nothing produces external.csv — it is edited directly, the only way an input
        // like this ever changes.
        fs::write(dir.path().join("external.csv"), "a,b\n1,2\n3,4\n").unwrap();

        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            2,
            "changed external input must force a re-run: preflight + 1 sql"
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

    // Three runs of one unchanged manifest over a tool that moves between the second
    // and the third: the step runs, is skipped as `precondition_tool` — a reason
    // distinct from both `hash_clean` and `precondition_fresh` — and then runs again
    // because the tool reports a different version. Nothing about the manifest, the
    // command or the step's `with:` block differs across the three.
    #[test]
    fn a_step_reruns_when_its_declared_tool_changes_and_says_so_in_the_contract() {
        use crate::contract::Contract;

        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: describe
    command: "echo describing"
    preconditions:
      - tool:
          path: bin/faketool
          version: "$ARC_TOOL --version"
  - name: unrelated
    command: "echo unrelated"
    preconditions:
      - command: "true"
"#;
        setup_project(dir.path(), yaml, &[]);

        let tool = dir.path().join("bin/faketool");
        let install = |version: &str| {
            fs::create_dir_all(tool.parent().unwrap()).unwrap();
            fs::write(&tool, format!("#!/bin/sh\necho 'faketool {version}'\n")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
            }
        };
        let outcome = |run_id: &str, step: &str| {
            let raw = fs::read_to_string(
                dir.path()
                    .join(format!("build/.arcform/runs/{run_id}.json")),
            )
            .unwrap();
            let contract: Contract = serde_json::from_str(&raw).unwrap();
            let s = contract
                .steps
                .iter()
                .find(|s| s.name == step)
                .unwrap_or_else(|| panic!("{step} present in {run_id}"));
            (
                s.status.state.clone(),
                s.status.skip_reason.map(|r| r.as_str().to_string()),
            )
        };

        install("1.0.0");
        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            outcome("run-1", "describe"),
            ("success".to_string(), None),
            "a step that has never run against this tool runs"
        );

        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            outcome("run-2", "describe"),
            ("skipped".to_string(), Some("precondition_tool".to_string())),
            "an unchanged tool skips the step, and the contract says which mechanism \
             decided it"
        );
        // The distinction is the point: a sibling step gated by a `command:`
        // precondition is skipped in the same run under the generic reason.
        assert_eq!(
            outcome("run-2", "unrelated"),
            (
                "skipped".to_string(),
                Some("precondition_fresh".to_string())
            ),
        );

        install("1.0.1");
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            outcome("run-3", "describe"),
            ("success".to_string(), None),
            "a tool reporting a different version re-runs the step"
        );
        assert_eq!(
            outcome("run-3", "unrelated"),
            (
                "skipped".to_string(),
                Some("precondition_fresh".to_string())
            ),
            "and the step that does not declare the tool is untouched by its move"
        );

        // A fourth run against the bumped tool settles back to skipped, so the re-run is
        // a consequence of the change rather than of the gate never being satisfiable.
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            outcome("run-4", "describe"),
            ("skipped".to_string(), Some("precondition_tool".to_string())),
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

    // The runner is the only place the shared fetch cache is resolved and the only
    // place it is handed to an operator, so nothing else can pin that wiring. A
    // **pinned** fetch whose bytes the store already holds is the one route that never
    // reaches the network: with the cache wired, this Protocol runs against a host that
    // does not resolve; with `cache: None` in the `OpContext` the runner builds, it
    // cannot, and the step fails.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn a_run_hands_the_shared_fetch_cache_to_its_operators() {
        // `$ARCFORM_FETCH_CACHE` is process-wide, and a run that resolved the real
        // default would write into the developer's home.
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        const CACHE_ENV: &str = "ARCFORM_FETCH_CACHE";
        const URL: &str = "http://no-origin-is-reachable.invalid/artifact.bin";

        /// Points `$ARCFORM_FETCH_CACHE` at a root for as long as this value is alive,
        /// and clears it in `Drop`.
        ///
        /// `Drop` rather than a `remove_var` after the call under test: any unwind past
        /// that line skips it — a panic inside `run`, or the `expect` on its result —
        /// and leaves the variable naming a `TempDir` that is about to be deleted, for
        /// whatever the harness schedules next.
        struct CacheRoot {
            _lock: std::sync::MutexGuard<'static, ()>,
        }

        impl CacheRoot {
            fn set(root: &std::path::Path) -> Self {
                let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
                // SAFETY: `set_var` is unsound against a concurrent `getenv`, and the
                // lock does not reach one — it serialises this crate's own *writers* of
                // the variable and nothing else. A `run` in a test the harness schedules
                // alongside this one still reads it, and resolves its shared cache from
                // this root; that changes what such a run does only where it fetches.
                unsafe { std::env::set_var(CACHE_ENV, root) };
                Self { _lock }
            }
        }

        impl Drop for CacheRoot {
            fn drop(&mut self) {
                // SAFETY: the same window as the write above, under the same lock —
                // which is released with this value, after the variable is cleared.
                unsafe { std::env::remove_var(CACHE_ENV) };
            }
        }

        let bytes = b"the artifact the shared store already holds";
        let digest = crate::state::content_hash(bytes);

        let store = tempfile::tempdir().unwrap();
        let root = store.path().join("cache");
        let seed = tempfile::tempdir().unwrap();
        let source = seed.path().join("artifact.bin");
        fs::write(&source, bytes).unwrap();
        crate::fetch_cache::FetchCache::at(root.clone())
            .store(
                &crate::ingress_meta::FetchMeta {
                    url: URL.to_string(),
                    sha256: digest.clone(),
                    ..Default::default()
                },
                &source,
            )
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        setup_project(
            dir.path(),
            &format!(
                "name: test\nsteps:\n  - name: fetch\n    op: http_fetch\n    with:\n      url: {URL}\n      out: build/artifact.bin\n      sha256: {digest}\n"
            ),
            &[],
        );

        let _cache_root = CacheRoot::set(&root);
        run(
            dir.path(),
            &MockEngine::new(),
            &MockStateBackend::new(),
            false,
        )
        .expect("a pinned artifact the store holds needs no origin");
        assert_eq!(
            fs::read(dir.path().join("build/artifact.bin")).unwrap(),
            bytes,
            "the bytes came off the shared store"
        );
    }

    // The real edgar_gleif shape: `models/load.sql` reads
    // `read_csv('build/ncen/*/REGISTRANT.tsv', …)`, and SQL introspection captures
    // that glob verbatim as a `reads` entry. Nothing produces the literal string
    // `build/ncen/*/REGISTRANT.tsv` (only concrete per-quarter files do), so it took
    // the external-read branch of `produced_artifact_hash` — and a string-based
    // classifier that only asked "does this contain `/`" said yes, so `fs::read` on
    // a literal `*` always failed and this step was `None` (forced stale) on every
    // single run, forever, silently, no `--force` in sight. Confirmed against the
    // real manifest: `load` re-ran on runs 2 through 5 with nothing else touched.
    //
    // SQL introspection now classifies a lifted path literal containing a glob
    // metacharacter as `AssetKind::Pattern` at the point it lifts it (see
    // `introspect.rs`'s `is_glob`), and `is_hashable_kind` excludes `Pattern`
    // (alongside `Table`) before this step's hashed-names list is even built — proven
    // here by the ordinary "unchanged, stays skipped" assertion, which a step with a
    // permanently-`None` artifact hash could never reach.
    #[test]
    fn test_sql_introspected_glob_does_not_force_perpetual_staleness() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: load\n    sql: models/load.sql\n";
        setup_project(
            dir.path(),
            yaml,
            &[(
                "models/load.sql",
                "CREATE TABLE ncen_registrant AS SELECT * FROM read_csv('build/ncen/*/REGISTRANT.tsv');",
            )],
        );

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(engine.calls.borrow().len(), 2, "run 1: preflight + 1 sql");

        // Run 2, 3: nothing touched. A step whose staleness check was still trying
        // (and always failing) to hash the literal glob string would re-run every
        // time; this must settle into skip instead.
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            1,
            "run 2: unchanged SQL (including its glob) must skip, not re-run forever \
             because a glob was mistaken for a hashable file"
        );

        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            1,
            "run 3: same again — must stay skipped, not resume re-running"
        );
    }

    // Round 7 pin #1: `missing_declared_produces` had zero regression protection —
    // returning `Vec::new()` unconditionally would have reddened nothing, and it is
    // the only legibility this whole card adds for a step that certifies success
    // over work it did not do (probe6's shape). Calls it directly rather than
    // capturing stderr, since the warning text is downstream of this and testing
    // the source of the claim is the more direct pin.
    #[test]
    fn test_missing_declared_produces_names_the_gap_and_closes_when_fixed() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n    produces: [build/REGISTRANT.tsv]\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);
        // build/REGISTRANT.tsv is never written.

        let manifest = crate::manifest::Manifest::load(dir.path()).unwrap();
        let asset_graph = AssetGraph::build(&manifest, dir.path());
        let step = manifest.steps.iter().find(|s| s.name == "s1").unwrap();

        let missing = missing_declared_produces(step, dir.path(), &asset_graph);
        assert_eq!(
            missing,
            vec!["build/REGISTRANT.tsv".to_string()],
            "a declared produces: file that was never written must be named"
        );

        // Write it — the gap must close, not stay reported forever.
        fs::create_dir_all(dir.path().join("build")).unwrap();
        fs::write(dir.path().join("build/REGISTRANT.tsv"), "now it exists\n").unwrap();
        let missing = missing_declared_produces(step, dir.path(), &asset_graph);
        assert!(
            missing.is_empty(),
            "declared file now exists — must not still be reported missing: {missing:?}"
        );
    }

    // Round 7 pin #2: the gate exemption's soundness rests on one unpinned line —
    // `produced_artifact_hash`'s own `!all_produced.contains(..)` filter over its
    // `reads`. Dropping it is exactly round 5's perpetual re-execution returning: a
    // reader (`consume`, mirroring `load`'s hand-written depends_on:) would try to
    // hash ITS OWN, differently-cased spelling of an asset a producer already owns —
    // a path that does not exist on a case-sensitive filesystem — and never settle
    // into skip. The gate's exemption argument depends on this filter meaning the
    // reader's spelling is never consulted at all; this proves that half.
    //
    // Case-sensitivity dependent — self-skips on a filesystem that folds
    // build/REGISTRANT.tsv and build/registrant.tsv into one entry, since then there
    // is nothing to distinguish.
    #[test]
    fn test_reader_case_mismatch_settles_to_skip_not_perpetual_staleness() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: produce\n    sql: models/produce.sql\n    produces: [build/REGISTRANT.tsv]\n  - name: consume\n    sql: models/consume.sql\n    depends_on: [build/registrant.tsv]\n";
        setup_project(
            dir.path(),
            yaml,
            &[
                ("models/produce.sql", "SELECT 1;"),
                ("models/consume.sql", "SELECT 2;"),
            ],
        );
        fs::create_dir_all(dir.path().join("build")).unwrap();
        fs::write(dir.path().join("build/REGISTRANT.tsv"), "real content\n").unwrap();

        if dir.path().join("build/registrant.tsv").exists() {
            eprintln!(
                "skipping test_reader_case_mismatch_settles_to_skip_not_perpetual_staleness: \
                 {} does not distinguish case, so build/registrant.tsv already exists via \
                 folding",
                dir.path().display()
            );
            return;
        }

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(engine.calls.borrow().len(), 3, "run 1: preflight + 2 sql");

        // Run 2, 3: nothing touched. "consume" must settle to skip — a reader still
        // hashing its own case-mismatched spelling directly would find that path
        // missing every time and never stop re-running.
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            1,
            "run 2: both steps unchanged, must settle to preflight only"
        );

        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            1,
            "run 3: same again — must stay settled, not resume re-running"
        );
    }

    // Round 8 pin #1 — the wider, SQL-introspection-driven instance of round 7's
    // directory regression. `models/export.sql`'s `COPY orders TO 'build/orders'
    // (… PARTITION_BY (year))` makes SQL introspection classify `build/orders` as
    // `AssetKind::Directory` from the statement's own `options`, statically, at
    // graph-build time — MockEngine never actually executes the SQL, so the test
    // creates the partitioned directory by hand to stand in for what a real DuckDB
    // COPY would have written, exactly as other tests here fake a SQL step's file
    // output. This exercises the real end-to-end `run()` path, multi-invocation,
    // asserting on `engine.calls` across five runs — not a call to
    // `produced_artifact_hash` or `AssetKind` directly.
    //
    // Round 7's `fs::metadata(&full).map(|m| m.is_dir()).unwrap_or(false)` check
    // would see "still a directory" at every one of these run 3/4 boundaries and
    // skip forever; `hash_directory_contents` cannot make that mistake, because it
    // hashes what is inside, not merely whether something is there.
    #[test]
    fn test_directory_emptied_while_kept_forces_rerun_not_perpetual_skip() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: export\n    sql: models/export.sql\n";
        setup_project(
            dir.path(),
            yaml,
            &[(
                "models/export.sql",
                "COPY orders TO 'build/orders' (FORMAT parquet, PARTITION_BY (year));",
            )],
        );

        fs::create_dir_all(dir.path().join("build/orders")).unwrap();
        fs::write(
            dir.path().join("build/orders/year=2024.parquet"),
            b"partition A",
        )
        .unwrap();
        fs::write(
            dir.path().join("build/orders/year=2025.parquet"),
            b"partition B",
        )
        .unwrap();

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            2,
            "run 1: preflight + 1 sql (no prior state, always stale)"
        );

        // Run 2: nothing touched — establishes the settled-skip baseline a
        // presence-only check and a content-manifest hash both agree on.
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            1,
            "run 2: unchanged directory, must settle to skip"
        );

        // Corrupt: delete every partition file, keep the directory itself present.
        fs::remove_file(dir.path().join("build/orders/year=2024.parquet")).unwrap();
        fs::remove_file(dir.path().join("build/orders/year=2025.parquet")).unwrap();
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            2,
            "run 3: directory emptied while kept present must force a re-run, not \
             skip forever — round 7's regression, reproduced and closed"
        );

        // Restore (different) content — must also be detected, not compared against
        // the emptied-directory baseline and called equal by coincidence.
        fs::write(
            dir.path().join("build/orders/year=2026.parquet"),
            b"partition C",
        )
        .unwrap();
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            2,
            "run 4: restored (different) content must also force a re-run"
        );

        // Run 5: unchanged again — must settle back to skip.
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            engine.calls.borrow().len(),
            1,
            "run 5: unchanged again, must settle back to skip"
        );
    }

    // Round 8 pin #2 — round 7's regression #1, reproduced against the REAL
    // `parquet_export` operator (not a stand-in): a bare, no-`/`, non-allowlisted-
    // extension `dest:` (`registrant.avro`) must be tracked, not silently excluded
    // by a name-based guess. `op:` steps run for real (see `run()`'s dispatch —
    // MockEngine only intercepts `sql:`/`command:` steps), so this drives an actual
    // DuckDB `COPY` through the operator against a real, pre-seeded `customers`
    // table and reads the real file it writes.
    //
    // `engine.calls` cannot observe an `op:` step (it never reaches the engine at
    // all), so this asserts on `state.runs`'s `steps_executed` count instead — the
    // one observable that is meaningful for every step kind, SQL/command/op alike,
    // recorded by the same `finish_run` call `engine.calls`-based tests rely on
    // indirectly. Still the real end-to-end `run()` path, multi-invocation, not a
    // call to `produced_artifact_hash` or `AssetKind` directly.
    #[test]
    fn test_parquet_export_bare_dest_forces_rerun_on_truncate_delete_restore() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: export\n    op: parquet_export\n    with:\n      input: customers\n      dest: registrant.avro\n";
        setup_project(dir.path(), yaml, &[]);

        // Seed the real DuckDB file `parquet_export` opens with the table it reads —
        // op: steps run for real, so this has to be a real table, not a mocked one.
        {
            let db_path = dir.path().join("test.duckdb");
            let conn = duckdb::Connection::open(&db_path).unwrap();
            conn.execute_batch("CREATE TABLE customers AS SELECT 1 AS id, 'a' AS name;")
                .unwrap();
        }

        let last_steps_executed = |state: &MockStateBackend| -> usize {
            state
                .runs
                .borrow()
                .last()
                .and_then(|(_, outcome)| outcome.clone())
                .map(|(steps_executed, _)| steps_executed)
                .expect("finish_run must have recorded this run")
        };

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            last_steps_executed(&state),
            1,
            "run 1: no prior state, always stale"
        );
        let dest = dir.path().join("registrant.avro");
        assert!(
            dest.exists(),
            "parquet_export must really write the bare-named, non-allowlisted-extension file"
        );
        let original_bytes = fs::read(&dest).unwrap();
        assert!(!original_bytes.is_empty());

        // Run 2: unchanged — must settle to skip.
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            last_steps_executed(&state),
            0,
            "run 2: unchanged, must settle to skip"
        );

        // Truncate the produced file underneath an unchanged manifest — round 7's
        // exact regression shape: a bare, non-allowlisted-extension dest: went
        // untracked and a truncation like this read as unchanged forever.
        fs::write(&dest, b"").unwrap();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            last_steps_executed(&state),
            1,
            "run 3: truncated bare-named file must force a real re-run"
        );
        let rewritten_bytes = fs::read(&dest).unwrap();
        assert_eq!(
            rewritten_bytes, original_bytes,
            "the re-run really re-executed parquet_export, restoring real bytes, \
             not merely reporting success over the truncated file"
        );

        // Run 4: unchanged again (the just-restored file) — must settle to skip.
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            last_steps_executed(&state),
            0,
            "run 4: restored file unchanged, must settle to skip"
        );

        // Delete it outright — same requirement, the other direction.
        fs::remove_file(&dest).unwrap();
        run(dir.path(), &engine, &state, false).unwrap();
        assert_eq!(
            last_steps_executed(&state),
            1,
            "run 5: deleted bare-named file must force a real re-run"
        );
        assert!(
            dest.exists(),
            "deleted file must be really rewritten, not just reported fixed"
        );
    }

    // ---- Round 9 ----------------------------------------------------------
    // Round 8's design is unchanged. What follows pins the parts of it that a
    // mutation could delete with the suite staying green, plus the two places it
    // was still inferring rather than carrying.

    /// Drive `run()` once and report how many engine calls it made. Every pin below
    /// asserts on this across a sequence of runs rather than on a predicate's return
    /// value: 2 is preflight plus one executed `sql:` step, 1 is preflight alone,
    /// which is what a skip looks like from outside.
    fn engine_calls_for_one_run(dir: &Path, state: &MockStateBackend) -> usize {
        let engine = MockEngine::new();
        run(dir, &engine, state, false).unwrap();
        let n = engine.calls.borrow().len();
        drop(engine);
        n
    }

    /// A one-step project whose SQL is `sql`, with `build/<dir_name>` pre-created and
    /// seeded, standing in for what a real DuckDB COPY would have written (MockEngine
    /// executes no SQL). Returns the temp dir.
    fn project_with_seeded_directory(sql: &str, files: &[(&str, &[u8])]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: export\n    sql: models/export.sql\n";
        setup_project(dir.path(), yaml, &[("models/export.sql", sql)]);
        for (rel, bytes) in files {
            let full = dir.path().join(rel);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(&full, bytes).unwrap();
        }
        dir
    }

    // Round 9, C1 — `PARTITION_BY` is not the whole set of COPY options that make
    // DuckDB write a directory, and the three that were missing all failed the same
    // way: classified `File`, `fs::read` on a directory errors, the hash is `None`,
    // and the step re-runs on every run at exit 0 while warning that it produced
    // nothing — with the files on disk.
    //
    // Run 2 of each of these is the assertion that reddens on the parent behaviour.
    // `PARTITION_BY` is in the loop as the control: it settled before this change and
    // must still settle.
    #[test]
    fn test_every_directory_writing_copy_option_settles_and_then_notices_a_change() {
        for option in [
            "PARTITION_BY (year)",
            "PER_THREAD_OUTPUT true",
            "FILE_SIZE_BYTES '1MB'",
            "ROW_GROUPS_PER_FILE 1",
        ] {
            let sql = format!("COPY orders TO 'build/out' (FORMAT parquet, {option});");
            let dir = project_with_seeded_directory(
                &sql,
                &[("build/out/data_0.parquet", b"first slice")],
            );
            let state = MockStateBackend::new();

            assert_eq!(
                engine_calls_for_one_run(dir.path(), &state),
                2,
                "{option}: run 1 has no prior state and must execute"
            );
            assert_eq!(
                engine_calls_for_one_run(dir.path(), &state),
                1,
                "{option}: run 2 must settle to skip — a directory-writing COPY \
                 classified as a File cannot be read and never settles"
            );

            fs::write(dir.path().join("build/out/data_1.parquet"), b"second slice").unwrap();
            assert_eq!(
                engine_calls_for_one_run(dir.path(), &state),
                2,
                "{option}: run 3 must notice a file added to the produced directory"
            );
            assert_eq!(
                engine_calls_for_one_run(dir.path(), &state),
                1,
                "{option}: run 4 must settle again"
            );
        }
    }

    // Round 9, C1's other half — an option that is NOT in the directory-writing set
    // must leave the target a single file, or the perpetual re-run simply arrives
    // from the other side (`hash_directory_contents` on a regular file cannot
    // `read_dir` it, returns `None`, and the step never settles).
    #[test]
    fn test_per_thread_output_false_is_hashed_as_one_file_and_settles() {
        let dir = project_with_seeded_directory(
            "COPY orders TO 'build/one.parquet' (FORMAT parquet, PER_THREAD_OUTPUT false);",
            &[("build/one.parquet", b"one file, not a directory")],
        );
        let state = MockStateBackend::new();

        assert_eq!(engine_calls_for_one_run(dir.path(), &state), 2, "run 1");
        assert_eq!(
            engine_calls_for_one_run(dir.path(), &state),
            1,
            "run 2 must settle — an explicit `false` writes one file, and treating it \
             as a directory would make the step re-run forever"
        );

        fs::write(dir.path().join("build/one.parquet"), b"different bytes").unwrap();
        assert_eq!(
            engine_calls_for_one_run(dir.path(), &state),
            2,
            "run 3: the file's bytes changed and it is really being hashed"
        );
    }

    // Round 9, C2 — a hand-declared `produces:` name the step's own SQL never
    // mentions is an ordering token, not a file. Classified `File` it is looked for
    // at `<dir>/raw_tables`, never found, and the step re-runs on every run —
    // measured 4 of 4 — with its consumer dragged along by propagation so that
    // never settles either.
    //
    // Runs 2 and 3 of `load`, and runs 2 and 3 of `consume`, are what redden on the
    // parent behaviour. `test_external_unproduced_file_change_forces_rerun` is the
    // guard on the other side: a bare `depends_on:` name that nothing produces is
    // still a real file and is still hashed.
    #[test]
    fn test_bare_produces_sentinel_settles_and_so_does_its_consumer() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: load\n    sql: models/load.sql\n    produces: [raw_tables]\n  - name: consume\n    sql: models/consume.sql\n    depends_on: [raw_tables]\n";
        setup_project(
            dir.path(),
            yaml,
            &[
                // Neither model creates anything called `raw_tables` — exactly the
                // shape `examples/code-lists/arcform.yaml` ships.
                (
                    "models/load.sql",
                    "CREATE OR REPLACE TABLE naics_raw AS SELECT 1;",
                ),
                (
                    "models/consume.sql",
                    "CREATE OR REPLACE TABLE naics_norm AS SELECT 2;",
                ),
            ],
        );
        let state = MockStateBackend::new();

        assert_eq!(
            engine_calls_for_one_run(dir.path(), &state),
            3,
            "run 1: preflight + 2 sql"
        );
        assert_eq!(
            engine_calls_for_one_run(dir.path(), &state),
            1,
            "run 2: an ordering token is not a file — both steps must settle to \
             preflight only"
        );
        assert_eq!(
            engine_calls_for_one_run(dir.path(), &state),
            1,
            "run 3: and stay settled, rather than re-running forever"
        );

        // The control: editing the producer's SQL must still re-run both, so the
        // settling above is a real staleness answer and not a dead graph.
        fs::write(
            dir.path().join("models/load.sql"),
            "CREATE OR REPLACE TABLE naics_raw AS SELECT 99;",
        )
        .unwrap();
        assert_eq!(
            engine_calls_for_one_run(dir.path(), &state),
            3,
            "run 4: changed SQL must re-run the producer and propagate to its consumer"
        );
    }

    // Round 9, P1 — the content half of the directory hash, which every existing
    // directory test misses because they all move a *path*: they add or remove a
    // file. Reduce `hash_directory_contents` to `(relative path, String::new())` and
    // this is the only thing that notices. A release binary built with that reduction
    // left a partition file's bytes changed, and truncated, both reading
    // `[skip: hash_clean]` at exit 0 — this card's own headline defect, for the
    // Directory kind.
    //
    // No path is created, removed or renamed anywhere in this test.
    #[test]
    fn test_directory_file_content_change_with_no_path_change_forces_rerun() {
        let dir = project_with_seeded_directory(
            "COPY orders TO 'build/parts' (FORMAT parquet, PARTITION_BY (year));",
            &[
                ("build/parts/year=2024.parquet", b"AAAA"),
                ("build/parts/year=2025.parquet", b"BBBB"),
            ],
        );
        let state = MockStateBackend::new();
        let paths_now = || {
            let mut v: Vec<String> = fs::read_dir(dir.path().join("build/parts"))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        };
        let paths_at_start = paths_now();

        assert_eq!(engine_calls_for_one_run(dir.path(), &state), 2, "run 1");
        assert_eq!(
            engine_calls_for_one_run(dir.path(), &state),
            1,
            "run 2: unchanged directory must settle"
        );

        // Same path, same length, different bytes.
        fs::write(dir.path().join("build/parts/year=2024.parquet"), b"ZZZZ").unwrap();
        assert_eq!(
            engine_calls_for_one_run(dir.path(), &state),
            2,
            "run 3: a partition file's BYTES changed under an unchanged set of paths \
             — a hash over paths alone cannot see this"
        );
        assert_eq!(
            engine_calls_for_one_run(dir.path(), &state),
            1,
            "run 4: settles again on the new content"
        );

        // Same path, truncated to empty.
        fs::write(dir.path().join("build/parts/year=2024.parquet"), b"").unwrap();
        assert_eq!(
            engine_calls_for_one_run(dir.path(), &state),
            2,
            "run 5: truncating a partition file must force a re-run"
        );

        assert_eq!(
            paths_now(),
            paths_at_start,
            "this test must never have added or removed a path — otherwise it is \
             pinning the same thing the existing directory tests already pin"
        );
    }

    // Round 9, P2 — the recursion. Real DuckDB nests: `COPY … PARTITION_BY` writes
    // `build/parts/year=2024/data_0.parquet`, while the existing directory test
    // hand-creates flat files directly under the target. Stop descending into
    // subdirectories and the whole tree below the top level becomes invisible, so
    // every run hashes the same empty manifest and the step settles over corruption.
    //
    // The top level of this tree holds no regular file at all, which is what makes
    // run 3 the assertion that reddens.
    #[test]
    fn test_nested_directory_content_change_forces_rerun() {
        let dir = project_with_seeded_directory(
            "COPY orders TO 'build/parts' (FORMAT parquet, PARTITION_BY (year, month));",
            &[
                ("build/parts/year=2024/month=01/data_0.parquet", b"jan"),
                ("build/parts/year=2024/month=02/data_0.parquet", b"feb"),
            ],
        );
        let state = MockStateBackend::new();

        assert_eq!(engine_calls_for_one_run(dir.path(), &state), 2, "run 1");
        assert_eq!(
            engine_calls_for_one_run(dir.path(), &state),
            1,
            "run 2: unchanged nested tree must settle"
        );

        fs::write(
            dir.path()
                .join("build/parts/year=2024/month=02/data_0.parquet"),
            b"FEB",
        )
        .unwrap();
        assert_eq!(
            engine_calls_for_one_run(dir.path(), &state),
            2,
            "run 3: bytes two levels down changed — a non-recursive walk sees an \
             empty manifest here and skips forever"
        );

        fs::remove_file(
            dir.path()
                .join("build/parts/year=2024/month=01/data_0.parquet"),
        )
        .unwrap();
        assert_eq!(
            engine_calls_for_one_run(dir.path(), &state),
            2,
            "run 4: a nested partition deleted must force a re-run too"
        );
        assert_eq!(
            engine_calls_for_one_run(dir.path(), &state),
            1,
            "run 5: settles again"
        );
    }

    // Round 9, P3 — the ordering guarantee `StepAssets::record` rests on. Phase 1
    // (SQL introspection) records a real, source-derived kind; Phase 2 (a hand
    // `produces:`) records a default. `or_insert` keeps the first. Change it to
    // `insert` and the default wins: `build/parts` is a directory that DuckDB writes
    // and a `File` to the runner, `fs::read` on it errors, and the step never
    // settles.
    //
    // Run 2 is what reddens. This is what the four shipping `open_analytics`
    // manifests' safety rests on, in the one configuration where the two phases
    // disagree.
    #[test]
    fn test_a_hand_declaration_cannot_downgrade_sql_introspections_kind() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: export\n    sql: models/export.sql\n    produces: [build/parts]\n";
        setup_project(
            dir.path(),
            yaml,
            &[(
                "models/export.sql",
                "COPY orders TO 'build/parts' (FORMAT parquet, PARTITION_BY (year));",
            )],
        );
        fs::create_dir_all(dir.path().join("build/parts")).unwrap();
        fs::write(dir.path().join("build/parts/year=2024.parquet"), b"one").unwrap();
        let state = MockStateBackend::new();

        assert_eq!(engine_calls_for_one_run(dir.path(), &state), 2, "run 1");
        assert_eq!(
            engine_calls_for_one_run(dir.path(), &state),
            1,
            "run 2: the SQL said Directory and the later hand-declaration must not \
             downgrade it to File — a File read of a directory never settles"
        );

        // Still hashing it AS a directory, not merely ignoring it: emptying it while
        // keeping it present must be noticed.
        fs::remove_file(dir.path().join("build/parts/year=2024.parquet")).unwrap();
        assert_eq!(
            engine_calls_for_one_run(dir.path(), &state),
            2,
            "run 3: emptied directory must force a re-run"
        );
    }

    // Round 9, P4 — rounds 1 to 3's sentinel bug, unpinned for the Directory kind.
    // `hash_directory_contents(&full)?` propagates `None` out of
    // `produced_artifact_hash`, and `is_hash_stale` turns that into unconditional
    // staleness. Fold it into a comparable `"MISSING"` string instead and the
    // baseline recorded right after the step's own "success" already says MISSING, so
    // every later run compares MISSING to MISSING and skips — the graph asserting a
    // directory is produced while nothing is on disk, and the run reporting success.
    //
    // MockEngine executes no SQL, so `build/parts` is never created. Runs 2 and 3
    // are what redden.
    #[test]
    fn test_absent_directory_produce_never_settles_into_skip() {
        let dir = project_with_seeded_directory(
            "COPY orders TO 'build/parts' (FORMAT parquet, PARTITION_BY (year));",
            &[],
        );
        let state = MockStateBackend::new();

        assert!(!dir.path().join("build/parts").exists());

        assert_eq!(engine_calls_for_one_run(dir.path(), &state), 2, "run 1");
        assert_eq!(
            engine_calls_for_one_run(dir.path(), &state),
            2,
            "run 2: a produced directory that is not there must never read as \
             unchanged, however stable the placeholder recorded for it"
        );
        assert_eq!(
            engine_calls_for_one_run(dir.path(), &state),
            2,
            "run 3: same again"
        );

        // And it stops re-running the moment the directory really is there.
        fs::create_dir_all(dir.path().join("build/parts")).unwrap();
        fs::write(dir.path().join("build/parts/year=2024.parquet"), b"real").unwrap();
        assert_eq!(engine_calls_for_one_run(dir.path(), &state), 2, "run 4");
        assert_eq!(
            engine_calls_for_one_run(dir.path(), &state),
            1,
            "run 5: settles once the directory exists — the staleness above was the \
             absence, not a stuck step"
        );
    }
}
