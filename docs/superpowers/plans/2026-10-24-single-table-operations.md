# Eight single-table operations — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `hide-column`, `rename-column`, `derive-column`, `cast-column`, `summarise-rows`, `dedupe-rows`, `limit-rows` and `fill-nulls` to arc's operation catalogue.

**Architecture:** Each operation is one `Operation` entry in `CATALOGUE` in `src/record.rs` with a `*_parameters` schema function and a `*_sql` function, as `sort-rows` is. Argument gates grow inside `Operation::admit` keyed by `x-kind`. The command line and `arc mcp` need no per-operation code; the one command-line change is integer coercion for `limit-rows`.

**Tech Stack:** Rust 2024, the `duckdb` crate (linked DuckDB 2.0.0), serde_json.

**Spec:** `docs/superpowers/specs/2026-10-24-single-table-operations-design.md`

## Global Constraints

- Long names lower case, hyphenated, verb first; one-sentence summary (capital first, one full stop, no other `.?!`).
- Model header `-- generated: <operation> on <table>`; the step is `CREATE OR REPLACE TABLE "<name>" AS\n<query>;\n`, each clause on its own line.
- Schemas closed (`additionalProperties: false`), `$schema` draft 2020-12, as `sort_rows_parameters`.
- `recognised: None` for every new operation.
- One cargo build at a time; record tests run one at a time on macOS.
- Public repo: no planning refs or private repo names in commits, PR text or files.

## Review Focus

1. A column name holding a space, a `"` or a reserved word (`order`) — written quoted, runs. Test in Task 1.
2. A fragment that runs on into a clause of its own, carries a trailing `--` comment or is a comma list where one item is expected (`1 FROM other`, `x -- c`, `a, b`) — refused, directory untouched. Tests in Tasks 3, 5, 6 and 8.
3. A fragment holding `;` and a second statement — refused by every SQL-typed argument. Test in Task 3 (shared gate) and Task 5.
4. `limit-rows` `count` given as `ten` or `-1` on the command line — refused naming `count`. Test in Task 7.
5. An empty column name — refused naming the argument. Test in Task 1.

## Verification per task

Each task's verification is its new unit tests plus the integration tests it adds, run as:

```
# set DUCKDB_LIB_DIR, DYLD_LIBRARY_PATH, PATH to the DuckDB 2.0.0 release
cargo test --features mcp --lib record::            # ~1 min after the first build
cargo test --features mcp --test operation_record <name> -- --test-threads=1
cargo test --features mcp --test operation_catalogue --test mcp_operation_describe
cargo fmt --check && cargo clippy --features mcp --all-targets -- -D warnings
```

Expected: every test named passes; fmt and clippy are clean. CI on Linux is the full gate.

## Shared test edits (every task)

- `tests/mcp_operation_describe.rs`: the lists of operation names (`called_with_each_operator_and_operation…`, `called_with_no_argument_it_lists_no_operator`) gain the new long name in catalogue order.
- `tests/operation_catalogue.rs`: any listing assertion that names the operations gains it.
- `CHANGELOG.md` `[Unreleased]`: one line per operation under Added.

---

### Task 1: `hide-column`, with column names and `column_ident`

**Files:** Modify `src/record.rs` (catalogue, `Recording`, admit, tests); Test `tests/operation_record.rs`.

**Interfaces — produces:**
- `const HIDE_COLUMN: &str = "hide-column";`
- `fn column_ident(name: &str) -> String` — bare when the name is ASCII letters, digits, `_`, not opening with a digit, and DuckDB reads `SELECT <name>` as one `COLUMN_REF` whose `column_names` is `[name]`; otherwise `quote_ident(name)`. Same thread-local connection pattern as `from_ident`.
- `Recording::column(&self, key) -> String` = `column_ident(self.text(key))`.
- `admit`: a property with `x-kind: "column"` whose value is `""` is refused: ``the column `<key>` of `<op>` is empty``.
- Schema: `{"column": {"type":"string","x-kind":"column", description}}`, required `["column"]`.
- SQL: `SELECT * EXCLUDE (<column>)\nFROM <t>`.

