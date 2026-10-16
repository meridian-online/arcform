---
name: building-and-checking-a-protocol
description: "Use once the data is profiled and its quality findings are agreed, to turn it into the tables that answer the question, and again whenever a run looks wrong. Records each step into the Protocol (raw, then cleaned and conformed, then aggregated, one purpose and one description per step), choosing the step kind in order: an operation arc holds, then SQL, then an operator for fetching, matching, embedding or export, and a shell command last. Runs the Protocol, reads the run record rather than the exit status (the run's outcome, each step's status, each table's row count), traces an empty or wrong table upstream to the step that caused it, and undoes edits through history. Do not use it to compute an answer outside the Protocol, or to fix a table by hand."
metadata:
  arc_version: "0.2.0"
---

# Building and checking a Protocol

Build records the steps that turn the profiled data into the tables the answer reads. Check runs them and reads what the run really did. The two stages loop: record, run, read, and record again until every table holds the rows you expect.

Recording runs nothing. A run replaces the tables its steps make, and the files its export steps write, with no earlier version kept: before a run that re-makes tables the user has seen, say which. History covers the spec, the steps you record and the files written through `protocol_write_file` or `report_write`, so an edit can be undone; a run cannot.

Load `references/step-kinds.md` when you choose how to record a step. Load `references/tracing-a-wrong-table.md` when a table is empty, smaller than it should be, or holds the wrong numbers.

**The worked Protocol** is `examples/brewtrend` in the arcform repository (github.com/meridian-online/arcform): six `http_fetch` steps with 24-hour preconditions, a raw load, a transform, a ranking with its Parquet export, and one shell command last, to print the top 20. Read it for the shape. Its SQL files were written by hand and each makes several tables; record yours one table per step.

## 1. Discover first

Before recording anything, know what the Protocol already makes and what arc can do for you.

| Step | CLI | MCP |
| --- | --- | --- |
| Read the spec and its models | Read `arcform.yaml` and `models/` in the Protocol folder | `sql_query` with `sql`, `dir`: `SELECT content FROM read_text('<dir>/arcform.yaml')`, with the absolute path. Before the first run there is no database to query: the replies of the record tools you called are the spec |
| List the operations arc holds | `arc operation list --json` | `operation_describe` (no arguments) |
| List the operators | None: the CLI has no listing; `arc operation describe <operator>` describes one | `operator_describe` with no arguments |
| One operator's `with:` | `arc operation describe <operator>` | `operator_describe` with `operator` |
| Each input table: rows, types, nulls | `arc table describe <table> --dir <dir>` | `table_describe` with `table`, `dir` |

## 2. Layer the steps

Build in three layers, in this order, and give each step one purpose.

- **Raw**: the data as it arrived. Fetches and loads, with no cleaning, so a wrong answer can always be checked against what the source sent.
- **Cleaned and conformed**: the fixes profiling called for. Cast types, drop or flag the rows that failed validation, give one thing one name and one unit, and bring sources to one shape before they are joined.
- **Aggregated**: the tables the answer reads, at the grain the question asks (one row per month, per category, per customer).

One purpose per step means a step does one thing its description can name: a filter, a join, a grouping. Split a step that filters and then groups, so that when a count is wrong its row counts show which of the two caused it. Name steps by layer (`raw_orders`, `clean_orders`, `orders_by_month`) so the reader sees the layering in the step list.

At the analysis and project sizes, stop for the user before a step that changes the grain or drops rows (a filter, an inner join, a deduplication): say how many rows it keeps, and why. For a quick look, carry on and fold it into the answer.

## 3. Choose the step kind

Take the first that fits; `references/step-kinds.md` has the forms and the reasons.

1. **An operation arc holds**, when `sql_recognise` names one for your statement (`arc sql recognise "<statement>"`).
2. **A SQL step** for any other statement: `sql_record` (`arc sql record`).
3. **An operator** for fetching, entity resolution, embedding, projection, export or publishing: `operator_record` (`arc operator record`), with its `with:` from `operator_describe`.
4. **A shell command**, last, only when none of these can do it.

## 4. Describe every step

Every record verb takes a description: `--description "<why>"` from a shell, `description` over MCP. Write one line saying what the step does and why, in the user's words ("Keep only paid orders: refunds are counted separately"). It is what the user, Brightfield and the next agent read; a step without one is a step nobody can check.

| Step | CLI | MCP |
| --- | --- | --- |
| Add a description to a step recorded without one | `arc edit-protocol --dir <dir> add steps[<i>] description "<why>"` | `protocol_edit` with `edit`, `path`, `key`, `value`, `dir` |

