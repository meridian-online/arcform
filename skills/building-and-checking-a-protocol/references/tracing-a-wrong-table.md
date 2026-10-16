# Tracing an empty or wrong table

A table that is empty, much smaller than its input, or holds the wrong numbers was made wrong by one step. Find that step before changing anything: a fix recorded further down hides the fault instead of mending it.

## Walk upstream

Start at the wrong table and walk towards the source, one step at a time, until you reach the first table that is wrong. The step that made it is the one to fix.

1. In the run record's `assets`, find the table. Its `produced_by` names the step that made it.
2. That step's inputs are the assets whose `consumed_by` lists it. Read each input's `row_count`.
3. If an input is wrong too, repeat from it. If every input looks right, the step itself is at fault.

From a shell, `arc run` prints the same graph: each asset with its row count, the step that produces it and the steps it feeds.

| Step | CLI | MCP |
| --- | --- | --- |
| A table's rows, types, null counts and five sample rows | `arc table describe <table> --dir <dir>` | `table_describe` with `table`, `dir` |
| Ask a question of a table, read-only | `arc sql query "<select>" --limit 20 --dir <dir>` | `sql_query` with `sql`, `limit`, `dir` |
| Read a step's model | Read `models/<file>.sql` in the Protocol folder | `protocol_run` returns the run record, where each SQL step's sql entry carries its model's text (this runs the Protocol; use the record you already have) |

## What the layer tells you

| Where the table first goes wrong | Usual cause | What to look at |
| --- | --- | --- |
| Raw: empty or tiny | The source or the load is wrong. The fetch got an error page, an empty file or the wrong file; the load reads the wrong path, or the wrong format options | The fetched file's `bytes` in the run record's assets; the file itself, read with `sql_query` (`read_text`, `read_csv`, `read_json` on its absolute path) |
| Cleaned and conformed: empty or far fewer rows | A filter is too tight, or an inner join's keys do not match | The range and values of the filtered column; the keys on both sides of the join (types, case, spaces, leading zeros); a cast that turned values into nulls that a filter then dropped |
| Aggregated: counts or sums wrong | The grouping or a join. A join to a key that is not unique repeats rows, so sums come out too high; a group at the wrong grain; null group keys gathered into one row | Count rows per join key on the side meant to be unique; the `GROUP BY` against the grain the question asks |
| Aggregated: right totals, wrong rows | A filter applied before the grouping that should come after it, or the reverse | Each step's description against what its SQL does |

## Diagnose with queries, fix with steps

`sql_query` is for finding the cause. The fix is a recorded step: change the step that went wrong and run again, so the Protocol reproduces the corrected answer. Never present a number from a diagnostic query.

| Fault in | How to change it |
| --- | --- |
| An operation or SQL step | Delete it and record it again under the same name, then move it back above the steps that read its table (see `step-kinds.md`) |
| An operator's `with:` | `protocol_edit` with `edit` "replace" and `path` "steps[<i>].with.<key>" (`arc edit-protocol --dir <dir> replace 'steps[<i>].with.<key>' <value>`) |
| The source itself | Tell the user what the source holds; the fix may be a different source, or a different question |

Then run, and read the result again in order: `run.outcome`, each step's `status.state`, each asset's `row_count`.

## A worked trace

The empty-filter Protocol keeps the sales over 1,000. The run succeeds and `large_sales` is empty. Its input, `sales`, holds 200 rows, so the load is fine and the filter step is at fault. Asking for the largest sale shows why: none is over 94. With the user's agreement on a threshold that fits the question, the step is recorded again:

```sh arc-example fixture=skills-empty-filter
arc run
arc table describe sales
arc sql query "SELECT max(amount) AS largest FROM sales"
arc edit-protocol delete 'steps[1]'
arc sql record "SELECT * FROM sales WHERE amount > 50" --name large_sales --description "Keep the sales over 50, the large ones in this data"
arc run
arc table describe large_sales
```

```text expect
"row_count": 96
```

Choosing the threshold is the user's call, not yours: a filter changed to make a table non-empty changes what the answer means, which is the Check stage's checkpoint.
