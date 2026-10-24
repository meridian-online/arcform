# Eight more single-table operations — design

## Intent

arc's operation catalogue holds `filter-rows` and `sort-rows`. An app or an agent that wants to record a common single-table step — hide a column, rename one, derive one, summarise — has nothing to name, so it records hand-written SQL that arc cannot describe. This adds eight operations, chosen by how many common data tools offer each, so the catalogue covers the single-table work an analyst does most. Each is described, recorded and reachable exactly as the two existing ones are: `arc operation list|describe|record` and `arc mcp`'s `operation_list`, `operation_describe` and `operation_record`.

## Goals

In build order: `hide-column`, `rename-column`, `derive-column`, `cast-column`, `summarise-rows`, `dedupe-rows`, `limit-rows`, `fill-nulls`. Each:

- is one `Operation` entry in `CATALOGUE` (`src/record.rs`): long name, one-sentence summary, reads by role (a table first), writes the one step table, a closed `parameters` JSON Schema, and a `sql` function;
- is recorded as a model headed `-- generated: <operation> on <table>`, appended through local history, running nothing;
- has tests that check the exact bytes its model holds and the rows `arc run` gives on a fixture, plus refusals for its arguments.

## Non-goals

The app's side of each operation; moving the app's arcform pin; joins and unions; other tools' words for each operation; recognisers (each new operation's `recognised` is `None`); a generic JSON Schema validator.

## Settled by Hugh

The catalogue's shape, the generated header, recording through history and running nothing, recognisers optional, the eight operations and their order.

## Approaches

1. **Every argument a SQL fragment, as `where` and `order_by` are.** A column would be `"note"` typed as SQL. Simple, but a column name is then free SQL, so each one needs its own parse gate, and a name with a space needs the caller to quote it.
2. **Column names as names; clauses and expressions as SQL fragments (chosen).** An argument that names a column is a plain name, which arc writes into the SQL bare where DuckDB reads it bare and double-quoted otherwise, the same rule `from_ident` applies to a table. An argument that is a condition, an order, an expression, a select list or a type is SQL, gated by DuckDB's parser before anything is written. A name cannot break out of the SQL, and the fragments keep the reach `where` already has (any function DuckDB offers).
3. **Typed structured arguments throughout** (aggregates as `{function, column, as}` lists). Closes off what the SQL can say, which works against `derive-column`'s point — "the expression carries the function" — and needs non-string arguments the command line cannot pass today.

## The operations

The table read is written as `from_ident` writes it; a column name as `column_ident` writes it (bare when plain and DuckDB reads it bare as a column of that name, else quoted with `"` doubled). Every step is `CREATE OR REPLACE TABLE "<name>" AS` and the query below.

| Operation | Arguments | Query |
|---|---|---|
| `hide-column` | `column` (name, required) | `SELECT * EXCLUDE (<column>) FROM <t>` |
| `rename-column` | `column`, `to` (names, required) | `SELECT * RENAME (<column> AS <to>) FROM <t>` |
| `derive-column` | `column` (name of the new column), `expression` (SQL expression), both required | `SELECT *, <expression> AS <column> FROM <t>` |
| `cast-column` | `column` (name), `type` (SQL type), both required | `SELECT * REPLACE (CAST(<column> AS <type>) AS <column>) FROM <t>` |
| `summarise-rows` | `aggregates` (select list, required), `group_by` (select list, optional) | `SELECT [<group_by>, ]<aggregates> FROM <t>[ GROUP BY ALL]` |
| `dedupe-rows` | `key` (partition key) and `order_by` (order), given together or not at all | neither: `SELECT DISTINCT * FROM <t>`; both: `SELECT * FROM <t> QUALIFY row_number() OVER (PARTITION BY <key> ORDER BY <order_by>\n) = 1` |
| `limit-rows` | `count` (integer ≥ 0, required), `order_by` (order, optional) | `SELECT * FROM <t>[ ORDER BY <order_by>] LIMIT <count>` |
| `fill-nulls` | `column` (name), `value` (SQL expression), both required | `SELECT * REPLACE (coalesce(<column>, <value>) AS <column>) FROM <t>` |

