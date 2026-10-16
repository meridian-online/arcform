# Analyst workflow, plan 2 of 3: the skills

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Write the five analyst skills that carry the workflow from scope to presentation, each checked against the tools and verbs the binary really has.

**Architecture:** Skills live in `skills/<name>/SKILL.md` at the repository root, each a short router with reference files under `skills/<name>/references/` loaded only when a step needs them. A Rust integration test reads every skill and checks each tool, argument, verb and flag it names against `tools/list` and `--help`, and runs each example marked for checking against a fixture Protocol.

**Tech Stack:** Markdown with YAML frontmatter; a Rust integration test (`tests/skills.rs`) driving the built `arc` binary.

**Spec:** `docs/superpowers/specs/2026-10-16-analyst-workflow-skills-design.md`, sections "The workflow", "The skills", and "Verification" (structural and examples).

**Order:** Runs after plan 1 (arc capabilities), which has landed: its verbs and tools are listed in README.md's CLI and MCP sections and `arc mcp`'s instructions, and its modules in docs/ARCHITECTURE.md. Plan 3 packages these skills.

## Global Constraints

- Every skill shows the CLI and MCP forms side by side, as a two-column table or paired blocks.
- Skills spend their words on arc's method, verification and traps, not on DuckDB SQL.
- The agent records every analysis step into the Protocol; the skills never tell it to compute an answer outside the Protocol.
- Each skill's frontmatter carries `name`, `description` (at most 1,024 characters, quoted if it contains a colon), and `metadata.arc_version`, the arc version it was written for.
- Each skill ends with when to switch to another skill, and puts its known traps in an error-to-fix table.
- The finetype version floor is stated where finetype is used, with why it exists.
- The maintainer `release` skill stays in `.claude/skills/`; nothing here moves it.
- Public repository: no private planning identifiers. British English. No hard-wrapped prose.

## Review Focus

1. **An agent in a desktop chat app with no shell** must be able to follow every step from the MCP column alone; a step whose MCP column is empty is a defect. Test in Task 1 (each table row has both columns filled).
2. **A dataset that is not a local CSV** (a URL, JSON, a spreadsheet) must route through a raw load step and `table_profile`, not stop at `profile`. Pinned by the profile skill's example in Task 4.
3. **A run that "succeeds" with an empty table** must be caught by the build skill's read order (outcome, step status, row counts). Example in Task 5 runs a fixture whose filter empties a table and shows the trace.
4. **Skills installed from GitHub that are newer than the user's arc** must be noticed by the entry skill's environment check. Example in Task 2.
5. **A quick question** must not be dragged through a project's ceremony; the scope skill's size rules must put a one-aggregate question at the quick size. Covered in Task 3's worked examples.

## Verification commands and what each costs

- `DUCKDB_LIB_DIR=/opt/homebrew/lib cargo test --features mcp --test skills`: builds `arc` (minutes from cold), then checks every skill in about a minute. Needs `finetype` at the floor on PATH for the finetype examples; without it those examples are skipped with a printed reason.

---

### Task 1: The skill checker

