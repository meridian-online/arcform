# Checks and measures Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Protocol can declare `check:` and `measure:` steps; arc runs them on every run, measures each produced table at its step's end, gives checks this run's measures and earlier runs' measures as tables, and records every check and measure in the run record under a new contract version.

**Architecture:** The manifest gains two step arms beside `sql:`, `command:` and `op:`. A check or measure file is a Protocol SQL file like any other (refusals, asset graph, run record `sql` section), but arc runs it inside a generated wrapper file in an answer directory of its own, executed through the same `Engine::execute_sql` as a SQL step, which writes one JSON answer that arc reads back. New logic lives in one new module, `src/check.rs`; the runner, contract and manifest change only where they must.

**Tech Stack:** Rust 2024, serde/serde_json/serde_yaml, the `duckdb` crate (linked library, for table measures after a step), the DuckDB 2.0 command line (for every SQL a Protocol writes), humantime.

**Spec:** `docs/superpowers/specs/2026-11-15-protocol-checks-and-tests-design.md`, sections Checks, Measures, Comparing with earlier runs and The run record. Telling the analyst (exit codes, summary, webhook, `arc runs show`), Protocol tests (`arc test`) and the hosted runner are other issues and are out of scope here.

## Global Constraints

- SQL a Protocol writes runs through the DuckDB command line (`Engine::execute_sql`), never through the linked library. The linked library may only read the database after a step's process has exited (as `contract::build_contract` already does).
- Every DuckDB process goes through `DuckDbProgram::command()` (it sets `DUCKDB_AGENT_MODE=0`); `execute_sql` already does, so the wrapper inherits it.
- Run one cargo build at a time on Hugh's Mac, and never two implementers at once. Before any cargo command, check no other cargo/rustc process is running (`pgrep -fl 'cargo|rustc'`) and wait if one is.
- Development builds need `DUCKDB_LIB_DIR` (and `DYLD_LIBRARY_PATH`) pointing at DuckDB's official v2.0.0 release library, and the v2.0.0 release `duckdb` CLI first on `PATH` for integration tests, as `CLAUDE.md`'s Known issues says. Never use `--features bundled-duckdb` for task verification.
- British English in every message, doc and comment. Error messages name the step and say what to do.
- This repo is public: no Kata refs, private repo names or local paths in commits, code or docs.
- A new file under `src/` must be added to `MODULE_FILES` in `tests/public_surface.rs` (the list is frozen; widening it is the deliberate act). New `pub` fields on `Step` and `Manifest` widen the published `spec` surface; run `cargo test --test public_surface` in each task that adds one and update its expectations if it pins fields.
- Quick checks before every commit: `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`, then the task's own tests. CI runs the full gate.

## Rulings taken for this plan (Hugh away; each reversible)

Recorded here so Hugh reads them in one place; each is restated in the task that implements it.

1. **Contract tag** `b5/1` (`CONTRACT_VERSION`). New fields are `#[serde(default)]` so a `b4/1` record still reads.
2. **Sample cap** 20 rows; each cell is the value cast to `VARCHAR` (SQL `NULL` stays JSON `null`), truncated to 200 characters with a trailing `…` when longer.
3. **Built-in measures live in the producing step's own entry**, as a `measure` section of the same shape a measure step has (`{"values": {"table:t.row_count": …, "table:t.bytes": …}}`), so `measure_history` reads one shape from every step. A step skipped as fresh still has its tables measured when the run reaches it, so history has no holes on runs where nothing changed.
4. **`table:<t>.bytes`** is the table's storage blocks (distinct `block_id` plus `additional_block_ids`, each `>= 0`, from `pragma_storage_info('<t>')`) times `block_size` from `pragma_database_size()`. It is coarse (tables sharing a block both count it) but stable and DuckDB's own; `duckdb_tables().estimated_size` is a row estimate, not bytes. `row_count` is `count(*)`. A table that cannot be measured (not a simple identifier, a view, gone) records no built-in measures and no error.
5. **A check or measure whose SQL fails to run** (a missing table, a type error) fails its step whatever its severity: a broken warn check is a broken Protocol, not a warning.
6. **A measure column whose value is SQL `NULL`** is recorded as JSON `null` and appears in `measures` with a `NULL` value; it is not refused.
7. **The ordering rule's "through the graph"** means: an `error` check that comes after a publishing step is refused when any table or file the check reads is one the publishing step reads or an upstream ancestor of one (a check on `soc` guards a publish of `build/soc.parquet` that a later `COPY soc TO …` wrote).
8. **`measures` and `measure_history` are reserved table names**: arc refuses a manifest in which a step produces either.
9. **Step names may not contain `:`**, which the spec relies on for the `table:` prefix and which `validate` does not enforce today.
10. **A failed `error` check exits as a failed step does today** (exit code 2, via `Error::exit_code`). The spec's 0/1 exit codes belong to the Telling-the-analyst issue, which should settle both together.
11. `check:` and `measure:` steps refuse `produces:`, `output:` and `preconditions:` (they produce nothing and are never skipped); `depends_on:`, `retry:`, `timeout_sec:` and `description:` are allowed. `risk:` and `severity:` are refused on any step that is not a check.

