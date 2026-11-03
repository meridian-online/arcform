# splink_resolve

arcform's first typed Python operator (`uv`-run). Resolves **SEC EDGAR** company tickers
(`cik`/`ticker`/`name`) to **GLEIF** legal entities (`lei`) by probabilistic name matching
(Fellegi-Sunter, [Splink](https://moj-analytical-services.github.io/splink/) 4, DuckDB
backend), and publishes the EDGAR↔GLEIF crosswalk as an open dataset, at
`https://openlake.meridian.online/library/edgar_lei.parquet`.

Both sources are open: SEC EDGAR (public domain), GLEIF golden copy (CC0).

<!-- arc:reference:start -->

## Reference

<!-- Generated from arc's operator catalogue (src/operator.rs). Edit it there, then run `ARC_UPDATE_READMES=1 cargo test --lib operator::reference`. -->

Matches EDGAR entities to GLEIF entities with Splink and writes the resolved crosswalk.

### What it wraps

- [Splink](https://moj-analytical-services.github.io/splink/): Splink 4's Fellegi-Sunter model scores each EDGAR and GLEIF name pair, with fixed priors so a run is reproducible.
- [DuckDB](https://duckdb.org): The backend Splink runs its comparisons on.

### Reads

| Key | Names | Required | Description |
|---|---|---|---|
| `edgar` | file | yes | The EDGAR entity Parquet file, the crosswalk's left side. |
| `gleif` | file | yes | The GLEIF golden-copy Parquet file, the crosswalk's right side. |

### Writes

| Key | Names | Required | Description |
|---|---|---|---|
| `out` | file | yes | The Parquet file the resolved crosswalk is written to. |

### Parameters

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `sample` | integer | no | `0` | Optional GLEIF-row cap for a fast smoke test; 0 resolves the full corpus. |

### Example step

```yaml
- name: resolve
  op: splink_resolve@1
  with:
    edgar: raw/edgar.parquet
    gleif: raw/gleif.parquet
    out: build/edgar_gleif_crosswalk.parquet
```

<!-- arc:reference:end -->

## Status — spike

- **Model (v1)** = a fully-specified Fellegi-Sunter name comparison with expert m/u priors:
  frozen and reproducible (fixed seed, no stochastic EM).
- **Name-only signal** — EDGAR carries no address/jurisdiction, so the sole comparison is
  the normalized company name. All real GLEIF registration statuses (ISSUED/LAPSED/RETIRED)
  are matched and `reg_status` is carried through (a lapsed registration doesn't change the
  entity's LEI); ties break by ISSUED > LAPSED > RETIRED then a US-jurisdiction preference,
  and residual ties are flagged `status='ambiguous'`.
- **Precision-first** — only exact-normalized-name matches auto-confirm (spot-checked
  ~95% precise). Name-only fuzzy (Jaro-Winkler) matches are only ~55% precise, so they are
  surfaced as `status='candidate'`, never auto-confirmed.
- **Full run** (GLEIF ≈ 3.2M rows, ticker grain): AAPL / COIN / MSFT / NVDA / TSLA all
  resolve to the correct LEI; **4,904 confirmed (47%)** + 186 ambiguous + 1,480 candidate,
  of 10,433 EDGAR ticker-rows.

## Run

Fetch the two inputs first, from Meridian's open datasets:

- `edgar.parquet` from `https://openlake.meridian.online/sec/company_tickers_exchange.parquet`
- `gleif.parquet` from `https://openlake.meridian.online/gleif/lei.parquet`

```bash
curl -fsSLo edgar.parquet https://openlake.meridian.online/sec/company_tickers_exchange.parquet
curl -fsSLo gleif.parquet https://openlake.meridian.online/gleif/lei.parquet
uv run operators/splink_resolve/resolve.py \
    --edgar edgar.parquet --gleif gleif.parquet --out edgar_gleif_crosswalk.parquet
# --sample N   cap GLEIF ISSUED rows for a fast smoke test
```

## Output columns

`cik, ticker, company_name, lei, legal_name, jurisdiction, match_weight, match_probability, method, status`

## Not built yet

- A **second comparison signal** (address, ISIN, or ticker cross-reference) to promote fuzzy
  candidates to confirmed — name alone cannot (~55% precise). This is the lever for recall
  beyond exact-name matching.
- The declared typed I/O contract + arcform-Rust invocation (Python stays at the edges;
  operators declare input/output columns + semantic types so lineage holds at the boundary).
