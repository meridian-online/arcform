# ducklake_write

Publishes a Protocol's tables into a DuckLake catalogue on object storage, such as Cloudflare R2, as one snapshot: readers attach the catalogue over https with no credentials and time-travel through every publish. Each table is upserted on its key, so a snapshot records only the rows that changed, and an unchanged rebuild commits nothing. Each table's Frictionless descriptor is written into the lake's `meta` schema in the same snapshot. Without the write credential in the environment, the step publishes into a local lake beside the Protocol instead and the run stays green; `arc run --publish` makes the live publish required. The step runs on every `arc run`, never skipped as fresh: a hash of its inputs cannot tell a local publish from a live one. Readers need DuckDB 2.0 or later. Its code lives in arc itself (`src/operator/ducklake_write.rs`, with the lake's building blocks in `src/lake/`).

<!-- arc:reference:start -->

## Reference

<!-- Generated from arc's operator catalogue (src/operator.rs). Edit it there, then run `ARC_UPDATE_READMES=1 cargo test --lib operator::reference`. -->

Publishes tables into a DuckLake catalogue on object storage, as one snapshot, upserting each on its key, or into a local lake when the write credential is not set.

### What it wraps

- [DuckLake](https://ducklake.select): Writes the tables' data files and records each publish as a snapshot readers can time-travel through, with a row-level change feed between snapshots.
- [DuckDB](https://duckdb.org): Runs the upsert, an `UPDATE`, `DELETE` and `INSERT` in one transaction, against the catalogue attached through its `ducklake` extension.
- [Cloudflare R2](https://developers.cloudflare.com/r2/): Holds the catalogue and its data; the catalogue is written conditionally on the version fetched, so two publishes never overwrite each other.

### Reads

| Key | Names | Required | Description |
|---|---|---|---|
| `tables.from` | list of file | yes | Each table's Parquet file, whose rows the table takes. |
| `tables.descriptor` | list of file | yes | Each table's Frictionless descriptor, written into the lake's `meta` schema. |
| `files.from` | list of any | no | Each extra file or directory, uploaded as it is. |

### Parameters

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `catalog` | string | yes | none | The catalogue's key under `public_base`, e.g. `catalog/open.ducklake`. Its immutable pinned copies go beside it, as `catalog/open-<sha16>.ducklake`. Relative, without `..`. |
| `credential` | mapping | yes | none | The write credential, its values read from the environment: the manifest's dotenv files, then the process environment. With any variable unset the step publishes into `local_catalog` and succeeds, unless the run is `arc run --publish`. |
| `data_path` | string | yes | none | The key prefix DuckLake writes the lake's data files under, e.g. `lake/`; the catalogue's persisted data path is `public_base` followed by it. Relative, without `..`. |
| `files` | list of mapping | no | none | Extra files uploaded as they are, once the catalogue holds the snapshot. |
| `local_catalog` | string | yes | none | The catalogue a run without the write credential publishes into instead, relative to the protocol directory, e.g. `build/lake/open.ducklake`. Its data goes in the folder beside it, `<local_catalog>.files/`, so the local lake attaches and reads back offline. |
| `manifest` | string | no | none | Optional. The key of a compatible `manifest.json` kept current for readers of today's manifest: each table with a `readable_copy` gets an entry, and the top-level `catalog.url` names the pinned catalogue. Written after the catalogue, and only by a live publish. |
| `public_base` | string | yes | none | The bucket's public base address, ending in `/`, e.g. `https://openlake.meridian.online/`. Readers resolve the catalogue and its data under it. Must be https; plain http is admitted only for 127.0.0.1 and localhost, for tests. |
| `tables` | list of mapping | yes | none | The tables to publish, committed together as one snapshot. Each is upserted on its key, so the snapshot records only the rows that changed, and an unchanged publish commits nothing. |

### Example step

```yaml
- name: publish
  op: ducklake_write@1
  with:
    public_base: https://openlake.meridian.online/
    catalog: catalog/open.ducklake
    data_path: lake/
    local_catalog: build/lake/open.ducklake
    tables:
      - table: bls.soc_2018
        from: build/soc_2018.parquet
        descriptor: soc_2018.datapackage.json
        readable_copy: bls/soc_2018.parquet
    credential:
      type: r2
      bucket: meridian-open
      env:
        key_id: R2_ACCESS_KEY_ID
        secret: R2_SECRET_ACCESS_KEY
        account_id: R2_ACCOUNT_ID
```

<!-- arc:reference:end -->
