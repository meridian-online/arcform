# finetype_validate

Checks a built Parquet file against a JSON Schema contract and fails the step when any row breaks it, so a change in the data stops the run instead of shipping. Its code lives in arc itself (`src/operator.rs`).

<!-- arc:reference:start -->

## Reference

<!-- Generated from arc's operator catalogue (src/operator.rs). Edit it there, then run `ARC_UPDATE_READMES=1 cargo test --lib operator::reference`. -->

Checks a Parquet file against a JSON Schema contract with FineType, failing the step on any rejected row.

### What it wraps

- [FineType](https://github.com/meridian-online/finetype): Its DuckDB extension's `ft_validate` checks every column against the contract, inside arc's own DuckDB.
- [DuckDB](https://duckdb.org): Runs the extension in-process, so no `finetype` or `duckdb` command is needed.

### Reads

| Key | Names | Required | Description |
|---|---|---|---|
| `parquet` | file | yes | The Parquet file to check. |
| `schema` | file | yes | The JSON Schema contract the file is checked against. |

### Parameters

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `extension` | string | no | none | Optional per-step override for the FineType DuckDB extension path (else FINETYPE_DUCKDB_EXT). |

### Example step

```yaml
- name: validate
  op: finetype_validate@1
  with:
    parquet: build/breweries.parquet
    schema: schema.finetype.json
```

<!-- arc:reference:end -->
