# splink_resolve

arcform's first typed Python operator (`uv`-run). Resolves **SEC EDGAR** company tickers
(`cik`/`ticker`/`name`) to **GLEIF** legal entities (`lei`) by probabilistic name matching
(Fellegi-Sunter, [Splink](https://moj-analytical-services.github.io/splink/) 4, DuckDB
backend), and publishes the EDGAR↔GLEIF crosswalk as an open dataset.

Both sources are open: SEC EDGAR (public domain), GLEIF golden copy (CC0).

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

```bash
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
