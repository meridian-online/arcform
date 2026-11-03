# gleif_ra_fetch

Pages through the GLEIF API for every legal entity registered with one registration authority and writes them to a CSV file. Its script is `fetch_gleif_ra.py` beside this README; arc runs it with uv.

<!-- arc:reference:start -->

## Reference

<!-- Generated from arc's operator catalogue (src/operator.rs). Edit it there, then run `ARC_UPDATE_READMES=1 cargo test --lib operator::reference`. -->

Pages through GLEIF's entities for one registration authority and writes them to a CSV file.

### What it wraps

- [GLEIF API](https://www.gleif.org/en/lei-data/gleif-api): The public API whose `lei-records` the step pages through, filtered to one registration authority.
- [dlt](https://dlthub.com): Pages the API and writes the records.

### Writes

| Key | Names | Required | Description |
|---|---|---|---|
| `out` | file | yes | The CSV file the entities are written to, relative to the protocol directory. |

### Parameters

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `page_size` | integer | no | `200` | GLEIF page[size]. |
| `ra` | string | yes | none | GLEIF registration-authority id to page (e.g. RA000665). |
| `user_agent` | string | no | `Meridian Protocol (open-analytics; research@meridian.online)` | User-Agent request header. |

### Example step

```yaml
- name: fetch_ra
  op: gleif_ra_fetch@1
  with:
    ra: RA000665
    out: raw/gleif_ra000665.csv
```

<!-- arc:reference:end -->
