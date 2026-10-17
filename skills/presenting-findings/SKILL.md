---
name: presenting-findings
description: "Use once a run is clean and every table holds the rows you expect, to hand the user the answer: whenever they want a chart, a summary, a write-up or a report of an analysis done in a Protocol. Writes each chart as a Mosaic spec in a YAML file under panels/ in the Protocol folder, reading a table the Protocol makes, in a chart kind Brightfield draws, and acts on any warning the panel check returns. States the answer, the quality caveats from profiling and the next steps; guides the user to open the Protocol folder in Brightfield to see the charts and how the answer was derived; writes report.md when they want a document. Do not use it to draw a chart with another tool, or to present a number no table in the Protocol holds."
metadata:
  arc_version: "0.2.0"
---

# Presenting findings

Present hands the user three things: the answer, how it was derived, and what else was found. The charts go into the Protocol as panels Brightfield opens, the answer goes in the chat (and in `report.md` when they want a document), and the Protocol folder itself is the deliverable.

Every number you present comes from a table the Protocol makes, and every chart reads one. If the answer needs a figure no table holds, go back and record the step that makes it; do not work it out in the chat.

Load `references/panels.md` before writing a panel: the chart kinds Brightfield draws, one example of each, and where Brightfield departs from Mosaic. Load `references/report.md` when the user wants a report.

## 1. Chart from the tables the Protocol makes

arc draws no chart. You write each one as a Mosaic spec, and arc saves it to `panels/<name>.yaml`, keeps it in history, and checks the names in it.

| Step | CLI | MCP |
| --- | --- | --- |
| See a table's columns and types before charting it | `arc table describe <table> --dir <dir>` | `table_describe` with `table`, `dir` |
| Write a chart as a panel | `arc protocol write-file panels/<name>.yaml --dir <dir> < panel.yaml` | `protocol_write_file` with `path`, `content`, `dir` |
| Check every panel against what the Protocol makes | `arc panel check --dir <dir>` | `panel_check` with `dir` |
| List a panel's recorded versions, to find the id to restore | `arc history list --json --file panels/<name>.yaml --dir <dir>` | `history_list` with `file`, `dir` |
| Roll a panel back | `arc history restore <id> --file panels/<name>.yaml --dir <dir>` | `history_restore` with `id`, `file`, `dir` |

- **One question per chart.** Chart the aggregated table the answer reads, not the raw one: then the number on the chart is the number in the table, and the report can show where it came from.
- **Choose the kind from `references/panels.md`**: a distribution, two measures, a map, counts over time, a grid of two categories, or ranked bars. Brightfield builds and tests those six.
- **Give every panel a `title`** saying what it shows. The report lists each panel under it.
- **Name the file for what it shows** (`panels/spend_by_category.yaml`), in lowercase with underscores.

## 2. Act on every warning

`protocol_write_file` checks a panel as it writes it and returns `warnings` (from a shell, `warning:` lines) for each mark that reads a table or a column the Protocol does not make. The file is still written. **Do not leave a warning standing**: a panel that names a missing table or column opens in Brightfield as an empty or broken chart.

Fix it in one of two ways, then write the panel again:

- the panel is wrong (a misspelt column, the raw table's name instead of the aggregate's): correct the name, from `table_describe`;
- the Protocol is missing the table: back to `building-and-checking-a-protocol` to record the step that makes it, and run.

Before presenting, run `panel_check` (`arc panel check`) over every panel. From a shell it prints one line per finding and exits 1 when there is any; over MCP it returns `clean` and each panel's `findings`. Present only when it is clean.

Write the panel after the Protocol has run. Before the first run there is no database, so the check knows only the tables the steps will make and cannot check a single column. Three things it does not check at all: an object channel (`{ sum: amount }`, `{ bin: amount }`), a `data:` written as a bare string, and a three-part `catalog.schema.table` name. Check those against `table_describe` yourself.

Here the fixture gains an aggregate step, runs, and a bar chart of its two columns is written as bare strings, so the check really reads them; it finds nothing:

```sh arc-example
arc sql record "SELECT category, sum(amount) AS total FROM sales GROUP BY category" --name spend_by_category --description "Total spend in each category"
arc run
arc protocol write-file panels/spend_by_category.yaml <<'YAML'
title: Spend by category
plot:
  - mark: barX
    data: { from: spend_by_category }
    x: total
    y: category
    sort: { y: -x }
yLabel: category
marginLeft: 90
YAML
arc panel check
```

```text expect
wrote panels/spend_by_category.yaml as a panel
```

## 3. State the answer

Say it in the chat, in this order, before anything else:

1. **The answer**, in one sentence, with its number and its unit, in the user's words.
2. **How it was derived**, in a line or two: the tables it reads and the steps that matter (a filter that dropped rows, a join). The detail is in the Protocol.
3. **The caveats from profiling** that bear on it: rows dropped and how many, columns that failed validation, a source that covers only part of the period. Say what each does to the answer.
4. **What else was found**, and **the next steps**: the question the answer raises, the data that would settle a caveat. Offer at least one outright, as something the user could do next and what it would tell them; opening, re-running or sharing the Protocol is not a next step.

