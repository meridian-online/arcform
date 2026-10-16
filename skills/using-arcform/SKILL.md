---
name: using-arcform
description: "Use for any question about tabular data, such as a CSV, a spreadsheet, JSON records, a Parquet file or a table on a web page, and for any chart, summary, model, extract or report made from such data, even when the user never mentions arc or arcform. Works through arc (the `arc` command or the `arc mcp` tools) so every step is recorded in a Protocol the user can re-run, inspect in Brightfield, commit to git and share, instead of a throwaway script. Start here: it covers what a Protocol is, the safety model, an environment check, the size of the work, and which analyst skill to load next. Do not use it to read one value off a small table the user pasted into the chat, or for data that is not tabular (prose, images, audio, source code)."
metadata:
  arc_version: "0.2.0"
---

# Using arcform

arc is a local-first engine that records an analysis as a **Protocol** and runs it over DuckDB. When the user brings data and a question, do the work in a Protocol rather than a one-off script, and tell them why in a sentence they would use: "I've used arcform to save tokens and to build you a process you can inspect in Brightfield, build on later, or share."

Every analysis step goes into the Protocol. Never compute an answer outside it: an answer the committed Protocol cannot reproduce is not one the user can check.

Load `references/environment-check.md` at the start of a session, before the first Protocol. Load `references/verb-map.md` whenever you need the exact CLI or MCP form of a verb.

## What a Protocol is

A folder holding `arcform.yaml`, which lists the steps in order, and `models/`, which holds each SQL step's statement. Steps make tables; arc reads each step to know which tables it reads and writes. The folder can also hold `schemas/` (JSON Schemas for tables), `panels/` (charts Brightfield opens), a `README.md` of requirements and a `report.md`. The folder is the deliverable: the user can run it again, commit it or hand it on.

## The safety model

- **Recording runs nothing.** `sql_record`, `operation_record` and `operator_record` write a step into the spec. Nothing touches the data.
- **A run runs.** `protocol_run` executes the steps: it writes tables and files and may reach the network. Say so before the first run of a step that fetches.
- **History undoes.** Every machine edit, and every file arc writes, keeps what it replaced. `history_list` and `history_restore` roll the spec, or one file, back.
- **Every write is a version.** Nothing arc writes is lost to an overwrite, so try things; but say what you are replacing.
- **Looking is read-only.** `table_describe`, `sql_query`, `table_profile` and `table_validate` never write the database.

| Step | CLI | MCP |
| --- | --- | --- |
| Make a Protocol | `arc create-protocol <dir> --name <name>` | `protocol_create` with `dir`, `name` |
| Record a step | `arc sql record "<select>" --name <step> --description "<why>"` | `sql_record` with `sql`, `name`, `description` |
| Run it | `arc run` | `protocol_run` with `dir` |
| Undo a change | `arc history restore <id>` | `history_restore` with `id` |

## Where results land

- **Tables** go into the Protocol's DuckDB database, outside the folder (in `~/.arcform/db/` unless the spec names a `db:`). Read them with `table_describe` and `sql_query`, never by opening the file.
- **The run record** is `build/.arcform/runs/<run-id>.json` in the folder; `protocol_run` returns it. Read `run.outcome`, then each step's `status.state`, then each asset's `row_count`. A run that exits 0 can still have skipped a step or made an empty table.
- **Files arc writes** (schemas, panels, README, report) go in the folder, at the path you give.
- **History** lives in `~/.arcform/history/`, with one line per version in the folder's `arcform-log.txt`.

## Sizes

Choose one with `scoping-an-analysis`, say which, and let the user override.

- **A quick look.** One or two aggregates answer it: minimal profiling, a short Protocol, one checkpoint at the end.
- **An analysis.** The default: all five stages, the plan played back after scoping and the quality findings shown after profiling.
- **A project.** Requirements agreed and written into the Protocol's `README.md` before building, a checkpoint at each stage and a report at the end.

## The stages and their skills

| Stage | Skill to load | Checkpoint with the user |
| --- | --- | --- |
| Scope | `scoping-an-analysis` | The plan, played back before anything is built |
| Profile | `profiling-and-typing-data` | The data-quality findings and what they mean for the question |
| Build and check | `building-and-checking-a-protocol` | Anything that changes the answer's meaning |
| Present | `presenting-findings` | The answer, how it was derived, and what else was found |

Start every task at Scope, even a quick look: choosing the size is what keeps a quick look quick.

## Check the binary

```sh arc-example
arc --version
```

```text expect
arc 
```

## When to switch

- The environment check passed: load `scoping-an-analysis`.
- The plan is agreed and the data is not yet typed: `profiling-and-typing-data`.
- The quality findings are shown: `building-and-checking-a-protocol`.
- The run record is clean and every table has the rows you expect: `presenting-findings`.
- The user changes the question mid-way: back to `scoping-an-analysis`, and keep the Protocol you have.

## Traps

| Error or symptom | Fix |
| --- | --- |
| You are about to answer with a pandas or Python snippet | Record the step instead (`sql_record`, or an operator through `operator_record`) and run it |
| `arc --version` differs from `metadata.arc_version` | Tell the user to upgrade arc or reinstall the skills; see `references/environment-check.md` |
| `no database for this Protocol yet` | The Protocol has not run; run it, then look |
| `only read-only SELECT statements can be queried` | `sql_query` only reads. Record the change as a step and run it |
| A run says `1 skipped (fresh)` after you changed an input | Steps whose inputs arc saw unchanged are skipped. Offer `arc run --force` (`force` on `protocol_run`); do not default to it |
| A run exits 0 but a table is empty | Read the run record: `run.outcome`, each step's `status.state`, each asset's `row_count`. Trace the empty table upstream |
| `finetype <version> is older than the minimum 0.6.54` | Tell the user to upgrade finetype to 0.6.54 or later; an older one mistypes columns |
| `finetype DuckDB extension not configured` | Set `FINETYPE_DUCKDB_EXT`, or validate an exported CSV with `validate` |
| A tool reports that `duckdb` or `finetype` cannot be found from a desktop app | The app lacks the shell's `PATH`; give the MCP server one in its configuration |
| `protocol_run` times out in Codex | Raise `tool_timeout_sec` for the `arc` server (600 seconds is enough for most runs) |
