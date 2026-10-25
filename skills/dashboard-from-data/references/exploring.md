# Exploring: finding the story

Exploration is a loop of read-only queries through `sql_query` (`arc sql query`), over the tables the Protocol has loaded, until you can state what drives the data as one claim. It records nothing and changes nothing: the steps come later, once you know which table the dashboard reads.

There is no fixed checklist. The drivers below are what to look for, not a list to tick off: each query is chosen from what the last one showed.

## The loop

1. **Start broad.** Each measure's range, median and spread; each dimension's distinct values and their counts; the time span, if there is a date. The profile from step 2 says which columns are measures, which are dimensions and which are identifiers.
2. **Follow the biggest surprise.** Two peaks, a dimension whose groups differ far more than the rest, a measure that moves with time. Query into it: group by the dimension, bin the measure, split by a segment.
3. **Go into segments.** A pattern across the whole table may vanish, or reverse, inside one group. Check the claim within the segments that matter before you trust it.
4. **Stop when no query would change the claim.** If the next query you can think of would only confirm what you have, you are done.

**The budget is four to twenty queries.** Fewer than four is a guess; past twenty, you are describing the data rather than finding its story. Say the budget to yourself at the start, and if you reach twenty without a claim, the data may have no story to tell (below).

Every number you state, in the chat or in a panel's title, comes from a query you ran. For a well-known dataset you may bring what is known about it to choose where to look ("penguin species differ in bill shape"), and say that you did; the figures still come from your queries.

## What drives a dataset

Look for these, in whatever order the data suggests.

- **A distribution with two peaks: two populations.** Bin the measure and count. A gap or a dip between two humps means two kinds of thing are mixed in one column; then find the dimension that separates them.

  ```sql
  SELECT floor(minutes / 5) * 5 AS bin, count(*) AS jobs FROM jobs GROUP BY bin ORDER BY bin
  ```

- **The dimension that splits a measure most.** For each candidate dimension, compare the measure's median across its groups. The one whose groups are furthest apart, relative to the spread within them, is the one the dashboard should let the user brush.

  ```sql
  SELECT zone, count(minutes) AS jobs, median(minutes) AS median_minutes, quantile_cont(minutes, 0.25) AS q1, quantile_cont(minutes, 0.75) AS q3 FROM jobs GROUP BY zone ORDER BY median_minutes
  ```

- **A correlation, and where it reverses.** `corr(x, y)` across the table, then within each segment. A correlation that holds overall and reverses inside every group (Simpson's paradox) is a story in itself, and a claim made on the overall figure would be wrong.

  ```sql
  SELECT courier_type, corr(distance_km, minutes) AS r, count(*) AS jobs FROM jobs GROUP BY courier_type
  ```

- **Change over time.** Count or sum by day, week or month, and look for a trend, a step, a season or a gap in the series. A gap is a quality finding as much as a story.

- **Overplotting that hides structure.** A scatter of many thousands of rows draws as one blot. Count the rows per cell of a grid over the two measures; if a few cells hold most of the rows, the dashboard wants a density view (binned counts) rather than dots.

  ```sql
  SELECT floor(distance_km / 2) * 2 AS km, floor(minutes / 10) * 10 AS mins, count(*) AS jobs FROM jobs GROUP BY km, mins ORDER BY jobs DESC LIMIT 10
  ```

Read the missing values as you go. If the column a claim would rest on is missing in many rows, or missing more in one segment than another, that belongs in the claim and in the quality view.

## Stating the claim

The claim is one sentence that names what drives the data and what does not: "Delivery time is two populations, bikes and vans; within each, zone barely moves it." It must be something the views can test by brushing: select the vans in one view and the long times light up in another.

Say what the claim is about as association, not cause, unless the data is from an experiment. "Vans take longer" is what the data shows; "vans make deliveries slow" is not.

## When there is no story

Some data has nothing to compose. Make no dashboard when the data has:

- **one usable column** (the rest are empty, constant or identifiers),
- **under about 30 rows**, too few for a distribution or a split to mean anything, or
- **only identifiers**: codes, keys, names with nothing measured about them.

Then write no panel. Hand the profile over and say why, in a sentence the user can act on: "This file is one column of 20 part codes, with nothing measured about them, so there is no dashboard to compose. Here is the profile; a file with a measure per code, such as stock or price, would give one."

Here the fixture's `codes.csv`, one column of codes, is loaded, described and profiled, and no panel is written:

```sh arc-example needs=finetype fixture=dashboard-from-data
arc sql record "SELECT * FROM read_csv('codes.csv')" --name raw_codes --description "Load the part codes as they arrive"
arc run
arc table describe raw_codes
arc table profile raw_codes
test ! -e panels
```

```text expect
"type": "representation.identifier.alphanumeric_id"
```
