# A general dlt operator, whose REST source comes from the Protocol

Design, 2026-11-18. Awaiting Hugh's approval; nothing below is built. Written while Hugh was away, from the design recorded on the work's issue and in the ruling that a paginated API is fetched by one dlt operator with each source configured in its Protocol. The calls made on his behalf are listed at the end.

## Intent

`gleif_ra_fetch@1` is one API call built as an operator. Its script holds GLEIF's base address, the `lei-records` endpoint, the registration-authority filter, cursor paging, the three fields it keeps (`lei`, `registeredAs`, `category`), a default User-Agent naming open-analytics, and a 0.3-second pause between pages. Any second paginated API would need a second operator, and any change to what the crosswalk takes from GLEIF ships as an arc release.

An operator is what many Protocols reuse; a source is what one Protocol configures. This design adds `dlt@1`, which pages any REST API through dlt's `rest_api` source with the source's settings taken from the step's `with:`, and writes the records it receives. The GLEIF settings move into open-analytics' edgar_gleif Protocol, and the field selection moves into its `load.sql`. Then `gleif_ra_fetch` is removed.

It is the second operator made general in the same week: `splink@1` does the same for the Splink model, and its pull request adds the review rule this operator must pass (an operator whose inputs are named for one source, or whose script holds one dataset's columns, endpoints, paging or model, is a finding at Medium or above).

## Goals

- **One operator for any paginated REST read.** `dlt@1` fetches one resource of a REST API and writes every record it returns. Nothing in its script or its defaults names a source, an endpoint, a field or a header value.
- **The source is dlt's own format.** The step's `source:` is a dlt `rest_api` configuration (`client` with `base_url`, `headers` and `paginator`; `resources` with an `endpoint` of `path`, `params` and `data_selector`), so dlt's documentation describes every field and a source tried in a notebook is pasted in unchanged.
- **Reproducible by construction.** A stateless full pull: no incremental state, no dlt pipeline, no destination. dlt is pinned exactly. Records are written one per line in a canonical form and sorted, so two runs over the same API answer write the same bytes.
- **A failed or short fetch writes nothing.** The file is written to `<out>.part` and renamed only when the fetch finished, and, when the step says where the API reports its total, only when the record count reaches it.
- **The crosswalk moves with its output unchanged.** On the same day's data, edgar_gleif on `dlt@1` builds the same `gleif_ra_sec` table, and publishes the same crosswalk, as on `gleif_ra_fetch@1`.

## Non-goals

- **Incremental loading.** dlt's incremental cursors keep state between runs, which makes a run's output depend on the runs before it. `dlt@1` refuses an `incremental` key anywhere in the source. A later minor version can add it if a Protocol needs it.
- **Authentication.** dlt's `auth` block takes a token or password, which would be written into a committed Protocol. Secrets management is deferred in arc, so `dlt@1` refuses `auth`. A source that needs a key waits for it.
- **Several resources in one step, and parent-child resources.** One step writes one file from one resource. A Protocol that reads three endpoints has three steps. A `resolve` parameter, which feeds one resource's records into another's requests, needs two resources and is refused.
- **dlt's normalisation and destinations.** dlt's pipeline flattens nested records into tables, renames fields to snake case and adds load ids. The operator iterates the resource directly and writes the records as the API sent them; shaping them is the Protocol's SQL.
- **Fixing or extending the crosswalk.** The move changes how GLEIF's RA registrations are fetched and read, and nothing about what the crosswalk does with them.

## Approaches

1. **A general operator over dlt's `rest_api` configuration, writing raw records as JSON Lines** (chosen). The `with:` is dlt's own format, the script owns no source's specifics, and field selection becomes SQL that `arc test` and arc's SQL reader see. Cost: the GLEIF intermediate grows from a three-column CSV of about 1 MB to raw records of tens of MB, and `load.sql` changes from `read_csv` to reading JSON.
2. **A general operator that also selects fields**, taking a list of JSON paths and writing a CSV or Parquet of those columns. It keeps the intermediate small and `load.sql` unchanged, but invents a field-selection language beside dlt's that SQL already provides, and a CSV writer whose quoting must match the old one byte for byte.
3. **Keep `gleif_ra_fetch` and take its specifics as parameters** (base address, filter, fields). That generalises nothing a second API could use: the paging, the endpoint shape and the field paths are still GLEIF's.

Approach 1 is the shape the ruling and bearing's first-class operators design name: the `dlt` operator owns paging, and the Protocol owns the source.

## The operator

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

- **Reads** nothing in the asset graph: it is an ingress operator, like `http_fetch`, so `arc test` asks for a fixture in its place.
- **Writes** one JSON Lines file at `out`: each record the resource yields after its `data_selector`, serialised with keys sorted, no whitespace between tokens and non-ASCII characters kept as UTF-8, one record per line with a trailing newline, and the lines sorted by their UTF-8 bytes. No records writes an empty file.
- **`expect_total`** names a dotted path into the first response's JSON body where the API reports its record count. The fetch is refused, and nothing written, when it returns fewer records than that count minus `tolerance` (default 0). This is the guard `gleif_ra_fetch` carried against a paging failure that stops early and returns a truncated file.
- **`page_delay_seconds`** pauses after each page, for APIs that throttle. dlt's REST client retries failed requests but has no pacing of its own.
- **Refuses**, before any request, each with one line naming what it found: a `source` that is not a mapping or has no `client.base_url`; `resources` with other than exactly one entry; `incremental`, `auth`, `processing_steps` or a `resolve` parameter anywhere in the source; a `write_disposition` other than `replace`; `expect_total` without a `path`, or a negative `tolerance` or `page_delay_seconds`.
- **Fails**, writing nothing, on any HTTP error dlt's retries do not recover, on a body the `data_selector` does not match, and on a short fetch under `expect_total`.
- **Runs** under uv with dlt pinned to one exact release, as a frozen script embedded in arc, the same way the other Python operators run.

## Moving the crosswalk

In open-analytics' edgar_gleif Protocol:

1. The `fetch_gleif_sec_registrations` step becomes the `dlt@1` step above, writing `build/gleif_ra_sec.jsonl`. Its settings are exactly what `fetch_gleif_ra.py` does today: the same base address, endpoint, filter (`filter[entity.registeredAt]`), page size, cursor start, `data_selector`, User-Agent and pause, and the completeness tolerance of one page.
2. `models/load.sql` builds `gleif_ra_sec` from that file, reading each line as a JSON object and taking `attributes.lei`, `attributes.entity.registeredAs` and `attributes.entity.category` as text. Everything after the read (the identifier typing, the CIK unpadding, the filter to the three EDGAR schemes) is unchanged.
3. The load step's declared inputs, the README's step table, the Protocol's comments that name `gleif_ra_fetch`, and `scripts/test_ingest_lei.py`'s fixture (which writes the CSV today) follow the new file.
4. The pinned arc revision moves to one with `dlt@1`.

**Parity.** Run `gleif_ra_fetch@1` and the new step back to back on the same day, each into its own copy of one build directory. They pass when the `gleif_ra_sec` table built from each is the same set of rows with the same count, compared with `EXCEPT` both ways, and the `tier` and `package` steps (the only steps downstream of `gleif_ra_sec`; the four-hour `resolve` step does not read it) give identical tables over the same `resolve` output. A difference that a re-run back to back removes is GLEIF's data changing between the two pulls, not a failure. Then `gleif_ra_fetch` is removed from arc's catalogue, with its script, its README and a CHANGELOG Removed entry.

## Landscape

landscape's `acquire.saas` class (reading from a SaaS or REST API) lists `gleif_ra_fetch` as a one-case operator, which leaves the class "not built" on its public page. Once `dlt@1` is released, the class's `operators` names `dlt` in its place. Whether that makes the class "Runs through an operator" is the open question below.

## Sequence

The work is two pull requests in arcform, one in open-analytics and one in landscape, in this order:

1. arcform: `dlt@1`. Starts from `main` once the Splink operator's pull request has merged, since both change the operator catalogue, the frozen-script operator list in its tests, the CI operator job and the architecture document, and the Splink one adds the review rule this operator is checked against. If it has not merged when the build starts, the branch starts from the Splink branch and rebases onto `main` when it merges.
2. open-analytics: the move, with the parity evidence in its description. It needs `dlt@1` on arcform's `main`, since the Protocol pins an arc commit there. It is independent of the Splink move, which changes the `resolve` step rather than the GLEIF fetch; whichever lands second rebases over the other's pinned arc revision and README step table.
3. arcform: remove `gleif_ra_fetch`, once open-analytics' default branch no longer names it.
4. landscape: `acquire.saas` names `dlt`, once the open question below is answered.

## Calls made on Hugh's behalf

- **Parity is the table and the crosswalk, not the file's bytes.** The issue asks for output byte-identical to `gleif_ra_fetch`'s. The new step writes raw records, not a three-column CSV, so no file of it can match the old CSV's bytes. What can be checked is what the CSV fed: the `gleif_ra_sec` table and the steps downstream of it. If Hugh holds to byte-identical files, Approach 2 returns, and the operator, its output, `load.sql` and every test change with it.
- **Raw records, not selected fields.** The operator writes every field the API returns and the Protocol's SQL chooses, rather than the operator taking a list of fields (Approach 2).
- **JSON Lines, not Parquet.** Raw API records are nested and vary between records; JSON Lines keeps them exactly, where writing Parquet would make the operator infer a schema.
- **No `auth`, no incremental, one resource per step.** Each is refused rather than half supported.
- **Two keys beside dlt's format.** `expect_total` and `page_delay_seconds` carry the two things `gleif_ra_fetch` did that dlt's `rest_api` configuration does not express: refusing a short fetch, and pacing.
- **No default User-Agent.** `gleif_ra_fetch`'s default named open-analytics, which the review rule forbids in an operator. dlt sends its own when the Protocol sets none.

## Open question

Does a general `dlt` operator make `acquire.saas` "Runs through an operator" on Meridian's public operation pages? The one-case rule leaves a class "not built" when its operator covers one case. `dlt@1` covers any paginated REST API without authentication, which is most public data APIs but not most SaaS products, whose APIs need a key. The recommendation is yes, with the page stating that authenticated APIs wait for secrets management. This is a public-facing ruling, so it waits for Hugh; only the landscape step depends on it.

## Left to the builder

- The exact dlt release to pin: the newest 1.x on PyPI when the build starts.
- How the script captures the first response and paces pages (a response hook the script adds to the endpoint is the expected route), and how it reads the dotted `expect_total` path.
- The Rust and Python split of the refusals, provided each refusal is tested once and fires before any request.
