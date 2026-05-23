# Spec Review: Selective Rematerialisation

**Reviewer:** Nightingale (fresh context)
**Date:** 2026-04-16
**Spec:** `specs/2026-04-16-selective-rematerialisation/spec.yaml`
**Verdict:** REVISE -- three blocking issues, several gaps

---

## 1. Assumption Audit

| # | Assumption | What happens when wrong? | Severity |
|---|-----------|-------------------------|----------|
| A1 | DuckDbStateBackend can read/write its metadata tables via the same mechanism as step execution | The spec never states HOW the backend executes SQL against DuckDB. The current Engine uses `duckdb -f file.sql` (CLI subprocess). Does the state backend shell out to `duckdb` too? Or does it use a Rust DuckDB library (`duckdb-rs`)? **See Critical Issue #1.** | **BLOCKING** |
| A2 | The state tables `_arcform_state` and `_arcform_runs` live in the same database file as user data | Not stated explicitly. If someone uses a different `db:` path per environment, state travels with the data. This is probably correct but should be a stated constraint. If wrong: state is lost when switching databases. | Medium |
| A3 | The asset graph can provide downstream dependencies for a given step | The current `AssetGraph` has NO method to query "given step X is stale, which downstream steps are also stale?" It only has `validate_order()` and `has_assets()`. **AC-06 requires this capability but the spec assumes it exists.** | **BLOCKING** |
| A4 | Step names are stable identifiers across runs | If a user renames a step, its old state row becomes orphaned and the renamed step runs as "new." This is probably fine but undocumented. | Low |
| A5 | SHA-256 of file contents is a reliable staleness signal | Reformatting SQL (adding whitespace) triggers a re-run even when semantically identical. This is the correct trade-off (safe side) but should be an acknowledged limitation. | Low |
| A6 | The runner currently receives only `&dyn Engine` -- there is no injection point for a state backend | The `run()` function signature is `fn run(dir: &Path, engine: &dyn Engine) -> Result<()>`. The spec does not address how `StateBackend` gets wired in. **See Critical Issue #2.** | **BLOCKING** |
| A7 | `--force` flag can be added to the `Run` command variant | The current `Commands::Run` has no fields. Adding a `--force` flag is straightforward (clap derive), but the spec doesn't mention how `force` propagates from CLI to runner to state backend. | Low |
| A8 | "First run" means no `_arcform_state` table exists | What if the table exists but is empty (e.g., after a manual `DELETE FROM _arcform_state`)? Should behave identically to "no table." Worth a test edge case. | Low |

---

## 2. Critical Issues (Blocking)

### Issue #1: How does DuckDbStateBackend execute SQL?

The spec introduces `DuckDbStateBackend` with methods like `get_step_state`, `record_step`, `start_run`, and `finish_run`. These must read/write DuckDB tables. But the current architecture has exactly one way to talk to DuckDB: shelling out to `duckdb -f file.sql` via the `Engine` trait.

**The spec must answer:** Does DuckDbStateBackend:

**(a)** Use the `duckdb-rs` crate (Rust bindings, in-process)?
- Pro: Clean, no temp files, proper error handling, can return query results
- Con: Adds a new dependency (~15MB), creates two access paths to the same `.duckdb` file (CLI for steps, library for state), potential file locking conflicts

**(b)** Shell out to `duckdb` CLI for its own queries?
- Pro: No new dependency, consistent access path
- Con: Parsing CLI stdout for query results is fragile, temp SQL files for every state check, slow (process spawn per staleness check)

**(c)** Use the existing `Engine` trait?
- Pro: Reuses existing code
- Con: `Engine::execute_sql` takes a file path (not inline SQL), returns `StepOutput` (stderr only, no query results), and has no way to return rows. The trait would need significant changes.

**Recommendation:** Option (a) is the only viable path. The state backend needs to execute `SELECT` queries and read results -- something the current Engine trait cannot do. This means adding `duckdb-rs` to `Cargo.toml`. The spec should state this explicitly and acknowledge the dual-access-path risk.

**File locking risk:** If a SQL step is running via `duckdb -f` (separate process) while the runner (same process) holds a `duckdb-rs` connection for state tracking, DuckDB's single-writer model may cause conflicts. The backend must open/close connections carefully, or the design needs WAL mode enforcement.

### Issue #2: Runner wiring -- no injection point for StateBackend

The current runner signature:

```rust
pub fn run(dir: &Path, engine: &dyn Engine) -> Result<()>
```

The spec adds a `StateBackend` trait but never specifies:
- Where is `StateBackend` instantiated?
- How does it reach the runner? New parameter? Embedded in a context struct?
- Who decides which backend to use? (The interview says "config" but defers the mechanism.)

The CLI dispatch (`cli.rs:run_pipeline`) hardcodes `DuckDbEngine` and calls `runner::run(&cwd, &engine)`. The spec needs to address the equivalent wiring for the state backend.

**Suggested resolution:** The runner signature should become something like:

