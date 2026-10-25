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

**Colour.** For a magnitude, the sequential default is one hue running light to dark, or `viridis`; never a rainbow such as `turbo`. A ramp whose dark end matches a dark background loses its top values: look at the dark-mode thumbnail before you hand over, and change the scheme if the largest values vanish. Colour a dimension of eight values or fewer by identity (see the limits below).

You decide which views drive and which respond. Usually the focal view drives; when the claim is about a category ("vans are the slow ones"), the bars of that category may lead and the distribution respond.

## The data's quality, beside the views

The quality view says how far to trust what the other views select. It is a count bar over the same table and the same selection, splitting the rows that are complete from those missing the column the claim rests on: with nothing selected it shows the whole table, and when the user clicks a category or brushes another column it shows how many of the selected rows are missing that value.

**A brush on the claim's own column cannot select its missing rows.** A range brush on `minutes` keeps the rows whose `minutes` falls inside the range, and a null falls inside no range, so while the focal view is brushed the quality view shows none missing. That is true of the brushed rows, not a finding about the data: name in the quality view's title the selections it answers for, not the jobs the focal view selects, and do not read the brushed view as the data being complete. The view answers for the selections the other views make (click "van": how many van jobs have no time).

Mosaic's way round this is a second selection that the focal brush does not feed, with the quality view reading that one. Brightfield implements the `crossfilter`, `intersect`, `union` and `single` selections, but feeding one click into two selections takes two interactors on one plot, and none of Brightfield's own examples does that. Do not rely on it: keep one selection, and word the quality view as the missing values among the rows the other views select.

- Record the split as a column of the table the dashboard reads (`CASE WHEN minutes IS NULL THEN 'missing' ELSE 'complete' END AS minutes_status`), since a bare-string channel must be a column.
- Draw it as `mark: barX` with `x: { count: }`, `y:` the status column, and `filterBy: $sel`, over a grey ghost of the same bar without the filter, so both categories stay on the axis when the brushed rows have none missing.
- Title it with what it counts and whose selection it follows: "Recorded time, among the courier types and zones clicked".
- **Never use a `text` mark for it.** Brightfield's text mark does not aggregate, so a "20,433 of 20,640 complete" line cannot be drawn as one; the count bar is how the figure follows the selection.

**When the claim's columns are complete, there is nothing to split.** A complete/missing bar with one bar in it is a flat view, so drop it like any other. Put in its place the view of the weakness that bears most on the claim, over the same table and the same selection. Look for it in the profile and the exploring queries: a measure on mixed scales or units, rows that fail the schema, a threshold that is read inconsistently. Its title says what it shows, and the selection it follows. Record the grouping as a column of the table, as for the missing-value split.

For example, a count of earthquakes above a magnitude-4.5 floor had no missing magnitudes, but 81% of the magnitudes were body-wave (`mb`) and the rest on other scales, so the floor is read on mixed scales. Its quality view was a bar of the count by magnitude scale, titled "Magnitude scale of the quakes selected: the 4.5 floor is read on mixed scales". That is the weakness in that data, not a view to add to yours.

When the profile found other problems (rows that fail the schema, a column typed as something other than its name says), name them in the claim you hand over. When the claim rests on a weak column, say so in the focal plot's title or the claim, in your own words.

## The grid

Every dashboard rests on the grid of its table, so the user can answer their row-level questions. Brightfield draws that grid itself, for the table the panel reads, and a brush in the panel filters it. Do not draw one in the panel: Brightfield draws no Mosaic `table` input.

## The claim and the credit, in a drawn title

Brightfield does not draw a panel's top-level `title`: not in the chart document, not in the tiles, not in `export-png`. Only the PDF report uses it, as a caption. A title that is drawn is a plot's own `title`, so:

