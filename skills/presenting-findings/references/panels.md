# Panels: the charts Brightfield draws

A panel is a Mosaic spec saved as `panels/<name>.yaml` in the Protocol folder. Brightfield finds it by that place alone, with no entry in `arcform.yaml`, and joins it to the Protocol by name: a mark's `data: { from: <table> }` names a table a step makes. Write it with `protocol_write_file` (`arc protocol write-file panels/<name>.yaml`); the content must be YAML with a `plot` key, or a `hconcat`/`vconcat` list of plots.

This page is taken from Brightfield's own sources (github.com/meridian-online/brightfield): its chart-kind registry, in `crates/brightfield-shell/src/chart_kinds.rs` and `ranked_bars.rs`, and its record of departures from Mosaic, `DEVIATIONS.md`. It says only what those files show.

## The shape every panel shares

```yaml
title: Spend by category
plot:
  - mark: barX
    data: { from: spend_by_category }
    x: total
    y: category
yLabel: category
```

- **Marks go in the list under `plot`.** Each reads its table through `data: { from: <table> }`.
- **Plot attributes are siblings of `plot`**, at its indent: `title`, `xLabel`, `yLabel`, `width`, `height`, `marginLeft`, `projectionType`. Indented one level further, under the last list entry, they parse as options of that mark or interactor and do something else, with no error.
- **A channel written as a bare string is a column** (`x: total`). An object is a transform Brightfield runs in SQL: `{ count: }`, `{ sum: amount }`, `{ bin: amount }`.
- **`title`** is the plot's title, and the line `report_write` lists the panel under.
- **A selection must be declared.** An interactor that binds `as: $sel` needs `params: { sel: { select: crossfilter } }` at the top level; without it Brightfield draws the chart and puts a banner over it saying the instruction had no effect.

## The six chart kinds

Brightfield's registry declares six kinds, in the order it prefers them when it chooses a first chart for a table. The examples below are the smallest form of each. Brightfield's own versions add interaction, declared as a crossfilter selection under `params`, and they do it two ways; add it only when the user will explore in Brightfield, and copy the way the kind uses:

- **`binned-histogram`, `scatter` and `point-map`** draw a ghost layer (the whole table in grey) under a second layer that reads `data: { from: <table>, filterBy: $sel }`, and brush with `intervalX` or `intervalXY`.
- **`ranked-category-bars` and `counts-over-time`** have one layer with no `filterBy`, and use `select: toggleY` (or `toggleX`) to publish a click and `select: highlight` to show the selection inside each bar. Brightfield's `ranked_bars.rs` warns that `filterBy` on these marks takes the total off the page and reorders the axis under the pointer.

`count-grid` adds no interaction.

| Kind | What it shows | Takes |
| --- | --- | --- |
| `binned-histogram` | A measure's distribution: rows counted per bin | One numeric column |
| `scatter` | Two measures against each other, as dots | Two numeric columns, x then y |
| `point-map` | Longitude and latitude as points on a projected map | Two numeric columns, longitude then latitude |
| `counts-over-time` | Rows counted per date, in time order | One `DATE` column |
| `count-grid` | Two categories crossed, rows counted per cell | Two category columns |
| `ranked-category-bars` | Rows counted per category, ranked by count | One category column |

The registry offers a category only up to 60 distinct values: past that, a grid or a bar list is a wall nobody can read. Aggregate or filter to fewer in a step first.

### `binned-histogram`

```yaml
title: Order amounts
plot:
  - mark: rectY
    data: { from: clean_orders }
    x: { bin: amount }
    y: { count: }
xLabel: amount
```

The bin and the count run in SQL, so the picture is of every row, not a sample.

### `scatter`

```yaml
title: Price against floor area
plot:
  - mark: dot
    data: { from: clean_listings }
    x: floor_area
    y: price
xLabel: floor_area
yLabel: price
```

A dot is drawn per row, so a large table costs what its rows cost. Aggregate first when the table is large and the question is about groups.

### `point-map`

```yaml
title: Store locations
plot:
  - mark: dot
    data: { from: clean_stores }
    x: longitude
    y: latitude
projectionType: equirectangular
```

`projectionType` is a plot attribute. With a projection the plot draws a labelled graticule instead of axes, so write no `xLabel` or `yLabel`, and no `aspectRatio`, which a projection refuses. The registry uses `equirectangular`, which puts each point where an unprojected scatter of the same columns would.

