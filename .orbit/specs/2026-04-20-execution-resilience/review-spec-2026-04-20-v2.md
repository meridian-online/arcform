# Spec Review

**Date:** 2026-04-20
**Reviewer:** Context-separated agent (fresh session)
**Spec:** orbit/specs/2026-04-20-execution-resilience/spec.yaml
**Verdict:** APPROVE

---

## Review Depth

| Pass | Triggered by | Findings |
|------|-------------|----------|
| 1 — Structural scan | always | 2 |
| 2 — Assumption & failure | content signal: Engine trait change, state schema change | 1 |
| 3 — Adversarial | not triggered | — |

## Findings

### [LOW] AC-05: MockEngine timeout simulation is assertion-only, not behavioral
**Category:** test-gap
**Pass:** 1
**Description:** AC-05's verification for MockEngine says "when timeout is Some, returns StepTimeout error (simulates timeout without real process)." This means the MockEngine unconditionally returns StepTimeout whenever timeout is Some, which proves that the runner handles StepTimeout correctly but does not test that a step *actually exceeding* a timeout triggers the error. The real `wait_with_timeout` polling logic in DuckDbEngine is only testable via integration tests with real subprocesses. This is an inherent limitation of the mock approach and is acknowledged by the spec's design (separating MockEngine behavior from DuckDbEngine implementation), but worth noting that full confidence requires an integration test.
**Evidence:** spec.yaml AC-05 verification: "Unit test: MockEngine called with Some(timeout) returns StepTimeout." Current MockEngine (engine.rs lines 196-351) has no mechanism to simulate elapsed time.
**Recommendation:** Acceptable for v1. The unit test proves the runner reacts correctly to StepTimeout. The DuckDbEngine polling logic is straightforward enough (try_wait + kill) that a manual smoke test or future integration test suffices. No spec change needed.

### [LOW] AC-06: Pipeline timeout uses simulated_duration which does not exist on MockEngine
**Category:** test-gap
**Pass:** 1
**Description:** AC-06's verification says "MockEngine configured with simulated_duration that exceeds remaining time." The current MockEngine has no `simulated_duration` field. This implies a MockEngine enhancement is needed (adding a field that causes execute calls to sleep or at minimum to advance some notion of elapsed time). The spec describes what to test but the mechanism for making MockEngine respect pipeline-level timeouts is not specified in any AC.
**Evidence:** spec.yaml AC-06 verification mentions "simulated_duration" but no AC specifies adding this to MockEngine. The current MockEngine (engine.rs line 199) has `simulated_stdout` but no duration simulation.
**Recommendation:** The implementation will need to add a `simulated_duration: Option<Duration>` field to MockEngine (or use `std::thread::sleep` in the mock). This is an implementation detail that does not need an AC of its own -- the verification statement makes the intent clear. No spec change needed.

### [LOW] AC-11: Validation boundary for max_attempts minimum
**Category:** assumption
**Pass:** 2
**Description:** AC-11 states "max_attempts must be >= 1" and "max_attempts=0 is rejected." This is correct (0 attempts means the step never runs), but the spec does not clarify whether this validation runs for step-level retry only or also when resolving the effective policy (defaults). If `defaults.retry.max_attempts = 0` and a step has no override, the error should surface at manifest load time (not at runtime). The spec says "Step-level retry validated with same rules" which implies defaults are also validated at manifest load time, but this could be more explicit.
**Evidence:** spec.yaml AC-11: "Manifest-level defaults.retry validation -- max_attempts must be >= 1... Step-level retry validated with same rules."
**Recommendation:** The wording is adequate -- "manifest-level defaults.retry validation" makes clear this fires at manifest load for the defaults struct, and "step-level retry validated with same rules" covers step overrides. The implementation should validate both in the same `validate()` pass. No spec change needed.

---

## Prior Review Findings — Resolution Check

The v1 review (REQUEST_CHANGES) raised 6 findings. The revised spec (v1.1) addresses all of them:

1. **AC-05 polling testability** — Resolved. AC-05 now separates MockEngine behavior ("returns StepTimeout") from DuckDbEngine implementation ("wait_with_timeout helper polls"). The spec no longer conflates the two.
2. **finish_run trait breaking change** — Resolved. Constraint 10 explicitly acknowledges: "finish_run trait change is acceptable for v0.1 (no external consumers) -- uses usize parameter, not a struct."
3. **AC-03/AC-09 overlap** — Resolved. AC-03 now focuses exclusively on retry loop mechanics + backoff_duration extraction. AC-09 owns the output separator contract.
4. **Timeout parameter position** — Resolved. AC-05 explicitly states: "timeout: Option<Duration> as the last parameter on both execute_sql and execute_command."
5. **Timeout + output capture interaction** — Resolved. AC-05 explicitly states: "For capture_stdout steps, timeout applies to the wait() call after stdout drain (no non-blocking I/O in v1)." Constraint 9 reinforces this.
6. **Backoff timing testability** — Resolved. AC-03 extracts `fn backoff_duration(policy, attempt) -> Duration` as a pure function with explicit unit tests: "backoff_duration(backoff_sec=2.0, attempt=1) = 2s, attempt=2 = 4s, attempt=3 = 8s."

---

## Gate-AC Verification Check

No ACs in this spec use `ac_type: gate`. All ACs are `ac_type: code`. Deterministic check passes — no findings.

---

## Honest Assessment

This spec is ready for implementation. The v1.1 revision cleanly resolves all concerns from the prior review. Every AC is specific enough to write tests against, constraints are internally consistent, and the scope matches the goal without over-reaching. The biggest remaining complexity is the DuckDbEngine timeout implementation (polling loop with kill), but the spec wisely separates the testable contract (MockEngine returns StepTimeout when timeout is Some) from the subprocess management detail (DuckDbEngine's wait_with_timeout). The constraint about capture_stdout timeout semantics ("applies to wait after stdout drain") is a pragmatic choice that avoids async complexity while being honest about its limitation. The 11 ACs cover the full feature surface without redundancy.