Every size stops for the user here; for a quick look it is the only checkpoint. A quick look skips the report, but still gets a panel when a chart helps, the pointer to Brightfield and the handover of the folder.

## 4. Guide them to Brightfield

The guided route ends in Brightfield. Tell the user to open the Protocol folder in Brightfield: it draws the panels against the Protocol's tables and shows the steps that made them, so they can see how the answer was derived and explore from there. Name the panels you wrote.

For someone who will not open Brightfield, offer the report.

## 5. Write the report, when they want a document

| Step | CLI | MCP |
| --- | --- | --- |
| Write `report.md` from your prose and the newest run | `arc report write --dir <dir> < prose.md` | `report_write` with `prose`, `dir` |
| Write it from an earlier run | `arc report write --run <run-id> --dir <dir> < prose.md` | `report_write` with `prose`, `run`, `dir` |

The prose is the answer, its caveats and the next steps, with no `##` headings of your own. arc adds three sections after it: how the answer was derived (each step, its description, status and row counts), the charts (each panel's title and anything `panel_check` finds), and the run. `references/report.md` says what to put in the prose. A project ends with a report whether or not the user asks.

Here the fixture gains the same aggregate step and runs, and the report is written from that run:

```sh arc-example
arc sql record "SELECT category, sum(amount) AS total FROM sales GROUP BY category" --name spend_by_category --description "Total spend in each category"
arc run
printf 'Dining had the highest total spend (spend_by_category), but all five categories are within 5%% of each other.\n\nThe amounts were not validated, so treat them as provisional.\n' | arc report write
cat report.md
```

```text expect
## How this was derived
```

The PDF, with the charts drawn in it, comes from Brightfield's export. That release of Brightfield is planned and has not shipped, so there is no command to give yet: offer `report.md` now, and say the PDF will come from Brightfield's export once that release is out.

## 6. Hand over the folder

End by telling the user the Protocol folder can be committed to git or shared as it is: the spec, its models, the schemas, the panels and the report are all in it, and anyone with arc can run it again to rebuild the tables. If a step reads a file by an absolute path (a source outside the folder that could not be moved into it), say so: the Protocol is then tied to this machine, and runs elsewhere only once that file is copied into the folder and the step reads it by a relative path. The tables themselves live in the Protocol's database, outside the folder unless the spec names a `db:` inside it, and are not part of what they share.

## When to switch

- A number looks wrong, or a chart shows something the tables should not: back to `building-and-checking-a-protocol`, and trace the table before presenting anything more.
- A panel names a table the Protocol does not make: record the step in `building-and-checking-a-protocol`, run, then write the panel again.
- The user asks a new question: back to `scoping-an-analysis`, and keep this Protocol.
- You need the exact form of another verb: `../using-arcform/references/verb-map.md`.

## Traps

| Error or symptom | Fix |
| --- | --- |
| `warning: a mark reads the column <c> of <t>, which <t> does not have` | The panel names a column the table lacks. Correct it from `table_describe` and write the panel again |
| `warning: a mark reads <t>, which is neither in the panel's data nor a table the Protocol makes` | Use the name of a table a step makes, or record the step that makes it and run |
| ``the panel has no `plot` key`` | A panel is a plot: put the marks in a list under `plot` (or in a `hconcat`/`vconcat` list of plots) |
| `arc panel check` exits 1 with `in the panels the protocol does not make` | Fix each panel it names, then check again; present only when it prints nothing |
| `panel_check` is clean but a chart is blank in Brightfield | The check does not look inside `{ sum: … }` or `{ bin: … }`, a bare-string `data:` or a `catalog.schema.table` name, and checks no column before the first run. Check those names with `table_describe` |
| A `title`, `xLabel` or `width` has no effect | It is indented under the last mark. Plot attributes are siblings of `plot`, at its indent |
| Brightfield shows a banner saying an instruction had no effect | An interactor binds `as: $<name>` that `params` does not declare. Declare it, or drop the interactor |
| A bar chart of dates comes out as the ten busiest days | It is ranked bars. Use `counts-over-time` from `references/panels.md`: no `sort`, no `limit` |
| `has not run yet: run the Protocol first` from `report_write` | Run the Protocol, then write the report |
| `the report has no prose` | Write the answer, its caveats and the next steps, and pass them as the prose |
| The report's Charts section shows a `Check:` line | A panel names something the Protocol does not make. Fix it and write the report again |
| The user asks for a PDF | Brightfield's headless export is not released yet. Give them `report.md` and the Protocol folder; do not draw the charts with another tool |
| You are about to compute a figure for the answer with `sql_query` | Record the step that makes it, run, and read it from the table |
