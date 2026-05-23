# Implementation Progress

**Spec:** specs/2026-04-15-asset-registry/spec.yaml
**Started:** 2026-04-15

## Hard Constraints
- [x] sqlparser-rs for SQL parsing (DuckDB dialect) — sqlparser 0.55, DuckDbDialect
- [x] Output patterns: CREATE TABLE, CREATE VIEW, CTAS, INSERT INTO, COPY TO
- [x] Input patterns: FROM and JOIN clauses
- [x] Validate-only — no topological reordering
- [x] Pre-execution validation (preflight-style) — runs after preflight, before steps
- [x] Graceful parse degradation (warn, don't block)
- [x] Backwards compatible — v0.1 manifests unchanged — all 24 v0.1 tests pass
- [x] Lineage visualisation out of scope
- [x] Single crate — add modules, not workspace members

## Acceptance Criteria
- [x] ac-01: SQL auto-discover produced assets (DDL) — introspect.rs: 4 tests, asset.rs: 1 test
- [x] ac-02: SQL auto-discover consumed assets (FROM/JOIN) — introspect.rs: 3 tests, asset.rs: 1 test
- [x] ac-03: DML outputs recognised (INSERT INTO, COPY TO) — introspect.rs: 2 tests, asset.rs: 1 test
- [x] ac-04: Command steps accept produces/depends_on — manifest.rs: 1 test, asset.rs: 1 test
- [x] ac-05: Top-level assets: override section — manifest.rs: 1 test, asset.rs: 1 test
- [x] ac-06: Dependency order validation (hard error) — asset.rs: 2 tests, runner.rs: 1 test
- [x] ac-07: Graceful parse degradation (warn, opaque) — introspect.rs: 1 test, asset.rs: 1 test, runner.rs: 1 test
- [x] ac-08: Backwards compatibility (v0.1 tests pass) — manifest.rs: 1 test, runner.rs: 1 test, all 24 v0.1 tests pass
- [x] ac-09: Multi-step chain validation — asset.rs: 1 test, runner.rs: 1 test
- [x] ac-10: Opaque command steps in graph — asset.rs: 1 test

## Test Summary
- **55 total tests** (24 v0.1 + 31 v0.2)
- introspect.rs: 13 tests
- asset.rs: 11 tests
- manifest.rs: 15 tests (11 v0.1 + 4 v0.2)
- runner.rs: 12 tests (8 v0.1 + 4 v0.2)
- cli.rs: 5 tests (v0.1, unchanged)

## New Files
- `src/introspect.rs` — SQL parsing via sqlparser-rs
- `src/asset.rs` — AssetGraph construction and validation

## Modified Files
- `Cargo.toml` — added sqlparser 0.55 dependency
- `src/main.rs` — registered asset and introspect modules
- `src/manifest.rs` — added produces/depends_on to Step, AssetOverride struct, assets field to Manifest
- `src/runner.rs` — integrated AssetGraph::build + validate_order before step execution
- `src/error.rs` — added DependencyOrder error variant
