# A join, a union and a file read in arc's operation catalogue

Date: 2026-10-25. Status: draft for approval.

## Intent

arc's operation catalogue holds ten operations, each reading one table. An analyst's work also joins tables, stacks them, and starts from a file. This adds three operations that do so, described, recorded and recognised through the same paths as the ten: `arc operation list|describe|record`, `arc sql recognise`, and `arc mcp`'s `operation_list`, `operation_describe`, `operation_record` and `sql_recognise`.

The governing rulings, all accepted by Hugh and not reopened here:

- Every input a step reads is a *role* in the description's `reads`: a name, a kind, one value or a list (with the fewest a list takes), and a sentence. No input is "the target". The node in view fills the first role; other roles are picked elsewhere (in brightfield, the spine). A step that reads nothing has an empty `reads` and starts a branch of the graph.
- The catalogue's shape, the `-- generated:` header, recording through local history, and recording running nothing stay as they are for the ten.
- A column argument is a name arc quotes; a SQL fragment is gated by DuckDB's parser in a probe that reproduces its place in the step.
- arc states what a source costs rather than refusing it.

## Goals

1. `join-tables`, `append-rows` and `read-file` in the catalogue, each with a description, SQL generation, admission checks, and tests that check the SQL recorded and the result it gives on a fixture when run.
2. A request names every read by its role, through a `reads` map, on the command line and in `arc mcp`. `on` stays as shorthand for the first role, so every existing request is unchanged.
3. A URL read in SQL is an asset of its own kind, `remote`, a node of the graph named by its URL, never hashed from disk.
4. The record answer notes when a file read is remote or outside the Protocol's folder.

## Non-goals

- Brightfield's side: the spine pick, the spine-foot row.
- Recognisers for the new operations. `sql_recognise` gains a `reads` key in its answer shape, but reads no statement as a join, union or file read.
- Multi-way joins in one step. A chain of joins is a chain of `join-tables` steps; a hand-written multi-join statement is recorded through `sql_record`, and the graph already reads every table it names.
- Operations that write a file or send a table out.
- A conditional staleness check for remote assets (ETag / Last-Modified for `https://`, the object ETag for `s3://`). Filed as its own issue; this branch's rule is that a step reading a remote asset reruns every time.

## Anti-goals

- Any change to the bytes the ten existing operations record, their headers, or the requests they accept.
- Modelling the second table as an argument (a value in `parameters` marked by `x-kind`), as `hide-column`'s column is. The 2026-09-30 ruling replaced "other tables are arguments".
- Refusing a `read-file` path for not existing, for being outside the Protocol, or for being remote.
- A remote read recorded as a file that arc then tries to hash from disk.

## Approaches

- **A. One `reads` map through the whole recording path — chosen.** `OperationRequest` and `Recording` carry a map from role name to its tables in place of the single `on`; each operation's SQL function reads its roles from that map; `on` is turned into `reads.table` at the edges. One reads check sits beside today's arguments check. The ten change only in where they get their table. It is the 2026-09-30 ruling in code: every read is a role, none is the target.
- **B. Keep `on` and add an `others` list.** A smaller diff, but it builds "a target plus arguments" into the code, and a third role (a lookup, a Splink-style list) needs another special case.
- **C. A record path of its own for each new operation.** Quickest, but breaks the property that a catalogue entry records through the CLI and `arc mcp` with no edit to either.

Hugh chose A, with the remote asset kind on this branch.

## The design

### The request

`OperationRequest` gains `reads`: a map from each role's name to one table name, or to a list of names for a list role. `on`, where given, fills the first role, `table`.

Checked against the description's `reads` the way `arguments` is checked against `parameters`, before anything is written. Refused, each naming what was wrong:

