# datapackage_describe

Writes a Frictionless Data Package descriptor (`datapackage.json`) for a built Parquet file. FineType types each column, and a curated overrides file adds the titles, descriptions and keys a person writes. Its code lives in arc itself (`src/operator.rs`).

<!-- arc:reference:start -->

## Reference

<!-- Generated from arc's operator catalogue (src/operator.rs). Edit it there, then run `ARC_UPDATE_READMES=1 cargo test --lib operator::reference`. -->

Writes a Data Package descriptor for a Parquet file, its columns typed by FineType and overlaid with curated overrides.

### What it wraps

- [FineType](https://github.com/meridian-online/finetype): Its `finetype profile` command types each column and writes the base descriptor that the overrides are laid onto.

### Reads

| Key | Names | Required | Description |
|---|---|---|---|
| `parquet` | file | yes | The Parquet file whose columns FineType types. |
| `overrides` | file | yes | The curated descriptor sidecar, in JSON, overlaid onto FineType's descriptor. |
| `nominations` | file | no | The declared column types FineType takes as given instead of inferring them. |

### Writes

| Key | Names | Required | Description |
|---|---|---|---|
| `out` | file | yes | The Data Package descriptor written, in JSON. |

### Parameters

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `expect_finetype_version` | string | no | none | Pin the run to one exact finetype release. Refuses, naming both versions, unless the resolved `finetype` on PATH reports exactly this version. |

### Example step

```yaml
- name: describe
  op: datapackage_describe@1
  with:
    parquet: build/breweries.parquet
    overrides: datapackage.overrides.json
    out: datapackage.json
```

<!-- arc:reference:end -->