Over MCP, `edit` is "add", `path` is "steps[<i>]" (the step's place in the list, from 0) and `key` is "description".

## 5. Run

| Step | CLI | MCP |
| --- | --- | --- |
| Run the Protocol and get its run record | `arc run` (in the Protocol folder) | `protocol_run` with `dir` |
| Re-run every step, fresh or not | `arc run --force` | `protocol_run` with `dir`, `force` |

Before the first run of a fetch step, say that it reaches the network. A run skips each step whose inputs and SQL are unchanged, and each fetch inside its precondition's period. **Offer `force`; do not default to it.** It re-runs every step, and every fetch goes back to the network. Offer it when something arc cannot see has changed, such as a source updated within its 24-hour window, and say what it will re-fetch.

## 6. Read the result, not the exit status

A run that exits 0 can still have made an empty table: a filter no row meets is a successful step. Read the run record in this order, every time.

1. **`run.outcome`**: `success`, `partial` (some steps ran, then one failed) or `error`. Anything but `success` means a step failed; fix it first. From a shell, `arc run` prints the failing step's error and exits non-zero. Over MCP the run record carries no error text: take the failing step's model text from its `sql` entry in the record and run the SELECT after its `CREATE OR REPLACE TABLE` line with `sql_query`, which returns DuckDB's error in its place. For a failing operator or command step there is no MCP route to its error; ask the user to run `arc run` in the Protocol folder and paste what it prints.
2. **Each step's `status.state`**: `success`, `failed` or `skipped`. A skip with a `skip_reason` (`hash_clean`, `precondition_modified_after`) was fresh. A skip with a null `skip_reason` never ran, because an earlier step failed.
3. **Each asset's `row_count`**: every table should hold about the rows you expect. Zero, or far fewer than its input, is a finding: trace it with `references/tracing-a-wrong-table.md` before you go on.

`protocol_run` returns the run record. From a shell, `arc run` prints each step's state and an asset graph with each table's rows (`large_sales [table, 0 rows]`); the full record is `build/.arcform/runs/<run-id>.json`, and `arc table describe` gives one table's `row_count`.

Here every step succeeds and the run exits 0, but the filter step's table is empty:

```sh arc-example fixture=skills-empty-filter
arc run
arc table describe large_sales
```

```text expect
"row_count": 0
```

The Check stage's checkpoint is anything that changes the meaning of the answer: a table that had to be rebuilt, rows dropped that the question counts, a join that lost a source. Tell the user before presenting.

## 7. Undo through history

| Step | CLI | MCP |
| --- | --- | --- |
| List the recorded versions of the spec | `arc history list --json --dir <dir>` | `history_list` with `dir` |
| Print one version | `arc history show <id> --dir <dir>` | None: no tool prints a version. Choose the id by each entry's at and way fields |
| Roll the spec back to a version | `arc history restore <id> --dir <dir>` | `history_restore` with `id`, `dir` |

A restore keeps the state it replaces, so a wrong restore is undone by restoring the entry it made. Restoring the spec removes the steps recorded after that version; a model file a removed step wrote stays in `models/`, unused. History does not bring back a table: after a restore, run again so the tables match the spec.

## When to switch

- A new input turns up (another file, another source): back to `profiling-and-typing-data` for it, then record its raw step here.
- The run's outcome is `success`, every step has the state you expect, and every table holds the rows you expect: load `presenting-findings`.
- What the build found changes what the question can answer: back to `scoping-an-analysis` with the user.
- You need the exact form of another verb: `../using-arcform/references/verb-map.md`.

## Traps

| Error or symptom | Fix |
| --- | --- |
| The run exits 0 and you are about to present | Read `run.outcome`, each step's `status.state` and each asset's `row_count` first |
| A table has 0 rows | Trace it upstream with `references/tracing-a-wrong-table.md`; do not present from it |
| `invalid manifest: duplicate step name` | A step of that name exists. To change it, delete it (`protocol_edit` with `edit` `delete`, or `arc edit-protocol --dir <dir> delete steps[<i>]`) and record it again |
| `dependency order violation: step '<a>' reads asset '<b>' but ... runs after it` | Steps are recorded at the end. Move the reader after the step that makes its table: `arc edit-protocol --dir <dir> reorder steps <from> <to>` (`protocol_edit` with `edit` `reorder`) |
| `sql_query` over `read_text` returns no rows | The path was relative, read from the server's working directory. Give the absolute path to the Protocol folder |
| `no database for this Protocol yet` | The Protocol has not run. Run it, then look |
| A run says `skipped (fresh)` after you changed a source | The step's inputs looked unchanged, or a fetch is inside its period. Offer `force`, saying what it re-fetches |
| A step is `skipped` with a null `skip_reason` | It never ran: an earlier step failed. Fix that one |
| `succeeded but does not appear to have produced: <name>` | A `produces:` names something the step does not write. Name the file or table it really writes, or drop the `produces:` (an operator's output file is already known) |
| You are about to compute the answer with `sql_query` | `sql_query` is for looking. Record the step and run it, so the Protocol reproduces the answer |
| You are about to add a shell command step | Check `operator_describe` first; a command is the last resort, and arc cannot see what it reads or writes |
