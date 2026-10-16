# Reading the results

What a profile, a validation report and a description tell you, and what each finding means for the user's question. Report findings in the user's terms ("12 orders have no date, so they drop out of the monthly totals"), not FineType's ("`order_date` validation_pass_rate 0.94").

## A profile, column by column

`table_profile` (`arc table profile`) and `profile` (`finetype profile`) give one entry per column. The fields that matter:

| Field | What it says | What to do with it |
| --- | --- | --- |
| `type` | FineType's semantic label, `domain.category.type` (`identity.person.email`, `datetime.date.iso`) | Check it against what the question needs the column to be. A label you did not expect is the first finding to report |
| `broad_type` | The storage type the label maps to (`DATE`, `BIGINT`, `DOUBLE`, `VARCHAR`) | The type the build should cast the column to |
| `transform` | The DuckDB expression that casts a value to that type, with `{col}` for the column | Use it in the build's cleaning step rather than writing your own cast |
| `confidence` and `quality_band` | How sure FineType is (0 to 1), and the band it falls in (`high`, `medium`, `low`) | A quality signal about the data, not an error in FineType. See below |
| `validation_pass_rate` | The share of sampled values that pass the label's own validation | Below 1.0 means some values do not look like the rest |
| `null`, `non_null` | Empty values in the sample | Rows the question may silently lose |
| `disambiguation_rule`, `runner_up`, `vetoed_type` | Why FineType chose this label over another, and the next best | Read these when the label surprises you |
| `is_generic` | The label is a generic one (a plain number or word) rather than a specific format | Fine for a measure; worth saying when the question needs a specific meaning (a currency, a code) |
| `x-finetype-enum` | A column with few distinct values, and the values | A closed set: check it holds the categories the question expects, and no stray spellings |
| `rows_profiled` (tables only) | How many rows were copied and profiled | The profile saw only these. A table larger than this was sampled from its first rows |

## Confidence as a quality signal

Confidence measures how well the sampled values agree on one type. High confidence on the expected type means the column is consistent. Low confidence has two causes, and they lead to different findings:

- **Dirty values.** Most values fit the label and some do not: a date column with `not known` in it, an amount with a stray currency symbol. Validation will reject those rows. Report how many, and propose how the build treats them (drop, null, or fix).
- **A generic or ambiguous column.** Every value is clean, but the column fits several labels: a bare number could be an amount, a count or a key. The `disambiguation_rule` says so (`amount_bare_number_veto` means a plain number FineType declined to call money). This is not dirty data; say what the column is from the question's side and carry on.

For example, an `amount` column of plain decimals (`5.00`, `8.70`) is the second case: every value is clean, typed `representation.numeric.decimal_number` at 60%, because nothing marks it as money. Summing it is sound.

When a label is wrong rather than low (a code typed as a number, which would drop leading zeros), say so, and plan the build to keep the column as text.

## The validation report

Validation checks every row against the saved schema, not a sample.

- **`validate` (`finetype validate`)** gives `total_rows`, `valid_rows`, `invalid_rows`, `rejects` (a row can break more than one column), `rejects_by_type` and a `grade` from A to F. It exits 1 when any row is rejected. It counts rejects but does not list them.
- **`table_validate` (`arc table validate`)** gives `valid` (true when nothing is rejected) and, for each column, `total`, `rejects` and a `sample_message` showing why a value failed. `arc table validate` also exits 1 on any reject.

Reading the size of the problem:

| Rejected rows | What it usually means | What to tell the user |
| --- | --- | --- |
| None | The data meets its contract | Say so in one line |
| A few percent, in one or two columns | Dirty values | How many rows, which columns, and how the build will treat them |
| Many, across most columns | The schema does not fit the data: a wrong delimiter or encoding, a header row in the middle, or a sample that missed most of the data | Check the load and the sample size before calling the data bad |

`rejects_by_type` splits `SEMANTIC_TYPE` (a value does not look like its label) from `TRANSFORM_FAILED` (it looked right but would not cast, such as `2024-02-30`). Both are rows the analysis would lose or mis-read.

To see which values fail, query the table read-only with `sql_query` (`arc sql query`) for the column's values that the label's pattern or cast rejects. Do not fix them there: fixing is a build step.

## Nulls

`table_describe` (`arc table describe`) gives each column's `null_count` over the whole table, and its `row_count`. Compare the nulls in the columns the question rests on with the row count:

- A key or a date with nulls means rows that drop out of a join or a time series. Say how many.
- A measure with nulls means totals that undercount, and averages over fewer rows than the user thinks.
- A column that is mostly null cannot carry the question. That goes back to the user before anything is built.

The saved schema lists as `required` the columns that had no nulls in the sample; a later load with nulls there will fail validation, which is the point of saving it.

## Mixed types

A column that holds two kinds of value (numbers and `n/a`, dates in two formats, codes with and without leading zeros) shows as low confidence, a `runner_up`, and rejected rows on validation. Name the two kinds and how many of each, and say which the question needs. The build then makes one conformed column from it, as its own step.

## What the user hears

The checkpoint is three or four lines:

1. What the data is: rows, and the columns the question rests on with their types.
2. What is wrong with it, in numbers: rejected rows, nulls, mixed columns, and which of those touch the question.
3. What the build will do about each, and anything the user must decide.
4. Anything that changes what the question can answer.