```rust
pub fn run(dir: &Path, engine: &dyn Engine, state: &dyn StateBackend, force: bool) -> Result<()>
```

Or introduce a `RunContext` struct. Either way, the spec needs a constraint covering this.

### Issue #3: AssetGraph has no downstream query capability

AC-06 requires: "stale upstream step makes dependent steps stale." The current `AssetGraph` can only validate ordering. It has no method to answer "given that step A is stale, which other steps depend on assets produced by step A?"

Implementing downstream propagation requires:
1. A reverse lookup: asset -> producing step
2. A forward lookup: step -> assets it reads
3. Transitive closure: if step B reads an asset produced by step A, and step C reads an asset produced by step B, then marking A stale must cascade to B and C

The spec should add an AC (or expand AC-06's verification) to cover the `AssetGraph` API addition, e.g.:

```
AssetGraph provides a method to compute the transitive set of downstream
steps given a set of stale steps.
```

Without this, the implementer must design this API on the fly, which violates the spec's role as a complete implementation contract.

---

## 3. Failure Mode Analysis

| AC | How it could pass in tests but fail in production |
|----|--------------------------------------------------|
| AC-02 | Test uses a fresh temp database. Production: the database already has user tables with names that collide with `_arcform_state` (unlikely but possible if a user creates tables with `_arcform` prefix). Consider `CREATE TABLE IF NOT EXISTS`. |
| AC-03 | Test hashes a known string. Production: file encoding differences (BOM, line endings) between platforms could cause hash mismatches on the same logical file. The spec doesn't specify normalization. SHA-256 of raw bytes is correct (no normalization) but cross-platform pipelines may get false staleness. |
| AC-04 | Test runs twice in-process with a mock. Production: between runs, the DuckDB file could be replaced or corrupted. The state backend should handle missing/corrupt state gracefully (treat as "all stale"). |
| AC-06 | Test uses a mock engine with a 3-step chain. Production: the asset graph might fail to parse SQL (unparseable SQL = opaque step = no known assets = no downstream propagation). A step with unparseable SQL that DOES produce assets will silently break the propagation chain. |
| AC-07 | Test verifies a failed step re-runs. Production: if the runner crashes (SIGKILL, power loss) BETWEEN executing the step and recording state, the step succeeds in DuckDB but `_arcform_state` never records it. Next run re-executes the step. For non-idempotent steps (INSERT INTO), this causes duplicate data. **See Issue #4 below.** |
| AC-09 | Test verifies `--force` runs all steps. Works fine. But: does `--force` also update state afterward? If not, the next non-force run would see stale state from before the force run. The spec should state: `--force` runs all steps AND records their new state. |
| AC-11 | Test checks stdout for `[SKIP]`/`[RUN]`. Production: if stdout is piped or redirected, the output format might differ (owo-colors may strip ANSI). Cosmetic only. |
| AC-12 | Test checks `_arcform_runs` has one row. Production: what happens after 10,000 runs? No mention of retention or cleanup. This is fine for v0.3 but worth acknowledging. |
| AC-13 | Test verifies existing tests pass. This is necessary but not sufficient -- it doesn't test that the new state-aware runner produces identical behavior when `StateBackend` is `None` or a no-op implementation. |

---

## 4. Additional Gaps

### Gap 1: Interruption / crash recovery (race condition)

The runner currently has no transactional boundary. The spec's execution model is:

1. Check staleness
2. Execute step
3. Record success/failure

If the process is killed between step 2 and step 3:
- The step executed (data changed in DuckDB) but state was never recorded
- Next run treats the step as stale and re-runs it
- For `CREATE TABLE`: harmless (idempotent via `CREATE OR REPLACE`)
- For `INSERT INTO`: **duplicates data**
- For `command` steps: depends entirely on the command

The spec should acknowledge this limitation and recommend `CREATE OR REPLACE` patterns, or add a constraint: "state is recorded best-effort; steps should be idempotent."

### Gap 2: `--force` state recording

The spec says `--force` runs all steps regardless of staleness. But it doesn't say whether `--force` also updates the stored hashes. If it doesn't, the next normal run would see the pre-force hashes and re-run everything again, making `--force` a one-shot that poisons subsequent runs.

**Expected behavior:** `--force` should execute all steps AND record their new hashes/status. Add this as an explicit constraint.

### Gap 3: Mixed step types in downstream propagation

Consider: Step A (SQL, stale) -> Step B (command, no preconditions) -> Step C (SQL, fresh).

Per the spec:
- Step A is stale (SQL changed) -> re-runs
- Step B always re-runs (command) -> re-runs regardless
- Step C: is it stale because A was stale?

The propagation chain depends on the asset graph. But if Step B is opaque (no `produces`/`depends_on` declared), the chain is broken. Step C would appear fresh even though its upstream data changed.

The spec should state: "downstream propagation follows the asset graph only. Opaque steps (no declared assets) do not participate in propagation chains." This is the correct behavior but needs to be explicit.

### Gap 4: State backend for command-only pipelines

A pipeline with only command steps has no SQL steps, so `engine.preflight()` is skipped. But `DuckDbStateBackend` still needs a database to write to. Where does the state go?

Options:
- State tables go in the project's `db:` database (but for command-only pipelines, should a DuckDB file be created just for state?)
- State backend uses a separate database (e.g., `.arcform/state.duckdb`)

The spec should address this. The `db_path` from the manifest defaults to `<name>.duckdb` -- using this for state even in command-only pipelines means `arc run` creates a DuckDB file that the user might not expect.

### Gap 5: Ontology schema is incomplete

The `ontology_schema` section defines `RunState` with 4 fields for `_arcform_state`, but `_arcform_runs` has no schema definition. AC-12 lists its fields (run_id, started_at, finished_at, steps_run, outcome) but there's no ontology entry. The `steps_run` field type is ambiguous -- is it an integer count or a list of step names?

### Gap 6: No error type for state backend failures

`error.rs` has no variant for state backend errors (e.g., failed to read state, failed to write state, corrupt state table). The spec should require new error variants, or at minimum state that state backend errors are wrapped in an existing variant.

---

## 5. Test Adequacy

| AC | Verification | Adequate? | Gap |
|----|-------------|-----------|-----|
| AC-01 | Trait compiles, mock used | Yes | -- |
| AC-02 | Test against fresh DB | Partial | Doesn't test `CREATE TABLE IF NOT EXISTS` idempotency (what if tables already exist from a prior version?) |
| AC-03 | Verify hash in state table | Yes | -- |
| AC-04 | Mock engine not called on second run | Yes | -- |
| AC-05 | Edit SQL, verify re-run | Yes | -- |
| AC-06 | 3-step chain propagation | Partial | Doesn't test broken chains (opaque middle step), doesn't test diamond dependencies (A->B, A->C, B->D, C->D) |
| AC-07 | Failed step re-runs | Yes | Doesn't test: what if the step fails twice in a row? (Should still be stale.) |
| AC-08 | Command runs twice | Yes | -- |
| AC-09 | --force runs fresh steps | Yes | Doesn't verify state is updated after --force |
| AC-10 | Fresh DB runs all | Yes | -- |
| AC-11 | Output includes SKIP/RUN | Partial | Doesn't specify skip reason (hash match vs. not applicable) |
| AC-12 | _arcform_runs has one row | Partial | Doesn't test partial runs (pipeline fails mid-way -- what's the outcome field?) |
| AC-13 | Existing tests pass | Yes | Necessary but not sufficient |