## Review Focus

1. **A check file with a leading comment, no trailing `;`, or a trailing `-- comment`** must still run: the wrapper embeds the statement text with newlines around it (Task 4 test `a_check_with_comments_and_no_semicolon_runs`).
2. **A measure over an empty table** (`avg(x)` is `NULL`) records `null` and the run goes on (Task 5 test `a_null_measure_is_recorded_as_null`).
3. **A runs folder holding a truncated record, a `b4/1` record, a failed run and the current run's own files**: history skips each and the run does not fail (Task 6 test `history_skips_unreadable_old_failed_and_current_records`).
4. **A check returning wide or long values** (a 10 kB string, a blob, a list): the sample stays capped at 20 rows of 200-character text cells (Task 4 test `a_sample_is_capped_and_cells_truncated`).
5. **A second run where the producing step is fresh**: its tables are still measured, so a comparison check sees this run's `row_count` (Task 5 test `a_fresh_step_still_has_its_tables_measured`).

---

### Task 1: Manifest: `check:` and `measure:` steps, their fields, and the top-level settings

**Files:**
- Modify: `src/manifest.rs` (`Step`, `Step::new`, `Manifest`, `validate`)
- Modify: every `Step { … }` struct literal the compiler names (`src/bridge.rs`, `src/record.rs`, tests)
- Test: unit tests in `src/manifest.rs`'s test module

**Interfaces:**
- Produces:
  - `Step.check: Option<String>`, `Step.measure: Option<String>` (paths relative to the manifest), `Step.risk: Option<String>`, `Step.severity: Option<Severity>`; all `#[serde(default, skip_serializing_if = "Option::is_none")]`.
  - `pub enum Severity { Error, Warn }`, `#[serde(rename_all = "lowercase")]`, with `Severity::as_str(self) -> &'static str`; `Step::severity_or_default(&self) -> Severity` (Error when unset).
  - `Step::sql_file(&self) -> Option<&str>`: the `sql`, `check` or `measure` path, whichever is set. Every reader of "the step's SQL file" switches to it in Task 2.
  - `Step::is_check(&self) -> bool`, `Step::is_measure(&self) -> bool`.
  - `Manifest.measure_history: MeasureHistory` (`#[serde(default)]`), `pub enum MeasureHistory { Runs(u32), Since(std::time::Duration) }` deserialised from `{ runs: N }` or `{ since: "<humantime>" }`, default `Runs(30)`.
  - `Manifest.check_samples: bool`, `#[serde(default = "default_true")]`.