- [ ] Unit tests: `column_ident` writes `amount` bare and `order`, `my col`, `a"b`, `1st` quoted; the hide schema takes one required `column` and no other key; an empty `column` is refused.
- [ ] Integration (`operation_record.rs`): `record_hide_writes_the_model_and_arc_run_drops_the_column` — model bytes are `-- generated: hide-column on orders\nCREATE OR REPLACE TABLE "no_note" AS\nSELECT * EXCLUDE (note)\nFROM orders;\n`; after `arc run`, `no_note`'s columns are `[id, amount]`. Also a column named `order` hidden from a table made with it, written quoted, runs.
- [ ] Run the verification block; commit `Add hide-column to the operation catalogue`.

### Task 2: `rename-column`

**Interfaces:** `RENAME_COLUMN`; schema `column`, `to`, both `x-kind: column`, both required. SQL `SELECT * RENAME (<column> AS <to>)\nFROM <t>`.

- [ ] Unit: schema shape; empty `to` refused.
- [ ] Integration: model bytes for `column=note`, `to=label`; after run, columns `[id, amount, label]`, values kept.
- [ ] Verify; commit.

### Task 3: `derive-column`, with the expression gate

**Interfaces — produces:**
- `fn bare_select(con_tree: &serde_json::Value) -> Option<usize>` — `Some(n)` when the tree's one statement is a `SELECT_NODE` with `from_table.type == "EMPTY"`, `where_clause`, `having`, `qualify`, `sample` null, `group_expressions`, `group_sets`, `modifiers` empty, `cte_map.map` empty; `n` = `select_list` length.
- `fn probe_items(probe: &str) -> Option<Vec<serde_json::Value>>` — the select list of `probe` when DuckDB reads it as one statement whose tree `bare_select` accepts; built on `bare_select`.
- Gate for `x-kind: "expression"` (spec, "Gates on arguments"): `SELECT <v> AS __arc_probe` gives exactly one item whose `alias` is `__arc_probe`; else refused ``the expression `<key>` of `<op>` is not one SQL expression``. A `;` with a second statement keeps the existing multi-statement wording.
- Schema `column` (x-kind column), `expression` (x-kind expression), both required. SQL: `CREATE OR REPLACE TABLE "<name>" AS\nSELECT *, <expression> AS __arc_derived\nFROM <t>;\nALTER TABLE "<name>" RENAME COLUMN __arc_derived TO <column>;\n` (spec, calls made: a clash must fail the run, not suffix). Same two-statement shape rename-column uses.

- [ ] Unit: `amount * 2`, `upper(note)`, `CASE WHEN amount > 100 THEN 'big' ELSE 'small' END`, `regexp_replace(note, 'a', 'b')` admitted; `1 FROM orders`, `amount; DROP TABLE orders`, `1) FROM orders WHERE (1`, `` refused, naming `expression`; also `amount, note`, `amount * 2 -- double` and `amount AS __arc_probe -- c` (the sentinel typed by the caller).
- [ ] Integration: model bytes for `column=doubled`, `expression=amount * 2`; after run, `doubled` is `[100, 300, 600, 200]` by id; `column=amount` records and `arc run` fails naming `amount`.
- [ ] Verify; commit.

### Task 4: `cast-column`

**Interfaces:** gate `x-kind: "type"`: `SELECT CAST(NULL AS <v>) AS __arc_probe` gives one item, class `CAST`, alias `__arc_probe`. Schema `column`, `type`. SQL `SELECT * REPLACE (CAST(<column> AS <type>) AS <column>)\nFROM <t>`.

- [ ] Unit: `VARCHAR`, `DECIMAL(10, 2)`, `INTEGER[]` admitted; `INTEGER) AS x, (1`, `INTEGER; DROP TABLE orders`, `` refused naming `type`.
- [ ] Integration: `column=amount`, `type=VARCHAR`; after run `typeof(amount)` is `VARCHAR` and values `'50','150','300','100'`.
- [ ] Verify; commit.

