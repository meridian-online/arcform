# Progress — Lifecycle Hooks

**Spec:** orbit/specs/2026-04-20-lifecycle-hooks/spec.yaml
**Branch:** rally/lifecycle-hooks
**Started:** 2026-04-20

## Acceptance Criteria

- [x] ac-01 — Hooks struct (on_init, on_success, on_failure, on_exit)
- [x] ac-02 — on_init hook (fatal on failure, on_exit still runs)
- [x] ac-03 — on_success hook (after all steps succeed)
- [x] ac-04 — on_failure hook (ARC_FAILED_STEP, ARC_EXIT_CODE)
- [x] ac-05 — on_exit hook (ARC_PIPELINE_STATUS, always runs)
- [x] ac-06 — Non-fatal hook failures (exit code honesty)
- [x] ac-07 — Manifest validation (reject invalid fields on hooks)
- [x] ac-08 — Backwards compatibility
- [x] ac-09 — Hook execution uses Engine trait
- [x] ac-10 — execute_hook helper function

## Implementation Order

1. manifest.rs — Hooks struct, Manifest field, validation (ac-01, ac-07, ac-08)
2. runner.rs — execute_hook helper, wrap step loop with hook calls (ac-02–ac-06, ac-09, ac-10)
3. Tests for all ACs

## Notes

- `cargo check --all-targets` passes (only pre-existing warnings)
- Tests cannot link on this machine (missing libduckdb.so) — compilation verified only
- Step loop extracted into a closure for clean success/failure branching
- Hooks run outside pipeline timeout boundary (spec constraint)
- on_init failure path: init → on_exit (with init_failed), no steps or on_failure
- Full lifecycle tests verify exact call order for both success and failure paths
