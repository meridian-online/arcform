# code-lists

A complete, runnable ArcForm pipeline that publishes two **public-domain
reference code lists** — **NAICS 2022** (industry classification, US Census) and
**ICD-10-CM FY2025** (diagnosis codes, CMS) — as normalised, well-sized Parquet
behind a frozen [DuckLake](https://ducklake.select) catalogue.

Where [`brewtrend`](../brewtrend) is the *Practical* reference (small data, fast
iteration), `code-lists` is the *Governed* reference: every stage is gated. A
GREEN-list **license gate** proves the sources are redistributable before any
data is transformed, a **finetype** quality gate proves the output columns before
anything is catalogued, and a **self-validation** gate blocks publication on any
regression.

It exercises the governance-shaped parts of ArcForm end to end:

| Capability | Where |
|---|---|
| Command steps (`curl` + `unzip`) | `fetch_naics`, `fetch_icd10cm` |
| Preconditions (`modified_after`) | each fetch is cached 24h |
| Execution resilience (retry + backoff) | `defaults.retry` |
| Runtime parameter | `as_of`, read in the transforms via `getenv()` |
| **Pre-SQL command gate that blocks** | `license_gate` (default-RED green-list) |
| SQL load → transform → export | `load` → `transform_*` → Parquet |
| **CLI quality gate (finetype)** | `finetype_gate` validates output columns |
| **Frozen lakehouse catalogue** | `catalog` builds `open.ducklake` by reference |
| **Self-validation, block on regression** | `validate` (row deltas, RI, golden rows) |
| Guarded remote publish (R2 → local) | `publish` |
| Frictionless descriptor (SPDX + attribution + as_of) | `datapackage.json` |

## Run it

```sh
cd examples/code-lists
arc run
```

This fetches the two sources into `data/`, clears the license gate, loads and
normalises each list, validates the columns with finetype, writes
`dist/naics.parquet` + `dist/icd10cm.parquet`, builds the frozen `dist/open.ducklake`
catalogue (registering the Parquet **by reference**), self-validates, and
publishes the open zone to `dist/open-zone/` (see *Publishing* below).

Stamp a different snapshot vintage onto the `as_of` column:

```sh
arc run --param as_of=2026-01-01
```

Fetches are cached for 24h; re-running soon after skips the downloads and
recomputes everything downstream. Force a full re-run with `arc run --force`.

## The normalised shape

Both lists are flattened to one shared contract — one row per code:

| column | type | meaning |
|---|---|---|
| `code` | string | the code (NAICS `31-33`; ICD-10-CM `S020XXA`) |
| `title` | string | official title / description |
| `level` | integer | 1 = top of the hierarchy … 5 = most specific |
| `parent` | string | parent `code`, or null for a root |
| `as_of` | date | snapshot date (the `as_of` param) |

`parent` always points at a real `code` (referential integrity is enforced by
construction and re-checked in `validate`), so each list is a self-contained tree.

## The gates

1. **License gate (`scripts/license_gate.sh`)** — a Pre-SQL command step that runs
   *before any transform or publish*. It reads each source's `x-spdx-license` and
   `x-redistribution` from `datapackage.json` and clears the run only if **every**
   source is on the green-list (`x-meridian-license-gate.green`). The policy is
   **default-RED**: an unknown SPDX id or a redistribution flag that isn't
   explicitly `allow` blocks the run (exit non-zero). Flip
   `sources[].x-redistribution` to `"deny"` and re-run to watch it block before a
   single row is read.

2. **finetype quality gate (`scripts/finetype_gate.sh`)** — validates the output
   columns of each Parquet against `schema/*.schema.json` using the finetype CLI.
   A column that violates the contract fails the step and stops the pipeline
   before anything is catalogued or published.

3. **Self-validation (`models/validate.sql`)** — computes row-count deltas against
   a committed baseline, checks referential integrity, diffs a set of golden rows,
   and confirms the frozen catalogue agrees with the Parquet. Any regression calls
   DuckDB's `error()` and blocks the run.

## The frozen catalogue

`models/catalog.sql` builds `dist/open.ducklake` — a DuckLake catalogue that
registers the two Parquet files **by reference** (`ducklake_add_data_files`); the
data is never copied or rewritten. Each run starts from a clean catalogue (reset
by the license gate) so the result is a single immutable snapshot of the current
`as_of` vintage. Query it directly:

```sh
duckdb -c "INSTALL ducklake; LOAD ducklake;
           ATTACH 'ducklake:dist/open.ducklake' AS open (DATA_PATH 'dist/', READ_ONLY);
           SELECT * FROM open.naics WHERE level = 1 ORDER BY code;"
```

## Publishing (open zone)

`scripts/publish.sh` uploads the artifacts to the **R2 open zone**, but is guarded:
when R2 credentials are absent it **stages locally and does not fail**. Out of the
box the step stages the open-zone layout under `dist/open-zone/code-lists/` and
writes a `_publish_receipt.json`
(sha256 + as_of + target key prefix). Provide `R2_ACCOUNT_ID`, `R2_ACCESS_KEY_ID`,
`R2_SECRET_ACCESS_KEY`, `R2_BUCKET` (and `rclone`) to switch to a real upload.

## Output

- `dist/naics.parquet`, `dist/icd10cm.parquet` — the normalised code lists (zstd,
  ~123k-row groups).
- `dist/open.ducklake` — the frozen catalogue registering those Parquet by reference.
- `dist/open-zone/code-lists/` — the staged open zone (+ publish receipt).
- `datapackage.json` — the [Frictionless Data Package](https://datapackage.org)
  descriptor: SPDX, attribution, and `as_of` per source, plus the green-list policy
  the license gate reads. This descriptor *describes* the data; it is **not**
  the runnable artifact (`arc run` executes `arcform.yaml`).

## Requirements

- The `duckdb` CLI on your `PATH` (with the `excel` and `ducklake` extensions —
  autoloaded on first run, which needs network once).
- `curl`, `unzip`, and `jq` for the fetch + license-gate steps.
- The `finetype` CLI on your `PATH` for the quality gate.

> **Building `arc` on macOS:** if the build fails with `ld: library 'duckdb' not
> found`, point the linker at Homebrew's copy:
> `LIBRARY_PATH=/opt/homebrew/lib cargo build`.
