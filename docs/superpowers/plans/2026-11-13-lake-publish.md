# Lake publish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `ducklake_write@1` step that ends a Protocol by upserting its tables into a DuckLake catalogue stored as one file on R2, as one snapshot, with descriptors in a `meta` schema, a readable download copy per table, a local fallback that stays green, and `arc run --publish` to require the live publish.

**Architecture:** A new `src/lake/` module holds the parts with no operator plumbing: an object-store seam (a filesystem store for tests and an S3 store for R2), descriptor-to-`meta` rows, the DuckDB commit, and the publish sequence (fetch catalogue, commit, upload files, conditional catalogue write, retry). `src/operator/ducklake_write.rs` parses the step, picks live or local, and reports. The runner passes `--publish` and the run id to operators through the step environment and uploads the run record after a live publish. `arc lake rebase` builds the open lake's new catalogue from the old one once.

**Tech Stack:** Rust 2024, the in-process `duckdb` crate (DuckDB 2.0.0, DuckLake 1.1), `ureq` (already a dependency) for HTTP, `rusty-s3` for S3 request signing, `sha2`, `serde_json`.

**Spec:** `docs/superpowers/specs/2026-11-13-lake-publish-design.md`

## Global Constraints

- DuckDB is pinned `duckdb = "=1.20000.0"` (DuckDB v2.0.0). Do not change it.
- New catalogues use DuckLake's current format (1.1 on DuckDB 2.0); do not pass `DUCKLAKE_VERSION`. Readers need DuckDB 2.0 or later.
- Every lake write goes into the local copy of the catalogue file, attached with `OVERRIDE_DATA_PATH true` to the lake's write path: the same objects the persisted https data path names, addressed so DuckDB can read and write them (`s3://<bucket>/<data_prefix>` live, a local folder in tests and for the local lake). Never write through https, and never override to a staging folder (DuckLake would look for existing files there).
- One snapshot per publish for all of a step's tables. An unchanged publish commits no snapshot.
- Upsert is `UPDATE` + `DELETE` + `INSERT` in one transaction, never `MERGE INTO` (DuckLake supports only one UPDATE/DELETE action in a MERGE).
- `meta` is a reserved schema name: a table may not be published into it.
- Credentials never appear in a report, a log line, an error message, or the run record.
- The new code is compiled under the `http-fetch` feature (which `cli` pulls in), like `http_fetch`.
- The repo is public: no local paths, private repo names or planning ids in code, tests, docs or commit messages.
- No write to the production R2 bucket without Hugh's explicit go-ahead in the session.
- Build env for every command below: `export DUCKDB_LIB_DIR=$DUCKDB_DIR DYLD_LIBRARY_PATH=$DUCKDB_DIR PATH=$DUCKDB_DIR:$PATH`, where `$DUCKDB_DIR` holds DuckDB v2.0.0's release library and CLI (see CLAUDE.md, Known issues). One cargo build at a time.
- Before each commit: `cargo fmt --all --check` and `cargo clippy --all-targets -- -D warnings`, plus the task's tests.

## Review Focus

1. **A failure partway through a live publish** (an upload refused, the network dropping) must leave the live lake exactly as it was: the catalogue object is written last, so at worst orphan data files remain. Test in Task 4.
2. **Column names that need quoting** (`order`, `Code Display`, mixed case) must publish and upsert correctly. Test in Task 3.
3. **Duplicate or NULL key values in the input** must fail the step, naming the key and an example value, rather than upsert unpredictably. Test in Task 3.
4. **A descriptor that disagrees with the Parquet** (names a column the file lacks, or misses one it has) must fail the step naming the columns, before anything is committed. Test in Task 2.
5. **A publish interrupted after the catalogue write but before the manifest write** must be repaired by the next publish, even when the data is unchanged: an `unchanged` publish still rewrites a manifest entry that is behind. Test in Task 5.

---

### Task 1: The object-store seam

**Files:**
- Create: `src/lake/mod.rs`, `src/lake/store.rs`
- Modify: `src/lib.rs` (declare `mod lake` under `#[cfg(feature = "http-fetch")]`), `Cargo.toml` (add `rusty-s3`, optional, to the `http-fetch` feature)
- Test: unit tests in `src/lake/store.rs`

