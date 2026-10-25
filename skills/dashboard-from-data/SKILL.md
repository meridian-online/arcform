---
name: dashboard-from-data
description: "Use when the user wants to see what is in a dataset or a folder of related files, or asks for a dashboard, rather than having a specific question answered. Sizes the work from the data instead of by interview, profiles it through arc, explores it with queries each chosen from the last result to find what drives it, states that story as a claim, and composes one linked, crossfiltered dashboard in a Protocol panel that Brightfield draws: a focal view that carries the claim, the views that let the user test it by brushing, the data's quality beside them and the table's grid within reach. The views follow from what the data says; no view type or count is fixed. Hands a dataset with no story back as its profile. Do not use it for a specific question, which goes to scoping-an-analysis."
metadata:
  arc_version: "0.2.0"
---

# Dashboard from data

This skill answers "show me what's in this". It takes one data file or a folder of related files to a Protocol whose panel is a composed, linked dashboard: one focal view that carries a claim about the data, and around it the views that let the user test that claim by brushing. `presenting-findings` writes one chart per question; this skill composes views that work together, which is the opposite job. When the user asks a specific question ("which region grew fastest?"), use `scoping-an-analysis` instead.

**No template.** Nothing here requires a view type or a number of views. A dashboard that is a KPI row, a trend and a table for every dataset has failed, and so has a histogram per column with a scatter on top. The views follow from what this data says, found by querying it.

**Keep the user in flow.** Something good arrives fast, the composed dashboard follows, and its story is stated as a claim the views let them test. Ask at most one question, and only when it helps: when you have two credible stories, or several sources whose relationships you cannot confirm. Otherwise act, and say in one line what you assumed, so the user can override it. Never open with questions you could answer by looking at the data.

Every number you state comes from a query you ran. Every view reads a table the Protocol makes.

Load `references/exploring.md` at step 4: the query loop, the drivers to look for, when to stop, and when there is no story to tell. Load `references/composing.md` at step 6: the focal view and its responders, how they link, the quality view, the grid, Brightfield's limits and the checks, with a worked panel. Load `references/sources.md` when you want the method behind a rule.

| Step | CLI | MCP |
| --- | --- | --- |
| Make the Protocol | `arc create-protocol <dir> --name <name>` | `protocol_create` with `dir`, `name` |
| Fetch a file from a URL | `arc operator record http_fetch --name <step> --with '{"url": "<url>", "out": "data/<file>"}' --description "<why>" --dir <dir>` | `operator_record` with `operator`, `name`, `with`, `description`, `dir` |
| Load a file, or make the table the dashboard reads | `arc sql record "<select>" --name <step> --description "<why>" --dir <dir>` | `sql_record` with `sql`, `name`, `description`, `dir` |
| Run | `arc run` (in the Protocol folder) | `protocol_run` with `dir` |
| Type every column | `arc table profile <table> --sample-size <n> --dir <dir>` | `table_profile` with `table`, `sample_size`, `dir` |
| Rows, types and nulls | `arc table describe <table> --dir <dir>` | `table_describe` with `table`, `dir` |
| Explore: read-only queries | `arc sql query "<select>" --limit 100 --dir <dir>` | `sql_query` with `sql`, `limit`, `dir` |
| Write the panel | `arc protocol write-file panels/<name>.yaml --dir <dir> < panel.yaml` | `protocol_write_file` with `path`, `content`, `dir` |
| Check the panel | `arc panel check --dir <dir>` | `panel_check` with `dir` |

## 1. Size from the data and the ask, not by interview

Look before you ask. Is it one tidy file, one wide file (many columns), or a folder of related files? Is the ask directed ("show me what drives price") or open ("what's in this")? Say what you see in one line and carry on: "One tidy file of 20,640 rows and 10 columns, an open ask: I'll profile it, look for what drives it and compose one dashboard." The user can override it.

- **One tidy file**: profile it and explore.
- **A wide file**: choosing the columns is the hard part. Profile every column, then explore from the measures with the most spread and the dimensions with few enough values to group by.
- **A folder of related files**: load each as a raw step, then establish how they relate (shared keys, and how many rows match) with queries before you join them. A relationship you cannot confirm is the one question worth asking. The dashboard reads one table, so record the join that makes it.

Create the Protocol, record a raw load step for each file, and run. Say before the first run that a fetch step reaches the network.

## 2. Profile, through `profiling-and-typing-data`

Load `profiling-and-typing-data` and follow its steps: FineType types every column, the schema is saved in the Protocol, and every row is validated against it. Then `table_describe` gives the row count and each column's nulls.

Do not stop at that skill's checkpoint. Keep the quality findings (which columns are missing values, how many rows fail the schema, a column that is not what its name says) for step 6, where they go into the dashboard beside the views. They are not a gate the user has to pass first.

Here the fixture's delivery jobs are loaded and profiled. The description shows `minutes` missing in 9 of the 180 jobs, a finding kept for the quality view:

```sh arc-example needs=finetype fixture=dashboard-from-data
arc run
arc table profile jobs --sample-size 180
arc table describe jobs
```

```text expect
"null_count": 9
```

## 3. Show something good fast

The first thing the user sees is a result, not a question. Brightfield draws a dashboard of its own for a data file opened in it: tell the user to open the file, or the Protocol folder, in Brightfield now, while you explore. If you write a first panel early, Brightfield draws each panel as it arrives. What matters is the time to the first picture, not which picture.

## 4. Explore to find the story

Load `references/exploring.md`. Run a loop of queries through `sql_query`, each chosen from the last result, broad first and then into segments, in a budget of four to twenty. Look for what drives the data: two peaks in a distribution (two populations), the dimension that splits a measure most, a correlation and where it reverses inside a segment, change over time, and where overplotting hides structure.