- [ ] **Step 1: Write the failing tests** in `src/manifest.rs`'s tests:
  - `a_check_step_loads_with_risk_and_default_severity`: YAML with `check: checks/a.sql` and `risk: x` loads; `severity_or_default() == Severity::Error`; `sql_file() == Some("checks/a.sql")`.
  - `a_check_without_risk_is_refused`: error message contains `step 'a': a check needs risk:`.
  - `an_empty_or_multiline_risk_is_refused`: `risk: ""` and `risk: "a\nb"` refused.
  - `severity_warn_loads_and_other_values_are_refused`: `severity: warn` loads; `severity: info` is a parse error.
  - `risk_or_severity_on_a_non_check_is_refused`: on a `sql:` step and on a `measure:` step, message contains `only a check step takes risk:` / `severity:`.
  - `check_and_measure_count_as_step_arms`: `sql:` plus `check:` is refused with `must have exactly one of 'sql', 'command', 'op', 'check' or 'measure'`; none of the five gives `must have one of …`.
  - `check_and_measure_refuse_produces_output_and_preconditions`: each refused, message names the field.
  - `a_step_name_with_a_colon_is_refused`: `name: table:x` refused, message contains `cannot contain ':'`.
  - `measure_history_defaults_to_30_runs_and_reads_runs_or_since`: absent → `Runs(30)`; `{ runs: 5 }` → `Runs(5)`; `{ since: 90d }` → `Since(90 days)`; `{ runs: 0 }`, `{ since: soon }` and `{ runs: 5, since: 90d }` refused.
  - `check_samples_defaults_to_true`.
  - `a_hook_cannot_be_a_check`: `hooks.on_exit` with `check:` and no `sql`/`command` is refused (existing rule; pin it).

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib manifest::`
Expected: compile errors for the missing fields and types.

- [ ] **Step 3: Implement** the fields, types and checks in `src/manifest.rs`. Extend the existing arm count in `validate` to five arms and update both its messages; add the check/measure field refusals and the colon refusal beside the empty-name check. `MeasureHistory` deserialises through a private `#[serde(deny_unknown_fields)] struct { runs: Option<u32>, since: Option<String> }` and `humantime::parse_duration`. Update the `from_yaml_str` doc comment's list of checks. Fix each `Step { … }` literal the compiler names with the four new fields as `None`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib manifest:: && cargo test --test public_surface`
Expected: PASS (update `public_surface` expectations only where it pins `Step`/`Manifest` fields).

- [ ] **Step 5: Commit**

```bash
git add src/manifest.rs src/bridge.rs src/record.rs tests/public_surface.rs
git commit -m "Add check and measure step kinds to the manifest"
```

---

### Task 2: Read check and measure files as Protocol SQL; refuse a file that is not one query; never skip them

