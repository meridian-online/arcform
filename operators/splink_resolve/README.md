# splink_resolve

arcform's first typed Python operator (`uv`-run). Resolves **SEC EDGAR** company tickers
(`cik`/`ticker`/`name`) to **GLEIF** legal entities (`lei`) by probabilistic name matching
(Fellegi-Sunter, [Splink](https://moj-analytical-services.github.io/splink/) 4, DuckDB
backend), and publishes the EDGAR↔GLEIF crosswalk as an open dataset.

Both sources are open: SEC EDGAR (public domain), GLEIF golden copy (CC0).

## Status — spike

- **v0 model** = a fully-specified Fellegi-Sunter name comparison with expert m/u priors:
  frozen and reproducible (fixed seed, no stochastic EM).
- **Name-only signal** — EDGAR carries no address/jurisdiction, so the sole comparison is
  the normalized company name. GLEIF is pre-filtered to `reg_status='ISSUED'`; a
  US-jurisdiction preference breaks ties, and any row with more than one top candidate is
  flagged `status='ambiguous'`.
- **First full run** (GLEIF ISSUED ≈ 1.9M rows): AAPL / COIN / MSFT / NVDA / TSLA all
  resolve to the correct LEI at p ≈ 0.99; ~2,720 confident (≥ 0.99) matches of 10,433
  filers, with a clean bimodal separation (exact-norm vs fuzzy).

## Run

```bash
uv run operators/splink_resolve/resolve.py \
    --edgar edgar.parquet --gleif gleif.parquet --out edgar_gleif_crosswalk.parquet
# --sample N   cap GLEIF ISSUED rows for a fast smoke test
```

## Output columns

`cik, ticker, company_name, lei, legal_name, jurisdiction, match_weight, match_probability, method, status`

## Not built yet

- EM / term-frequency tuning so the fuzzy (Jaro-Winkler) tiers contribute beyond
  exact-normalized-name matches.
- The declared typed I/O contract + arcform-Rust invocation (Python stays at the edges;
  operators declare input/output columns + semantic types so lineage holds at the boundary).
