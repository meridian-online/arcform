# A general dlt operator: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `dlt@1`, an operator that pages one resource of a REST API through dlt's `rest_api` source configured in the step's `with:`, move open-analytics' GLEIF registration fetch onto it with its table unchanged, and remove `gleif_ra_fetch`.

**Architecture:** A frozen Python script, embedded in arc and run under uv like the other Python operators, builds dlt's `rest_api` resource from the source the step gives, iterates it directly (no pipeline, no destination, no state), and writes each record as one canonical JSON line, sorted, atomically. The Rust side declares the step as ingress, parses `with:` and builds the argv. Field selection moves into the Protocol's `load.sql`.

**Tech Stack:** Rust (arc), Python 3.12 under uv, dlt 1.x pinned exactly, DuckDB 2.0.0.

**Spec:** `docs/superpowers/specs/2026-11-18-dlt-operator-design.md`

## Global Constraints

- Operator name `dlt`, version 1.0.0, addressed as `dlt@1`. Ingress (`Operator::ingress` true): reads nothing, produces `out` as a file.
- The script is `operators/dlt/rest_fetch.py`, never `dlt.py`: a script named `dlt.py` would shadow the `dlt` package on `import dlt`, since the script's own folder is first on `sys.path`. Test files are `test_rest_fetch.py` and `test_rest_fetch_e2e.py` for the same reason.
- PEP 723 block pins `dlt==<newest 1.x on PyPI when Task 1 starts>` exactly, and `requires-python = ">=3.10"`. Record the version in the CHANGELOG entry.
- `with:` keys: `source` (mapping, dlt's `rest_api` configuration, required), `expect_total` (`{path: string, tolerance: integer >= 0, default 0}`, optional), `page_delay_seconds` (number >= 0, optional, default 0), `out` (path, required). Unknown keys refused.
- Output: JSON Lines, each record `json.dumps(record, sort_keys=True, separators=(",", ":"), ensure_ascii=False)`, one per line, each line ending `\n`, lines sorted by their UTF-8 bytes. Zero records writes a zero-byte file. Written to `<out>.part`, renamed to `out` only on success; on any failure `out` is not created or replaced and `<out>.part` is removed.
- Refusals, exit 2, one line on stderr each, before any request: `source` not a mapping; no `client.base_url`; `resources` not a list of exactly one entry; `incremental`, `auth`, `processing_steps` at any depth; a `params` value that is a mapping with `type: resolve`; `write_disposition` present and not `replace`.
- Fetch failures, exit 1, one line on stderr, nothing written: an HTTP error dlt's retries do not recover; a short fetch under `expect_total` (`fetched N of M records; refusing to write a partial file`); an `expect_total.path` absent from the first response body.
- Nothing in the script, its defaults or the catalogue entry names a source, an endpoint, a field or a header value, other than the catalogue's example step. This is the review rule the Splink operator's pull request adds to REVIEW.md.
- Public repos (arcform, open-analytics, landscape): no Kata refs, private repo names or local paths in branch names, commits, PR text or tracked files.
- One cargo build at a time; build and test against DuckDB's v2.0.0 release library, as CLAUDE.md's Known issues describe.

## Review Focus

- A query parameter key with brackets (`filter[entity.registeredAt]`, `page[cursor]`) reaches the request unchanged after YAML to JSON to dlt, and an integer param (`page[size]: 200`) stays an integer.
- Following `links.next` sends the next URL as given, without re-adding the first request's params (dlt's `JSONLinkPaginator` clears them; a duplicate filter or a reset cursor would loop or double-count).
- A record with non-ASCII text (`Société Générale`, `株式会社`) is written as UTF-8, not `\u` escapes, and sorts by bytes the same way on every run.
- A stale `<out>.part` left by a killed run is overwritten, not appended to.
- An `out` that already exists from an earlier good run survives a failed fetch unchanged.

---

### Task 1: The frozen script

**Files:**
- Create: `operators/dlt/rest_fetch.py`
- Create: `operators/dlt/test_rest_fetch.py` (stdlib only)
- Create: `operators/dlt/test_rest_fetch_e2e.py` (under uv, with dlt)