**Files:**
- Create: `tests/skills.rs`, `tests/fixtures/skills/` (a small Protocol: one CSV of 200 rows with a date, a category, an amount and an email column, and an `arcform.yaml` that loads it)
- Create: a `README.md` in the new `skills/` folder (what the directory is, for a person browsing GitHub)
- Modify: `.claude/skills/release/SKILL.md` (set each skill's `metadata.arc_version` when bumping the version)

**Interfaces:**
- Produces, for later tasks:
  - In a skill, an MCP tool is written as `` `tool_name` `` followed by its arguments in a JSON block or as `` `argument` `` names in the same table row; a CLI form is a fenced `sh` block or `` `arc …` `` inline. The checker reads both.
  - An example the checker runs is a fenced block with the info string `sh arc-example`; it runs with the fixture Protocol as its working directory, and an optional following block with info string `text expect` holds a substring its output must contain.

- [ ] **Step 1: Write the tests** (they pass vacuously until a skill exists, so also add a probe skill, a `SKILL.md` in a `skills/_probe/` folder, naming one real tool, one fake tool and one fake flag, to see them fail):
  - `every_named_tool_is_advertised`: start `arc mcp`, call `tools/list`, and assert every `` `name` `` in a skill that looks like a tool (snake_case, in an MCP column or JSON block) is advertised, and each argument named with it is in that tool's `inputSchema.properties`.
  - `every_cli_form_exists`: for each `arc <verb> [<sub>]` in a skill (its SKILL.md and every file under `references/`), `arc <verb> [<sub>] --help` exits 0, and each `--flag` named with it appears in that help.
  - `every_verb_table_row_has_both_columns`.
  - `frontmatter_is_loadable`: `name` matches the folder, `description` at most 1,024 characters, a `:` in an unquoted description is refused, `metadata.arc_version` equals `CARGO_PKG_VERSION`.
  - `examples_run`: each `sh arc-example` block exits 0 against a fresh copy of the fixture and its output contains the `text expect` substring.
- [ ] **Step 2: Run** `cargo test --features mcp --test skills`. Expected: the probe's fake tool and fake flag fail with messages naming the skill file and line.
- [ ] **Step 3: Delete the probe skill; run again.** Expected: pass.
- [ ] **Step 4: Teach the release skill the version rule.** In `.claude/skills/release/SKILL.md`, add a step: when bumping the crate version, set every skill's `metadata.arc_version` to it, since `frontmatter_is_loadable` requires them equal.
- [ ] **Step 5: Commit** "Check agent skills against the tools and verbs arc has".

### Task 2: The entry skill, `using-arcform`

**Files:**
- Create: the folder `skills/using-arcform/` with its `SKILL.md`, and in its `references/` folder `verb-map.md` and `environment-check.md`

Its description is the most important text in the set. It must make an agent use arc for any question about tabular data (CSV, spreadsheet, JSON records, a table on the web), a chart from data, or a data report, without arc being named; and it must say when not to use arc (a single value read off a small file the user pasted; non-tabular data). Write three candidate descriptions and keep the one the plan 3 eval does best with; until then, keep the first.

SKILL.md covers, briefly: what a Protocol is (a folder with `arcform.yaml` and `models/`); why arc over a script, in the words the agent can say to the user; the safety model (recording runs nothing, a run runs, history undoes, and every write is a version); where run results land; the three sizes, each in one line; the five stages and which skill carries each; "Load when" pointers to the two references.

- [ ] **Step 1: Write the skill and references.** `verb-map.md` is the full CLI | MCP table for every verb the five skills use, including plan 1's. `environment-check.md`: `arc --version` against `metadata.arc_version` (when they differ, upgrade arc or reinstall the skills from where they came; plan 3 adds `arc skills install` here); whether `arc mcp` is available; `finetype --version` at or above the floor; what to tell the user for each failure.
- [ ] **Step 2: Add one `sh arc-example`**: `arc --version`, expecting `arc `.
- [ ] **Step 3: Run** `cargo test --features mcp --test skills`. Expected: pass.
- [ ] **Step 4: Commit** "Add the entry skill for the analyst workflow".

### Task 3: `scoping-an-analysis`

**Files:**
- Create: the folder `skills/scoping-an-analysis/` with its `SKILL.md`

Covers: restate the question and the end state in the user's words; choose the size by the rules (a quick look when one or two aggregates answer it; an analysis by default; a project when the user wants a report, the work spans several datasets, or the answer will be shared); say the size and let the user override; play the plan back (data source, stages, what they will get) before building; for a project, write the requirements into the Protocol's `README.md` through `protocol_write_file` / `arc protocol write-file` and have them agreed; create the Protocol (`protocol_create` / `arc create-protocol`). Three short worked examples, one per size.

- [ ] **Step 1: Write the skill**, with an `sh arc-example` that creates a Protocol in a temporary folder and writes a README with `arc protocol write-file README.md`.
- [ ] **Step 2: Run** the skills test. Expected: pass.
- [ ] **Step 3: Commit** "Add the scoping skill".

### Task 4: `profiling-and-typing-data`

**Files:**
- Create: the folder `skills/profiling-and-typing-data/` with its `SKILL.md`, and in its `references/` folder `reading-the-results.md` and `what-finetype-teaches.md`

Covers: get the data in first (a local CSV can be profiled directly; anything else is loaded by a raw step, an `http_fetch` operator or a SQL step, then profiled as a table with `table_profile` / `arc table profile`); `profile` or `table_profile`; save a table-level JSON Schema with `format: json-schema` written by `protocol_write_file` to `schemas/<table>.json`; `validate` against it (`table_validate` / `arc table validate` for a table, run in DuckDB by finetype's extension; `validate` for a local CSV or Parquet file); do not stop after profiling; `table_describe` for row and null counts; what each result means for the user's question (`reading-the-results.md`: confidence as a quality signal, the reject report, nulls, mixed types); the finetype version floor and the reason for it (finetype 0.6.53 and earlier read any eight-digit number as a confident date). `what-finetype-teaches.md` takes everything finetype's own two agent skills teach and, for each, gives the route through arc (the `finetype_validate` operator as a check that fails a step when a built Parquet breaks its JSON Schema contract; typed materialisation into a DuckDB table with a reject sidecar is not reachable through arc, so say what to run instead; a Data Package through `datapackage_describe`; one type's schema through `taxonomy`) or says it is not reachable and what to run instead.

- [ ] **Step 1: Read finetype's two agent skills** (`finetype-cli` and `finetype-pipeline` in the finetype repository's `.claude/skills/`) and list what each teaches.
- [ ] **Step 2: Write the skill and references**, with `sh arc-example` blocks for profile, json-schema to file, and validate on the fixture CSV, and one for `arc table profile` on the fixture's loaded table.
- [ ] **Step 3: Run** the skills test. Expected: pass.
- [ ] **Step 4: Commit** "Add the profiling and typing skill".

