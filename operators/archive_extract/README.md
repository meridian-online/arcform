# archive_extract

Extracts named or matching members of a zip archive into a directory, so a later step reads the files inside. Its code lives in arc itself (`src/operator.rs`).

<!-- arc:reference:start -->

## Reference

<!-- Generated from arc's operator catalogue (src/operator.rs). Edit it there, then run `ARC_UPDATE_READMES=1 cargo test --lib operator::reference`. -->

Extracts members of a zip archive into a directory.

### What it wraps

- [zip](https://github.com/zip-rs/zip2): Reads the zip archive and inflates the members the step names or matches.

### Reads

| Key | Names | Required | Description |
|---|---|---|---|
| `archive` | file | yes | The zip archive to extract, relative to the protocol directory. |

### Writes

| Key | Names | Required | Description |
|---|---|---|---|
| `dest` | directory | yes | The directory the members are extracted into, recorded as each file under it when `members` names them. |

### Parameters

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `members` | list of string | no | none | Explicit member names to extract (exact match). Supply this or `pattern`. |
| `pattern` | string | no | none | Regex tested (unanchored) against each member name; matches are extracted. Supply this or `members`. |

### Example step

```yaml
- name: unzip
  op: archive_extract@1
  with:
    archive: raw/breweries.zip
    pattern: '\.csv$'
    dest: raw/breweries/
```

<!-- arc:reference:end -->