For a well-known dataset you may bring what is known about it, and say that you did. Every number you state still comes from a query you ran.

Stop when no further query would change the claim. If the data has no story to tell (one usable column, under about 30 rows, or only identifiers), compose nothing: hand the profile over and say why. `references/exploring.md` has the rule and an example.

Here a first query over the fixture splits the delivery times by courier type:

```sh arc-example fixture=dashboard-from-data
arc run
arc sql query "SELECT courier_type, count(minutes) AS jobs, min(minutes) AS fastest, median(minutes) AS median_minutes, max(minutes) AS slowest FROM jobs GROUP BY courier_type ORDER BY courier_type"
```

```text expect
          24
        ],
        [
          "van",
          75,
          25,
```

Bikes took 6 to 24 minutes and vans 25 to 71: two populations that do not overlap. The next queries test whether zone moves either of them.

## 5. State the story as a claim, and check it

Write the story as one claim the views can test: "Delivery time is two populations, bikes and vans; courier type matters far more than zone." Then run every view you plan as a query before you compose it. A view that comes out flat, empty or uninformative is dropped, not shown. If two claims are equally credible, this is the moment for your one question.

## 6. Compose one linked panel

Load `references/composing.md`. One panel holds every view, over one table and one shared crossfilter selection, so brushing one view isolates the mechanism in the others. A focal view carries the claim and drives the selection; the views around it respond. You decide which views drive and which respond, which views there are, and how many.

Two things are always within reach, though not necessarily on the first screen:

- **The grid** of the table the dashboard reads, so the user can answer their row-level questions. Brightfield draws that grid itself. A brush in the panel filters the grid, and a filter typed in the grid narrows the views, given the panel declares `sel`; tell the user both at hand-over. Do not try to draw it in the panel.
- **The data's quality**, as a view over the same table and the same selection: how many of the rows the other views select are complete, and how many are missing the column the claim rests on. When that column is complete, the flat split is dropped and the view shows the weakness that bears most on the claim instead (mixed scales in the measure, say). A brush on that column itself can never select its missing rows, so under such a brush the quality view shows none missing; it speaks for the selections made in the other views, and for the whole table when nothing is selected.

A claim that rests on a weak column says so in its own words, in the focal plot's title or the claim you state.

A concat panel's top-level `title` is not drawn. Keep the claim there for Mosaic and the report (arc's `report_write` and Brightfield's PDF), and put it as the focal plot's `title` too; for CC BY data, append the credit line to that plot title, since only a drawn title reaches an exported picture.

## 7. Check what was made

- `panel_check` (`arc panel check`) comes back clean. Act on every warning as `presenting-findings` says, and write the panel again.
- **The story test**: the takeaway fits in one sentence, and that sentence is the claim.
- **The intuition test**: the dashboard reads without hover. Titles and axis labels say what each view shows.
- **The honesty test**: nothing is claimed as causal that is only associated, every number in a title came from a query you ran, and the quality view is there for the column the claim rests on, or for the data's main weakness when that column is complete.

## 8. Hand over

Follow `presenting-findings`' route: the user opens the Protocol folder in Brightfield, and the chat carries the claim, how to test it (which view to brush), and its caveats from step 2. Offer at least one next step that the dashboard raises. The Protocol folder is the deliverable.

## When to switch

- The user asks a specific question, or narrows to one after seeing the dashboard: `scoping-an-analysis`, and keep this Protocol.
- The profile shows the column a claim would rest on is mostly empty or not what its name says: say so, and explore from what is sound, or hand over the profile.
- A table is empty or smaller than it should be: `building-and-checking-a-protocol`, and trace it before composing.
- The user wants a report as well: `presenting-findings`.
- You need the exact form of another verb: `../using-arcform/references/verb-map.md`.

## Traps

| Error or symptom | Fix |
| --- | --- |
| You are about to ask the user what they want to see | Look first. Size from the data, say what you assumed in one line, and act |
| The dashboard is a histogram per column, or a KPI row and a trend | That is a template. Find the claim first (step 4), and compose the views that test it |
| Every view has the same size and weight, and no title states a claim | A chart gallery, not a story. Choose the focal view, make it the largest, and put the claim in the top-level `title` and in the focal plot's `title`, which is the one that is drawn |
| A number in a title or in the chat came from what you know about the dataset | Run the query that gives it, and quote that |
| A planned view comes out flat or empty when you query it | Drop it. Do not show a view that says nothing |
| The claim rests on a column with missing values and nothing on the panel says so | Add the quality view for that column (`references/composing.md`) |
| The quality view would be a complete/missing bar with nothing missing | It is flat, so drop it. Show the weakness that bears most on the claim instead (`references/composing.md`) |
| The claim or the CC BY credit sits only in the top-level `title`, and the exported picture lacks it | Brightfield does not draw that title. Put the claim in the focal plot's `title`, and append the credit to it |
| A hexbin on a map projection, `r` or `opacity` on dots, or a log colour scale is not drawn | Brightfield cannot draw them. Use a hexbin over longitude and latitude, and clamp a skewed count with `colorDomain` (`references/composing.md`) |
| `warning:` lines from `arc protocol write-file`, or `arc panel check` exits 1 | The panel names a table or column the Protocol does not make. Fix it from `table_describe` and write it again |
| Brushing one view changes nothing in the others | Each responder must read `data: { from: <table>, filterBy: $sel }`, and `params` must declare `sel`. See `references/composing.md` |
| Ranked bars reorder, or their totals vanish, under a brush | They carry `filterBy`. Ranked bars and counts over time use `toggleY` and `highlight` instead |
| The data has one usable column, a handful of rows or only identifiers | No story to tell. Hand over the profile and say why |