**Files:**
- Modify: `src/asset.rs:155` (`AssetGraph::build`), `src/engine.rs:1783-1810` (`ProtocolSql::new`, `protocol_sql`), `src/contract.rs` (`build_step`'s `sql` section and `kind`), `src/runner.rs` (`sql_hash` at ~line 628, `compute_staleness` ~line 1516)
- Modify: `src/contract.rs` `StepKind` (add `Check`, `Measure`)
- Create: `src/check.rs` (starts here with the refusals; Tasks 3–6 add to it); add `mod check;` in `src/lib.rs` and `"check.rs"` to `MODULE_FILES` in `tests/public_surface.rs`
- Test: `tests/checks.rs` (new integration test file, real `arc` binary, `mod common;` as `tests/run_outputs.rs` does)

**Interfaces:**
- Consumes: `Step::sql_file`, `Step::is_check`, `Step::is_measure` (Task 1).
- Produces:
  - `pub(crate) fn refuse_check_shapes(manifest: &Manifest, dir: &Path, graph: &AssetGraph) -> Result<()>` in `src/check.rs`: for each check/measure step, its file must read (through `introspect::read_sql`, i.e. the DuckDB reader arc already uses) as exactly one statement whose DuckDB form is `Form::Query`; and no step may produce `measures` or `measure_history` (ruling 8). Refusals are `Error::ManifestValidation` naming the step and file, e.g. `step 'parents_exist': checks/parents_exist.sql holds 2 statements; a check file holds exactly one query`. `read_sql`'s `StepSql` carries no statement form today: add `is_query: Vec<bool>` beside `read_by`, true for a DuckDB-read statement of `Form::Query` and for a sqlparser-read `Statement::Query`; keep it `pub(crate)`.
  - `pub(crate) fn check_query(dir: &Path, step: &Step) -> Result<String>`: the one statement's text, sliced from the file by its `byte_ranges` entry with any trailing `;` dropped; refused as above when the range is `None`. Task 4 embeds this text in its wrapper.
  - `contract::StepKind::{Check, Measure}` serialised `check` / `measure`.

- [ ] **Step 1: Write the failing tests** in `tests/checks.rs` (each builds a Protocol in a temp dir as `tests/run_outputs.rs::project` does, and runs `common::arc_run_raw`):
  - `a_check_file_with_two_statements_is_refused_before_any_step_runs`: exit non-zero, stderr names the step and `exactly one query`, and no run record exists.
  - `a_check_file_holding_a_create_is_refused`: `CREATE TABLE x AS SELECT 1` refused the same way.
  - `a_step_producing_measures_is_refused`: a `sql:` step `CREATE TABLE measures AS SELECT 1` refused, message names `measures` as reserved.
  - `a_check_reads_its_tables_in_the_asset_graph`: a passing run with `sql: models/t.sql` then `check: checks/c.sql` reading `t`; the run record's step entry for the check has `kind: "check"`, a `sql` section with `model_path: "checks/c.sql"` and one statement reading `t`, and the `t` asset's `consumed_by` lists the check.
  - `a_check_runs_again_on_an_unchanged_second_run`: run twice; the second run's record shows the `sql:` step `skipped` and the check step not skipped.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --test checks`
Expected: FAIL (the manifest loads from Task 1, but the runner treats the step as having no SQL).

- [ ] **Step 3: Implement.** Switch each "the step's SQL file" reader listed above from `step.sql` to `step.sql_file()`. In `compute_staleness`, a check or measure step is stale unconditionally, as a `command:` step without preconditions is (same early `continue`). Call `check::refuse_check_shapes` in `run_with_publish` right after the asset graph is built and before run state is set up, so a refused Protocol leaves no run behind (the same place the existing SQL-reader refusals are raised). For now the runner executes a check or measure step's file with `engine.execute_sql` as a plain SQL step; Task 4 replaces that.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test checks && cargo test --lib asset:: contract:: runner::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/check.rs src/lib.rs src/asset.rs src/engine.rs src/contract.rs src/runner.rs tests/checks.rs tests/public_surface.rs
git commit -m "Read check and measure files as one-query Protocol SQL, and never skip them"
```

---

### Task 3: The ordering rule: no publish before an error check on what it publishes

**Files:**
- Modify: `src/operator.rs` (`Operator` trait: add `fn publishes(&self) -> bool { false }`), `src/operator/ducklake_write.rs` and `src/operator/ducklake_publish.rs` (return `true`)
- Modify: `src/asset.rs` (add `upstream_assets`)
- Modify: `src/check.rs`, `src/runner.rs` (call site beside `refuse_check_shapes`)
- Test: unit tests in `src/check.rs`; one case in `tests/checks.rs`

**Interfaces:**
- Consumes: `refuse_check_shapes`'s call site (Task 2), `Step::severity_or_default` (Task 1).
- Produces:
  - `Operator::publishes(&self) -> bool`.
  - `AssetGraph::upstream_assets(&self, assets: &BTreeSet<String>) -> BTreeSet<String>`: the given assets plus every asset any producer of one of them reads, to a fixed point (mirror `downstream_steps`).
  - `pub(crate) fn refuse_publish_before_error_check(manifest: &Manifest, graph: &AssetGraph) -> Result<()>` in `src/check.rs`, implementing ruling 7. Message: `step '<publish>' publishes before the error check '<check>' that reads <asset>, so a failing check would come too late: move the check before it, or make it severity: warn`.

- [ ] **Step 1: Write the failing tests** (unit tests in `src/check.rs` build a `Manifest` from YAML and an `AssetGraph` by hand, as `src/asset.rs`'s tests do):
  - `an_error_check_after_a_publish_of_its_table_is_refused`: steps `load` (produces `soc`), `export` (reads `soc`, produces `build/soc.parquet`), `publish` (`op: ducklake_write` reading `build/soc.parquet`), `check` (error, reads `soc`) → refused, message names `publish`, `check` and `soc`.
  - `a_warn_check_after_the_publish_is_allowed`.
  - `an_error_check_before_the_publish_is_allowed`.
  - `an_error_check_on_an_unrelated_table_after_the_publish_is_allowed`.
  - `tests/checks.rs::the_ordering_rule_refuses_before_any_step_runs`: the first case as a real Protocol with a local `ducklake_write` (copy its `with:` from `tests/ducklake_write.rs`), exit non-zero and no run record.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib check:: && cargo test --test checks the_ordering_rule`
Expected: FAIL (`refuse_publish_before_error_check` not defined).

- [ ] **Step 3: Implement** the trait method, the two overrides, `upstream_assets` and the refusal, and call it after `refuse_check_shapes`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib check:: asset:: operator:: && cargo test --test checks`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/operator.rs src/operator/ducklake_write.rs src/operator/ducklake_publish.rs src/asset.rs src/check.rs src/runner.rs tests/checks.rs
git commit -m "Refuse a Protocol that publishes before an error check on what it publishes"
```

---

### Task 4: Run a check, record its section, and stop the run on a failed error check

**Files:**
- Modify: `src/check.rs` (wrapper, answer, `CheckSection`), `src/engine.rs` (`AnswerDir` becomes `pub(crate)` with `pub(crate) fn path(&self) -> &Path`), `src/error.rs` (`CheckFailed`), `src/runner.rs` (check arm and outcomes), `src/contract.rs` (`StepOutcome`, `StepEntry`, `RunInfo`, `CONTRACT_VERSION`)
- Test: `tests/checks.rs`; unit tests in `src/check.rs` and `src/contract.rs`

**Interfaces:**
- Consumes: Task 2's reading (the one statement's text, without its `;`).
- Produces:
  - In `src/contract.rs`: `pub struct CheckSection { name: String, risk: String, severity: String, success: bool, rows_returned: u64, sample: Vec<Vec<Option<String>>>, sample_columns: Vec<String> }` (all `pub`, `Serialize, Deserialize, Clone`); `StepEntry.check: Option<CheckSection>` and `StepOutcome.check: Option<CheckSection>`, both `#[serde(default, skip_serializing_if = "Option::is_none")]` on the entry; `pub struct ChecksSummary { passed: u32, warned: u32, failed: u32 }` and `RunInfo.checks: ChecksSummary` (`#[serde(default)]`), counted in `build_contract` from the step outcomes (`success` → passed; not success and warn → warned; not success and error → failed); `CONTRACT_VERSION = "b5/1"`.
  - In `src/check.rs`: `pub(crate) fn run_check(step: &Step, query: &str, ctx: &QueryContext) -> Result<CheckSection>` where `pub(crate) struct QueryContext<'a> { engine: &'a dyn Engine, db_path: &'a Path, env: &'a HashMap<String, String>, timeout: Option<Duration>, loaded: &'a mut LoadedExtensions, measures: &'a [(String, Option<f64>)], history: &'a [HistoryRow] }`. (`HistoryRow` is defined in Task 6; until then pass `&[]` and define it here as `pub(crate) struct HistoryRow { run_id: String, run_at: String, name: String, value: Option<f64> }`.)
  - `Error::CheckFailed { step: String, check: Box<CheckSection> }`, displayed `check '<step>' failed: <risk> (<n> rows returned)`, in `exit_code`'s first arm with `StepFailed` (ruling 10), not retried.