**Interfaces:**
- Produces: CLI `rest_fetch.py --source-json JSON --out PATH [--expect-total-path PATH --expect-total-tolerance N] [--page-delay-seconds X]`; exit 0, 1 or 2 as Global Constraints say.
- Produces, importable without dlt (dlt is imported inside `fetch()`, as `umap_project.py` imports its libraries inside `main()`):
  - `source_problems(source: object) -> list[str]`: one message per refusal, empty when the source can run.
  - `dotted_get(body: object, path: str) -> object`: walks `a.b.c` through mappings; raises `KeyError` with the path when a part is missing.
  - `canonical_line(record: object) -> str`: the serialisation above, without the newline.
  - `write_records(records: list, out: str) -> None`: sorts the canonical lines, writes `<out>.part`, renames.
- Produces, needing dlt: `fetch(source: dict, expect_total_path: str | None, page_delay_seconds: float) -> tuple[list, int | None]`: the records and the first response's total. It builds the resource with `dlt.sources.rest_api.rest_api_resources(source)` and iterates it; it appends its own callable to the endpoint's `response_actions` that records the first response's JSON body and, from the second response on, sleeps `page_delay_seconds`.

- [ ] **Step 1: Write the stdlib tests in `test_rest_fetch.py`.** One test per refusal in Global Constraints, each asserting the message names the key and where it was found (e.g. `resources[0].endpoint.incremental`). Plus:
  - `test_the_gleif_source_has_no_problems`, using the spec's example source.
  - `test_dotted_get_reads_a_nested_count` (`meta.pagination.total` → `28347`) and `test_dotted_get_names_the_missing_part`.
  - `test_canonical_line_sorts_keys_and_keeps_utf8`: `{"b":1,"a":"株式会社"}` → `{"a":"株式会社","b":1}`.
  - `test_write_records_sorts_by_bytes_and_ends_each_line`.
  - `test_write_records_overwrites_a_stale_part`.
  - `test_zero_records_write_an_empty_file`.
- [ ] **Step 2: Run `python3 operators/dlt/test_rest_fetch.py`.** Expected: fails, names undefined.
- [ ] **Step 3: Implement the stdlib functions and `main()`.** `main()` parses args, `json.loads` the source, prints each of `source_problems` and exits 2 if any, calls `fetch`, applies the `expect_total` guard (`len(records) < total - tolerance` exits 1), then `write_records`. Any exception from `fetch` removes `<out>.part` and exits 1 with one line.
- [ ] **Step 4: Run the stdlib tests.** Expected: pass.
- [ ] **Step 5: Write `test_rest_fetch_e2e.py`.** It starts `http.server` on `127.0.0.1:0` in a thread, serving a three-page `json_link` API (`data`, `links.next`, `meta.pagination.total`) that records each request's query string, and runs the script as a subprocess with `RUNTIME__REQUEST_MAX_ATTEMPTS=1` in its environment so failure tests do not wait on retries. Tests:
  - `test_follows_every_page_and_writes_sorted_lines`: five records over three pages; the file's lines equal the expected sorted canonical lines.
  - `test_bracketed_params_reach_the_first_request_and_next_urls_are_followed_as_given`: the first query string holds `filter[x]=1&page[size]=2&page[cursor]=*` (decoded); the second and third are exactly the `links.next` query strings.
  - `test_two_runs_write_identical_bytes`.
  - `test_a_short_fetch_is_refused_and_writes_nothing`: server reports total 10; exit 1; `out` absent.
  - `test_a_failed_page_keeps_the_earlier_file`: `out` pre-written; page 2 returns 500; exit 1; `out` unchanged; no `.part`.
  - `test_a_missing_total_path_is_refused`.
  - `test_an_empty_first_page_writes_an_empty_file`.
- [ ] **Step 6: Run `uv run operators/dlt/test_rest_fetch_e2e.py`.** Expected: pass, offline.
- [ ] **Step 7: Commit.** `Add the dlt operator's frozen script`

### Task 2: The operator in arc's catalogue

