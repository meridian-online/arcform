# html_link_discover

Fetches an index page and writes the links on it that match a pattern, one address per line, for a later step to download. Its code lives in arc itself (`src/operator.rs`).

<!-- arc:reference:start -->

## Reference

<!-- Generated from arc's operator catalogue (src/operator.rs). Edit it there, then run `ARC_UPDATE_READMES=1 cargo test --lib operator::reference`. -->

Fetches an index page and lists the links on it that match a pattern, one address per line.

### What it wraps

- [ureq](https://github.com/algesten/ureq): The HTTP client that fetches the index page.
- [regex](https://github.com/rust-lang/regex): Tests each link on the page against `pattern`.

### Writes

| Key | Names | Required | Description |
|---|---|---|---|
| `out` | file | yes | The file the matching addresses are written to, one per line. |

### Parameters

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `headers` | mapping | no | none | Extra request headers (a default User-Agent is set unless overridden). |
| `pattern` | string | yes | none | Regex tested (unanchored) against each raw href; matches are kept. |
| `url` | string | yes | none | Index page to fetch (http/https). |

### Example step

```yaml
- name: discover
  op: html_link_discover@1
  with:
    url: https://example.org/downloads/
    pattern: '\.zip$'
    out: build/zip_urls.txt
```

<!-- arc:reference:end -->