The wrapper is the one piece the signature does not determine. `run_check` writes `measures.json` and `history.json` (arrays of objects) and `wrapper.sql` into a fresh `AnswerDir`, runs `wrapper.sql` with `ctx.engine.execute_sql`, then reads `answer.json`:

```sql
CREATE TEMP TABLE measures AS
  SELECT * FROM read_json('<dir>/measures.json', columns = {name: 'VARCHAR', value: 'DOUBLE'});
CREATE TEMP TABLE measure_history AS
  SELECT * FROM read_json('<dir>/history.json',
    columns = {run_id: 'VARCHAR', run_at: 'TIMESTAMP', name: 'VARCHAR', value: 'DOUBLE'});
CREATE TEMP TABLE __arc_rows AS
<the statement text>
;
COPY (SELECT
  (SELECT count(*) FROM __arc_rows) AS rows_returned,
  (SELECT list(column_name) FROM (DESCRIBE __arc_rows)) AS columns,
  (SELECT list(r) FROM (SELECT [COLUMNS(*)::VARCHAR] AS r FROM __arc_rows LIMIT 20)) AS sample
) TO '<dir>/answer.json' (FORMAT json);
```

Paths are written as SQL string literals with `'` doubled. If `[COLUMNS(*)::VARCHAR]` does not expand on DuckDB 2.0, use `list_value(COLUMNS(*)::VARCHAR)`; either way, verify against the release CLI. Truncate cells to 200 characters in Rust after reading (ruling 2). An empty `measures.json` is `[]`, and `read_json` with explicit `columns` must give zero rows; verify.

