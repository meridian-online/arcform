# dlt

Pages one resource of a REST API with [dlt](https://dlthub.com) and writes its records to a JSON Lines file. The step's `source` is dlt's own `rest_api` source configuration, the base URL, headers, paginator, endpoint, query parameters and data selector, and dlt's documentation of its REST API source describes every field of it; arc passes it to dlt as JSON and names no API of its own. The records are written as the API returns them, after the data selector, with keys sorted and the lines sorted by their bytes so that two runs over the same data give the same file; renaming, flattening and typing them belongs in the Protocol's SQL steps after this one, where the change can be seen and tested. The step refuses `auth`, incremental loading and more than one resource before it sends a request: a credential does not belong in a manifest, so only public APIs are fetched; incremental loading keeps state between runs, and this step fetches everything on every run so that its file depends on the API alone; and one resource per step keeps one step to one file, so a Protocol that needs several resources fetches them in several steps. Its script is `rest_fetch.py` beside this README, named so that it cannot shadow the `dlt` package it imports; arc runs it with uv, with dlt pinned to one release.

<!-- arc:reference:start -->

## Reference

<!-- Generated from arc's operator catalogue (src/operator.rs). Edit it there, then run `ARC_UPDATE_READMES=1 cargo test --lib operator::reference`. -->

Pages one resource of a REST API with dlt and writes its records to a sorted JSON Lines file.

### What it wraps

- [dlt](https://dlthub.com): Pages a REST API from a `rest_api` source configuration and yields its records.

### Writes

| Key | Names | Required | Description |
|---|---|---|---|
| `out` | file | yes | The JSON Lines file the records are written to, relative to the protocol directory, replaced only by a fetch that succeeds in full. |

### Parameters

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `expect_total` | mapping | no | none | Optional guard against a fetch that stops early. `path` is a dotted path into the first response's JSON body where the API reports its record count; the fetch is refused, and nothing written, when it returns fewer records than that count minus `tolerance` (an integer, 0 or more, default 0). |
| `page_delay_seconds` | number | no | `0.0` | Seconds to pause before each request after the first, for an API that throttles. dlt retries failed requests but has no pacing of its own. |
| `source` | mapping | yes | none | dlt's own `rest_api` source configuration, as dlt's documentation describes it: the client's base URL, headers and paginator, and exactly one resource with its endpoint, query parameters and data selector. Passed to dlt as JSON, so a bracketed query key stays as written and an integer stays an integer. Refused, before any request: no `client.base_url`; other than one resource; `incremental`, `auth`, `processing_steps` or a `resolve` parameter anywhere; a `write_disposition` other than `replace`. |

### Example step

```yaml
- name: fetch_gleif_sec_registrations
  op: dlt@1
  with:
    source:                                  # dlt's rest_api configuration
      client:
        base_url: https://api.gleif.org/api/v1/
        headers:
          User-Agent: "Meridian Protocol (open-analytics; research@meridian.online)"
        paginator: { type: json_link, next_url_path: links.next }
      resources:
        - name: lei_records
          endpoint:
            path: lei-records
            data_selector: data
            params:
              "filter[entity.registeredAt]": RA000665
              "page[size]": 200
              "page[cursor]": "*"
    expect_total: { path: meta.pagination.total, tolerance: 200 }   # optional
    page_delay_seconds: 0.3                                          # optional, default 0
    out: build/gleif_ra_sec.jsonl
```

<!-- arc:reference:end -->
