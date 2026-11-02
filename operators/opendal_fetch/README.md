# opendal_fetch

Copies one object from a storage service, such as S3 or a plain HTTPS address, to a file in the Protocol directory. It is in an `arc` built with the `opendal` feature. Its code lives in arc itself (`src/operator.rs`).

<!-- arc:reference:start -->

## Reference

<!-- Generated from arc's operator catalogue (src/operator.rs). Edit it there, then run `ARC_UPDATE_READMES=1 cargo test --lib operator::reference`. -->

Copies one object from a storage backend, such as S3 or HTTPS, to a file.

### What it wraps

- [Apache OpenDAL](https://opendal.apache.org): Reads the object through the storage service its address names, such as S3 or HTTP.

### Writes

| Key | Names | Required | Description |
|---|---|---|---|
| `to` | file | yes | The file the object is written to, relative to the protocol directory. |

### Parameters

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `from` | string | yes | none | Scheme-dispatched source: https://host/key, s3://bucket/key, … |
| `headers` | mapping | no | none | Extra request headers (a default User-Agent is set unless overridden). |

### Example step

```yaml
- name: fetch_breweries
  op: opendal_fetch@1
  with:
    from: s3://example-bucket/raw/breweries.csv
    to: raw/breweries.csv
```

<!-- arc:reference:end -->
