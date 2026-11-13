# A Protocol run publishes into a DuckLake on R2

## Intent

`arc run` should end by publishing, with nothing outside the run. A skilled analyst's alternative is a build script that ends with `aws s3 cp`, so publishing has to be at least that easy. What arc adds is what DuckLake gives a writer: every publish is a snapshot readers can time-travel through, a row-level change feed between releases, file sizing and statistics, and a catalogue any DuckDB attaches over https with no credentials and no server. Cloudflare R2 charges no egress, so readers cost nothing.

The first datasets to publish this way are the two Standard Occupational Classification (SOC) Protocols in open-analytics: BLS's US SOC 2018 and ONS's UK SOC 2020, each one Protocol writing several tables. Meridian's other open datasets follow onto the same step in open-analytics' own work.

This replaces publishing that happens outside the run today: a script uploads files, another rebuilds the catalogue from scratch and uploads it, and a third reconstructs from run records that the bytes came from a finished run. Once publishing is a step, the run record showing the step succeeded is the provenance.

## Goals

1. A new operator, `ducklake_write@1`, ends a Protocol by writing one or more tables into a DuckLake catalogue, committing them as one snapshot.
2. DuckLake writes the data. Each table is upserted on its primary key, so a snapshot records only the rows that changed, and an unchanged rebuild commits nothing.
3. The catalogue can live on R2 as a single DuckLake file, written by one owner and read by anyone over https.
4. Each table's Frictionless descriptor is written into the lake in the same snapshot as its rows: `meta.descriptors` (the whole descriptor) and `meta.fields` (one row per column). Descriptions also go into the catalogue as `COMMENT ON`.
5. Each table is also served as one readable Parquet file at `<namespace>/<name>.parquet`, stamped with its descriptor, for readers who download rather than attach.
6. A run without the write credential publishes to a local catalogue and stays green. `arc run --publish` makes the live publish required.
7. A compatible `manifest.json` can be written for the existing readers of today's manifest until they move to the catalogue.

## Non-goals

- Moving the open-analytics Protocols onto the step, retiring infra's publish scripts, and moving web and the uptime monitor off `manifest.json`. Each is its own work in its own repo; this delivers what they need.
- Environments (dev, staging, production targets) beyond the local fallback and `--publish`. That needs its own design, compared against dbt, Prefect and Airflow.
- Issuing per-user scoped tokens for outside publishers, OS keychain storage, and short-lived credentials. The design leaves room for them.
- A hosted Postgres catalogue. DuckLake supports one, and `catalog:` already accepts a connection string, but this build targets the file catalogue on R2.
- Matching a user's column to a lake key using `meta.fields`. That belongs to dovetail; this build only writes the tables it reads.
- Splitting or compacting files beyond what DuckLake does by default.

## Anti-goals

- A green run that silently published nothing when the person meant to publish.
- A catalogue that more than one owner can write. DuckLake's only permissions are read-only or read-write on the whole catalogue, so a shared catalogue would let any publisher rewrite any other's tables.
- Published state that only a second program can reconstruct.
- A dataset going stale because a publish fell back to local without anyone noticing.

## Rulings from the brainstorm (Hugh, 2026-11-13)