### Task 5: `summarise-rows`, with the select-list gate

**Interfaces:** gate `x-kind: "select-list"`: `SELECT <v>, 1 AS __arc_probe` gives at least two items, the last aliased `__arc_probe`. Each item also passes Task 3's star walk (no `STAR`/`OPERATOR_UNPACK` node outside a subquery), so `*` cannot widen a grouping into every column. Schema `aggregates` required, `group_by` optional. SQL with group: `SELECT <group_by>, <aggregates>\nFROM <t>\nGROUP BY ALL`; without: `SELECT <aggregates>\nFROM <t>`.

- [ ] Unit: `count(*) AS n, sum(amount) AS total` admitted; `count(*) FROM other`, `a UNION SELECT b`, `count(*); DROP TABLE orders`, `count(*) AS n -- all` refused naming `aggregates`; `big -- by size` refused naming `group_by`.
- [ ] Integration: fixture rows grouped by `amount > 100 AS big` with `count(*) AS n` gives `{false: 2, true: 2}`; without `group_by`, one row `n = 4`.
- [ ] Verify; commit.

### Task 6: `dedupe-rows`, with `dependentRequired`

**Interfaces:** `admit` reads `dependentRequired` (`{"key": ["order_by"], "order_by": ["key"]}`) and refuses ``…`<a>` needs `<b>` as well``. Gate `x-kind: "partition"` per the spec. `fn order_alone(key, value) -> Result<()>`: `SELECT 1 ORDER BY <v>` tree's `modifiers` is exactly one `ORDER_MODIFIER`; used by every operation whose order is not at the statement's end (here and Task 7). Schema `key` (partition), `order_by` (order), both optional. SQL as the spec's table, with a newline after the order.

- [ ] Unit: `order_by` without `key`, and `key` without `order_by`, refused; `key=note AS n`, `key=note ORDER BY amount` and `key=note) = 1 OR TRUE OR row_number() OVER (PARTITION BY note` refused; `order_by=amount LIMIT 1` refused.
- [ ] Integration: a fixture with a duplicated row: no key gives distinct rows; `key=note`, `order_by=amount desc` keeps the highest amount per note.
- [ ] Verify; commit.

### Task 7: `limit-rows`, with integer arguments on the command line

**Interfaces — produces:** `pub(crate) fn typed_arguments(long_name: &str, arguments: Map) -> Map` in `src/record.rs`: for an operation held, each string value whose property's `type` is `integer` and which parses as `i64` becomes that integer. Called in `src/cli.rs` right after `operation_arguments`. `admit` checks `minimum`; `order_by` also passes `order_alone` from Task 6, and the step puts a newline after it. Schema `count` (`integer`, `minimum: 0`, required), `order_by` (order, optional). SQL `SELECT *\nFROM <t>\n[ORDER BY <o>\n]LIMIT <count>`.

- [ ] Unit: `typed_arguments` turns `"2"` into `2` for `limit-rows` and leaves `where` a string for `filter-rows`; `-1` refused naming `count` and `minimum`.
- [ ] Integration: `--arg count=2 --arg order_by=amount desc` gives ids `[3, 2]`; `--arg count=ten` refused naming `count`; the MCP tool with `{"count": 2, …}` writes the same bytes as the terminal.
- [ ] Verify; commit.

### Task 8: `fill-nulls`

**Interfaces:** schema `column` (x-kind column), `value` (x-kind expression), both required. SQL `SELECT * REPLACE (coalesce(<column>, <value>) AS <column>)\nFROM <t>`.

- [ ] Unit: `value=0 --` and `value=0) , (1` refused naming `value`.
- [ ] Integration: a fixture with a null amount; `value=0` fills it and leaves the others.
- [ ] Verify; commit.

### Task 9: Fold and close out

- [ ] Put the catalogue's eight operations, the argument kinds and their gates into `docs/ARCHITECTURE.md`'s operation section; delete this plan and the spec; commit.
- [ ] Branch review (`roborev review --branch --wait`), fix Medium and above; checks green; Forge link; mark the PR ready.