### Task 5: `building-and-checking-a-protocol`, and brewtrend as its worked example

**Files:**
- Create: the folder `skills/building-and-checking-a-protocol/` with its `SKILL.md`, and in its `references/` folder `step-kinds.md` and `tracing-a-wrong-table.md`
- Modify: `examples/brewtrend/arcform.yaml` (fetch steps use `http_fetch` instead of `curl` commands) and `examples/brewtrend/README.md`
- Create: `tests/fixtures/skills-empty-filter/` (the fixture Protocol plus a filter step whose condition no row meets)

Covers: discover first (read `arcform.yaml`, `operation_describe` with no argument, `operator_describe` with no argument, `table_describe` on inputs); layer the steps raw, then cleaned and conformed, then aggregated, one purpose per step, with the plain-words rationale; the step-kind decision table (`step-kinds.md`: a catalogue operation when `sql_recognise` names one; else `sql_record`; an operator through `operator_record` for fetching, entity resolution, embedding, projection, export or publishing, with `operator_describe` for its `with:`; a shell command last); a description on every step; run with `protocol_run` / `arc run`; read the result in the order `run.outcome`, each step's status, each asset's row count, and never the exit status alone; trace an empty or wrong table upstream (`tracing-a-wrong-table.md`: raw empty means the source or load is wrong, cleaned empty means a filter is too tight, aggregated counts wrong means a grouping or join); offer `force` rather than default to it, because it re-fetches; undo with `history_list` and `history_restore`. Points at `examples/brewtrend` as the complete worked Protocol.

*My call, flagged for review:* brewtrend's fetches change from `curl` commands to `http_fetch`, so the worked example follows the skill's own rule that a shell command comes last. Its outputs must not change.

- [ ] **Step 1: Change brewtrend's fetch steps** to `http_fetch` with the same URLs and output paths and keep the 24-hour preconditions; run `arc run` in `examples/brewtrend` and confirm `ranking.parquet` has the same row count and columns as before.
- [ ] **Step 2: Write the skill and references**, with an `sh arc-example` that runs the empty-filter fixture and shows the step whose table has 0 rows (`text expect`: `"row_count": 0`).
- [ ] **Step 3: Run** the skills test. Expected: pass.
- [ ] **Step 4: Commit** "Add the build-and-check skill; brewtrend fetches with http_fetch".

### Task 6: `presenting-findings`

**Files:**
- Create: the folder `skills/presenting-findings/` with its `SKILL.md`, and in its `references/` folder `panels.md` and `report.md`

Covers: write each chart as a Mosaic spec to `panels/<name>.yaml` through `protocol_write_file`, reading from a table the Protocol makes, and act on any warning the write returns (`panel_check`); the chart kinds Brightfield draws (`panels.md`, taken from Brightfield's chart-kind registry and its documented departures from Mosaic, with one example per kind); state the answer, the quality caveats from profiling, and next steps; guide the user to open the Protocol folder in Brightfield to see the charts and how the answer was derived; when they want a document, write the report with `report_write` / `arc report write` and offer the PDF through Brightfield's export (`report.md`: what to put in the prose, which sections arc adds). Ends by telling the user the Protocol folder can be committed or shared as it is.

- [ ] **Step 1: Read Brightfield's chart-kind registry and its deviations list** in the Brightfield repository and write `panels.md` from them.
- [ ] **Step 2: Write the skill**, with `sh arc-example` blocks that write a bar-chart panel for the fixture and run `arc panel check` (expect no findings), and that write a report after a run.
- [ ] **Step 3: Run** the skills test. Expected: pass.
- [ ] **Step 4: Commit** "Add the presenting skill".

### Task 7: Point MCP clients at the workflow

**Files:**
- Modify: `src/mcp/mod.rs` (`INSTRUCTIONS`)

The `initialize` instructions open with the five stages in one line each and name the entry skill, so a client with no skill support still gets the method in outline.

- [ ] **Step 1: Add a test** `instructions_outline_the_workflow`: `INSTRUCTIONS` names each stage and `using-arcform`.
- [ ] **Step 2: Run** `cargo test --features mcp instructions_`. Expected: fail, then pass after the edit.
- [ ] **Step 3: Commit** "Outline the analyst workflow in arc mcp's instructions".
