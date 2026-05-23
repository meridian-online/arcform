# Progress — Execution Resilience

**Spec:** orbit/specs/2026-04-20-execution-resilience/spec.yaml
**Branch:** rally/execution-resilience
**Started:** 2026-04-20

## Acceptance Criteria

- [x] ac-01 — RetryPolicy struct, Defaults struct
- [x] ac-02 — Step retry/timeout_sec fields, Manifest defaults/timeout_sec
- [x] ac-03 — Retry execution loop with exponential backoff, backoff_duration()
- [x] ac-04 — Defaults resolution (step overrides wholesale)
- [x] ac-05 — Step timeout (Engine trait timeout param, wait_with_timeout, MockEngine)
- [x] ac-06 — Pipeline timeout (Instant tracking, remaining time clamp)
- [x] ac-07 — StepTimeout and PipelineTimeout error variants
- [x] ac-08 — State records final outcome only, finish_run total_retries
- [x] ac-09 — Retry output separators
- [x] ac-10 — Backwards compatibility
- [x] ac-11 — Validation (max_attempts >= 1, backoff_sec >= 0)

## Implementation Order

1. error.rs — StepTimeout, PipelineTimeout variants (ac-07)
2. manifest.rs — RetryPolicy, Defaults, new fields, validation (ac-01, ac-02, ac-11)
3. engine.rs — timeout param on Engine trait, MockEngine timeout sim, wait_with_timeout (ac-05)
4. state.rs — finish_run total_retries param (ac-08)
5. runner.rs — retry loop, backoff_duration, defaults resolution, pipeline timeout, separators (ac-03, ac-04, ac-06, ac-09)
6. Tests for ac-08, ac-10 (backwards compat)

## Notes

- `cargo check --all-targets` passes (only pre-existing warnings)
- Tests cannot link on this machine (missing libduckdb.so) — compilation verified only
- backoff_duration() extracted as a pure function for testability
- Pipeline timeout clamps step timeout to remaining time
- Retry separators use `--- retry N/M ---` format on stderr