### `counts-over-time`

```yaml
title: Orders per day
plot:
  - mark: barY
    data: { from: clean_orders }
    x: order_date
    y: { count: }
xLabel: order_date
```

Write no `sort` and no `limit`: with neither, the bars come in ascending order of the date, which is time order, and no day is dropped. The column must be a `DATE`. A `TIMESTAMP` on a bar's axis puts no ink on the page; record a step that casts it to a date, or buckets it, and chart that.

### `count-grid`

```yaml
title: Orders by region and channel
plot:
  - mark: cell
    data: { from: clean_orders }
    x: region
    y: channel
    fill: { count: }
  - legend: color
```

### `ranked-category-bars`

```yaml
title: Orders by category
plot:
  - mark: barX
    data: { from: clean_orders }
    x: { count: }
    y: category
    sort: { y: -x, limit: 10 }
yLabel: category
marginLeft: 90
```

`sort: { y: -x, limit: 10 }` ranks the categories by count and keeps the top ten, Brightfield's default. `marginLeft: 90` leaves room for category names, which the default margin is sized for numbers. Never point this kind at dates: it would show the ten busiest days, ranked by count, and lose the series.

### Bars of a measure the Protocol computed

The kinds above count rows. When a step has already aggregated the answer (one row per category, with a total), chart the column itself:

```yaml
title: Spend by category
plot:
  - mark: barX
    data: { from: spend_by_category }
    x: total
    y: category
    sort: { y: -x }
yLabel: category
marginLeft: 90
```

Prefer this: the number on the chart is then the number in a table the Protocol makes, which `arc panel check` can check and the report lists.

## Past eight series

Brightfield colours categories from an eight-colour palette, ordered so that neighbouring colours stay distinct for colourblind readers. A ninth series takes the first colour again, and Brightfield puts a note on the panel saying the colours repeat. Two lines in the same colour cannot be told apart, so do not leave a chart there. Choose one of three ways out, by what the answer is about.

| The answer is about | Do this |
| --- | --- |
| One to three series out of many: the store that grew fastest, the one that shrank | Highlight them in colour and draw the rest in grey |
| The biggest few categories, where the rest only add up | Fold the tail into one `Other` category in a step |
| Every series equally, each with its own shape | Split them across panels of at most eight series each |

### Highlight one to three series and grey the rest

Colour the series by its column as usual, then write the colours yourself: `colorDomain` lists every series, the ones the answer is about first, and `colorRange` gives each a colour, in the same order. The focus series take the palette's first colours, in its order (`#0083c4` blue, `#cca124` gold, `#31aa8c` teal), and every other series takes the same grey (`#c8c8c8`). Keep the legend: it lists the focus series first, in colour, and the greys after them. Put the finding in the `title`, so the reader knows why those lines are coloured.

Here a step has loaded a year of monthly revenue for twelve stores, and the answer is that Leeds grew fastest and Hull shrank:

```sh arc-example fixture=skills-many-series
arc run
arc protocol write-file panels/store_revenue.yaml <<'YAML'
title: Leeds grew fastest and Hull shrank
plot:
  - mark: line
    data: { from: store_sales }
    x: month
    y: revenue
    stroke: store
  - legend: color
colorDomain: [Leeds, Hull, Aberdeen, Bristol, Cardiff, Derby, Exeter, Glasgow, Norwich, Oxford, Swansea, York]
colorRange: ["#0083c4", "#cca124", "#c8c8c8", "#c8c8c8", "#c8c8c8", "#c8c8c8", "#c8c8c8", "#c8c8c8", "#c8c8c8", "#c8c8c8", "#c8c8c8", "#c8c8c8"]
xLabel: month
yLabel: revenue
YAML
arc panel check
```

- **List every series in `colorDomain`.** One left out is not drawn grey: it takes the palette's first colour, the same blue as the first focus series, and looks like one. Read the series from the table (`SELECT DISTINCT store FROM store_sales`) rather than from memory.
- **`colorDomain` and `colorRange` are plot attributes**, siblings of `plot`, like `title`.
- **Do not build the grey from two layers** (a grey line of every series with `z:` under a coloured line). Brightfield does not read `z`, so the grey layer draws as one line zigzagging through every series, and the coloured layer still colours every series.