Each clause starts its own line in the model, as the filter's and the sort's do.

## Gates on arguments

`Operation::admit` already checks required keys, closed schemas, JSON types and, through `x-kind`, a `condition` or an `order` as one statement. It grows:

- `x-kind: column` — a name: refused when empty. Written through `column_ident`, so no gate on its characters is needed.
- Each SQL fragment is gated in a probe that reproduces what follows it in the step, so a trailing `--` comment, a comma list or a clause that runs on is caught where it would bite. The probe ends in a sentinel alias, `__arc_probe`, which a trailing comment swallows and a comma list displaces:
  - `x-kind: expression` — `SELECT <value> AS __arc_probe` must be one statement whose tree is a bare select (no `FROM`, `WHERE`, `GROUP BY`, `HAVING`, `QUALIFY`, sample, modifiers or CTEs) of exactly one item, aliased `__arc_probe`. That refuses `1 FROM other`, `amount, note`, `amount * 2 -- double` and `a UNION SELECT b`.
  - `x-kind: select-list` — `SELECT <value>, 1 AS __arc_probe` must be a bare select of at least two items, the last aliased `__arc_probe`.
  - `x-kind: type` — `SELECT CAST(NULL AS <value>) AS __arc_probe` must be a bare select of one `CAST` item aliased `__arc_probe`.
  - `x-kind: partition` (dedupe's `key`) — passes the `select-list` probe, so a `)` that would close the window does not parse and a clause such as `ORDER BY` is not a bare select; and `SELECT row_number() OVER (PARTITION BY <value>) AS __arc_probe` must be a bare select of one item aliased `__arc_probe`, so an alias in the key, which `PARTITION BY` does not take, is refused.
  - Any fragment holding `__arc_probe`, in any case, is refused, so a value cannot supply the sentinel itself.
  - An `order` written anywhere but the statement's end (limit-rows before `LIMIT`, dedupe-rows inside `OVER`) is also checked as `SELECT 1 ORDER BY <value>` whose tree carries one `ORDER_MODIFIER` and no other modifier, so `amount LIMIT 1` is refused; and the step puts a newline after it, so a trailing comment ends there. `sort-rows`' existing gate is unchanged.
- `minimum` on an integer, and `dependentRequired` (`key` and `order_by` each require the other), each with a message naming the argument.

## Integer arguments on the command line

`--arg` gives strings. `limit-rows`' `count` is an integer, so the command line turns a `--arg` value into an integer where the operation's schema types that argument `integer` and the value parses as one; anything else is left a string and refused by `admit` as the wrong type, naming the argument. `arc mcp` passes JSON and is untouched.

## Calls made on Hugh's behalf

- Column arguments are plain names, not SQL (approach 2).
- Each `column` role stays `required: false`, as it is on the filter and the sort: the catalogue's rule is that a step records from the table and arguments alone, and where an operation needs a column its argument carries it. The role's description says which argument names it.
- `summarise-rows` uses `GROUP BY ALL`, so a grouping item may carry an alias (`date_trunc('month', at) AS month`).
- `dedupe-rows` with a key keeps one row per key through `QUALIFY row_number()`, and the key and the order are given together: a key alone would keep an arbitrary row and make the step's table differ between runs, and an order alone would be a sort.
- The row dedupe-rows keeps is fixed when the order is unique within each key; where two rows tie on the order, either may be kept. No tie-break over the whole row is added: the order is the analyst's statement of which row wins.
- A fragment with a trailing `--` comment is refused rather than rewritten: the gate cannot tell a comment from text the step would lose.
- `cast-column` uses `CAST`, so a value that does not convert fails the run, not the recording; `TRY_CAST` would hide it as a null.
- `derive-column` onto a name the table already holds is not refused at record time: arc does not open the database, so the run reports the duplicate.

## Left to the builder

Summaries and role descriptions; test names and fixtures; whether the gate helpers live beside `one_statement` or on `Clause`.