**Interfaces:**
- Produces:
  - `pub struct ETag(pub String)` (opaque, compared as given by the store)
  - `pub enum PutCondition { Any, IfMatch(ETag), IfAbsent }`
  - `pub enum StoreError { PreconditionFailed, Throttled, NotFound, Other(String) }` with `Display` that never includes credentials
  - `pub trait ObjectStore: Send + Sync { fn get(&self, key: &str) -> Result<Option<(Vec<u8>, ETag)>, StoreError>; fn put(&self, key: &str, bytes: &[u8], cond: PutCondition, cache_control: Option<&str>) -> Result<ETag, StoreError>; fn put_file(&self, key: &str, path: &Path, cond: PutCondition, cache_control: Option<&str>) -> Result<ETag, StoreError>; fn etag(&self, key: &str) -> Result<Option<ETag>, StoreError> /* metadata only: DirStore hashes the existing file, S3Store a signed HEAD (404 → None), PublicHttpStore an unsigned HEAD */; }`
  - `pub struct DirStore { root: PathBuf }` (`DirStore::new(root)`): keys are paths under `root`; the ETag is the SHA-256 of the bytes; `IfMatch` and `IfAbsent` are checked and the write is made atomic with a lock file plus rename, so two threads racing on one key give exactly one winner.
  - `pub struct S3Store` (`S3Store::r2(account_id, bucket, key_id, secret)`, `S3Store::new(endpoint_url, bucket, region, key_id, secret)`): signs requests with `rusty-s3`, sends them with `ureq`. `IfMatch` sends `If-Match`, `IfAbsent` sends `If-None-Match: *`, both as signed headers. 412 maps to `PreconditionFailed`, 429 to `Throttled`, 404 on get to `Ok(None)`.
  - `pub struct PublicHttpStore { base_url }`: read-only `get` over plain https (no credentials), `put` returns `StoreError::Other("read-only")`. Used by `arc lake rebase` (Task 7) to read a catalogue given by URL.

- [ ] **Step 1: Write the failing tests**

  - `dir_store_round_trips_and_reports_etag`: put then get returns the bytes and the same ETag.
  - `if_match_refuses_a_stale_etag`: put A, put B, then put C `IfMatch(etag_of_A)` → `Err(PreconditionFailed)`, object still B.
  - `if_absent_refuses_an_existing_key`.
  - `racing_conditional_writes_have_one_winner`: 8 threads each `get` then `put IfMatch` on one key; exactly one `Ok`.
  - `s3_store_signs_conditional_headers`: build the PUT request an `S3Store::r2("acct", "bucket", "k", "s")` would send for `IfMatch(ETag("\"abc\""))` (expose `fn request(&self, method, key, cond) -> PreparedRequest { url, headers }` as `pub(crate)` for this) and assert the URL host is `acct.r2.cloudflarestorage.com`, `if-match` is among the signed headers, and the secret appears nowhere in the URL or headers.
  - `s3_status_codes_map_to_store_errors`: `fn map_status(u16) -> Option<StoreError>` gives 412 → `PreconditionFailed`, 429 → `Throttled`.
  - `a_transport_error_names_no_secret`: a request to a closed port on `127.0.0.1` (via `S3Store::new("http://127.0.0.1:<closed>", …)`) fails with `StoreError::Other` whose `Display` names the object key and contains neither the key id, the secret, `X-Amz-Signature` nor `X-Amz-Credential`. `S3Store` errors carry the HTTP status and the object key only, never a request URL.

- [ ] **Step 2: Run them and see them fail**

  Run: `cargo test --lib lake::store`
  Expected: compile errors, `lake` not found.

- [ ] **Step 3: Implement the module.** If `rusty-s3` cannot sign `If-Match` as a header, sign the request with SigV4 by hand using `sha2` and an HMAC built from it; the test above decides.

- [ ] **Step 4: Run the tests and see them pass**

  Run: `cargo test --lib lake::store`
  Expected: 7 passed.

- [ ] **Step 5: Commit** `Add an object-store seam for publishing a lake to S3-compatible storage`

### Task 2: Descriptors as `meta` rows

**Files:**
- Create: `src/lake/meta.rs`
- Test: unit tests in `src/lake/meta.rs`

**Interfaces:**
- Produces:
  - `pub struct TableDescriptor { pub table_name: String, pub descriptor: serde_json::Value, pub fields: Vec<FieldRow>, pub key: Vec<String>, pub table_comment: Option<String> }`
  - `pub struct FieldRow { pub column_name: String, pub ordinal: i32, pub r#type: String, pub finetype_label: Option<String>, pub description: Option<String>, pub constraints: serde_json::Value, pub is_key: bool }`
  - `pub fn table_descriptor(table_name: &str, datapackage: &serde_json::Value, parquet_columns: &[String]) -> Result<TableDescriptor>`: takes the package's single resource (fails if there is not exactly one); strips the resource's `bytes` and `hash` and the package's `created` from the stored descriptor; `key` is `schema.primaryKey` (string or array); `finetype_label` is the field's `x-finetype-label`; `table_comment` is the package's `description`. Fails, naming both lists, when the descriptor's field names and `parquet_columns` differ as sets.
  - `pub const META_SCHEMA: &str = "meta";`