### Fold the tail into `Other`

When the answer is about the largest categories and the rest only add up (a stacked bar's `fill`, say), record a step that keeps the top seven by total and sums the rest into one `Other`, and chart that table. Seven plus `Other` is eight colours:

```sh
arc sql record "WITH ranked AS (SELECT category, total, row_number() OVER (ORDER BY total DESC) AS rank FROM spend_by_category) SELECT CASE WHEN rank <= 7 THEN category ELSE 'Other' END AS category, sum(total) AS total FROM ranked GROUP BY 1" --name spend_top_categories --description "Total spend in the seven largest categories, the rest summed as Other"
```

Say in the panel's `title` or the answer what `Other` holds: how many categories, and their share of the total.

### Split the series across panels

When every series matters equally, give each group of at most eight its own panel, each reading a table a step limits to that group (by region, or by rank: the first eight stores by revenue, then the next). Do not facet with `fx` or `fy`: Brightfield does not draw facets, and the marks fall onto one plot.

## Where Brightfield departs from Mosaic

Brightfield aims to draw any Mosaic spec, and records each place it does not in `DEVIATIONS.md`. Most change only a default; these are the ones that change what a panel you write will show.

| You write | Brightfield does |
| --- | --- |
| No `colorScheme` on a numeric colour | Uses `viridis`, not Mosaic's `turbo`. Write `colorScheme: turbo` to get turbo |
| No colour on a mark, or no categorical scheme | Uses the Meridian palette (first colour blue `#0083c4`), not Observable's |
| `colorScheme: meridian` | Brightfield's own name for the Meridian sequential ramp; Mosaic has no such scheme. Brightfield writes it out as explicit `colorRange` stops when it exports the spec |
| `colorScale: diverging` with no `colorScheme` | Blue at the low end and red at the high end, the reverse of Mosaic's `rdbu`. Write `colorScheme: rdbu` for Mosaic's colours |
| `colorScale: diverging` with no `colorPivot` | Pivots at 0 when the rows hold both negative and positive values, and at their median otherwise. Write `colorPivot` to fix it |
| `colorDomain` of two numbers on a diverging scale | Draws the ends as written, even when they are uneven about the pivot |
| `colorScale: quantize` with `colorN` | Draws exactly that many equal steps (1 to 256), where Mosaic rounds to tidy edges |
| `xScale: log` on a binned column | Bins in base 10, so the edges fall on 1, 10, 100 where the axis labels are; rows at or below zero are dropped |
| `xDomain` or `yDomain` as two numbers, low first | Fixes that axis's ends. High first (`[100, 0]`) is not read as a reversal: the key is ignored and a banner names it. Use `xReverse` to turn an axis. On an axis of dates or of names two numbers change nothing, and a banner says so |
| `xDomain: Fixed`, `yDomain: Fixed` | Read, as in Mosaic. `xyDomain: Fixed`, `fxDomain` and `fyDomain` are not: facets are not drawn |
| `fx` or `fy` on a mark | Not drawn as facets: every row lands on the one plot |
| `z` on a line | Not read: a line with no colour channel joins every series into one zigzag |
| More than eight categories on a colour | Repeats the eight palette colours and notes on the panel that colours repeat. See "Past eight series" |
| `axis`, `facetGrid`, `facetLabel`, `padding`, `align` | Not read; the chart draws as if they were absent, and a banner names them |
| `xTicks`, `xTickFormat`, `xGrid`, `grid`, `xZero`, `xNice`, `xReverse` (or the `y` ones) on a plot with `projectionType` | Not used: a projected plot has no axes. A banner names them |
| `xTickFormat` or `yTickFormat` on an axis of category names | Not used; the names print as they are |
| `stackOffset: normalize` on the plot, over a binned `rectY` with a column on `fill` | Brightfield's own key: each bin's stack is drawn as shares of that bin. Mosaic has no such option, so the panel draws differently there |
| `colorDomain: Fixed` | Pins a numeric colour scale's ends from the first render; not a colour of category names |
| A `dot` past an end that two numbers fixed | Brightfield adds, outside the frame, a count of the rows past each end |

Brightfield opens the Protocol's database read-only, so a panel can never change the data. Its rendering is not yet checked mark for mark against Mosaic's own web renderer; when a chart must also open in Mosaic, keep to the kinds above and say so to the user.
