# Join, union and file read operations — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** arc's operation catalogue gains `join-tables`, `append-rows` and `read-file`, every read named by its role in a `reads` map, and a URL read becomes a `remote` asset.

**Architecture:** One `reads` map runs through the record path in place of the single `on` (`on` stays as shorthand for the `table` role, converted at the edges). Each new operation is a `CATALOGUE` entry in `src/record.rs` like the ten. `AssetKind` gains `Remote`, never hashed and always stale.

**Tech Stack:** Rust 2024, DuckDB 2.0.0 (linked through `DUCKDB_LIB_DIR`), serde_json, clap.

**Spec:** `docs/superpowers/specs/2026-10-25-join-union-read-file-design.md`. Read it before any task; it is the authority on behaviour, and this plan on files, names and order.

## Global Constraints

- Build and test environment, every command below: `DUCKDB_LIB_DIR`, `DYLD_LIBRARY_PATH` and `PATH` pointed at the DuckDB 2.0.0 release libraries and CLI, as CLAUDE.md's Known issues describes. Never the `bundled-duckdb` feature. One cargo build at a time.
- The ten existing operations record byte-identical models, headers and manifest lines; their existing tests are not edited except where a task names the edit.
- Recording runs nothing and opens no protocol database (the in-memory parse database only).
- Every refusal leaves the protocol directory untouched and names what was wrong.
- Column arguments are names arc quotes (`column_ident`); SQL fragments are gated by DuckDB's parser; table names go through `from_ident`.
- A public repo: no Kata refs, private repo names or local paths in commits, tracked files or PR text.
- Prose in comments and docs matches the surrounding module's style: British English, full sentences, the doc-comment density `src/record.rs` already has.
- Before each commit: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, and the task's tests.

## Review Focus

1. **A table name that needs quoting in a join or union** (`"my orders"`, a reserved word `order`): the `FROM`, `JOIN`, `COLUMNS(<t>.*)` qualifier and the union's `SELECT * FROM` all write it through `from_ident`, and the step runs. Test in Task 3 and Task 4.
2. **A `using` key or prefix that needs quoting** (`"customer id"`, a prefix holding `'`): the `EXCLUDE` list quotes the key, the `AS '…\0'` pattern doubles a `'`. Test in Task 3.
3. **`sql_recognise`'s answer fed back to `operation_record` unchanged** still records, for a recognised `filter-rows`. Test in Task 2.
4. **A `read-file` path holding `'`** (`data/o'brien.csv`) is written with the quote doubled and reads back as the path. Test in Task 5.
5. **An `https://` URL in a hand-written `depends_on:`** gives no missing-file warning and the step reruns each run. Test in Task 1.

---

### Task 1: The `remote` asset kind

**Files:**
- Modify: `src/asset_kind.rs` (enum, `default_kind_for_declared_name`, a new `is_remote`)
- Modify: `src/introspect.rs:83-89` (`read_path_kind`) and `copy_to_target_kind`
- Modify: every `match` on `AssetKind` the compiler then names: `src/runner.rs` (missing check ~1448, `is_file_like` ~1491, hashing ~1877, test helper ~7965), `src/contract.rs:637-660`, `src/protocol_files.rs:169`, `src/operator.rs:1511`, `src/cli.rs:1066`, `src/edit.rs` if it matches
- Modify: `src/record.rs` test `ROLE_KINDS` (~3659) gains `"remote"`
- Modify: `CHANGELOG.md` `[Unreleased]`
- Test: unit tests beside each change; `tests/remote_reads.rs` (new)

