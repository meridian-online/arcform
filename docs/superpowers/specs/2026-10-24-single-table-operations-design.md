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
| `dedupe-rows` | `key` (select list, optional), `order_by` (order, optional, needs `key`) | no key: `SELECT DISTINCT * FROM <t>`; key: `SELECT * FROM <t> QUALIFY row_number() OVER (PARTITION BY <key>[ ORDER BY <order_by>]) = 1` |
| `limit-rows` | `count` (integer ≥ 0, required), `order_by` (order, optional) | `SELECT * FROM <t>[ ORDER BY <order_by>] LIMIT <count>` |
| `fill-nulls` | `column` (name), `value` (SQL expression), both required | `SELECT * REPLACE (coalesce(<column>, <value>) AS <column>) FROM <t>` |

Each clause starts its own line in the model, as the filter's and the sort's do.

## Gates on arguments

`Operation::admit` already checks required keys, closed schemas, JSON types and, through `x-kind`, a `condition` or an `order` as one statement. It grows:

- `x-kind: column` — a name: refused when empty. Written through `column_ident`, so no gate on its characters is needed.
- `x-kind: expression` — one SQL expression: DuckDB must read `SELECT (<value>)` as one statement whose tree is a bare select of one expression (no `FROM`, `WHERE`, `GROUP BY`, `HAVING`, `QUALIFY`, modifiers or CTEs). The parentheses stop a value that runs on into a clause of its own (`1 FROM other`).
- `x-kind: select-list` — `SELECT <value>` must be one statement with that same bare shape and at least one item. That refuses `count(*) FROM other` and `a UNION SELECT b`.
- `x-kind: type` — `SELECT CAST(NULL AS <value>)` must be one statement of that bare shape with one item.
- `minimum` on an integer, and `dependentRequired` (`order_by` needs `key`), each with a message naming the argument.

## Integer arguments on the command line

`--arg` gives strings. `limit-rows`' `count` is an integer, so the command line turns a `--arg` value into an integer where the operation's schema types that argument `integer` and the value parses as one; anything else is left a string and refused by `admit` as the wrong type, naming the argument. `arc mcp` passes JSON and is untouched.

## Calls made on Hugh's behalf

- Column arguments are plain names, not SQL (approach 2).
- Each `column` role stays `required: false`, as it is on the filter and the sort: the catalogue's rule is that a step records from the table and arguments alone, and where an operation needs a column its argument carries it. The role's description says which argument names it.
- `summarise-rows` uses `GROUP BY ALL`, so a grouping item may carry an alias (`date_trunc('month', at) AS month`).
- `dedupe-rows` with a key keeps one row per key through `QUALIFY row_number()`, and an `order_by` without a `key` is refused rather than read as a sort.
- `cast-column` uses `CAST`, so a value that does not convert fails the run, not the recording; `TRY_CAST` would hide it as a null.
- `derive-column` onto a name the table already holds is not refused at record time: arc does not open the database, so the run reports the duplicate.

## Left to the builder

Summaries and role descriptions; test names and fixtures; whether the gate helpers live beside `one_statement` or on `Clause`.
