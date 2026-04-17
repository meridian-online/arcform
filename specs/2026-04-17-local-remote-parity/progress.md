# Implementation Progress

**Spec:** specs/2026-04-17-local-remote-parity/spec.yaml
**Started:** 2026-04-17

## Hard Constraints
- [x] Use dtolnay's `semver` crate — no custom semver implementation
- [x] engine_version is optional — backwards compatible
- [x] Engine::preflight() returns EngineInfo (detection) — runner does comparison (policy)
- [x] Parse duckdb --version output; graceful degradation if unparseable
- [x] All existing tests must pass (91 existing + 14 new = 105 total)
- [x] No local-only features introduced

## Acceptance Criteria
- [x] ac-01: EngineInfo struct with version: Option<semver::Version> — engine.rs
- [x] ac-02: Engine::preflight() returns Result<EngineInfo> — trait + both impls updated
- [x] ac-03: DuckDbEngine parses duckdb --version output — test_lrp_ac03_* (3 tests)
- [x] ac-04: Manifest gains optional engine_version field — test_lrp_ac04_* (2 tests)
- [x] ac-05: Runner compares version against manifest constraint — test_lrp_ac05_* (2 tests)
- [x] ac-06: Rich error with both required and found versions — test_lrp_ac06_*
- [x] ac-07: Missing engine_version skips check — test_lrp_ac07_*
- [x] ac-08: Invalid engine_version caught at validation — test_lrp_ac08_*
- [x] ac-09: arc init scaffolds engine_version: ">=1.0" — test_lrp_ac09_*
- [x] ac-10: MockEngine configurable version — test_lrp_ac10_*
- [x] ac-11: All existing tests pass with new return type — 91/91 passing
- [x] ac-12: Unparseable version warns and continues — test_lrp_ac12_* (2 tests)
- [x] ac-13: Portability principle documented (doc AC) — recorded in spec constraints