**Missing test scenarios:**
- State table schema migration (v0.3 -> v0.4 adds a column -- how?)
- Concurrent `arc run` invocations on the same project
- State table exists but is empty
- State table has entries for steps that no longer exist in the manifest

---

## 6. Constraint Check

| Constraint | Realistic? | Issues |
|-----------|-----------|--------|
| "StateBackend is a trait" | Yes | Straightforward Rust pattern |
| "SHA-256 content hash" | Yes | `sha2` crate needed (not in Cargo.toml) or use `ring` |
| "Downstream propagation via asset graph" | Partial | Asset graph needs new API (Issue #3) |
| "Failed steps always stale" | Yes | Simple: check `status != 'success'` |
| "Command steps always re-run" | Yes | Simple: skip staleness check for command steps |
| "Two DuckDB metadata tables" | Yes | But needs `duckdb-rs` (Issue #1) |
| "--force flag overrides all staleness" | Yes | Needs plumbing from CLI to runner (Issue #2) |
| "State backend must not block on first run" | Yes | Handle "table does not exist" gracefully |

**Missing dependency:** Neither `sha2`/`ring` (for SHA-256) nor `duckdb` (for state backend) are in `Cargo.toml`. The spec should list new dependencies as a constraint.

---

## 7. Recommendations

### Must fix before implementation:

1. **Add a constraint specifying the DuckDB access mechanism for the state backend.** Recommend: `duckdb-rs` crate, with a stated constraint that the connection is opened/closed around state operations (not held during step execution) to avoid file locking conflicts with CLI-based step execution.

2. **Add a constraint (or AC) for runner wiring.** Specify the new `run()` signature and how `StateBackend` + `force` flag are threaded from CLI to runner.

3. **Add an AC for the AssetGraph downstream propagation API.** The current graph has no such method. This is non-trivial new code that should be specified.

4. **Add a constraint: `--force` records state after execution.** Without this, the behavior is ambiguous.

5. **Add `sha2` and `duckdb` to the new-dependency list** (or whichever crates are chosen).

### Should fix:

6. Acknowledge the crash-recovery limitation (steps should be idempotent).
7. Clarify behavior for command-only pipelines (where does state go?).
8. Add `_arcform_runs` to the ontology schema.
9. Add error variants for state backend failures to the error.rs plan.

### Nice to have:

10. Test for diamond dependencies in downstream propagation.
11. Test for orphaned state entries (steps removed from manifest).
12. State what happens when SQL is unparseable for downstream propagation purposes (chain breaks silently).
