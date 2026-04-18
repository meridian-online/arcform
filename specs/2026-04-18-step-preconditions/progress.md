# Implementation Progress

**Spec:** specs/2026-04-18-step-preconditions/spec.yaml
**Started:** 2026-04-18

## Hard Constraints
- [x] Field name is `preconditions` (Dagu-compatible)
- [x] Preconditions are a list of typed entries — extensible for future types
- [x] Two initial types: `modified_after` (path + age) and `command` (shell)
- [x] Polarity: all pass = fresh = skip
- [x] AND semantics: ALL must pass
- [x] SQL + preconditions: AND(hash_fresh, preconditions_fresh)
- [x] Command without preconditions: always stale (backwards compat)
- [x] `--force` overrides all preconditions
- [x] Precondition execution errors halt pipeline
- [x] All 105 existing tests pass unchanged — 105/105 still passing
- [x] Use humantime crate for duration parsing
- [x] `modified_after` paths relative to manifest directory
- [x] Use `filetime` crate in tests for mtime control

## Acceptance Criteria
- [x] ac-01: Precondition enum with ModifiedWithin and Command variants — precondition.rs
- [x] ac-02: Step struct gains preconditions field — test_pre_ac02_* (2 tests)
- [x] ac-03: modified_after evaluates file mtime — test_pre_ac03_* (2 tests, uses filetime)
- [x] ac-04: Missing/inaccessible file = stale — test_pre_ac04_*
- [x] ac-05: command precondition exit 0 = fresh — test_pre_ac05_* (2 tests)
- [x] ac-06: AND semantics — test_pre_ac06_* (2 tests)
- [x] ac-07: Command steps with preconditions can be fresh — test_pre_ac07_* (2 tests)
- [x] ac-08: Command steps without preconditions always re-run — test_pre_ac08_*
- [x] ac-09: SQL + preconditions AND(hash, preconditions) — test_pre_ac09_* (4 tests: all combos)
- [x] ac-10: SQL without preconditions unchanged — test_pre_ac10_*
- [x] ac-11: --force overrides preconditions — test_pre_ac11_*
- [x] ac-12: Precondition command error halts pipeline — test_pre_ac12_*
- [x] ac-13: Duration parsing — test_pre_ac13_*
- [x] ac-14: All 105 existing tests pass — 133 total (105 existing + 28 new)
- [x] ac-15: Validation rejects invalid preconditions — test_pre_ac15_* (5 tests)