- Keep the claim in the top-level `title`, for Mosaic and for the report's caption.
- Put the claim as the focal plot's `title` as well, so the person looking at the dashboard, or at its exported picture, reads it.
- When the data's licence asks for credit (CC BY, for one), append the credit line to the focal plot's `title` ("Notable AI models over time. Data: Epoch AI, CC BY 4.0"). Only a drawn title reaches an exported picture, so a credit anywhere else is lost there. Brightfield draws no `subtitle`, and a `meta.credit` key is not drawn either.

## Brightfield's limits

A composition that reaches past these draws wrongly or not at all.

- **At most eight colours.** Brightfield's palette has eight and repeats past them. Colour by a dimension of eight values or fewer, or highlight the few the claim is about and grey the rest (`panels.md`, "Past eight series").
- **At most 60 categories** on an axis. Past that, aggregate or fold the tail into `Other` in a step.
- **No facets.** `fx` and `fy` are not drawn: every row lands on one plot. Use separate plots in the `hconcat`/`vconcat` instead.
- **No `table` input.** The grid is Brightfield's, outside the panel.
- **No `text` mark for a count.** It does not aggregate.
- **No hexbin through a map projection.** For a dense point map, draw a hexbin over plain longitude and latitude, which is an equirectangular map, and label the axes `longitude` and `latitude`.
- **`opacity` and `r` on dots are not drawn.** Overplotting cannot be thinned by opacity or sized by radius: bin it (a hexbin) or colour it.
- **No log, sqrt or symlog colour scale.** The colour of a skewed count is linear. See "Colour for a skewed count" below.
- **`colorScale: quantize` on a hexbin fill does not match its legend, and `colorLabel` is not drawn.** Do not use either; say in the plot's title what the colour counts.

### Colour for a skewed count

When most bins hold a few rows and a handful hold very many, a linear ramp spends its whole range on the handful. Use a sequential scheme (`colorScheme: viridis`, below) and clamp the ramp with a numeric `colorDomain` whose upper end is a percentile you queried (`quantile_cont(n, 0.99)` over the bin counts, say), so the top colour means "this many or more". The legend cannot carry the cap, so state it in the plot's title: "(colour capped at 110 a hexagon)".

## The checks

Before you hand over:

1. `panel_check` (`arc panel check`) is clean, and `protocol_write_file` gave no `warnings`. It checks tables and bare-string columns; it does not look inside `{ bin: … }` or `{ count: }`, so check those column names with `table_describe`.
2. The takeaway fits in one sentence, and the top-level `title` and the focal plot's `title` say it. For CC BY data, the focal plot's `title` also carries the credit.
3. The dashboard reads without hover: every plot has a title or axis labels that say what it shows.
4. Nothing is claimed as causal that is only associated.
5. Every number in a title came from a query you ran.
6. The quality view is there for the column the claim rests on, or, when those columns are complete, for the weakness that bears most on the claim.

## One composition, worked

This is the composition for one dataset's story, not a layout to copy. The fixture holds 180 delivery jobs; exploring found that delivery time has two peaks, bikes and vans, and that `minutes` is missing in 9 jobs. The bikes' and vans' median times were 14 and 47 minutes, while within each type the zone medians ran from 12.5 to 15.5 minutes for bikes and from 42.5 to 52 for vans, so courier type matters far more than zone. So the focal view is the distribution of `minutes`, brushed; the responders are the jobs' distances against their times, the courier types and the zones; and the quality view counts the jobs the other views select with and without a time.

A step makes the one table the dashboard reads, with the quality split as a column; the run makes it; the panel is written and checked:

```sh arc-example fixture=dashboard-from-data
arc sql record "SELECT *, CASE WHEN minutes IS NULL THEN 'missing' ELSE 'complete' END AS minutes_status FROM jobs" --name jobs_dashboard --description "The jobs the dashboard reads, with whether each has a recorded time"
arc run
arc protocol write-file panels/overview.yaml <<'YAML'
title: Delivery time is two populations, bikes and vans; courier type matters far more than zone
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
      title: Delivery time is two populations, bikes and vans; courier type matters far more than zone
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
      title: Recorded time, among the courier types and zones clicked
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