- [ ] **Step 1: Write the failing tests**, using a fixture `tests/fixtures/lake/soc_sample.datapackage.json` cut down from a real SOC descriptor (three fields, `primaryKey: ["code"]`, one field with `x-finetype-label` and a `pattern` constraint, a resource carrying `bytes` and `hash`, a package `created`):
  - `fields_come_from_the_descriptor_in_order`: three rows, ordinals 0..2, `code` has `is_key = true`, label `representation.identifier.numeric_code`, `constraints["pattern"] == "^[0-9]{2}-[0-9]{4}$"`.
  - `the_stored_descriptor_drops_bytes_hash_and_created`.
  - `a_string_primary_key_is_one_column`.
  - `a_descriptor_naming_a_missing_column_is_refused`: error message contains both the descriptor-only and the file-only column names.
  - `a_package_with_two_resources_is_refused`.

- [ ] **Step 2: Run them and see them fail**: `cargo test --lib lake::meta` → compile error.

- [ ] **Step 3: Implement `table_descriptor`.**

- [ ] **Step 4: Run them and see them pass**: `cargo test --lib lake::meta` → 5 passed.

- [ ] **Step 5: Commit** `Turn a table's Frictionless descriptor into the lake's meta rows`

### Task 3: The commit

**Files:**
- Create: `src/lake/commit.rs`
- Test: unit tests in `src/lake/commit.rs`

**Interfaces:**
- Consumes: `TableDescriptor`, `META_SCHEMA` (Task 2).
- Produces:
  - `pub struct TableInput { pub table: String /* schema.table */, pub parquet: PathBuf, pub descriptor: TableDescriptor, pub key: Vec<String> /* defaults to descriptor.key */, pub replace_on_schema_change: bool }`
  - `pub struct TableChange { pub table: String, pub action: TableAction /* Registered | Unchanged | Replaced */, pub inserted: u64, pub updated: u64, pub deleted: u64, pub rows: u64 }`
  - `pub struct CommitOutcome { pub snapshot_id: i64, pub committed: bool, pub tables: Vec<TableChange>, pub new_files: Vec<String> /* paths relative to the data path, data and delete files this snapshot added */ }`
  - `pub fn commit(catalog_file: &Path, data_path: &str /* persisted, used only when creating */, write_path: &str /* local folder or s3:// URL, ending in / */, secret_sql: Option<&str> /* executed before the attach; never in an error */, tables: &[TableInput], commit_info: &serde_json::Value) -> Result<CommitOutcome>`: opens an in-memory DuckDB, `LOAD ducklake`, creates the catalogue with `DATA_PATH data_path` if the file is absent, re-attaches with `DATA_PATH write_path, OVERRIDE_DATA_PATH true`, then in ONE transaction per call: for each table, create schema/table if missing (`CREATE TABLE … AS FROM read_parquet(…)`); otherwise compare columns (an added trailing column → `ALTER TABLE ADD COLUMN`; any other difference → error naming it, unless `replace_on_schema_change`, which drops and recreates); refuse duplicate or NULL keys (error names the key and one example value); upsert with `UPDATE … FROM` where `IS DISTINCT FROM` on any non-key column, `DELETE … WHERE NOT EXISTS`, `INSERT … WHERE NOT EXISTS`; a table with an empty key is deleted and reinserted whole; then replace its rows in `meta.descriptors(table_name VARCHAR, descriptor JSON)` and `meta.fields(table_name, column_name, ordinal, type, finetype_label, description, constraints JSON, is_key)` only when they differ from the stored rows (compare with `IS DISTINCT FROM`), and set `COMMENT ON` for the table and its columns only where the text differs; finally, only if any statement above changed something (count the rows each one affected), `CALL set_commit_message('arcform', …, extra_info => commit_info)`. No run-specific value is written into any table. Every identifier is double-quoted. `committed` is false when the newest snapshot id did not change. `new_files` is read from `ducklake_data_file` and `ducklake_delete_file` rows whose `begin_snapshot` is the new snapshot.
  - Refuses `META_SCHEMA` as a table's schema.