**Files:**
- Modify: `src/operator.rs`: a `DLT` static and registry entry; a `"dlt"` arm in the `with:` schema (`source` an object, `expect_total` an object with `path` and `tolerance`, `page_delay_seconds` a number, `out` a string), in `configuration_fields` and in `read_as`; a `Described` entry with `wraps` id `dlt` (name `dlt`, url `https://dlthub.com`, use: pages a REST API from a `rest_api` source configuration and yields its records) and the spec's example step; `struct DltConfig` (`deny_unknown_fields`), `struct ExpectTotal { path: String, #[serde(default)] tolerance: u64 }`, `impl Operator for Dlt`, and `fn dlt_invocation(cfg: &DltConfig, dir: &Path) -> Result<Vec<String>>`. Add `"dlt"` to the test module's `SCRIPT_OPERATORS`.
- Modify: `tests/operation_catalogue.rs`: `"dlt"` in the operator list and in `a_fetcher_reads_nothing_and_a_check_or_a_publish_writes_nothing`'s fetchers.
- Modify: `docs/ARCHITECTURE.md`: `dlt` in the list of ingress operators under `arc test`'s refused tests.
- Create: `operators/dlt/README.md`. One hand-written paragraph above the generated Reference: that `source` is dlt's own `rest_api` configuration and dlt's docs describe it; that records are written raw and shaping them belongs in the Protocol's SQL; that `auth`, incremental loading and several resources are refused, and why.
- Modify: `.github/workflows/ci.yml`: `python3 operators/dlt/test_rest_fetch.py` in the `operators` job; `uv run operators/dlt/test_rest_fetch_e2e.py` in the job that installs uv.

**Interfaces:**
- Consumes: Task 1's CLI.
- Produces: `op: dlt@1`. `assets()` records `out` as an `AssetKind::File` produce and nothing read. `run` creates `out`'s parent, then runs `uv` with `OutputMode::Capture`, so a refusal line reaches the run's error. `dlt_invocation` passes the source as `--source-json` (`serde_json::to_string` of the YAML value), `--out` as `dir.join(out)`, and the optional flags only when set. `materialize_frozen_script("dlt", "1.0.0", DLT_PY)`.

- [ ] **Step 1: Write the Rust unit tests** in the operator tests module:
  - `dlt_is_ingress_and_produces_only_out`
  - `dlt_argv_carries_the_source_as_json_with_bracketed_keys_and_integers_intact`
  - `dlt_argv_omits_unset_options`
  - `dlt_refuses_an_unknown_key` and `dlt_refuses_a_source_that_is_not_a_mapping`
  - `dlt_refuses_a_negative_page_delay`
  - `dlt_resolves_by_semver` (`dlt@1` ok, `dlt@2` err)
- [ ] **Step 2: Run `cargo test --lib dlt_`.** Expected: fails to compile.
- [ ] **Step 3: Implement the config, operator, schema, description and registration.** Config refusals are `Error::ManifestValidation` beginning `dlt: `.
- [ ] **Step 4: Run `cargo test --lib dlt_`, then `ARC_UPDATE_READMES=1 cargo test --lib operator::reference`, then `cargo test --test operation_catalogue`.** Expected: pass, with the generated section written into the new README.
- [ ] **Step 5: Run `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`.** Expected: clean.
- [ ] **Step 6: Commit.** `Add dlt@1 to arc's operator catalogue`

### Task 3: The docs and the changelog (arcform)

**Files:**
- Modify: `docs/ARCHITECTURE.md`, section "Operator descriptions and READMEs": the spec's outcome (what `dlt@1` reads, writes and refuses), and the rulings with their reasons: one source is not an operator; parity is the table, not the file's bytes; raw records with shaping in SQL; no `auth` until secrets management; no incremental state; one resource per step.
- Modify: `CHANGELOG.md` `[Unreleased]` `### Added`: a `dlt@1` entry in the file's style, a bold one-sentence summary and then the reason, naming the pinned dlt version.
- Delete: this plan and the spec, once Tasks 1–4 are done and before the branch review.

- [ ] **Step 1: Make the edits.**
- [ ] **Step 2: Run the full gate** (below). Expected: clean.
- [ ] **Step 3: Commit, push, open the PR** titled `dlt@1: page any REST API from a source the Protocol configures`, and follow methods' build loop to a ready PR: roborev branch review, `gh pr checks --watch`, Forge link. Stop there; Hugh merges.

### Task 4: The GLEIF fetch on dlt@1 (open-analytics)

Starts once Task 3's PR is merged into arcform's `main`.

