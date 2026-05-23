# 0015 Execution Resilience — Design Interview

## Context

Arcform runs pipelines that fetch from external APIs and execute long-running SQL queries. Currently, any failure halts the pipeline immediately and any hung process blocks indefinitely. Card 0015 adds retry policies and timeouts so transient failures recover automatically and stuck processes get killed.

Key source files: `runner.rs` (step execution loop), `manifest.rs` (YAML parsing), `engine.rs` (subprocess spawning), `error.rs`.

## Q&A

**Q: Where does retry config live — step-level only, or also manifest-level defaults?**
A: Both. Step-level `retry` field plus manifest-level `defaults.retry` for DRY pipelines. Step overrides defaults wholesale (no field-merging). Both optional; omitting preserves backwards compatibility.

**Q: What backoff strategy?**
A: Exponential only: `backoff_sec * 2^(attempt-1)`. No jitter — local-first tool, no thundering-herd risk. Fixed delay is the degenerate case; add a `backoff:` discriminator later if demand emerges.

**Q: Do retries re-evaluate preconditions?**
A: No. Preconditions evaluate once before the first attempt. They're a freshness gate ("should this step run at all?"), not a readiness probe. If preconditions say "fresh", the step skips entirely and retries never fire.

**Q: How do we kill a running subprocess on timeout?**
A: `child.try_wait()` polling loop (~100ms) against an `Instant` deadline. On timeout, `child.kill()` sends SIGKILL. DuckDB CLI is a single process — no process-group management needed. Logic lives in a `wait_with_timeout` helper in `engine.rs`.

**Q: Pipeline-level timeout — where is the timer tracked?**
A: `Instant` started at top of `run()`. Before each step executes, check remaining time. Step timeout is clamped to `min(step.timeout_sec, remaining_pipeline_time)`. Skipped (fresh) steps don't count — the check fires only before execution.

**Q: How do retries appear in state?**
A: Record only the final outcome. One row per step in `_arcform_state`. Retry count goes into `_arcform_runs.total_retries` for observability.

**Q: Output streaming across retries?**
A: Show all attempts with separator lines: `[retry N/M, backoff Xs]`. Visibility into each attempt matters for debugging flaky APIs.

**Q: What about exit code filtering?**
A: Dropped for v1. Retry on any non-zero exit code. Simplifies config — `exit_codes` field can be added later if a real use case surfaces.

## Summary

### Goal

Step-level retry with exponential backoff + step/pipeline timeouts. Transient failures handled, stuck processes killed, backwards compatible.

### Constraints

- Preconditions evaluate once (freshness gate, not readiness probe)
- Pipeline timeout clamps step timeout — no step can outlive the pipeline
- State records only final outcome, not per-attempt rows
- No exit code filtering in v1 (retry on any non-zero)

### Success Criteria

1. Step with `retry` field retries on failure up to `max_attempts` with exponential backoff
2. Step with `timeout_sec` is killed after the deadline
3. Manifest-level `timeout_sec` halts the pipeline when total execution time is exceeded
4. `defaults.retry` provides inheritable retry policy; step-level overrides wholesale
5. Steps without retry/timeout behave identically to today (backwards compatible)
6. All retry attempts stream output in real-time with attempt separators

### Decisions Surfaced

- **Exponential-only backoff** — simplest useful strategy, no jitter needed locally
- **Preconditions evaluate once** — freshness and retry are separate concerns
- **Final-outcome-only state** — per-attempt rows would complicate staleness queries
- **Drop exit_codes for v1** — retry on any non-zero, add filtering later if needed

### YAML Shape

```yaml
timeout_sec: 1800           # pipeline-level

defaults:
  retry:
    max_attempts: 3
    backoff_sec: 2

steps:
  - name: fetch-api
    command: "curl -f https://api.example.com/data > data.json"
    timeout_sec: 120
    retry:
      max_attempts: 5
      backoff_sec: 1
```

### Structs

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    #[serde(default = "default_backoff")]
    pub backoff_sec: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Defaults {
    #[serde(default)]
    pub retry: Option<RetryPolicy>,
}
```

### Files That Change

| File | Change |
|------|--------|
| `manifest.rs` | Add `timeout_sec`, `defaults` to Manifest; add `timeout_sec`, `retry` to Step; `RetryPolicy`, `Defaults` structs; validation |
| `engine.rs` | Add `timeout: Option<Duration>` to Engine trait; `wait_with_timeout` helper |
| `runner.rs` | Pipeline-level `Instant` tracking; retry loop around step execution; resolve effective retry policy |
| `error.rs` | `StepTimeout`, `PipelineTimeout` variants |
| `state.rs` | `total_retries` column on `_arcform_runs` |