- [ ] **Step 1: Write the failing tests** (each builds its input Parquet with `COPY … TO` in a temp dir, as `src/operator/ducklake_publish.rs`'s tests do):
  - `a_first_commit_creates_the_tables_and_meta`: two tables in one call → one snapshot; both tables hold their rows; `meta.fields` has a row per column; `duckdb_tables().comment` holds the descriptor's description.
  - `a_rebuild_with_three_rows_changed_records_only_those`: second commit with ids 5,6,7 changed, one row gone, one new → `TableChange { updated: 3, deleted: 1, inserted: 1 }` and `ducklake_table_changes` shows `update_preimage`/`update_postimage` for 3 rows.
  - `an_unchanged_rebuild_commits_nothing`: `committed == false`, snapshot count unchanged, `new_files` empty.
  - `a_new_run_with_the_same_data_and_descriptor_commits_nothing`: two commits with identical rows and descriptor but different `commit_info` run ids → the second has `committed == false`.
  - `null_values_compare_as_values`: a non-key column going from NULL to NULL is unchanged; NULL to 'x' is an update.
  - `quoted_column_names_publish_and_upsert`: columns `order`, `Code Display`, `MixedCase` (Review Focus 2).
  - `duplicate_keys_are_refused` and `null_keys_are_refused`: error names the key column and an offending value; nothing committed (Review Focus 3).
  - `an_added_trailing_column_is_applied_in_the_same_snapshot`: earlier snapshot (time travel `AT (VERSION => n)`) lacks it.
  - `a_retyped_column_is_refused_unless_replace_is_set`.
  - `a_table_without_a_key_is_replaced_whole`.
  - `the_meta_schema_is_reserved`.
  - `new_files_name_the_files_this_snapshot_added`: every name in `new_files` exists under the write path, and the list is empty after an unchanged commit.

- [ ] **Step 2: Run them and see them fail**: `cargo test --lib lake::commit` → compile error.

- [ ] **Step 3: Implement `commit`.**

- [ ] **Step 4: Run them and see them pass**: `cargo test --lib lake::commit` → 13 passed.

- [ ] **Step 5: Commit** `Commit a Protocol's tables into a DuckLake as one upserted snapshot`

### Task 4: The publish sequence

**Files:**
- Create: `src/lake/publish.rs`
- Test: unit tests in `src/lake/publish.rs`, with a static file server over a `DirStore` root on `127.0.0.1` in a thread (`std::net::TcpListener`, GET and HEAD) in `src/lake/test_server.rs` under `#[cfg(test)]` (the `lake` module is crate-private, so an integration test cannot reach it)

**Interfaces:**
- Consumes: `ObjectStore`, `DirStore`, `PutCondition`, `StoreError` (Task 1); `commit`, `TableInput`, `CommitOutcome` (Task 3).
- Produces:
  - `pub struct Lake { pub catalog_key: String /* e.g. catalog/open.ducklake */, pub write_path: String /* s3://<bucket>/<data_prefix> live, a local folder in tests */, pub data_path: String /* persisted https data path, e.g. https://openlake.meridian.online/lake/ */, pub data_prefix: String /* store key prefix matching data_path, e.g. lake/ */, pub pinned_prefix: String /* e.g. catalog/open- */ }`
  - `pub struct ExtraFile { pub from: PathBuf, pub to: String /* key, or key prefix ending in / for a directory */ }`
  - `pub struct Published { pub outcome: CommitOutcome, pub uploaded: Vec<UploadedObject /* key, bytes, sha256 */>, pub superseded: Vec<String>, pub pinned_catalog: Option<String>, pub catalog_etag: ETag, pub attempts: u32 }`
  - `pub fn publish(store: &dyn ObjectStore, lake: &Lake, work: &Path, secret_sql: Option<&str>, tables: &[TableInput], readable: &[(PathBuf, String)] /* file, key */, extra: &[ExtraFile], commit_info: &serde_json::Value, max_attempts: u32) -> Result<Published>`: each attempt: `get(catalog_key)` (absent → new catalogue), write it to `work/catalog.ducklake`, `commit` with `lake.write_path` and `secret_sql` (DuckLake reads existing files and writes new ones in place there), then if `committed`: `put` the catalogue at `pinned_prefix + <first 16 hex of its SHA-256> + ".ducklake"` with `IfAbsent` and `Cache-Control: public, max-age=31536000, immutable`, then at `catalog_key` with `IfMatch(fetched etag)` or `IfAbsent` when it was new, `Cache-Control: public, max-age=60`; on `PreconditionFailed` or `Throttled` from that final write, sleep `250ms * attempt` and start again, and after `max_attempts` fail naming the catalogue key and the attempt count. If not `committed`: `get` the catalogue again and start again if its ETag moved; otherwise `put` the fetched catalogue's pinned copy with `IfAbsent` (a no-op when present). Only after that, for each readable copy and extra file: first read the file's own key's current ETag with a metadata-only call (`ObjectStore::etag`, a HEAD on S3), or note it absent, then check the catalogue; every upload, in both branches below, uses `IfMatch(that etag)` or `IfAbsent`. A `PreconditionFailed` is not proof of a newer publish (an older publish's slow upload can land first): re-check `newer_change` for the file's tables against this publish's snapshot, put the key in `superseded` only if it is true, and otherwise retry in the same order as the first pass (re-read the key's ETag, then re-check the catalogue, then upload), at most 3 times; if every retry is refused and no newer change appears, fail the publish naming the key and the attempt count, never listing it in `superseded`. If the catalogue's ETag is still the one this attempt wrote or verified, `put_file` it; if the catalogue moved, fetch the catalogue, attach it read-only, and skip the file only if a snapshot newer than this publish's changed the file's table (an extra file belongs to every table in the step). "Changed" means either `ducklake_table_changes(lake, schema, table, since + 1, latest)` returns any row, or `ducklake_table_changes(lake, 'meta', 'descriptors', since + 1, latest)` returns a row whose `table_name` is this table (a descriptor-only change). Both are by name, so DuckLake resolves table ids itself; the map in `ducklake_snapshots()` is not used, because it names most changes by id; a skipped key goes in `Published.superseded` (a newer publish of that table owns it). Another Protocol's commit to a different table never stops this publish's uploads. Expose that check as `pub fn newer_change(store: &dyn ObjectStore, lake: &Lake, work: &Path, since_snapshot: i64, table: &str) -> Result<bool>`. This narrows the window in which two successful publishes can leave the stable copy behind the newest snapshot to the time between that check and the write; the next publish repairs it either way. `Published.pinned_catalog` is always set.

- [ ] **Step 1: Write the failing tests**
  - `a_publish_is_readable_over_http`: publish two tables into a `DirStore`; attach `ducklake:http://127.0.0.1:<port>/catalog/open.ducklake` READ_ONLY with `data_path` set to `http://127.0.0.1:<port>/lake/`; both tables read their rows; `meta.descriptors` has two rows.
  - `a_second_publish_time_travels`: after a change, `AT (VERSION => first)` over http reads the old rows.
  - `an_unchanged_publish_writes_no_catalogue`: catalogue ETag unchanged, no pinned copy added.
  - `a_lost_race_retries_and_both_publishes_land`: two threads publish different tables into one store; both succeed, one with `attempts >= 2`; the final catalogue holds both tables.
  - `a_refused_upload_leaves_the_live_lake_as_it_was`: a store wrapper that fails the pinned-copy write → `publish` errs; the catalogue object, its ETag and the readable copy's bytes are unchanged (Review Focus 1).
  - `a_commit_to_another_table_does_not_stop_this_publishs_readable_copy`: a store wrapper that lets a publish of a different table land just before this publish's readable upload → this publish still uploads its copy and `superseded` is empty.
  - `a_newer_descriptor_only_change_supersedes_the_readable_copy`: the newer publish changes only table A's descriptor → the older publish reports A's readable copy in `superseded`.
  - `an_older_publishs_late_copy_is_replaced_by_the_newer_one`: a wrapper writes an older snapshot's bytes to the key after this publish read its ETag and before its upload, with no newer catalogue → the key ends with this publish's bytes and `superseded` is empty.
  - `a_newer_publish_during_the_retry_is_not_overwritten`: after a refusal, a newer publish of the same table lands between this publish's re-read of the key's ETag and its upload → the key holds the newer bytes and this publish lists it in `superseded`.
  - `a_superseded_publish_does_not_overwrite_the_readable_copy`: a store wrapper that, just before this publish's readable upload, lets a second publish of the same table land → this publish reports the key in `superseded`, and the stable copy holds the second publish's bytes.
  - `a_publish_that_loses_every_race_leaves_the_readable_copy_alone`: a store wrapper that always answers `PreconditionFailed` on the catalogue write → `publish` errs after `max_attempts`; the readable copy's bytes are unchanged.
  - `the_pinned_copy_is_byte_identical_to_the_catalogue`.
  - `extra_files_upload_under_their_prefix`: a directory `from` uploads each file under `to`.

- [ ] **Step 2: Run them and see them fail**: `cargo test --lib lake::publish` → compile error.

- [ ] **Step 3: Implement `publish`.**

- [ ] **Step 4: Run them and see them pass**: `cargo test --lib lake::publish` → 11 passed.

- [ ] **Step 5: Commit** `Publish a lake's catalogue last, with a conditional write and retry`

### Task 5: The compatible manifest

**Files:**
- Create: `src/lake/manifest.rs`
- Test: unit tests in `src/lake/manifest.rs`; one case added to `src/lake/publish.rs`'s tests

**Interfaces:**
- Consumes: `ObjectStore`, `PutCondition` (Task 1); `Published` (Task 4).
- Produces:
  - `pub struct ManifestEntry { pub address: String /* namespace/name */, pub url: String, pub rows: u64, pub bytes: u64, pub sha256: String, pub published: String /* RFC 3339 commit time */, pub run: serde_json::Value /* {"id","record"} */, pub extra: serde_json::Map<String, serde_json::Value> }`
  - `pub fn update_manifest(store: &dyn ObjectStore, key: &str, current: &dyn Fn() -> Result<(Vec<ManifestEntry>, Option<String>)>, max_attempts: u32) -> Result<bool /* written */>`: `current` returns the entries whose table `newer_change` (Task 4) reports unchanged since this publish's snapshot, and the pinned address only if the live catalogue's ETag still equals `Published.catalog_etag`; it is called in every attempt after the manifest is read and before the put, so a retry after a refusal re-filters (read the manifest's version, check the catalogue, then write): get the manifest (absent → `{"datasets":{}}`; present but not JSON, or without a `datasets` object → error, never start from empty); set each entry under `datasets[address]`, keeping keys it doesn't set; set top-level `catalog.url` when `pinned_catalog` is given; write only if the result differs, with `IfMatch`, retrying on `PreconditionFailed`/`Throttled` like Task 4. The operator (Task 6) supplies `current`.

- [ ] **Step 1: Write the failing tests**
  - `an_entry_is_written_and_other_entries_are_kept`.
  - `a_manifest_that_is_not_json_is_refused`.
  - `an_identical_manifest_is_not_rewritten`: returns `false`, ETag unchanged.
  - In `src/lake/publish.rs`'s tests: `a_newer_publish_of_one_table_leaves_only_its_entry_alone`: two tables published; a newer publish changes table B; filtering with `newer_change` keeps A's entry and drops B's.
  - In `src/lake/publish.rs`'s tests: `an_unchanged_publish_repairs_a_stale_manifest_entry` (Review Focus 5): publish, overwrite the manifest entry with an old `sha256` and `catalog.url` with an older pinned address, publish again unchanged → both restored.

- [ ] **Step 2: Run them and see them fail**: `cargo test --lib lake::manifest` → compile error.

- [ ] **Step 3: Implement `update_manifest`.**

- [ ] **Step 4: Run them and see them pass**: `cargo test --lib lake::manifest && cargo test --lib lake::publish` → 3 and 13 passed.

- [ ] **Step 5: Commit** `Keep a compatible manifest.json while its readers move to the catalogue`

### Task 6: The `ducklake_write@1` operator, `--publish`, and the run record

**Files:**
- Create: `src/operator/ducklake_write.rs`, `operators/ducklake_write/README.md` (generated)
- Modify: `src/operator.rs` (register: the static, `catalog()` under `http-fetch`, the `with_schema` arm, the `Described` entry with `wraps` DuckLake, DuckDB and Cloudflare R2 and an example step, the `configuration_fields` and `configured` arms); `src/cli.rs` (`Run { publish: bool }`, `#[arg(long)]`, help: "Require the publish steps to publish live; fail if a write credential is missing"); `src/runner.rs` (put `ARC_RUN_ID` and, with `--publish`, `ARC_PUBLISH=required` into the step environment; after `write_contract`, upload pending run records); `skills/building-and-checking-a-protocol/references/step-kinds.md` (the new step)
- Test: unit tests in `src/operator/ducklake_write.rs`; `tests/ducklake_write.rs` (runs `arc run` on a fixture Protocol in `tests/fixtures/lake/protocol/`)

**Interfaces:**
- Consumes: Tasks 1 to 5.
- Produces:
  - Config, `deny_unknown_fields`: `public_base: String` (the bucket's public https base, e.g. `https://openlake.meridian.online/`), `catalog: String` (the catalogue's key, e.g. `catalog/open.ducklake`), `data_path: String` (the data prefix key, e.g. `lake/`; the persisted data path is `public_base + data_path`), `local_catalog: String` (relative to the Protocol), `tables: Vec<{ table, from, key: Option<Vec<String>>, descriptor, readable_copy: Option<String>, on_schema_change: Option<"fail"|"replace">, manifest_extra: Option<Map> }>`, `files: Vec<{ from, to }>`, `manifest: Option<String>` (store key, e.g. `manifest.json`), `credential: { type: "r2"|"s3", env: { key_id, secret, account_id } (r2) or { key_id, secret, endpoint, region } (s3), bucket: String }`. `Lake.pinned_prefix` is `catalog` with `.ducklake` replaced by `-`. Refuse a `public_base` that does not end in `/` or is not https (plain http is allowed only for `127.0.0.1` and `localhost`, for tests), and a `catalog` or `data_path` that is absolute or contains `..`.
  - `assets`: reads every `from`, `descriptor` and `files[].from`.
  - `run`: resolve each credential variable from `ctx.env` then the process environment. All set → live: `S3Store`; The live `Lake`'s `write_path` is `s3://<bucket>/<data_path>`; the live run builds a `CREATE TEMPORARY SECRET` for DuckDB from the resolved credential (type `r2` with `ACCOUNT_ID`, or `s3` with `ENDPOINT` and `REGION`; `URL_STYLE 'path'` for `s3`) and passes it as `secret_sql`, never logging it. `publish`, then `update_manifest` if `manifest` is set, then register the run record upload. Any unset → if `ctx.env["ARC_PUBLISH"] == "required"`, fail naming the unset variables (names only); else local: `publish` (with `secret_sql` `None`) into a `DirStore` rooted at `<local_catalog>`'s folder, with a local `Lake` whose `catalog_key` is `<local_catalog>`'s file name, `data_prefix` `<that name>.files/`, persisted `data_path` the absolute path of `<local_catalog>`'s folder joined with `data_prefix` (so `…/build/lake/open.ducklake.files/`), `write_path` that same `<local_catalog>.files/` folder, and `pinned_prefix` `<name stem>-` (local runs write pinned copies too, harmlessly), so the local lake reads back from disk; print `ducklake_write: published locally to <local_catalog>; the live lake needs <VARS> set`. The live catalogue is not fetched. The step succeeds in both cases.
  - Report: `{ "target": "live"|"local", "snapshot_id", "committed", "attempts", "tables": [TableChange…], "uploaded": [{key, bytes, sha256}], "pinned_catalog", "run_record": "runs/<run_id>.json" (live only), "missing_credential": [names] (local only) }`.
  - Commit info stamped on the snapshot: `{ "op": "ducklake_write", "run_id", "run_record": "<public base>/runs/<run_id>.json", "tables": [{table, rows}] }`.
  - `pub(crate) fn queue_run_record_upload(store: Arc<dyn ObjectStore>, key: String)` and `pub(crate) fn take_run_record_uploads() -> Vec<(Arc<dyn ObjectStore>, String)>` in `src/lake/mod.rs`, a process-wide `Mutex<Vec<…>>`. The runner, after `write_contract`, uploads `<runs_dir>/<run_id>.json` to each queued key with `PutCondition::IfAbsent`; a failure prints the error naming the key and the committed snapshot and makes `arc run` exit non-zero.

- [ ] **Step 1: Write the failing tests**
  - Unit: `config_refuses_an_unsafe_key_or_base`; `config_refuses_the_meta_schema`; `assets_read_every_input`.
  - Integration (`tests/ducklake_write.rs`), with no credential set unless a test says otherwise:
    - `without_a_credential_the_run_publishes_locally_and_is_green`: exit 0; report `target == "local"`, `missing_credential` lists the variable names; the local catalogue holds the tables.
    - `with_publish_a_missing_credential_fails_the_step`: `arc run --publish` exits non-zero; stderr names the variables and contains no value.
    - `a_local_lake_reads_back`: a fixture table of 50 rows (above DuckLake's inlining limit, so it is written to a data file), published locally twice with a change between; attaching `<local_catalog>` read-only returns the second run's rows, and time travel to the first snapshot the first run's.
    - `the_report_and_logs_never_contain_a_credential_value`: with `type: s3`, `endpoint` a closed port on `127.0.0.1`, and `key_id` and `secret` set to sentinel values, the step fails and neither the run record nor stderr contains either sentinel. `key_id` and `secret` are the secret parameters; `account_id`, `endpoint`, `region` and `bucket` are not.
  - `operation_catalogue` and `public_surface` tests already enumerate operators; update their expectations for the new operator.

- [ ] **Step 2: Run them and see them fail**: `cargo test --test ducklake_write` → compile error.

- [ ] **Step 3: Implement the operator, the CLI flag, the runner changes and the run record upload.** Generate the README with `ARC_UPDATE_READMES=1 cargo test --lib operator::reference`.

- [ ] **Step 4: Run them and see them pass**: `cargo test --test ducklake_write && cargo test --lib operator && cargo test --test operation_catalogue --test public_surface` → all pass.

- [ ] **Step 5: Commit** `Add ducklake_write: a Protocol publishes its tables into a lake, live or locally`

### Task 7: `arc lake rebase`

**Files:**
- Create: `src/lake/rebase.rs`
- Modify: `src/cli.rs` (`arc lake rebase --from <catalogue file or URL> --out <file> --data-path <https URL> [--upload <catalogue URL> --bucket <name> --credential-env <type>:<param>=<VAR>,…]`)
- Test: unit tests in `src/lake/rebase.rs`

**Interfaces:**
- Consumes: `ObjectStore`, `S3Store`, `PutCondition` (Task 1).
- Produces: `pub fn rebase(from: &str, out: &Path, data_path: &str) -> Result<RebaseReport { tables: Vec<(String, usize /* files */)>, views: Vec<String> }>`: creates a new catalogue at `out` with `DATA_PATH data_path`; for each table in `from` (every schema but `meta`), creates it from the first registered file's schema and registers each of its current data files by absolute path with `ducklake_add_data_files`; recreates each view from `duckdb_views().sql` with the old catalogue's alias replaced; one snapshot. With `--upload`, writes the result with `IfAbsent` (so it never overwrites a live catalogue by accident) and prints the key. Fails if a table has delete files (row deletions can't be carried by re-registering files) naming the table.

- [ ] **Step 1: Write the failing tests**
  - `rebase_carries_tables_files_and_views`: an old catalogue with two tables registered by absolute path and one view over one of them → new catalogue reads the same rows, the view works, `data_path` is the new one.
  - `rebase_refuses_a_table_with_deletions`.
  - `a_rebased_catalogue_accepts_a_ducklake_write_commit`: `commit` (Task 3) a changed table into the rebased catalogue; the new rows are in a DuckLake-named file under the write path, the old registered file is deleted logically, and the view still reads.

- [ ] **Step 2: Run them and see them fail**: `cargo test --lib lake::rebase` → compile error.

- [ ] **Step 3: Implement `rebase` and the subcommand.**

- [ ] **Step 4: Run them and see them pass**: `cargo test --lib lake::rebase` → 3 passed. Then rebase a downloaded copy of the live catalogue into the scratchpad: `arc lake rebase --from https://openlake.meridian.online/catalog/open.ducklake --out <scratch>/open.ducklake --data-path https://openlake.meridian.online/lake/` → eight tables, eight views; attach the result read-only and `SELECT count(*)` one table over https.

- [ ] **Step 5: Commit** `Add arc lake rebase, to move a lake onto a new data path`

### Task 8: Live check against R2, with Hugh

No code. Needs Hugh's go-ahead and a write token for a test prefix; nothing touches `catalog/open.ducklake` or any live dataset key.

- [ ] **Step 1: Ask Hugh** for the go-ahead and a test prefix (for example `lake-test/`) on the open bucket, and for the credential in a `.env` file outside the repo.
- [ ] **Step 2: Run** a fixture Protocol with `public_base: https://openlake.meridian.online/`, `catalog: lake-test/catalog/test.ducklake`, `data_path: lake-test/lake/` and the open bucket's name, `arc run --publish`. Expected: report `target: live`, `committed: true`. Then publish a change to the same table (more than 10 rows changed, so DuckLake writes a data file): this proves the commit reads the existing files and writes new ones through `s3://` with the override, and an https attach reads the result.
- [ ] **Step 3: Prove the conditional write**: run the same publish from two shells at once with different changes. Expected: both succeed, one with `attempts >= 2`, and an attach over https reads both changes.
- [ ] **Step 4: Read it from outside**: from a fresh DuckDB 2.0, `ATTACH 'ducklake:https://openlake.meridian.online/lake-test/catalog/test.ducklake' AS t (READ_ONLY)` and query a table, `meta.fields`, and a time-travel version.
- [ ] **Step 5: Clean up** the test prefix with Hugh's agreement, and record the outcome on the pull request.

### Task 9: Fold the design into the docs

**Files:**
- Modify: `docs/ARCHITECTURE.md` (a "Publishing into a lake" section: the step's sequence, the `meta` schema, one catalogue per owner, the local fallback and `--publish`, the cutover order, and each of Hugh's rulings with its reason), `CHANGELOG.md` (`[Unreleased]`: the operator, `--publish`, `arc lake rebase`), `examples/code-lists/` left as it is (it uses its own script)
- Delete: `docs/superpowers/specs/2026-11-13-lake-publish-design.md`, `docs/superpowers/plans/2026-11-13-lake-publish.md`

- [ ] **Step 1: Write the sections.**
- [ ] **Step 2: Check**: `cargo test --test public_surface` (docs references) passes; `grep -rn "superpowers/specs\|superpowers/plans" docs src` finds nothing.
- [ ] **Step 3: Commit** `Fold the lake publish design into the architecture docs`
