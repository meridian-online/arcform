# Implementation Progress

**Spec:** specs/2026-04-16-selective-rematerialisation/spec.yaml
**Started:** 2026-04-16

## Hard Constraints
- [x] StateBackend is a trait — DuckDbStateBackend is the first implementation
- [x] DuckDbStateBackend uses `duckdb` crate for in-process access
- [x] Connection opened/closed around state operations
- [x] SHA-256 content hash, raw bytes, no normalisation
- [x] Downstream propagation via asset graph, opaque steps don't participate
- [x] Failed steps always stale
- [x] Command steps always re-run
- [x] --force executes AND records new state
- [x] State tables co-located in project database
- [x] New deps: `duckdb`, `sha2`
- [x] Steps should be idempotent (crash recovery acknowledged)

## Acceptance Criteria
- [x] ac-01: StateBackend trait — `state::StateBackend` trait with `init`, `get_step_state`, `record_step`, `start_run`, `finish_run`. MockStateBackend for testing. Tests: `test_ac01_mock_state_backend`, `test_ac01_mock_run_tracking`.
- [x] ac-02: DuckDbStateBackend creates tables on first use — `_arcform_state` and `_arcform_runs` via CREATE TABLE IF NOT EXISTS. Idempotent init. Test: `test_ac02_duckdb_backend_init`.
- [x] ac-03: SQL content hash stored in _arcform_state — SHA-256 via `sha2` crate. Test: `test_ac03_content_hash_stored`.
- [x] ac-04: Fresh SQL step skipped — hash comparison in `compute_staleness()`. Test: `test_v03_ac04_fresh_step_skipped`.
- [x] ac-05: Stale SQL step re-runs — hash change detected. Test: `test_v03_ac05_stale_step_reruns`.
- [x] ac-06: Downstream propagation — `AssetGraph::downstream_steps()` computes transitive closure. Tests: `test_v03_ac06_downstream_propagation`.
- [x] ac-07: AssetGraph.downstream_steps method — fixed-point iteration over asset reads/produces. Tests: `test_v03_ac07_downstream_steps`, `test_v03_ac07_downstream_opaque_breaks_chain`.
- [x] ac-08: Failed step always stale — `StepStatus::Failed` check in `compute_staleness()`. Test: `test_v03_ac08_failed_step_reruns`.
- [x] ac-09: Command steps always re-run — `step.command.is_some()` in `compute_staleness()`. Test: `test_v03_ac09_command_always_reruns`.
- [x] ac-10: --force runs all AND records state — force flag in `compute_staleness()` + state recording in runner. Test: `test_v03_ac10_force_runs_all`.
- [x] ac-11: First run = all stale — `None` match in `compute_staleness()`. Test: `test_v03_ac11_first_run_all_stale`.
- [x] ac-12: Step progress output shows skip/run — `[skip]` suffix for fresh steps, `...` for executing. Verified by output format in runner.
- [x] ac-13: _arcform_runs records history — run_id, started_at, finished_at, steps_executed, outcome. Test: `test_ac13_run_history`.
- [x] ac-14: Runner signature updated, CLI wires backend — `run(dir, engine, state, force)`. CLI creates `DuckDbStateBackend` from manifest `db_path`. `Commands::Run { force }` with `--force` flag.
- [x] ac-15: All existing tests pass — 91 tests, 0 failures. All existing tests updated to pass `MockStateBackend` and `false` for force.

## Test Summary
- **91 tests total** — 0 failures
- All existing v0.1 and v0.2 tests pass unchanged (signature updated)
- 8 new staleness tests in `runner::tests`
- 5 new state backend tests in `state::tests`
- 5 new asset graph tests in `asset::tests`