**Interfaces:**
- Produces: `AssetKind::Remote`; `pub fn is_remote(path: &str) -> bool` in `src/asset_kind.rs` — true when `path` starts with an ASCII-letter scheme of letters, digits, `+`, `-`, `.`, followed by `://` (so `https://`, `s3://`, `hf://`; not `C:\`, not `data/x.csv`). Task 5 uses it.
- Kind name `"remote"` wherever kinds are printed (contract, operator kind names, CLI).

- [ ] **Step 1: Write the failing unit tests**

In `src/asset_kind.rs` tests: `is_remote` true for `https://x.org/a.csv`, `s3://b/k/*.parquet`, `hf://datasets/x`; false for `data/a.csv`, `/abs/a.csv`, `C:\\a.csv`, `file.csv`, `://x`. `default_kind_for_declared_name("https://x.org/a.csv") == AssetKind::Remote` and for `"s3://b/*.parquet"` too (remote wins over pattern).

In `src/introspect.rs` tests: `SELECT * FROM 'https://x.org/a.csv'`, `SELECT * FROM read_csv('s3://b/*.csv')` and `COPY t TO 's3://b/out.parquet'` each give `kinds.get(<url>) == Some(&AssetKind::Remote)`; existing file and glob cases unchanged.

In `src/runner.rs` tests: a step whose input is declared `Remote` is stale on a second run with nothing changed, and is not reported missing.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --lib asset_kind:: introspect:: runner:: 2>&1 | tail -20`
Expected: compile error, no `AssetKind::Remote` / `is_remote`.

- [ ] **Step 3: Implement**

Add the variant with a doc comment in the style of the others ("A URL DuckDB reads through one of its file systems… Never hashed: a step reading one is stale on every run until a conditional check exists"). `is_remote` is checked first in `read_path_kind`, `copy_to_target_kind` and `default_kind_for_declared_name`. In the runner, `Remote` hashes to `None` (forcing staleness) and is never missing; in the contract its kind string is `"remote"` with no path/bytes/hash. Do not bump the contract's `b4/1` tag.

- [ ] **Step 4: Integration test `tests/remote_reads.rs`**

A protocol with one SQL step `CREATE OR REPLACE TABLE t AS SELECT * FROM 'https://example.invalid/a.csv'` and one hand-written step with `depends_on: [https://example.invalid/b.csv]` and a `command:` of `true`. Assert: `arc describe` / the asset graph JSON (whichever surface existing tests use for kinds — see `tests/table_function_lineage.rs`) lists both URLs with kind `remote`; `arc run` on the command step twice runs it both times (no `[skip`), and stderr names no missing file for the URL. Do not run the SQL step (no network).

- [ ] **Step 5: Run and pass**

Run: `cargo test --lib asset_kind:: introspect:: runner:: && cargo test --test remote_reads --test table_function_lineage --test engine_contract`
Expected: PASS. Cost: about 1 minute after a warm build.

- [ ] **Step 6: Changelog and commit**

`CHANGELOG.md` `[Unreleased]`: one entry saying a URL a step reads or writes, in SQL or written in `depends_on:`/`produces:`/`assets:`, is now a `remote` asset, never read from disk; a step reading one reruns every run, now without a missing-file warning.

```bash
git add src/asset_kind.rs src/introspect.rs src/runner.rs src/contract.rs src/protocol_files.rs src/operator.rs src/cli.rs src/edit.rs src/record.rs tests/remote_reads.rs CHANGELOG.md
git commit -m "Read a URL as a remote asset, never hashed from disk"
```

---

### Task 2: Every read named by its role

**Files:**
- Modify: `src/record.rs` — `OperationRequest`, `Recording`, `Reading`, `record_operation`, `recognise`, a new `Operation::admit_reads`, `typed_reads`, the `Operation` struct (a `notes` field), the header, the catalogue-rule test (~2150-2200)
- Modify: `src/cli.rs:476-500` (`Record` args), `:1487-1507`, `:1663-1678`
- Modify: `src/mcp/hero.rs:405-481` (`operation_record`, its schema)
- Test: `src/record.rs` unit tests; `tests/operation_record.rs`; `tests/sql_recognise.rs`

**Interfaces:**
- Produces, in `src/record.rs`:
  - `OperationRequest<'a> { long_name, on: Option<&'a str>, reads: &'a serde_json::Map<String, serde_json::Value>, name, arguments, description }` — `reads` values are a string or an array of strings.
  - `pub(crate) struct Reads(Vec<(&'static str, Vec<String>)>)` — admitted reads in role order, table roles only; `fn one(&self, role: &str) -> &str` (first value, `""` if absent) and `fn list(&self, role: &str) -> &[String]`, and `fn tables(&self) -> impl Iterator<Item = &str>` in role order.
  - `Recording<'a> { name, on: &'a str, reads: &'a Reads, arguments }` — `on` is `reads.one("table")`, kept so the ten SQL writers do not change.
  - `fn admit_reads(&self, on: Option<&str>, reads: &Map) -> Result<Reads>` on `Operation`.
  - `pub(crate) fn typed_reads(long_name: &str, pairs: &[(String, String)]) -> serde_json::Map<String, Value>` — for the CLI: a list role's values become an array (one value, an array of one); a single role given once is a string, given twice an array (which `admit_reads` then refuses naming the role).
  - `Operation.notes: Option<fn(&serde_json::Map<String, Value>) -> Vec<String>>` — `None` for the ten.
  - `pub(crate) struct Recorded { pub(crate) model: PathBuf, pub(crate) notes: Vec<String> }`; `record_operation(...) -> Result<Recorded>`.
  - `Reading` becomes `(String, Map /*reads*/, Map /*arguments*/)`; `recognise` prints `"reads"` only when non-empty (so today's answers are unchanged).
- Consumes: nothing new.

- [ ] **Step 1: Write failing unit tests in `src/record.rs`**

For a test-only catalogue entry or `filter-rows` plus a test entry reading `table` and `other` (one) and `others` (list, `min_items: 1`) — use the existing `test-op` pattern at ~2435:
- `on` alone fills `table`; `reads: {"table": "orders"}` alone does too; both refused, message names `on` and `table`.
- `on: orders, reads: {other: customers}` admitted.
- missing `table`, unknown role `nope`, `other: ["a","b"]` (list for one), `others: "a"` (one for list), `others: []` (below `min_items: 1`), `reads: {"column": "x"}` — each refused naming the role.
- a role whose value is not a string or array of strings, refused.
- header: a recording reading `orders` then `customers` writes provenance `test-op on orders, customers`; an operation with empty `reads` writes `test-op`.
- the catalogue rule, rewritten: every operation's `reads` is empty or begins with `table` (kind `table`, not a list, required); role names unique within `reads`.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --lib record:: 2>&1 | tail -20` — Expected: compile errors on the new names.

- [ ] **Step 3: Implement the record path**

`record_operation`: resolve the operation, `admit` the arguments, `admit_reads`, run `made_by_a_step` for every table in `reads.tables()`, write the SQL, provenance `"{long_name} on {tables joined by ", "}"` or `long_name` alone when no table, collect `notes` from `op.notes`. An operation with empty `reads` given `on` or any role is refused ("`<op>` reads no table").

- [ ] **Step 4: CLI**

`Record`: `on: Option<String>`; `--read ROLE=TABLE` repeatable (`reads: Vec<String>`, value name `ROLE=TABLE`), an entry without `=` refused as `--arg`'s is. Build `reads` with `typed_reads`. After recording, print the existing line to `out`, then each note to stderr as `note: <sentence>`.

- [ ] **Step 5: MCP**

`operation_record`: `on` optional, `reads` an optional object; answer gains `"notes": [...]`. Schema: `required` becomes `["operation", "name"]`; describe `reads` ("each role the operation reads, as `operation_describe` lists them, to a table name or a list of names") and that `on` is shorthand for `reads.table`.

- [ ] **Step 6: Integration tests**

In `tests/operation_record.rs`: `--read table=orders` records the same bytes as `--on orders`; `--on orders --read table=orders` refused, directory untouched; the MCP request `{on, reads: {}}` records as before and its answer carries `notes: []`. In `tests/sql_recognise.rs`: recognise a `filter-rows` statement, send its answer's `operation`, `on`, `arguments` (and `reads` if present) to `operation_record` unchanged, and it records (Review Focus 3).

- [ ] **Step 7: Run and pass**

Run: `cargo test --lib record:: && cargo test --test operation_record --test sql_recognise --test operation_catalogue --test mcp_operation_describe --test record_contract --test public_surface`
Expected: PASS, with the existing tests of the ten unedited. Cost: about 30 seconds warm.

- [ ] **Step 8: Commit**

```bash
git add src/record.rs src/cli.rs src/mcp/hero.rs tests/operation_record.rs tests/sql_recognise.rs
git commit -m "Name every table an operation reads by its role"
```

---

### Task 3: `join-tables`

**Files:**
- Modify: `src/record.rs` — `const JOIN_TABLES: &str = "join-tables"`, a `CATALOGUE` entry after `FILL_NULLS`, `join_tables_parameters`, `join_tables_sql`, `join_tables_checks`
- Modify: `tests/operation_catalogue.rs` (the listed names), `tests/mcp_operation_describe.rs` if it pins the list
- Test: `src/record.rs` unit tests; `tests/operation_record.rs` (or a new `tests/join_tables.rs` if the file passes ~100 KB)

**Interfaces:**
- Consumes: `Reads::one("other")`, `Recording`, `admit_reads` (Task 2).
- Produces: nothing later tasks use.

Description per the spec's `join-tables` section: `reads` = `table`, `column` (of `table`, optional), `other` (table, one, required); `writes` = `STEP_TABLE`; parameters `kind` (`enum` of the seven, `default: "left"`), `using` (array of strings, `minItems: 1`, `items.x-kind: column`), `condition` (`x-kind: condition`), `prefix` (string; description states the clash rule: `table`'s columns keep their names, every column of `other` comes through prefixed, default `<other>_`, except the `using` keys, which appear once and first). Closed schema.

SQL per kind, `t` = `from_ident(on)`, `o` = `from_ident(other)`, `p` = prefix with `'` doubled, `k` = `using` keys each through `column_ident`:
- `using`, kinds left/inner/right/full: `SELECT k…, COLUMNS(t.* EXCLUDE (k…)), COLUMNS(o.* EXCLUDE (k…)) AS 'p\0'\nFROM t\n<KIND> JOIN o USING (k…)`
- `condition`, kinds left/inner/right/full: `SELECT t.*, COLUMNS(o.*) AS 'p\0'\nFROM t\n<KIND> JOIN o ON <condition>`
- semi/anti (either key form): `SELECT t.*\nFROM t\n<KIND> JOIN o <USING (k…) | ON condition>`
- cross: `SELECT t.*, COLUMNS(o.*) AS 'p\0'\nFROM t\nCROSS JOIN o`

each wrapped `CREATE OR REPLACE TABLE <quote_ident(name)> AS\n…;\n`. `COLUMNS(t.*)` qualifies by the table's name as written in `FROM`; checked on DuckDB 2.0 with `"my orders"`, a reserved-word table `"order"`, a key `"customer id"` and a prefix `o's_` written `'o''s_\0'`.

`join_tables_checks`: exactly one of `using`/`condition` unless `kind` is `cross`, which takes neither; `prefix` given on semi/anti refused; `condition` probed in its place with `ends_no_statement("SELECT 1 FROM a JOIN b ON ", "condition", "condition", value)` after the shared `x-kind: condition` gate. `admit_reads` or the record path refuses `other` equal to `table` (case-insensitive, as `made_by_a_step` compares).

- [ ] **Step 1: Failing unit tests** — the description's `reads`, `writes`, `parameters` keys; the exact SQL for: left `using: ["customer_id"]`; full `using` two keys; inner `condition`; semi `condition`; anti `using`; cross; a custom `prefix: "c_"`; a table `"my orders"` and a key `"customer id"` (Review Focus 1, 2); a prefix holding `'`. Refusals: both `using` and `condition`; neither on `left`; `using` on `cross`; `prefix` on `semi`; `kind: "outer"`; `using: []`; a condition `a = b; DROP TABLE x`; `other` the same as `table`.
- [ ] **Step 2: Run, see fail** — `cargo test --lib record::tests::join 2>&1 | tail` — compile error / FAIL.
- [ ] **Step 3: Implement** the entry and its three functions.
- [ ] **Step 4: Integration test, recorded and run.** A fixture protocol with `orders(id, customer_id, name, amount)` and `customers(customer_id, name)` where one order's customer is absent and one customer has no order. Record and `arc run`, then read the result through `duckdb` as the existing `columns()` helper does: left `using` → columns `customer_id, id, name, amount, customers_name`, every order kept, the unmatched order's `customers_name` null; full `using` → the customer with no order has its `customer_id` set (not null); `condition` `orders.customer_id = customers.customer_id` → `customers_customer_id` present; semi → only orders with a customer, only `orders`' columns; anti → the other order; the recorded model's first line is `-- generated: join-tables on orders, customers`. Assert the asset graph has edges from both `orders` and `customers` to the step (use the surface `tests/table_function_lineage.rs` uses). The same request through `arc mcp` writes the same bytes.
- [ ] **Step 5: Run and pass** — `cargo test --lib record:: && cargo test --test operation_record --test operation_catalogue --test mcp_operation_describe` — PASS. About 30 s warm.
- [ ] **Step 6: Commit** — `git commit -m "Add join-tables to the operation catalogue"` with the files touched.

---

### Task 4: `append-rows`

**Files:** as Task 3, with `const APPEND_ROWS: &str = "append-rows"`, `append_rows_parameters` (empty closed schema: `properties: {}`, `additionalProperties: false`), `append_rows_sql`.

**Interfaces:** consumes `Reads::list("other")`.

`reads` = `table`, `column` (optional), `other` (table, list, required, `min_items: 1`). SQL: `CREATE OR REPLACE TABLE <name> AS\nSELECT * FROM t\nUNION ALL BY NAME\nSELECT * FROM o1\n[UNION ALL BY NAME\nSELECT * FROM o2 …];\n`, each through `from_ident`. A table repeated in `other`, or equal to `table`, is admitted.

- [ ] **Step 1: Failing unit tests** — description; SQL for one and for three others, in order; a quoted name; `other: []` refused; `other: "x"` (one for a list) refused through the MCP shape.
- [ ] **Step 2: Run, see fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Integration test** — `orders_2024(id, amount)` and `orders_2025(id, amount, channel)`, a duplicate row across them; record with `--on orders_2024 --read other=orders_2025`, run: row count is the sum, `channel` null for 2024's rows, the duplicate kept twice; header `-- generated: append-rows on orders_2024, orders_2025`; graph edges from both. A three-table union through `arc mcp` with `reads: {other: [b, c]}` writes the members in that order.
- [ ] **Step 5: Run and pass** — as Task 3.
- [ ] **Step 6: Commit** — `git commit -m "Add append-rows to the operation catalogue"`.

---

### Task 5: `read-file`

**Files:** as Task 3, with `const READ_FILE: &str = "read-file"`, `read_file_parameters`, `read_file_sql`, `read_file_notes`; the `admit` branch for a new `x-kind: "path"` (not empty or blank, no control character).

**Interfaces:** consumes `Operation.notes`, `Recorded.notes` (Task 2), `asset_kind::is_remote` (Task 1).

`reads` = `[]`; `writes` = `STEP_TABLE`; parameter `path` (required string, `x-kind: path`, description per spec). SQL: `CREATE OR REPLACE TABLE <name> AS\nSELECT *\nFROM '<path with ' doubled>';\n`. Header `-- generated: read-file`. `read_file_notes`: when `is_remote(path)`, the sentence "`<path>` is read live at each run and no copy is kept; for a snapshot, fetch it with `http_fetch` or `opendal_fetch` and read the file that writes."; else when the path is absolute or its normalised components leave the folder through `..`, "`<path>` is outside the Protocol's folder, so the step works on this machine only." Wording may be tightened; the two conditions may not change.

- [ ] **Step 1: Failing unit tests** — description has `reads: []`; SQL for `data/orders.csv`, `data/*.csv`, `https://x.org/a.parquet`, `data/o'brien.csv` (Review Focus 4); notes: none for `data/a.csv` and `./data/a.csv`, outside for `/tmp/a.csv` and `../a.csv` and `data/../../a.csv`, remote for `s3://b/k.csv`; refusals: `path: ""`, `path: "  "`, a path holding `\n`, `on` given, `reads: {table: x}` given.
- [ ] **Step 2: Run, see fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Integration test** — a protocol with no steps but a `data/orders.csv` and `data/2024.csv`/`data/2025.csv`: record `read-file --name orders --arg path=data/orders.csv` and a glob; run; row counts match the files; the model's header is `-- generated: read-file`; a path to a file that does not exist records (no refusal) and `arc run` fails naming it; `--arg path=https://example.invalid/a.csv` records, stderr carries `note:` naming it as live, and the graph shows a `remote` node upstream of the step; through `arc mcp` the same remote request answers `notes` with one sentence.
- [ ] **Step 5: Run and pass** — as Task 3, plus `--test remote_reads`.
- [ ] **Step 6: Commit** — `git commit -m "Add read-file to the operation catalogue"`.

---

### Task 6: Fold the design into the docs

**Files:**
- Modify: `docs/ARCHITECTURE.md` — the `CATALOGUE` paragraph (~line 75): the thirteen operations, the `reads` map and `on` shorthand, the header listing every table, the join's column rule, the union's `BY NAME`, `read-file`'s notes; the asset-kind section: `remote`.
- Modify: `CHANGELOG.md` `[Unreleased]`: one entry per operation, in the style of the `fill-nulls` entry, and one for `--read`/`reads`.
- Delete: `docs/superpowers/specs/2026-10-25-join-union-read-file-design.md`, `docs/superpowers/plans/2026-10-25-join-union-read-file.md`.

- [ ] **Step 1: Write the doc changes.**
- [ ] **Step 2: Check the fallback reader.** Run `ARC_SQL_READER=sqlparser cargo test --test operation_record join` (or the join test's name). If it fails because sqlparser cannot read `COLUMNS(…) AS '…'`, note the gap in `docs/ARCHITECTURE.md`'s SQL-reader section and in the PR; do not change the SQL.
- [ ] **Step 3: Run the docs checks CI runs** — `cargo test --test public_surface --test skills` — PASS.
- [ ] **Step 4: Commit** — `git commit -m "Describe the join, union and file read in the architecture notes"`, including the two deletions.