- [ ] **Step 1: Write the failing tests**
  - `tests/checks.rs::a_passing_check_records_success_and_no_rows`: record's check entry has `check.success == true`, `rows_returned == 0`, `sample == []`, `severity == "error"`, `risk` as written; `run.checks == {passed: 1, warned: 0, failed: 0}`; `contract_version == "b5/1"`.
  - `a_failed_warn_check_leaves_the_run_green`: exit 0, step `status.state == "success"`, `check.success == false`, `rows_returned == 3`, `sample_columns == ["code", "parent"]`, three sample rows; `run.outcome == "success"`, `run.checks.warned == 1`; stderr has a `warning:` line naming the check, its risk and `3 rows`.
  - `a_failed_error_check_stops_the_run_before_the_next_step`: a `command:` step after the check never runs (its marker file is absent), exit 2, stderr contains `check 'c' failed:` and the risk, record step state `failed` with its `check` section filled, `run.checks.failed == 1`, `run.outcome` is `partial`.
  - `a_check_whose_sql_fails_fails_the_step_even_as_warn` (ruling 5).
  - `a_check_with_comments_and_no_semicolon_runs` (Review Focus 1): file `-- leading\nSELECT 1 WHERE false -- trailing` passes.
  - `a_sample_is_capped_and_cells_truncated` (Review Focus 4): a check returning 50 rows of `repeat('x', 10000)` gives `rows_returned == 50`, 20 sample rows, each cell 201 characters ending `…`.
  - `a_check_can_read_its_parameters`: `getenv('ARC_PARAM_LIMIT')` inside a check works as in a `sql:` step.
  - `src/contract.rs::a_b4_record_reads_with_no_checks`: an existing `b4/1` fixture deserialises with `run.checks` zeroed and no `check` sections.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --test checks && cargo test --lib contract::`
Expected: FAIL.

- [ ] **Step 3: Implement.** In the runner's per-attempt `result` (`src/runner.rs` ~line 688), a check step runs `check::run_check` after the same `check_again` refusal pass a SQL step gets; on success it returns a `StepOutput` and the arm stores the `CheckSection` in the step's `StepOutcome`; when `success` is false and severity is error it returns `Err(Error::CheckFailed { .. })`, which the existing non-retryable `Err(e)` arm handles; that arm must now also keep the section (take it out of `CheckFailed` when inserting the outcome). A warned check prints one `warning:` line on stderr: `warning: check '<name>' found <n> rows: <risk>`. `build_step` copies the outcome's section into the entry. Bump `CONTRACT_VERSION` and every `b4/1` literal in tests that means "the current version" (leave fixtures that mean "an old record" as `b4/1`); update `src/mcp/hero.rs`'s doc comment.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test checks && cargo test --lib contract:: runner:: check::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/check.rs src/engine.rs src/error.rs src/runner.rs src/contract.rs src/mcp/hero.rs tests/checks.rs
git commit -m "Run check steps, record each check's outcome and sample, and stop on a failed error check"
```

---

### Task 5: Measures: measure steps, built-in table measures, and the `measures` table

