# Composing one linked panel

A dashboard is one panel: a Mosaic spec at `panels/<name>.yaml`, written with `protocol_write_file` (`arc protocol write-file`), that Brightfield draws as linked views. This page says how the views link, which one leads, how the data's quality sits beside them, and what Brightfield cannot draw. `../presenting-findings/references/panels.md` has the chart kinds Brightfield draws, one example of each, and where it departs from Mosaic; read it first, and come back here to put them together.

The views follow from the claim you found in `references/exploring.md`. Nothing on this page says which views to use or how many: a two-view dashboard can be right, and so can a six-view one.

## One panel, one table, one selection

- **One panel.** Lay the views out as a `hconcat` or `vconcat` list of plots, nested as deep as the layout needs. Each plot has its own `plot` list of marks and its own attributes (`title`, `xLabel`, `width`, `height`), as siblings of its `plot` key.
- **One table.** Every view reads the same table, through `data: { from: <table> }`. A crossfilter can only isolate rows that every view shares. When the views need columns from several sources, or a derived column (a status flag, a bucket), record one step that makes a table holding all of them, run, and point every view at it.
- **One selection.** Declare it once at the top level, `params: { sel: { select: crossfilter } }`, and have every interactor publish into it (`as: $sel`) and every responder read it. Without the declaration Brightfield draws the views and puts a banner over them saying the interaction had no effect.

## The focal view and its responders

**The focal view carries the claim.** It is the view where the story is seen: the distribution with two peaks, the scatter whose correlation reverses, the series with its step. Make it the largest view and give it the first place in the layout. It is the brush source: an `intervalX` (or `intervalXY` on a scatter or map) that publishes into `$sel`.

**The responders let the user test the claim.** Each shows what the brushed rows are like on another column: brush the slow deliveries in the focal view, and the responders show they are the vans, spread across every zone. Choose a responder because brushing the focal view changes it in a way that tests the claim; a view that would not move under a brush adds nothing.

How a responder reads the selection depends on its kind:

- **A binned histogram, a scatter or a map** draws two layers: the whole table in grey (`fill: "#b7b3ae"`) as a ghost, and on top the same mark reading `data: { from: <table>, filterBy: $sel }`. The ghost keeps the total on the page, so the brushed rows read as a share of it. Add `xDomain: Fixed` so the axis does not move under the brush.
- **Ranked bars and counts over time** use one layer with no `filterBy`: `select: toggleY` (or `toggleX` for vertical bars) publishes a click into `$sel`, and `select: highlight` with `by: $sel` shows the selected rows inside each bar. Brightfield's ranked bars take their total off the page and reorder the axis under the pointer when they carry `filterBy`, so do not give them one. These views drive as well as respond: a click on a bar filters the others.

You decide which views drive and which respond. Usually the focal view drives; when the claim is about a category ("vans are the slow ones"), the bars of that category may lead and the distribution respond.

## The data's quality, beside the views

The quality view says how far to trust what the brush shows. It is a count bar over the same table and the same selection, splitting the rows that are complete from those missing the column the claim rests on, so that as the user brushes it shows how many of the brushed rows the claim can speak for.

- Record the split as a column of the table the dashboard reads (`CASE WHEN minutes IS NULL THEN 'missing' ELSE 'complete' END AS minutes_status`), since a bare-string channel must be a column.
- Draw it as `mark: barX` with `x: { count: }`, `y:` the status column, and `filterBy: $sel`, over a grey ghost of the same bar without the filter, so both categories stay on the axis when the brushed rows have none missing.
- Title it with what it counts: "Jobs with a recorded time".
- **Never use a `text` mark for it.** Brightfield's text mark does not aggregate, so a "20,433 of 20,640 complete" line cannot be drawn as one; the count bar is how the figure follows the selection.

When the profile found other problems (rows that fail the schema, a column typed as something other than its name says), name them in the claim you hand over. When the claim rests on a weak column, say so in the panel's title or the claim, in your own words.

## The grid

Every dashboard rests on the grid of its table, so the user can answer their row-level questions. Brightfield draws that grid itself, for the table the panel reads, and a brush in the panel filters it. Do not draw one in the panel: Brightfield draws no Mosaic `table` input.

## The credit line

When the data's licence asks for credit (CC BY, for one), put the credit in the panel's top-level `title` text ("Notable AI models over time. Data: Epoch AI, CC BY 4.0"). Brightfield draws no `subtitle`, and a `meta.credit` key is not drawn either.

