# Analyst workflow, plan 1 of 3: arc capabilities

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give arc every verb and MCP tool the five analyst skills tell an agent to use, so an agent with only `arc mcp` and no file access of its own can run the whole workflow.

**Architecture:** Each capability is a CLI verb in `src/cli.rs` backed by a library function, plus an MCP tool in `src/mcp/hero.rs` that calls the same function, so the same request writes the same bytes from either (the pattern `sql_record` and `table_describe` already follow). Every write goes through `LocalHistory`, so it is a version `arc history` lists and restores.

**Tech Stack:** Rust 2024, clap, serde_json, serde_yaml (already a dependency), DuckDB through the existing `inspect` module, the `finetype` CLI through the existing federation in `src/mcp/finetype.rs`.

**Spec:** `docs/superpowers/specs/2026-10-16-analyst-workflow-skills-design.md`, section "Capability work arc needs before the line", items 1 to 5. Item 6, the install command, is in plan 3.

**Order:** This plan runs first. Plan 2 (the skills) drafts against the names in this plan's Interfaces blocks; plan 3 (packaging and evaluation) needs plan 2's skills.

## Global Constraints

- The CLI is the canonical surface. Every MCP tool maps to a CLI capability; a new MCP tool needs its CLI verb.
- arc stays local-first: no cloud service in the path.
- Every file a verb writes is inside the Protocol folder and is recorded in history like any other machine edit (`LocalHistory::record_save_for_file`, or the spec's own history for `arcform.yaml`).
- The finetype federation keeps its version floor, `MIN_VERSION` in `src/mcp/finetype.rs`.
- Public repository: no private planning identifiers in code, comments, tests or commit messages.
- British English in user-facing text, matching the existing tool descriptions.
- A refusal leaves the Protocol folder untouched, and a test pins that, as the existing record tests do.
- `tests/public_surface.rs` freezes the crate's module list (`MODULE_FILES`) and public surface. A task that creates a module adds it to `MODULE_FILES`; new functions stay `pub(crate)`, and integration tests in `tests/` drive the `arc` binary (`env!("CARGO_BIN_EXE_arc")`) rather than calling library functions.
- A task that registers an MCP tool adds it to `tools_list_registers_every_tool` (names and count) and names it in `INSTRUCTIONS` in the same commit, so every commit's tests pass.
- Builds need `DUCKDB_LIB_DIR=/opt/homebrew/lib` (or wherever DuckDB 1.5.6 is) on this machine. Run one cargo build at a time.

## Review Focus

1. **A path that escapes the Protocol folder** (`../x`, an absolute path, a symlink inside `panels/` pointing out) given to `protocol_write_file` must be refused with nothing written. Test in Task 3.
2. **A panel that reads inline data** (a top-level `data:` key, as Brightfield's own examples do) must not be reported as naming a missing table. Test in Task 5.
3. **An operator step whose `with:` fails the operator's schema** must be refused naming the failing field, not written and left for `arc run` to reject. Test in Task 1.
4. **Profiling a table with zero rows** must return a result saying so, not an error from finetype about an empty file. Test in Task 4.
5. **A report written before any run** (no contract under `build/.arcform/runs/`) must be refused with the message telling the agent to run the Protocol first. Test in Task 6.

## Verification commands and what each costs

- Focused: `DUCKDB_LIB_DIR=/opt/homebrew/lib cargo test --features mcp <filter>`. About a minute after the first build; the first build of a fresh worktree takes several minutes.
- Before each commit: `cargo fmt --check` and `cargo clippy --features mcp --all-targets -- -D warnings` (a few minutes), plus the focused tests the task touches.
- The full suite runs in CI; run it locally only to reproduce a CI failure.

---

### Task 1: Record an operator step

**Files:**
- Modify: `src/record.rs` (add `OperatorRequest` and `record_operator`)
- Modify: `src/cli.rs` (add `arc operator record` under a new `OperatorCmd`, beside the existing `operation` verbs)
- Modify: `src/mcp/hero.rs` (add the `operator_record` tool)
- Test: `tests/operator_record.rs`; MCP cases in `src/mcp/hero.rs`'s test module

**Interfaces:**
- Consumes: `crate::operator::with_schema(op_name) -> Option<Value>`, `crate::operator::catalog_names()`, `crate::history::record_step_with_history` (or the splice path `record_step` uses), the `ValidatedSpec` load gate.
- Produces:
  - `pub struct OperatorRequest<'a> { pub operator: &'a str, pub name: &'a str, pub with: &'a serde_json::Value, pub description: Option<&'a str> }`
  - `pub fn record_operator(dir: &Path, request: &OperatorRequest, history: &LocalHistory) -> Result<ValidatedSpec>`: splices a step `{name, op, with, description}` onto the end of `steps`, after checking `with` against the operator's schema.
  - CLI: `arc operator record <operator> --name <NAME> --with <JSON> [--description <TEXT>] [--dir <DIR>]`
  - MCP tool `operator_record`, arguments `operator` (required), `name` (required), `with` (object, required), `description`, `dir`; returns `{ "step": name, "operator": operator }`. Annotation `WRITES`. Description ends "Writes the step; runs nothing."

- [ ] **Step 1: Write the failing tests** in `tests/operator_record.rs`:
  - `records_http_fetch_step_at_end`: in a Protocol from `create-protocol`, record `http_fetch` named `fetch_rates` with a `with` the schema accepts; assert `arcform.yaml` now ends with a step whose `name` is `fetch_rates`, `op` is `http_fetch`, and `with` round-trips equal to the input, and that `arc history list` has one more entry.
  - `refuses_with_that_fails_schema`: a `with` missing a required field is refused; the error names that field; `arcform.yaml` is byte-identical to before.
  - `refuses_unknown_operator`: the error says to call `operator_describe` with no arguments to list the catalogue, matching the existing wording in `hero.rs`.
  - `refuses_duplicate_step_name`: directory untouched.
- [ ] **Step 2: Run** `cargo test --features mcp --test operator_record`. Expected: fails to compile (`record_operator` not defined).
- [ ] **Step 3: Implement `record_operator`.** Check `with` against `with_schema(operator)` with a small validator over the subset of JSON Schema the operator schemas use (required, type, enum, properties, additionalProperties); confirm that subset by reading `with_schema`'s entries before writing it, and if a JSON Schema crate is already in `Cargo.lock`, use that instead. Splice through the same refusal-first path `record_step` uses; the step is YAML, so emit `with` as a block mapping.
- [ ] **Step 4: Add the CLI verb and the MCP tool**, both calling `record_operator`. Add MCP tests in `hero.rs`: the tool and the CLI verb given the same request write byte-identical `arcform.yaml` files (the pattern the `sql_record` tests use).
- [ ] **Step 5: Run** `cargo test --features mcp operator_record`. Expected: all pass.
- [ ] **Step 6: Commit** "Record an operator step from the CLI or MCP".

### Task 2: Edit a step and use history over MCP

**Files:**
- Modify: `src/mcp/hero.rs` (tools `protocol_edit`, `history_list`, `history_restore`)
- Test: `src/mcp/hero.rs` test module

**Interfaces:**
- Consumes: `crate::cli::edit_protocol(dir, op: EditOp, history)`, `LocalHistory::{entries, restore, entries_for_file, restore_for_file}`.
- Produces:
  - `protocol_edit`: arguments `dir`, `edit` (one of `replace`, `add`, `append`, `delete`, `reorder`, `rewrite`) and that edit's fields named as the `arc edit-protocol <edit> --help` arguments are; returns the edited step list. Annotation `WRITES`.
  - `history_list`: arguments `dir`, `file` (optional, relative to `dir`); returns the entries `arc history list --json` prints (add `--json` to that verb if it has none). Annotation `READ_ONLY`.
  - `history_restore`: arguments `dir`, `id` (required), `file`; returns `{ "restored": id, "file": path }`. Annotation `WRITES`.

- [ ] **Step 1: Write failing tests:** `protocol_edit_matches_cli` (same edit through the tool and `arc edit-protocol` gives identical bytes); `history_list_lists_after_record`; `history_restore_undoes_record` (record a SQL step, restore the entry before it, `arcform.yaml` equals the earlier text); `history_restore_for_panel_file` (with `file: "panels/x.yaml"`).
- [ ] **Step 2: Run** `cargo test --features mcp history_ protocol_edit`. Expected: fail, tools not registered.
- [ ] **Step 3: Implement the three tools** as thin handlers over the CLI's functions; add `--json` to `arc history list` if absent.
- [ ] **Step 4: Run** the same filter. Expected: pass. Update `tools_list_registers_every_tool` and the `initialize` instructions test to the new tool set.
- [ ] **Step 5: Commit** "Edit a step and use history over MCP".

### Task 3: Write the workflow's files

**Files:**
- Create: `src/protocol_files.rs`
- Modify: `src/lib.rs`, `src/cli.rs` (`arc protocol write-file`), `src/mcp/hero.rs` (`protocol_write_file`)
- Test: `tests/protocol_write_file.rs`

**Interfaces:**
- Produces:
  - `pub enum ProtocolFileKind { Schema, Panel, Readme, Report }` with `fn from_path(rel: &Path) -> Option<Self>`: `schemas/<name>.json`, `panels/<name>.yaml`, `README.md`, `report.md`. Any other path is refused.
  - `pub fn write_protocol_file(dir: &Path, rel: &Path, content: &str, history: &LocalHistory) -> Result<WrittenFile>`; `pub struct WrittenFile { pub path: PathBuf, pub kind: ProtocolFileKind, pub warnings: Vec<String> }`. Requires an `arcform.yaml` in `dir`. Validates content by kind: a schema parses as JSON, a panel parses as YAML with a `plot` key, the others are any UTF-8. Writes atomically, then records the file with `record_save_for_file`.
  - CLI: `arc protocol write-file <PATH> [--dir <DIR>]`, content on standard input.
  - MCP tool `protocol_write_file`: `dir`, `path`, `content` (all but `dir` required); returns `{ path, kind, warnings }`. Annotation `WRITES`.
  - Task 5 fills `warnings` for panels; Task 6 is the only writer of `report.md` through its own verb, so this tool refuses `report.md` and names `report_write`.

- [ ] **Step 1: Write failing tests:** `writes_schema_and_records_history`; `writes_panel_under_panels`; `refuses_path_outside_protocol` (`../x.json`, `/tmp/x.json`, and a `panels` symlink to a directory outside, each refused, nothing written); `refuses_unlisted_path` (`models/x.sql`, with a message naming the four allowed places); `refuses_invalid_schema_json`; `refuses_report_and_names_report_write`; `refuses_without_arcform_yaml`.
- [ ] **Step 2: Run** `cargo test --features mcp --test protocol_write_file`. Expected: fail to compile.
- [ ] **Step 3: Implement** `write_protocol_file`. Check the path kind, the `arcform.yaml` and the content first; then canonicalise the nearest existing ancestor of the target and refuse when it is not under the canonical `dir`, before creating any directory, so a refusal leaves no new empty `schemas/` or `panels/` behind.
- [ ] **Step 4: Add the CLI verb and MCP tool;** run the tests. Expected: pass.
- [ ] **Step 5: Commit** "Write a Protocol's schema, panel and README files from the CLI or MCP".

### Task 4: Profile a table

**Files:**
- Modify: `src/inspect.rs` (add `profile_table`), `src/cli.rs` (`arc table profile`), `src/mcp/hero.rs` (`table_profile`), `src/mcp/finetype.rs` (expose the version-checked runner to `inspect`)
- Test: `tests/table_profile.rs` (skips with a printed reason when `finetype` at the floor is not on PATH, as the existing finetype tests do)

**Interfaces:**
- Consumes: `inspect::open_read_only`, the finetype version check and command runner in `src/mcp/finetype.rs`.
- Produces:
  - `pub(crate) fn profile_table(dir: &Path, table: &str, format: ProfileFormat, sample_size: Option<usize>) -> Result<Value>` with `pub enum ProfileFormat { Json, JsonSchema }`. Copies up to `sample_size` rows (default the finetype default) of the table to a temporary CSV outside the Protocol, runs `finetype profile` on it, and returns finetype's JSON with `"table"` and `"rows_profiled"` added.
  - CLI: `arc table profile <TABLE> [--format json|json-schema] [--sample-size N] [--dir DIR]`.
  - MCP tool `table_profile`: `table` (required), `format`, `sample_size`, `dir`. Annotation `READS_OPEN`.

- [ ] **Step 1: Write failing tests:** `profiles_table_columns` (a Protocol whose one SQL step makes a table of dates and emails; the result names both columns with the types finetype gives the same CSV directly); `json_schema_format_saves_through_write_file` (the `json-schema` output written with Task 3's verb then passes `finetype validate`); `empty_table_says_so` (zero rows gives `{"rows_profiled": 0}` and a message, not an error); `refuses_missing_table` (lists the tables there are, as `describe_table` does).
- [ ] **Step 2: Run** `cargo test --features mcp --test table_profile`. Expected: fail.
- [ ] **Step 3: Implement** `profile_table` with DuckDB's `COPY (SELECT * FROM <quoted table> LIMIT n) TO '<tmp>' (HEADER)`, through the read-only connection.
- [ ] **Step 4: Run** the tests. Expected: pass.
- [ ] **Step 5: Commit** "Profile a table in a Protocol's database with finetype".

### Task 5: Check panels against the Protocol

**Files:**
- Create: `src/panel_check.rs`
- Modify: `src/lib.rs`, `src/cli.rs` (`arc panel check`), `src/mcp/hero.rs` (`panel_check`), `src/protocol_files.rs` (fill `warnings` on a panel write)
- Test: `tests/panel_check.rs`

**Interfaces:**
- Produces:
  - `pub fn check_panel(dir: &Path, panel: &Path) -> Result<Vec<PanelFinding>>`; `pub fn check_panels(dir: &Path) -> Result<Vec<(PathBuf, Vec<PanelFinding>)>>` over `panels/*.yaml`.
  - `pub enum PanelFinding { MissingTable { from: String }, MissingColumn { from: String, column: String } }`, each displayed as one plain sentence.
  - Rule: for each mark under `plot`, `data.from` must be a key of the panel's top-level `data:` or a table in the Protocol's database; when it is a database table, each channel whose value is a bare string (`x`, `y`, `fill`, `stroke`, `text`, `z`, and the rest of the channel names Mosaic uses) must be a column of it. Channel values that are objects (expressions, aggregates) are not checked.
  - With no database yet, `MissingTable` is checked against the relations the asset graph says the steps produce (SQL introspection in `crate::asset`), not the step names, since one step can make several tables (brewtrend's `rank` makes `details` and `ranking`); columns are not checked.
  - CLI: `arc panel check [--dir DIR]`, exit 1 when any finding. MCP tool `panel_check`, `dir`; annotation `READS_OPEN`.

- [ ] **Step 1: Write failing tests** using Brightfield's own example shape (`plot: [{mark: barY, data: {from: T}, x: name, y: count}]`): `inline_data_is_not_missing`; `missing_table_reported`; `missing_column_reported`; `expression_channel_not_checked`; `before_first_run_checks_produced_tables` (a step that makes two tables; a panel reading the second is not reported); `write_file_returns_findings_as_warnings` (writing a panel with a missing column succeeds and returns the warning).
- [ ] **Step 2: Run** `cargo test --features mcp --test panel_check`. Expected: fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the tests. Expected: pass.
- [ ] **Step 5: Commit** "Check a Protocol's panels name tables and columns it makes".

### Task 6: Write a report

**Files:**
- Create: `src/report.rs`
- Modify: `src/lib.rs`, `src/cli.rs` (`arc report write`), `src/mcp/hero.rs` (`report_write`)
- Test: `tests/report.rs`

**Interfaces:**
- Consumes: `crate::contract::runs_dir`, the contract's `run`, `steps[]` and `assets[]`; `check_panels` from Task 5; `write_protocol_file`'s history recording.
- Produces:
  - `pub fn write_report(dir: &Path, prose: &str, run_id: Option<&str>, history: &LocalHistory) -> Result<PathBuf>`: writes `report.md` as the agent's prose, then three sections arc generates: "How this was derived" (each step in order with its description, status and the row count of the table it made), "Charts" (each panel's path and title, with any panel-check finding), and "Run" (run id, finished time, outcome, and each asset's content hash). Uses the newest contract unless `run_id` names one.
  - CLI: `arc report write [--run RUN_ID] [--dir DIR]`, prose on standard input. MCP tool `report_write`: `prose` (required), `run`, `dir`; returns `{ path }`. Annotation `WRITES`.

- [ ] **Step 1: Write failing tests:** `report_has_prose_then_generated_sections` (prose first, then the three headings in order); `row_counts_come_from_contract`; `refuses_before_any_run` (message: run the Protocol first, with `arc run` or `protocol_run`); `named_run_used`; `rewrite_is_a_history_version` (two writes leave two entries for `report.md`).
- [ ] **Step 2: Run** `cargo test --features mcp --test report`. Expected: fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the tests. Expected: pass.
- [ ] **Step 5: Commit** "Write a report from an agent's findings and the run that produced them".

### Task 7: Document the new verbs

**Files:**
- Modify: `README.md` (the CLI and MCP sections), `docs/ARCHITECTURE.md` (source layout: `protocol_files.rs`, `panel_check.rs`, `report.rs`), `CHANGELOG.md` (`[Unreleased]`), `src/mcp/mod.rs` (`INSTRUCTIONS` names every new tool; its test enforces that)

- [ ] **Step 1: Run** `cargo test --features mcp initialize_instructions_name_every_native_tool`. Expected: fails until `INSTRUCTIONS` names the new tools.
- [ ] **Step 2: Update the four files.**
- [ ] **Step 3: Run** the same test. Expected: pass.
- [ ] **Step 4: Commit** "Document the analyst workflow's verbs and tools".