- **DuckLake writes the data (option B).** The step upserts rows into the lake, and DuckLake chooses file names, sizes, sorting and statistics. Rejected: registering the run's own validated Parquet with `ducklake_add_data_files` (A). It keeps the stamped footer in every lake file and the exact validated bytes, but each publish then replaces the whole table, so the change feed shows the whole table replaced and arc rather than DuckLake owns the files. Hugh judged B the better fit for the DuckDB ecosystem and its downsides solvable.
- **The descriptor moves into the lake.** The lake's own files no longer carry it in their footers. It lives in `meta.descriptors` and `meta.fields`, versioned in the same snapshot, so time travel gives the descriptor that matched the data. The readable download copy still carries it in its footer.
- **One catalogue per owner.** A publisher writes only its own catalogue and its own R2 prefix. Meridian's open lake is the first owner. A Managed Dataset user publishing from their own machine gets a catalogue of their own that readers attach separately. Hugh: it is the right way to do it, and likely a necessary separation for future products and data security.
- **Plain `arc run` publishes to the lake the Protocol names.** A run without the write credential publishes locally and stays green. The comparison is an `npm` build run locally: the person has done nothing wrong, so the run is not a failure. `--publish` makes the live publish required, for scheduled jobs and for a person who means to publish.
- **The credential comes from the environment**, which includes a `.env` file the manifest lists. Hugh's own publishes use a `.env` on his machine. Managed runs later inject the same variables from a cloud secret store.
- **The readable `<namespace>/<name>.parquet` stays.** Downloading the whole file with `curl`, without DuckDB and without auth, is a use Meridian publishes for. It is a separate object that the catalogue never registers, because DuckLake's maintenance may delete files it owns.
- **Descriptors are for users and agents solving a Protocol.** Lakes are reference data. An agent holding a column it cannot place, such as postal codes, should be able to find which table holds those values as a key. That is why `meta.fields` is one row per column with its finetype label, constraints and key flag.
- **Readers move to DuckDB 2.0.** DuckDB 2.0 writes the catalogue file in its 2.0 storage format on every write, which DuckDB 1.x cannot open. Hugh chose to move readers to 2.0 over writing the lake with a pinned DuckDB 1.5. The published `ATTACH` snippets and dataset pages state DuckDB 2.0 or later from the first publish. Since readers need 2.0 regardless, the catalogue uses DuckLake's current format, 1.1, rather than pinning 1.0 (agent's call).
- **Every open dataset moves onto this step**, as follow-up work in open-analytics. This build has to support what all of them need, including gleif's search parts.

## How a publish works

### Where things live

A lake is a DuckLake catalogue in DuckDB format, stored as one object, plus a data prefix. For Meridian's open lake:

| Thing | Location |
|---|---|
| Catalogue | `https://openlake.meridian.online/catalog/open.ducklake` |
| Catalogue's persisted data path | `https://openlake.meridian.online/lake/` |
| Lake data files | `lake/<schema>/<table>/ducklake-<uuid>.parquet`, named by DuckLake |
| Readable copy | `<namespace>/<name>.parquet` |
| Extra files (gleif's search parts) | wherever the step names, under the owner's prefix |

The catalogue's persisted data path is the https address readers use. DuckLake records each data file's path relative to it. The writer never writes through https.

### The step, in order

1. **Resolve the target.** Look up the write credential (below). With it, the target is the live lake. Without it, the target is a local catalogue in the Protocol's build folder, and the step reports `published locally` naming the missing variables. Under `--publish`, a missing credential fails the step instead.
2. **Fetch the catalogue.** Download the live catalogue object and remember its ETag. If none exists yet, create one with the https data path, at DuckLake's current format (1.1 on DuckDB 2.0).
3. **Attach it read-write with the data path overridden to the lake's write path** (`OVERRIDE_DATA_PATH true`): for the live lake, the same objects addressed through R2's S3 API (`s3://<bucket>/lake/`), with a temporary DuckDB secret built from the credential. DuckLake records paths relative to the data path, so a relative path resolves to the same object under the https address readers use and under the S3 address the writer uses. The upsert reads the table's existing files and DuckLake writes new ones through that S3 address. An override cannot point at a local staging folder: DuckLake would then look for the lake's existing files there too.
4. **In one transaction, for each table:**
   - create the schema and table if missing, from the input's columns;
   - `UPDATE` rows whose key matches and whose values differ, `DELETE` rows whose key is gone, `INSERT` rows whose key is new;
   - replace that table's row in `meta.descriptors` and its rows in `meta.fields`, only when they differ from what is stored;
   - set `COMMENT ON` for the table and each column from the descriptor, only where the text differs;
   - if anything above changed, stamp the commit's metadata with the run's identity and each table's row counts.
   If nothing changed (no rows, no descriptor, no comment), the transaction makes no write, DuckDB commits no snapshot, and the step reports `unchanged` with the current snapshot. Nothing run-specific is written into a table, so a rebuild of the same data by a new run is still unchanged.
5. **The new data and delete files are already in the bucket**, written by DuckLake in step 4. That is safe before the catalogue upload because nothing references a file until the catalogue does; files from a commit that is then lost or fails are orphans that DuckLake's cleanup can remove.
6. **Upload the catalogue conditionally**, only if its ETag is still the one fetched in step 2. If another publish won, discard the local commit, go back to step 2, and redo the transaction. Data files the lost commit wrote are not lost: any the retry does not reference are orphans that DuckLake's cleanup can remove. Retry a bounded number of times, then fail, naming the conflict. When nothing was committed, instead re-read the catalogue's ETag and go back to step 2 if it moved.
7. **Upload the readable copy** of each table (the Protocol's stamped Parquet, byte for byte) and any extra files, only now that the catalogue holds this snapshot. Readers fetch these at fixed addresses, so they are never written before the commit lands: a failed or lost publish leaves them as they were. A copy is skipped only when a snapshot newer than this publish's changed that same table, so a newer publish's copy is never overwritten by an older one, while another Protocol's commit to a different table never stops this publish's uploads. The manifest entries follow the same rule. The remaining window between that check and the write is closed by the table's next publish.
8. **Report** each table's action (`registered`, `unchanged`), snapshot id, row counts by change type, the readable copies and extra files uploaded with their sizes and SHA-256, and the data files DuckLake wrote (by path), in the step's report in the run record.

### Upsert, not MERGE

DuckLake's `MERGE INTO` supports only a single `UPDATE` or `DELETE` action today. The step issues `UPDATE`, `DELETE` and `INSERT` in one transaction instead. A spike on DuckDB 2.0 showed this commits one snapshot, records updated rows as `update_preimage` and `update_postimage` in `ducklake_table_changes`, and commits no snapshot at all when nothing differs. Comparison is `IS DISTINCT FROM` across every non-key column, so NULLs compare correctly. A table with no declared primary key is replaced whole (delete all, insert all) and says so in the report.

### The `meta` schema

Every catalogue arc publishes into holds:

- `meta.descriptors(table_name VARCHAR, descriptor JSON)` — the table's whole Frictionless descriptor. `table_name` is `schema.table`. DuckLake enforces no primary keys, so the step keeps `table_name` unique itself; the run that wrote a row is in the snapshot's commit metadata, not in the row.
- `meta.fields(table_name, column_name, ordinal, type, finetype_label, description, constraints JSON, is_key BOOLEAN)` — derived from the descriptor's `schema.fields` and `primaryKey`.

`meta` is reserved: no namespace may take that name. A few rows each, so DuckLake inlines them into the catalogue rather than writing files. The descriptor written here omits the resource's `bytes` and `hash`, which describe a single file and have no meaning for a lake table. It also omits the package's `created`, as the footer copy already does.

### Credential

The manifest declares, under the step's `credential:`, the secret type (`r2` or `s3`) and the environment variable for each secret parameter, as the existing publish operator does. Values come from the step's environment: the manifest's `dotenv:` files, then the process environment. Uploads and the conditional catalogue write use the same values. The token needs read and write on the owner's prefix only.

### Configuration

```yaml
- name: publish
  op: ducklake_write@1
  with:
    public_base: https://openlake.meridian.online/
    catalog: catalog/open.ducklake                 # a key under public_base
    data_path: lake/                               # persisted data path is public_base + data_path
    local_catalog: build/lake/open.ducklake      # the fallback catalogue; its data goes in build/lake/open.ducklake.files/
    tables:
      - table: bls.soc_2018
        from: build/soc_2018.parquet               # or a DuckDB relation the Protocol produced
        key: [code]                                # defaults to the descriptor's primaryKey
        descriptor: soc_2018.datapackage.json
        readable_copy: bls/soc_2018.parquet
    files:                                         # extra objects uploaded as they are
      - from: build/search/
        to: gleif/lei/
    credential:
      type: r2
      bucket: meridian-open
      env: { key_id: R2_ACCESS_KEY_ID, secret: R2_SECRET_ACCESS_KEY, account_id: R2_ACCOUNT_ID }
```

The step reads each `from:` and `descriptor:`, so it runs after the steps that produce them and re-runs when they are rebuilt.

### Compatible manifest

With `manifest: manifest.json` set, the step also reads the live manifest, updates each published table's entry, and writes it back with the same conditional write. It is written after the catalogue, so a reader never sees a manifest entry for a snapshot the catalogue doesn't hold. The option exists so the follow-up work can ship before web and the monitor read the catalogue, and it is removed after.

When a publish commits nothing, the step still pins the current catalogue (its pinned address is written only if absent) and passes it to the manifest, so a manifest left behind by an interrupted publish is repaired by the next one. Each entry keeps the keys today's readers use (agent's call): `url` is the readable copy's address; `rows`, `bytes` and `sha256` describe that copy; `published` is the commit time; `run` names the run and its record at `runs/<run_id>.json`. The top-level `catalog` names the pinned catalogue address. A table entry can carry extra keys from the step's `files:` (for gleif: `searchUrls`, `searchRows`), declared in the step. The old per-dataset fixed snapshots (`<namespace>/<name>/<sha16>.parquet`) are no longer written.

### The cutover (agent's call)

The live catalogue and manifest have writers in infra that rebuild and overwrite them without checking what they read, and a version collector that deletes files the manifest no longer names, which the new catalogue still registers. So the migration below, the notice to readers that they need DuckDB 2.0, stopping infra's catalogue rebuild and manifest flip, and stopping the collector all happen together, before the first live publish, as open-analytics' and infra's follow-up work. After the cutover, a dataset whose Protocol has not yet moved keeps serving its current files and is refreshed only once it moves. This build supplies the migration and documents the cutover; it does not change infra.

### Moving the live open lake

The live `open.ducklake` is format 1.0, but its persisted data path is a local folder on the machine that built it. DuckLake cannot change a persisted data path, so DuckLake-written files could not be resolved by readers. Its 34 snapshots are the record of rebuilds from scratch, not dataset history. The first publish into the open lake therefore starts a new catalogue with the https data path, and registers the existing datasets' current files by URL, with their compatibility views, using `ducklake_add_data_files`, as the rebuild script does today. A one-off `arc` subcommand or a documented SQL script does this; the builder chooses. It runs once, with Hugh, before the first live publish.

### Schema changes (agent's call)

When a table already exists and the input's columns differ, a column added at the end is applied with `ALTER TABLE … ADD COLUMN` in the same snapshot, so earlier snapshots read as they were and later ones have the column. Anything else (a column dropped, renamed or retyped, or the key changed) fails the step and names the difference, unless the table's entry sets `on_schema_change: replace`, which drops and recreates the table in the same snapshot and says so in the report. Failing is the default because a silent replace would make the change feed report the whole table as deleted and reinserted.

### The local catalogue (agent's call)

Without the credential, the step publishes into a lake of its own at `local_catalog`, whose data path is the local folder beside it (`<local_catalog>.files/`), so the contributor can attach it and query exactly what the run built, offline. Its history is the history of local runs, separate from the live lake's. An earlier draft started each local run from a copy of the live catalogue to report the live diff, but a catalogue whose data path is the live https address cannot read files a local run adds, so the local lake would not read back.

### The run record beside the snapshot (agent's call)

An earlier ruling asks that a served object's run record be served beside it. The run record is complete only when the run ends, after the step. So when a run's publish went live, arc uploads the finished run record to `runs/<run_id>.json` under the owner's prefix as the run ends, with the same credential, and the snapshot's commit metadata names that address. A run whose record upload fails is reported as failed, with the snapshot already committed and named.

### A pinned catalogue address (agent's call)

A reader whose attach spans a catalogue overwrite can read bytes from two versions; today's measurements show DuckDB fails loudly rather than silently when that happens, and a retry heals it. To keep the zero-tear address readers have today, the step uploads an immutable copy of the new catalogue at `catalog/open-<sha16>.ducklake`, cached forever, before the conditional write of the stable `catalog/open.ducklake`. The compatible manifest records it.

## Approaches considered

- **A. Register the run's own validated Parquet** (`ducklake_add_data_files` on a content-addressed copy). Exact validated bytes, footer stamped in every lake file, an extension of today's operator. Rejected: each publish replaces the whole table, so the change feed loses its value, and DuckLake does little more than index arc's files.
- **B. DuckLake writes the data (chosen).** Native file management, a row-level change feed, statistics, inlining, compaction. Costs the footer in lake files, moved into `meta` and kept in the download copy, and a check before committing to avoid empty snapshots, which DuckDB turned out to give for free.
- **C. One step per table.** Smallest change, but six tables become six snapshots and six catalogue uploads, with windows where the tables disagree. Rejected.

Catalogue backends considered: a file on R2 (chosen: no server, no running cost, the pattern DuckLake documents for public lakes); hosted Postgres (a standing service and a credential for anonymous readers, who would still need a frozen export); R2 Data Catalog (Cloudflare's managed Iceberg: writable from DuckDB, but Iceberg rather than DuckLake). Postgres remains the step up when one lake has many concurrent writers, without moving any data.

## Constraints

- DuckDB 2.0 and its DuckLake extension, as arcform links today, write the lake. Readers need DuckDB 2.0 or later.
- One writer per catalogue, enforced by the conditional upload. Throughput is bounded by the seconds a catalogue fetch, commit and upload take, far above what a published lake needs.
- The repo is public. The open lake's addresses are public; credentials never appear in a manifest, a report or a log.
- No live write to the production bucket during the build without Hugh's go-ahead.

## Proven by the spike (DuckDB 2.0, local folder served over http)

- Writing with `OVERRIDE_DATA_PATH` to another folder records relative paths, and a reader attaching the catalogue over http resolves them against the persisted data path. An override to a separate folder also hides the lake's existing files, which is why the writer overrides to the same objects through another address.
- Not yet exercised: DuckLake reading and writing through `s3://` on R2 with the override, and a relative path resolving to the same object under both addresses. Task 8's live check proves it; the fallback is downloading the step's tables into a local folder before the commit.
- `UPDATE`, `DELETE`, `INSERT` in one transaction: one snapshot, a correct change feed, and no snapshot when nothing changed.
- Small changes and the `meta` tables are inlined into the catalogue, with no data files.
- `COMMENT ON` and time travel are visible to http readers.
- `DUCKLAKE_VERSION '1.0'` on `ATTACH` pins a new catalogue's DuckLake format to 1.0, but not its DuckDB storage format (below).

- R2's S3 API documents `If-Match` and `If-None-Match` on `PutObject`, checked atomically against committed state, so the conditional catalogue write is supported. R2 also limits writes to one per second per key and answers `429` above it, which the retry must treat as a conflict. Not yet exercised against a live bucket.
- **DuckDB 1.x cannot open any catalogue DuckDB 2.0 has written.** Pinning the DuckLake format to 1.0 is not enough: the catalogue is itself a DuckDB database file, and DuckDB 2.0's DuckLake extension writes it in DuckDB's 2.0 storage format on any write, even to a catalogue created by 1.5. Neither `storage_compatibility_version` nor creating the file at an older storage version first prevents it. DuckDB 1.5.2 refuses the result (`Trying to read a database file with version number 999`). The live catalogue is in 1.5 storage today, so its readers need DuckDB 1.5 or later; after the first publish they need 2.0.

## Left to the builder

- Which S3 client performs uploads and the conditional write, and how it handles R2's endpoint.
- The local catalogue's exact folder layout.
- Retry count and backoff on a lost conditional write.
- Whether `ducklake_publish@1` is kept beside the new operator or retired. Nothing published uses it today.
- Test strategy for R2: a local S3-compatible server in CI, or a fake object store behind a trait. The live bucket is exercised once, by hand, with Hugh.
- The exact `meta.fields` columns beyond those named, so long as the ones named are there.
