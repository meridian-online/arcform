# 0017 Lifecycle Hooks — Design Interview

## Context

Arcform pipelines currently have no setup or teardown mechanism. If a step fails, the pipeline exits immediately with no opportunity for cleanup or notification. Card 0017 adds declarative lifecycle hooks — init, success, failure, exit — so pipelines can set up preconditions, clean up after failure, and notify on completion.

Key source files: `runner.rs` (pipeline execution loop), `manifest.rs` (YAML parsing), `engine.rs` (subprocess spawning), `error.rs`.

Depends on 0016's `env: &HashMap<String, String>` parameter on the Engine trait for injecting failure context variables.

## Q&A

**Q: Are hooks full Step objects (SQL or command), or command-only?**
A: Full Step objects, reusing the existing struct. Hooks can be SQL or shell commands and get the same `name`/`sql`/`command` validation as pipeline steps. They live in a top-level `hooks:` map keyed by lifecycle phase.

**Q: What's the precise execution order?**
A: `on_init → steps[0..n] → (on_success | on_failure) → on_exit`. Key guarantee: `on_exit` runs if `on_init` was attempted (even if init failed). This mirrors try/finally semantics. If init fails, `on_failure` does NOT run (no step failed — init failure is a separate path).

**Q: Does on_failure get context about which step failed?**
A: Yes, via env vars: `ARC_FAILED_STEP` (step name), `ARC_EXIT_CODE` (exit code). `on_exit` also gets `ARC_PIPELINE_STATUS` ("success", "failed", or "init_failed"). These are injected via the same `Command::envs()` mechanism from 0016.

**Q: What happens if a hook itself fails?**
A: Hook failures are reported (printed to stderr) but non-fatal to the pipeline exit code. The pipeline exit code reflects the original outcome. Exception: `on_init` failure IS fatal — it prevents step execution. `on_exit` always runs, even if `on_failure` fails.

**Q: Do hooks participate in staleness detection?**
A: No. Hooks always run when their phase triggers. Init is setup, success/failure are notifications, exit is cleanup — all must fire every time. Validation rejects preconditions on hook steps.

**Q: Are hook executions recorded in state?**
A: No. Hooks are not recorded in the state backend's step-state table. They don't affect staleness. Terminal output is sufficient observability for v1.

**Q: Where in runner.rs do hooks wire in?**
A: The step loop in `run()` gets wrapped: init before the loop, success/failure branching after, exit in a finally-style block. The step loop body can be extracted into a helper that returns `Result<()>` so the outer function can branch cleanly.

## Summary

### Goal

Declarative lifecycle hooks (init, success, failure, exit) for pipeline setup, teardown, and notification. Hooks are full Step objects — SQL or command.

### Constraints

- Hooks are full `Step` objects (reuse existing struct and validation)
- `on_exit` always runs if `on_init` was attempted (try/finally semantics)
- Hook failures are non-fatal (except `on_init`)
- Hooks do not participate in staleness — always run when triggered
- Hooks are not recorded in state backend (terminal output is enough for v1)
- Hook names must not collide with pipeline step names
- Preconditions on hooks are rejected at validation

### Success Criteria

1. `on_init` hook runs before any pipeline step; failure aborts the pipeline
2. `on_success` hook runs after all steps succeed
3. `on_failure` hook runs when a step fails, with `ARC_FAILED_STEP` and `ARC_EXIT_CODE` env vars
4. `on_exit` hook always runs (success or failure), with `ARC_PIPELINE_STATUS` env var
5. Hook failures are reported but don't change the pipeline exit code (except `on_init`)
6. All hook keys are optional — omitting means that phase is a no-op
7. Existing manifests without hooks work identically (backwards compatible)

### Decisions Surfaced

- **Full Step objects for hooks** — reuse existing struct, hooks can be SQL or command
- **try/finally semantics** — `on_exit` always runs, even if `on_init` or `on_failure` fails
- **Non-fatal hook failures** — pipeline exit code reflects the original outcome, not hook outcome
- **No state recording for hooks in v1** — terminal output is sufficient observability
- **No staleness for hooks** — they always run; preconditions rejected at validation

### YAML Shape

```yaml
hooks:
  on_init:
    name: setup
    command: "mkdir -p /tmp/staging && echo 'pipeline starting'"
  on_success:
    name: notify-ok
    command: "curl -X POST https://hooks.example.com/ok"
  on_failure:
    name: cleanup-and-alert
    command: 'echo "Step $ARC_FAILED_STEP failed (exit $ARC_EXIT_CODE)"'
  on_exit:
    name: teardown
    sql: hooks/teardown.sql

steps:
  - name: load
    sql: models/load.sql
  - name: transform
    sql: models/transform.sql
```

### Structs

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Hooks {
    pub on_init: Option<Step>,
    pub on_success: Option<Step>,
    pub on_failure: Option<Step>,
    pub on_exit: Option<Step>,
}
```

### Files That Change

| File | Change |
|------|--------|
| `manifest.rs` | Add `hooks: Hooks` to Manifest; `Hooks` struct; validation (sql-xor-command, name uniqueness, reject preconditions) |
| `runner.rs` | Wrap step loop with hook calls; new `execute_hook()` and `run_exit_hook()` helpers; extract step loop into helper |
| `engine.rs` | Accept optional env vars for hook context (shared with 0016's env propagation) |
