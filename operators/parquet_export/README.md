# parquet_export

Writes a table or view from a Protocol's DuckDB database to a Parquet file, the usual last step before a dataset is published. Its code lives in arc itself (`src/operator.rs`).

<!-- arc:reference:start -->

## Reference

<!-- Generated from arc's operator catalogue (src/operator.rs). Edit it there, then run `ARC_UPDATE_READMES=1 cargo test --lib operator::reference`. -->

Writes a table of the pipeline's database to a Parquet file.

### What it wraps

- [DuckDB](https://duckdb.org): Its `COPY … TO … (FORMAT parquet)` statement writes the file, with the codec, row-group size, order and footer metadata the step sets.

### Reads

| Key | Names | Required | Description |
|---|---|---|---|
| `input` | table | yes | The table or view in the pipeline's database to export. |

### Writes

| Key | Names | Required | Description |
|---|---|---|---|
| `dest` | file | yes | The Parquet file written, relative to the protocol directory. |

### Parameters

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `compression` | string | no | `zstd` | Parquet codec. |
| `metadata` | mapping | no | none | Key-value metadata stamped into the Parquet footer. Keys and values are strings, written as UTF-8 bytes; read them back with decode(key)/decode(value) over parquet_kv_metadata(), since Parquet stores them untyped and DuckDB returns BLOB. Entries are emitted in sorted key order. Declaring any metadata changes the file's bytes and so its hash; declaring none leaves the output unchanged. |
| `order_by` | string | no | none | Optional ORDER BY clause applied to the export. |
| `row_group_size` | integer | no | none | Optional Parquet row-group size. |

### Example step

```yaml
- name: export
  op: parquet_export@1
  with:
    input: brewery_trend
    dest: build/brewery_trend.parquet
    order_by: year, state
```

<!-- arc:reference:end -->