**Files:**
- Modify: `src/check.rs` (`run_measure`, `measure_tables`), `src/runner.rs` (measure arm; measuring after each step reached; the run's measure list), `src/contract.rs` (`MeasureSection`)
- Test: `tests/checks.rs`; unit tests in `src/check.rs`

**Interfaces:**
- Consumes: `QueryContext`, the wrapper and `AnswerDir` (Task 4).
- Produces:
  - `pub struct MeasureSection { values: IndexMap<String, Option<f64>> }` in `src/contract.rs`; `StepEntry.measure` and `StepOutcome.measure`, `Option<MeasureSection>`, serialised as Task 4's `check` is. Key order: a measure step's columns in query order, then built-ins in produced-table order (sorted by name).
  - `pub(crate) fn run_measure(step: &Step, query: &str, ctx: &QueryContext) -> Result<IndexMap<String, Option<f64>>>`: the same wrapper as `run_check` with a different final `COPY`, giving the row count, each column's name and type from `DESCRIBE`, and the first row's values cast to `DOUBLE`. Refused (step fails, `Error::StepExecution`-style message naming the step) unless exactly one row and every column type is numeric (`TINYINT` … `HUGEINT`, unsigned forms, `FLOAT`, `DOUBLE`, `DECIMAL(…)`). Names are `<step>.<column>`.
  - `pub(crate) fn measure_tables(db_path: &Path, tables: &[String]) -> IndexMap<String, Option<f64>>`: opens the database with the linked `duckdb` crate read-only, and for each simple-identifier table gives `table:<t>.row_count` (`count(*)`) and `table:<t>.bytes` (ruling 4); a table it cannot measure is left out.
  - The runner keeps `run_measures: Vec<(String, Option<f64>)>` in run order and passes it as `QueryContext.measures`.

- [ ] **Step 1: Write the failing tests**
  - `tests/checks.rs::a_measure_step_records_its_values`: `SELECT count(*) AS n, 0.5 AS share FROM t` → entry `measure.values == {"m.n": 3, "m.share": 0.5}`.
  - `a_measure_returning_two_rows_or_text_fails_its_step`: each fails, message names the step and the rule.
  - `a_null_measure_is_recorded_as_null` (Review Focus 2).
  - `each_produced_table_is_measured_at_its_step`: the `sql:` step's entry has `measure.values["table:t.row_count"] == 3` and a positive `table:t.bytes`.
  - `a_check_reads_this_runs_measures`: a check `SELECT * FROM measures WHERE name = 'table:t.row_count' AND value <> 3` passes, and with `<> 4` fails (warn).
  - `a_check_before_a_measure_does_not_see_it`: `measures` holds only what ran earlier in the run.
  - `a_fresh_step_still_has_its_tables_measured` (Review Focus 5): second run, `sql:` step skipped, its entry still has `table:t.row_count`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --test checks`
Expected: FAIL.

- [ ] **Step 3: Implement** `run_measure`, `measure_tables` and the runner wiring: after a step succeeds or is skipped as fresh, measure the tables the asset graph says it produces (kind `table`) and store the section on its outcome; after a measure step succeeds, append its values too. Store a measure section only when it has values.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test checks && cargo test --lib check:: contract::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/check.rs src/runner.rs src/contract.rs tests/checks.rs
git commit -m "Record measure steps and each produced table's row count and bytes, and give checks this run's measures"
```

---

### Task 6: `measure_history` from earlier successful runs

**Files:**
- Modify: `src/check.rs` (`load_history`), `src/runner.rs` (load once before the first step; pass to every check)
- Test: unit tests in `src/check.rs` (records written by hand into a temp runs folder); `tests/checks.rs`

**Interfaces:**
- Consumes: `HistoryRow` (Task 4), `MeasureHistory` (Task 1), `contract::contracts_by_age`.
- Produces: `pub(crate) fn load_history(runs_dir: &Path, current_run_id: &str, window: &MeasureHistory, now: std::time::SystemTime) -> Vec<HistoryRow>`: every record in `runs_dir` that deserialises as a `Contract`, is not `current_run_id`, has `run.outcome == "success"` and has a `started_at`; each step's `measure` section becomes rows with `run_at = run.started_at`. `Runs(n)` keeps the `n` newest runs by `started_at`; `Since(d)` keeps runs started at or after `now - d`. Rows are returned oldest run first (newest last), steps in record order.

- [ ] **Step 1: Write the failing tests**
  - `history_keeps_the_newest_n_successful_runs_oldest_first`.
  - `history_since_keeps_runs_inside_the_window`.
  - `history_skips_unreadable_old_failed_and_current_records` (Review Focus 3): a truncated `.json`, a `b4/1` record with no measure sections, a `partial` run and the current run id each contribute nothing, and nothing errors.
  - `tests/checks.rs::a_comparison_check_sees_the_previous_run`: run 1 loads 4 rows; run 2 loads 1 row (the `sql:` file changes between runs); a warn check `SELECT name FROM measures m JOIN (SELECT name, last(value ORDER BY run_at) AS prev FROM measure_history GROUP BY name) p USING (name) WHERE m.value < p.prev * 0.5` passes on run 1 (empty history) and warns on run 2 with `table:t.row_count` in its sample.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib check:: && cargo test --test checks a_comparison`
Expected: FAIL.

- [ ] **Step 3: Implement** `load_history` and call it once in `run_with_publish` after the run id is chosen and before the first step.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib check:: && cargo test --test checks`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/check.rs src/runner.rs tests/checks.rs
git commit -m "Give checks earlier successful runs' measures as measure_history"
```

---

### Task 7: The published record: samples redacted, or left out with `check_samples: false`

**Files:**
- Modify: `src/runner.rs` (`public_record` and `upload_run_records` take `check_samples: bool` from the manifest)
- Test: unit tests beside `public_record`'s existing tests in `src/runner.rs`

**Interfaces:**
- Consumes: `Manifest.check_samples` (Task 1), `CheckSection` (Task 4).
- Produces: `public_record(bytes: &[u8], dir: &Path, secrets: &[String], check_samples: bool)`; with `false`, every step's `check.sample` becomes `[]` and `check.sample_columns` stays.

- [ ] **Step 1: Write the failing tests**
  - `check_samples_false_empties_every_sample_in_the_published_copy`.
  - `a_sample_holding_a_secret_is_refused_as_any_other_string_is`: a sample cell containing a dotenv secret makes `public_record` refuse, as the existing secret rule does for other strings.
  - `check_samples_true_keeps_samples`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib --features http-fetch runner::public_record`
Expected: FAIL (wrong arity).

- [ ] **Step 3: Implement** the parameter and the emptying pass, after `scrub` and before the secret check.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib --features http-fetch runner::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/runner.rs
git commit -m "Keep check samples out of the published run record when check_samples is false"
```

---

### Task 8: The issue's end-to-end check, docs and changelog

**Files:**
- Create: `tests/protocols_checks/` fixture Protocol (or build it in the test as the other tests do)
- Modify: `tests/checks.rs`, `docs/ARCHITECTURE.md` (a "Checks and measures" section after "Declared outputs", and the Key types / Execution model lists), `CHANGELOG.md` (`[Unreleased]`), `skills/building-and-checking-a-protocol/SKILL.md` (one short passage: write a rule as a `check:` step with a `risk:` line, not as `sql:` calling `error()`)
- Test: `tests/checks.rs::checks_and_measures_end_to_end`; `cargo test --features mcp --test skills`

**Interfaces:**
- Consumes: everything above.

- [ ] **Step 1: Write the end-to-end test** `checks_and_measures_end_to_end`, the issue's acceptance: a Protocol with a load step, a measure step, a warn check, a comparison check over `measure_history`, an error check and a local `ducklake_write` after the error check. Run 1 passes. Change the data so the error check fails, run 2: the record shows each check's outcome, sample rows and the measures; the warn check is counted in `run.checks.warned` and the run's `warning:` line is on stderr; the error check stops the run, the publish step's state is `skipped` (never reached) and the local lake holds only run 1's snapshot.

- [ ] **Step 2: Run it**

Run: `cargo test --test checks checks_and_measures_end_to_end`
Expected: PASS (it exercises only what Tasks 1–7 built; a failure here is a bug in one of them).

- [ ] **Step 3: Write the docs.** `docs/ARCHITECTURE.md`'s new section states the step kinds, the wrapper and answer directory, the ordering rule, built-in measures, `measure_history`, the record's `b5/1` sections and `check_samples`, and lists this plan's rulings with their reasons in its own words. The changelog entry names the new step kinds and the contract version change.

- [ ] **Step 4: Run the doc-bound tests**

Run: `cargo test --features mcp --test skills && cargo test --test public_surface`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add tests/checks.rs docs/ARCHITECTURE.md CHANGELOG.md skills/building-and-checking-a-protocol/SKILL.md
git commit -m "Document check and measure steps, and test them end to end"
```

---

## Verification commands and their cost

| Command | Where | Cost on Hugh's Mac (incremental) |
|---|---|---|
| `cargo test --lib <module>::` | Tasks 1–7 | about 1–2 min to rebuild the lib test binary, seconds to run |
| `cargo test --test checks` | Tasks 2–8 | about 1–3 min (builds the `arc` binary), then real DuckDB runs, under a minute |
| `cargo test --test public_surface` | Tasks 1, 2, 8 | under a minute after a lib build |
| `cargo test --lib --features http-fetch runner::` | Task 7 | a feature rebuild, 2–4 min |
| `cargo test --features mcp --test skills` | Task 8 | an `mcp` feature rebuild, 3–5 min |
| `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` | before each commit | seconds; clippy about as long as a build |
| Full gate | CI only | not run locally unless reproducing a CI failure |