**Files** (in a worktree of open-analytics on a new branch from `origin/main`):
- Modify: `datasets/edgar_gleif/arcform.yaml`: `fetch_gleif_sec_registrations` becomes the spec's `dlt@1` step, writing `build/gleif_ra_sec.jsonl`, with `expect_total: { path: meta.pagination.total, tolerance: 200 }` and `page_delay_seconds: 0.3`; the load step's input list names the `.jsonl`; comments naming `gleif_ra_fetch` say `dlt`.
- Modify: `datasets/edgar_gleif/models/load.sql`: `gleif_ra_sec` reads `read_ndjson_objects('build/gleif_ra_sec.jsonl')` and takes `json_extract_string(json, '$.attributes.lei') AS lei`, `'$.attributes.entity.registeredAs' AS registered_as`, `'$.attributes.entity.category' AS category`, in a subquery the unchanged `SELECT … WHERE` reads from.
- Modify: `datasets/edgar_gleif/scripts/test_ingest_lei.py`: its fixture writes GLEIF-shaped JSON lines instead of the CSV.
- Modify: the README step table (regenerate as `scripts/check_protocol_readme.py` documents); `ARC_REV` in `.github/workflows/descriptors.yml` and `Cargo.lock` to arcform's merged `main` commit.

- [ ] **Step 1: Build both arcs.** arc at the commit before Task 3's merge (has `gleif_ra_fetch`) and at the merge (has `dlt@1`).
- [ ] **Step 2: Fetch with each, back to back,** into two copies of one build directory: the old step with the old arc, the new step with the new arc. Each takes one to two minutes.
- [ ] **Step 3: Run the `load`, `tier` and `package` steps in each copy** over the same `resolve` output (copied in, not re-run).
- [ ] **Step 4: Compare.** For `gleif_ra_sec`, `tier`'s table and `package`'s table: row counts equal and `EXCEPT` both ways empty. Expected: 0 rows each way. If `gleif_ra_sec` differs, repeat Step 2; a difference that does not survive a back-to-back re-run is GLEIF's data moving.
- [ ] **Step 5: Run the repo's own checks:** `arc validate` on the Protocol, `python3 datasets/edgar_gleif/scripts/test_ingest_lei.py`, `scripts/check_protocol_readme.py`, and the Protocol's `arc test` if it has `tests.yaml`.
- [ ] **Step 6: Commit, push and open the PR,** with the parity counts in its description. Follow the build loop to a ready PR and stop.

### Task 5: Remove gleif_ra_fetch (arcform)

Starts once Task 4's PR is merged and `grep -rn gleif_ra_fetch` over open-analytics' `main` finds nothing.

**Files** (new branch from arcform `origin/main`):
- Delete: `operators/gleif_ra_fetch/`.
- Modify: `src/operator.rs`: remove `GleifRaFetch`, its config, invocation, static, registry entry, `Described` entry, schema arms, its `SCRIPT_OPERATORS` entry and its `OPTIONS_ARC_NEVER_PASSES` entry (which then becomes an empty slice, or goes if nothing else uses it), and its tests.
- Modify: `tests/operation_catalogue.rs`, `docs/ARCHITECTURE.md`'s ingress list, and `src/runner.rs`'s comment that names it.
- Modify: `CHANGELOG.md` `[Unreleased]` `### Removed`: `gleif_ra_fetch@1`, replaced by `dlt@1` with the source in the Protocol.

- [ ] **Step 1: Make the removal.** Run `grep -rn "gleif_ra_fetch\|GleifRaFetch\|GLEIF_RA" src tests docs operators .github`. Expected: only the CHANGELOG entry.
- [ ] **Step 2: Run the full gate.** Expected: clean.
- [ ] **Step 3: Commit, push, open the PR, follow the build loop to ready, and stop.**

### Task 6: acquire.saas names dlt (landscape)

Starts once Hugh answers the spec's open question and `dlt@1` is on arcform's `main`.

- [ ] **Step 1:** In `operators/vocabulary.yaml`, the `acquire.saas` class's `operators` names `dlt` in place of `gleif_ra_fetch`, and its built status follows Hugh's answer, with the page's note on authenticated APIs if he says yes. Regenerate with `operators/build.py` as its docs say, and run its checks.
- [ ] **Step 2: Commit, push, open the PR, follow the build loop to ready, and stop.**

## Verification and what it costs

| Command | Where | Cost |
|---|---|---|
| `python3 operators/dlt/test_rest_fetch.py` | Task 1, CI `operators` | seconds |
| `uv run operators/dlt/test_rest_fetch_e2e.py` | Task 1, CI | under a minute after the first dependency resolve; offline |
| `cargo test --lib dlt_`, `cargo test --test operation_catalogue` | Task 2 | a few minutes warm; one Rust build slot |
| Full gate: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, both Python tests | Tasks 3, 5 | about 15 minutes warm; one Rust build slot |
| Two live GLEIF pulls and the `load`, `tier`, `package` steps twice | Task 4 | about 10 minutes; network to api.gleif.org; no `resolve` re-run |