- a required table role with no value (`table` missing from both `on` and `reads`);
- a role the operation does not read;
- one value given where the role is a list, or a list where it is one;
- a list shorter than its `min_items`;
- `on` together with `reads.table`. `on` beside any other role is admitted, so `on: orders, reads: {other: customers}` records;
- `on` or any table role on an operation that reads none (`read-file`);
- a table name, in any role, that no step of the protocol makes (today's `made_by_a_step`, run for each).

Only roles of kind `table` are filled through `reads`. The `column` role of the ten is still supplied by where the operation is asked from and is not recorded; `reads.column` is refused as a role the request does not fill. *(Builder's call, see below, if a reason appears to admit it.)*

**Command line.** `arc operation record <long_name> [--on <table>] [--read <role>=<table>]… --name … [--arg …]`. `--read` repeats; repeated for a list role it appends, in order; repeated for a single role it is refused. `--on` with `--read table=` is refused. `--on` becomes optional.

**`arc mcp` `operation_record`.** Gains an optional `reads` object; `on` becomes optional. Its schema describes both and the rule between them.

**`sql_recognise` / `arc sql recognise`.** The answer gains `reads`, so it stays exactly a request `operation_record` accepts. For the ten, recognised as today, the answer is `on` plus `reads: {}`. *(Builder's call whether an empty `reads` is printed or omitted; whichever, `operation_record` must accept the answer as given, and a test holds that.)*

### The header

`-- generated: <long_name> on <t1>, <t2>, …`: every table read, in role order, a list's members in their order, role names left out. A single-table operation writes exactly today's header. An operation that reads no table writes `-- generated: <long_name>` alone. `sql_is_generated` reads only the marker, so the list is for a person reading the model. It is not a format for programs: names go in unquoted, as today, so a name holding `, ` reads ambiguously. Brightfield's reader (`brightfield-shell`'s `generated.rs`) already reads a generated step's table from its SQL template and refuses an operation it does not know, so it is unaffected now, and when it learns joins it reads the `FROM` and `JOIN` lines, not the header. *(My call.)*

### The catalogue rule

The rule that an operation reads a table first gives way to: an operation's `reads` is empty, or its first role is `table`, of kind `table`, one value, required. The description tests are rewritten to hold this. Every role's name is unique within `reads`, as today.

### `join-tables`

Summary: joins a table to another on a key, keeping the rows the join's kind keeps.

`reads`: `table` (table, one, required), `column` (column of `table`, optional, as the ten have it: the cursor's column, a candidate key), `other` (table, one, required).

`writes`: the step's table.

`parameters` (closed):

- `kind`: string, one of `left` (default), `inner`, `right`, `full`, `semi`, `anti`, `cross`.
- `using`: array of column names (each `x-kind: column`, quoted by arc), at least one; the columns both tables share by name.
- `condition`: string, `x-kind: condition`, a SQL condition naming both tables by their names; gated in a probe that reproduces its place, `… JOIN <other> ON <condition>`.
- `prefix`: string; prepended to each column of `other` in the result. Default `<other>_`. Written into a DuckDB rename pattern (`AS '<prefix>\0'`), so `'` is doubled; a prefix that is empty, holds `\` or a control character is refused; the default built from a table name has its `\` escaped as `\\` and its `'` doubled. Refused with `semi` or `anti`, where it has no effect.

Exactly one of `using` or `condition` is required for every kind but `cross`, which takes neither. Refused otherwise, naming the rule.

**What a clashing column becomes.** Recording opens no database, so arc cannot know which names clash. The rule, stated in `prefix`'s description: `table`'s columns keep their names; every column of `other` comes through prefixed, except the `using` keys, which appear once, bare, first. DuckDB 2.0, checked:

```sql
-- using
CREATE OR REPLACE TABLE "orders_customers" AS
SELECT customer_id, COLUMNS(orders.* EXCLUDE (customer_id)), COLUMNS(customers.* EXCLUDE (customer_id)) AS 'customers_\0'
FROM orders
LEFT JOIN customers USING (customer_id);

-- condition
CREATE OR REPLACE TABLE "orders_customers" AS
SELECT orders.*, COLUMNS(customers.*) AS 'customers_\0'
FROM orders
LEFT JOIN customers ON orders.customer_id = customers.id;
```

The bare `using` keys come first because `orders.*` on a right or full join gives a null key for a row only `other` holds; the bare key is DuckDB's coalesced one (checked: a full join kept key 3 bare and lost it under `orders.*`). `semi` and `anti` return `table`'s columns alone (`SELECT orders.* … SEMI JOIN …`), and take no prefix. `cross` prefixes all of `other`'s columns. Line layout above is indicative; the builder settles it, and table names go through `from_ident` as today.

**Same table on both sides.** `table` and `other` naming the same table is refused in this branch (self-joins need aliases, which the condition would have to name). *(My call; reversible.)*

### `append-rows`

Summary: stacks the rows of a table and of each other table after it, matching columns by name.

`reads`: `table` (one, required), `column` (optional, as the ten), `other` (table, list, required, `min_items: 1`). `writes`: the step's table. `parameters`: none (an empty closed schema).

```sql
CREATE OR REPLACE TABLE "all_orders" AS
SELECT * FROM orders_2024
UNION ALL BY NAME
SELECT * FROM orders_2025;
```

Columns match by name; a column one table lacks is null in its rows (checked on 2.0). Duplicate rows are kept; `dedupe-rows` drops them. A table repeated in the list is admitted (it stacks twice, as the SQL says).

### `read-file`

Summary: makes a table from a file, a glob of files or a URL, as DuckDB reads it.

`reads`: empty. `writes`: the step's table. `parameters` (closed): `path`, required, string, `x-kind: path`: a relative or absolute path, a glob, or a URL, as DuckDB's `FROM '…'` reads it.

```sql
CREATE OR REPLACE TABLE "orders" AS
SELECT *
FROM 'data/orders.csv';
```

arc writes `path` as a single-quoted literal, `'` doubled, so the only gate is that it is not empty or blank and holds no control character. The file is not required to exist: a fetch step may write it later, and DuckDB fails the run on a path that matches nothing. The format comes from DuckDB's reading of the ending, as `FROM '…'` gives.

**Notes on the record answer.** Recording returns notes alongside the model path; `arc mcp` returns them as `notes`, a list of sentences, and the command line prints each to stderr as `note: …`. Two:

- the path is outside the Protocol's folder (absolute, starting with `~`, or leaving it by `..`): the step works on this machine only;
- the path is remote (has a `scheme://` prefix): the data is read live at each run and no copy is kept; a snapshot is `http_fetch` or `opendal_fetch` followed by a `read-file` of what it wrote.

The ten and the other two record no notes; the field is an empty list.

### The `remote` asset kind

`AssetKind` gains `Remote`: a URL a step reads, named in the graph by the URL as written. Introspection marks a read path as `Remote` when it has a `scheme://` prefix (any scheme DuckDB reads through its file systems: `http`, `https`, `s3`, `gs`, `gcs`, `r2`, `az`, `azure`, `abfss`, `hf`, and any other `<scheme>://`), whether it reaches the SQL through `FROM '…'` or a reader function (`read_csv('https://…')`). A glob URL is `Remote`, not `Pattern`.

Staleness: a `Remote` asset is never hashed, and a step that reads or produces one is stale on every run, through an explicit force-stale check that holds for every kind of step (a precondition-gated `command:` step included), not by leaving the asset out of the hash. `file://` counts as remote for the graph and staleness: arc does not strip the scheme, so it cannot hash the file, and forcing the step stale is the rule that never reports a false "unchanged". `read-file`'s note for a `file://` path is the outside-folder one, not the live one. It is never checked as missing before a run. Its name in the description vocabulary and the run contract is `remote`; the role kinds the description test allows gain `remote`. Every match on `AssetKind` handles the variant: the runner's hashing and missing checks, the run contract, `protocol_files`, the operator kind names, the CLI's printing.

A URL written by hand in a manifest's `depends_on:`, `produces:` or `assets:` is `Remote` too: `default_kind_for_declared_name` gains the `scheme://` rule, so the runner never reads it from disk and the run contract falls back to the same kind. Such a step already reran every run (the URL was missing as a file); it now reruns without a missing-file warning. That is a behaviour change, with its own tests and a changelog line. `http_fetch`'s `url` stays a configuration value, not an asset: making it `Remote` would make every fetch step rerun on every run, and `fetch_cache` already handles its freshness. *(My calls.)*

The run contract's version tag (`b4/1`) is not bumped: the JSON's shape is unchanged and `kind` gains one value. Brightfield reads a kind it does not know as unknown until its own branch draws `remote`; the pull request says so. *(My call.)*

A remote path a step writes (`COPY … TO 's3://…'`) is `Remote` too, and never hashed; nothing in this branch writes one, and the graph treats it as a node like any other.

### The graph

arc learns a generated step's inputs by parsing its SQL. A join's `other` reaches the graph from the JOIN; a union's list from each `SELECT * FROM`; a file read from `FROM '…'`, as a file, pattern or remote node. No new graph code is needed for the joins and the union under the default DuckDB reader; the graph tests run under it. Under the forced fallback reader (`ARC_SQL_READER=sqlparser`), `COLUMNS(…) AS '…'` may not parse; the builder checks, and where it does not, the gap is noted in the pull request and in `docs/ARCHITECTURE.md` rather than the SQL changed. *(My call.)* tests hold that a recorded `join-tables` and `append-rows` step has an edge from every table it reads, and a `read-file` step reading a URL has a `remote` node upstream.

## Verification

Per operation, unit tests in `src/record.rs` for the SQL recorded byte for byte and for each refusal; integration tests that record each new operation into a fixture protocol, run it with `arc run`, and check the result's rows and columns: a join of each kind with `using` and with a condition, the prefix and the bare keys on a full join; a union across tables with differing columns; a `read-file` of a CSV and of a glob. The existing ten's tests stay green unchanged. A remote read is tested at the introspection and graph level (kind, no hash, always stale), not by fetching.

## Left to the builder

- Internal names and module layout, the exact line layout of the generated SQL, and the wording of each description sentence and refusal, within the shapes above.
- Whether `sql_recognise` prints an empty `reads` or omits it.
- How the CLI prints notes beyond the `note:` prefix.
- Where `reads` admission lives (a method beside `Operation::admit` is the natural place).
- How the join's `condition` is probed. Today `x-kind: condition` means a `WHERE` probe (`Clause::of`); the join's `ON` probe can run through the operation's own `checks`, leaving the shared `x-kind` as it is.

## Calls made on Hugh's behalf

- The `prefix` argument and its default `<other>_`, the bare `using` keys first, and semi/anti taking no prefix.
- `table` and `other` naming the same table is refused.
- `read-file`'s header names no table and no path.
- The record answer's notes are a `notes` list in `arc mcp` and `note:` lines on stderr.
- Any `scheme://` prefix counts as remote.
- `reads.column` is refused; the column role stays unrecorded.
- The header list is for people; programs read the SQL.
- A hand-written manifest URL is `Remote`; `http_fetch`'s `url` is not an asset.
- No run-contract version bump for the `remote` kind.
- Graph tests hold under the default reader; a fallback-reader gap is noted, not designed around.
