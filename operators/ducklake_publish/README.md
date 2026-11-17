# ducklake_publish

Publishes a built Parquet file as the rows of a table in a DuckLake catalog, as one snapshot, so readers see the old rows or the new ones and never a mix. Its code lives in arc itself (`src/operator/ducklake_publish.rs`).

<!-- arc:reference:start -->

## Reference

<!-- Generated from arc's operator catalogue (src/operator.rs). Edit it there, then run `ARC_UPDATE_READMES=1 cargo test --lib operator::reference`. -->

Publishes a Parquet file as the rows of a table in a DuckLake catalog, as one snapshot.

### What it wraps

- [DuckLake](https://ducklake.select): Registers a copy of the file with `ducklake_add_data_files` and replaces the table's rows in one snapshot.
- [DuckDB](https://duckdb.org): Attaches the DuckLake catalog through its `ducklake` extension.

### Reads

| Key | Names | Required | Description |
|---|---|---|---|
| `file` | file | yes | The built Parquet file to publish, of which a byte-for-byte copy is registered. |

### Parameters

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `catalog` | string | yes | none | The DuckLake catalog: a local catalog file, relative to the protocol directory, or a DuckLake connection string such as `postgres:dbname=lake`. A leading `ducklake:` is accepted. |
| `credential` | mapping | no | none | Optional. A DuckDB secret whose values come from the environment; the step refuses, naming the variables, when any is unset. |
| `data_path` | string | no | none | Optional DATA_PATH for the attach. Omitted, DuckLake uses the path the catalog records, or `<catalog>.files/` for a new file catalog. Must be local in this version. |
| `table` | string | yes | none | `table` (schema main) or `schema.table`. Created from the file's schema if it does not exist. Each publish replaces the table's rows with the file's, as one snapshot; an unchanged file is a no-op that reports the snapshot already holding it. |

### Example step

```yaml
- name: publish
  op: ducklake_publish@1
  with:
    file: build/breweries.parquet
    catalog: lake.ducklake
    table: breweries
```

<!-- arc:reference:end -->