## Brightfield's limits

A composition that reaches past these draws wrongly or not at all.

- **At most eight colours.** Brightfield's palette has eight and repeats past them. Colour by a dimension of eight values or fewer, or highlight the few the claim is about and grey the rest (`panels.md`, "Past eight series").
- **At most 60 categories** on an axis. Past that, aggregate or fold the tail into `Other` in a step.
- **No facets.** `fx` and `fy` are not drawn: every row lands on one plot. Use separate plots in the `hconcat`/`vconcat` instead.
- **No `table` input.** The grid is Brightfield's, outside the panel.
- **No `text` mark for a count.** It does not aggregate.

## The checks

Before you hand over:

1. `panel_check` (`arc panel check`) is clean, and `protocol_write_file` gave no `warnings`. It checks tables and bare-string columns; it does not look inside `{ bin: … }` or `{ count: }`, so check those column names with `table_describe`.
2. The takeaway fits in one sentence, and the panel's title says it.
3. The dashboard reads without hover: every plot has a title or axis labels that say what it shows.
4. Nothing is claimed as causal that is only associated.
5. Every number in a title came from a query you ran.
6. The quality view is there for the column the claim rests on.

## One composition, worked

This is the composition for one dataset's story, not a layout to copy. The fixture holds 180 delivery jobs; exploring found that delivery time has two peaks, bikes and vans, that zone barely moves it within either, and that `minutes` is missing in some jobs. So the focal view is the distribution of `minutes`, brushed; the responders are the jobs' distances against their times, the courier types and the zones; and the quality view counts the brushed jobs with and without a time.

A step makes the one table the dashboard reads, with the quality split as a column; the run makes it; the panel is written and checked:

```sh arc-example fixture=dashboard-from-data
arc sql record "SELECT *, CASE WHEN minutes IS NULL THEN 'missing' ELSE 'complete' END AS minutes_status FROM jobs" --name jobs_dashboard --description "The jobs the dashboard reads, with whether each has a recorded time"
arc run
arc protocol write-file panels/overview.yaml <<'YAML'
title: Delivery time is two populations, bikes and vans; within each, zone barely moves it
params:
  sel: { select: crossfilter }
hconcat:
  - vconcat:
    - plot:
      - mark: rectY
        data: { from: jobs_dashboard }
        x: { bin: minutes }
        y: { count: }
        fill: "#b7b3ae"
      - mark: rectY
        data: { from: jobs_dashboard, filterBy: $sel }
        x: { bin: minutes }
        y: { count: }
      - select: intervalX
        as: $sel
      title: Minutes per delivery
      xDomain: Fixed
      xLabel: minutes
      width: 560
      height: 280
    - plot:
      - mark: dot
        data: { from: jobs_dashboard }
        x: distance_km
        y: minutes
        fill: "#b7b3ae"
      - mark: dot
        data: { from: jobs_dashboard, filterBy: $sel }
        x: distance_km
        y: minutes
      title: Distance against time
      xDomain: Fixed
      xLabel: distance_km
      yLabel: minutes
      width: 560
      height: 220
  - vconcat:
    - plot:
      - mark: barX
        data: { from: jobs_dashboard }
        x: { count: }
        y: courier_type
        sort: { y: -x, limit: 10 }
        label: count
      - select: toggleY
        as: $sel
      - select: highlight
        by: $sel
      title: Courier type
      yLabel: courier_type
      width: 320
      height: 120
      marginLeft: 90
    - plot:
      - mark: barX
        data: { from: jobs_dashboard }
        x: { count: }
        y: zone
        sort: { y: -x, limit: 10 }
        label: count
      - select: toggleY
        as: $sel
      - select: highlight
        by: $sel
      title: Zone
      yLabel: zone
      width: 320
      height: 200
      marginLeft: 90
    - plot:
      - mark: barX
        data: { from: jobs_dashboard }
        x: { count: }
        y: minutes_status
        fill: "#b7b3ae"
      - mark: barX
        data: { from: jobs_dashboard, filterBy: $sel }
        x: { count: }
        y: minutes_status
      title: Jobs with a recorded time
      yLabel: minutes_status
      width: 320
      height: 100
      marginLeft: 90
YAML
arc panel check
```

```text expect
wrote panels/overview.yaml as a panel
```

`arc panel check` printing nothing, and exiting 0, is the clean result. Over MCP, `panel_check` returns `clean` as true.
