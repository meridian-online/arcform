# Spec Review

**Date:** 2026-04-20
**Reviewer:** Context-separated agent (fresh session)
**Spec:** orbit/specs/2026-04-20-execution-resilience/spec.yaml
**Verdict:** REQUEST_CHANGES

---

## Review Depth

| Pass | Triggered by | Findings |
|------|-------------|----------|
| 1 — Structural scan | always | 3 |
| 2 — Assumption & failure | content signals (cross-system boundary: Engine trait change, state schema change) | 3 |
| 3 — Adversarial | not triggered | — |

## Findings

### [MEDIUM] AC-05: Polling interval as implementation detail without testability contract
**Category:** test-gap
**Pass:** 1
**Description:** AC-05 specifies "polls child.try_wait() in ~100ms intervals" and "On timeout, child.kill()". The verification says "MockEngine with simulated long-running step; verify StepTimeout error" but the MockEngine currently returns results synchronously (no child process to poll). The AC conflates the DuckDB-specific implementation (`wait_with_timeout` helper that polls a real process) with the Engine trait abstraction. The MockEngine cannot simulate a child process being polled -- it can only simulate a returned error. This means the core timeout logic (the polling loop) is untestable via MockEngine.
**Evidence:** Current `MockEngine::execute_sql` and `MockEngine::execute_command` in `engine.rs` lines 287-351 return `Result<StepOutput>` synchronously. There is no mechanism to simulate "takes N seconds then returns".
**Recommendation:** Split AC-05 into two concerns: (1) Engine trait gains `timeout: Option<Duration>` parameter -- testable via MockEngine returning StepTimeout when timeout is Some; (2) DuckDbEngine implements `wait_with_timeout` with polling -- tested via integration test with a real `sleep` command. Alternatively, add a `sleep_duration: Option<Duration>` field to MockEngine that blocks before returning, so timeout-based interruption can be tested.

### [LOW] AC-08: StateBackend::finish_run signature change is a breaking trait change
**Category:** missing-requirement
**Pass:** 1
**Description:** AC-08 specifies adding `total_retries: usize` to `StateBackend::finish_run`. This changes a public trait method signature, which will break any external implementations. The current `finish_run` signature (state.rs line 67) takes `(run_id, steps_executed, outcome)`. Adding a parameter changes every implementor.
**Evidence:** state.rs line 67: `fn finish_run(&self, run_id: &str, steps_executed: usize, outcome: &str) -> Result<()>;`
**Recommendation:** This is acceptable for v0.1 where there are no external consumers, but worth noting. The spec should explicitly acknowledge this is a trait-breaking change. Alternatively, consider passing a struct (e.g., `RunSummary { steps_executed, total_retries, outcome }`) to allow future extension without further trait changes.

### [LOW] AC-03/AC-09: Overlapping retry output specification
**Category:** constraint-conflict
**Pass:** 1
**Description:** AC-03 specifies "Print separator lines between attempts: '[retry N/M, backoff Xs]'" and AC-09 specifies "Before each retry (not the first attempt), print a separator: '[retry N/M, backoff Xs]'". These describe the same behaviour in two ACs, creating ambiguity about which is the canonical source. AC-09 adds the nuance of "not the first attempt" and "real-time streaming", but the separator format is duplicated.
**Evidence:** spec.yaml lines 27-28 (AC-03) and lines 57-59 (AC-09).
**Recommendation:** Remove the separator format from AC-03 (keep it focused on the retry loop logic), and let AC-09 own the output contract exclusively.

### [MEDIUM] AC-05: timeout parameter position on Engine trait methods
**Category:** assumption
**Pass:** 2
**Description:** AC-05 states "Engine trait methods gain timeout: Option<Duration> parameter" but does not specify the parameter position relative to the existing `env: &HashMap<String, String>` and `capture_stdout: bool` parameters. The MockEngine has different method signatures for `execute_sql` and `execute_command`, so the timeout placement matters for ergonomics and the implementation spec should be explicit.
**Evidence:** engine.rs line 57: `fn execute_sql(&self, db_path: &Path, sql_path: &Path, env: &HashMap<String, String>) -> Result<StepOutput>;` and line 62: `fn execute_command(&self, command: &str, env: &HashMap<String, String>, capture_stdout: bool) -> Result<StepOutput>;`
**Recommendation:** Specify the position explicitly (e.g., "timeout: Option<Duration> as the last parameter on both methods") or move to a params struct for Engine methods to avoid continued parameter creep.

### [MEDIUM] AC-05/AC-06: Timeout interaction with output capture (command steps)
**Category:** failure-mode
**Pass:** 2
**Description:** The spec describes timeout via `child.try_wait()` polling and `child.kill()`, but for command steps with `capture_stdout: true`, the current implementation (engine.rs lines 124-133) reads stdout to completion *before* calling `child.wait()`. If the child is blocked on stdout and doesn't exit, the read will hang forever -- the timeout polling of `try_wait()` only works if you can interleave checking with output consumption. A killed child's partial stdout buffer state is also unspecified.
**Evidence:** engine.rs lines 122-133 show synchronous stdout reading before wait. AC-05's polling approach assumes stderr piped + try_wait loop, which works for SQL steps but conflicts with the stdout-capture pattern.
**Recommendation:** Add a constraint or AC clarifying how timeout interacts with `capture_stdout: true`. Options: (a) timeout only applies to the wait after stdout is drained, (b) spawn a timeout thread that kills the child regardless of reader state, (c) use non-blocking reads with select/poll. This is a real edge case that will surface during implementation.

### [LOW] AC-03: Retry backoff timing precision not specified for testing
**Category:** test-gap
**Pass:** 2
**Description:** AC-03 uses `std::thread::sleep` for backoff delays. The verification tests timing via "verify 3 engine calls" which proves attempt count but not backoff correctness. The exponential formula is specified (`backoff_sec * 2^(attempt-1)`) but there is no AC verifying the actual sleep durations are correct. Unit tests cannot easily assert on sleep timing without mocking time.
**Evidence:** AC-03 verification: "Verify 3 engine calls" -- tests that retry happened, not that backoff was correct.
**Recommendation:** This is acceptable for v1 (testing attempt count is the critical path; backoff correctness is observable via separator output format). Optionally, extract backoff calculation into a pure function `fn backoff_duration(policy: &RetryPolicy, attempt: u32) -> Duration` and test that directly.

---

## Honest Assessment

This spec is well-structured and covers the feature comprehensively. The biggest risk is the timeout mechanism for command steps with output capture (finding 5) -- the current stdout-reading pattern in `execute_command` is inherently blocking and will deadlock if you try to poll `try_wait()` while also waiting on stdout. This needs a design decision before implementation begins. The remaining findings are minor (trait signature ergonomics, overlapping ACs, test strategy for polling). With the timeout-capture interaction clarified and the AC-03/AC-09 overlap resolved, this spec is ready to implement.
