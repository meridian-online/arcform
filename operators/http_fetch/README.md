# http_fetch

Downloads one file over HTTP or HTTPS into the Protocol directory. Pin the file's SHA-256 and a run takes it from arc's shared fetch cache without asking the origin again. Its code lives in arc itself (`src/operator.rs`).

<!-- arc:reference:start -->

## Reference

<!-- Generated from arc's operator catalogue (src/operator.rs). Edit it there, then run `ARC_UPDATE_READMES=1 cargo test --lib operator::reference`. -->

Downloads the file at one HTTP or HTTPS address.

### What it wraps

- [ureq](https://github.com/algesten/ureq): The HTTP client that makes the request; arc writes the body atomically and, with `sha256` set, checks it against the pin.

### Writes

| Key | Names | Required | Description |
|---|---|---|---|
| `out` | file | yes | The file the download is written to, relative to the protocol directory. |

### Parameters

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `headers` | mapping | no | none | Extra request headers (a default User-Agent is set unless overridden). |
| `sha256` | string | no | none | Pin the artifact's SHA-256. Pinned bytes are taken from the shared fetch cache without asking the origin, and a transfer that does not match is refused. |
| `url` | string | yes | none | Source URL (http/https). |

### Example step

```yaml
- name: fetch_tickers
  op: http_fetch@1
  with:
    url: https://openlake.meridian.online/sec/company_tickers_exchange.parquet
    out: raw/company_tickers_exchange.parquet
```

<!-- arc:reference:end -->
