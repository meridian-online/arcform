-- Build the SEC entity resolution universe — the LEFT side of the crosswalk at full
-- scale — as a table the `parquet_export` operator writes to build/sec_entities.parquet.
--
-- Every named SEC filer (CIK + name) from the full cik-lookup list (~1.05M name-variant
-- rows, INCLUDING SEC former names → extra match surface), with ticker/exchange joined
-- where the filer has one. Individuals (insider Form 3/4/5 filers) are left in: they
-- carry no LEI, so they can never match GLEIF and the crosswalk OUTPUT is entity-only
-- by construction.
--
-- Re-expressed from the retired scripts/fetch_sec_entities.sh: the `curl` of
-- cik-lookup-data.txt is now the `fetch_cik_lookup` http_fetch step (→ build/cik_lookup.txt),
-- and this SQL is the DuckDB transform that script inlined. `min(...)` replaces the
-- script's `any_value(...)` for the per-CIK ticker/exchange pick so the build is
-- DETERMINISTIC (any_value returned an arbitrary row under DuckDB's parallel scan);
-- ticker/exchange are only enrichment (package.sql re-aggregates tickers), so the pick
-- doesn't affect the crosswalk edge set, only which single ticker label is carried.
CREATE OR REPLACE TABLE sec_entities AS
  WITH ck AS (
    -- CIK is the trailing :NNNNNNNNNN: ; names may themselves contain colons, so parse
    -- the CIK off the end by regex rather than splitting on ':'.
    SELECT CAST(regexp_extract(line, ':([0-9]{10}):$', 1) AS BIGINT)::VARCHAR AS cik,
           regexp_replace(line, ':[0-9]{10}:$', '') AS company_name
    FROM read_csv('build/cik_lookup.txt', delim='\x1e', header=false, all_varchar=true,
                  columns={'line':'VARCHAR'}, ignore_errors=true)
    WHERE line SIMILAR TO '.*:[0-9]{10}:' AND regexp_replace(line, ':[0-9]{10}:$', '') <> ''
  ),
  tk AS (
    SELECT CAST(cik AS BIGINT)::VARCHAR AS cik, min(ticker) AS ticker, min(exchange) AS exchange
    FROM read_parquet('build/edgar.parquet') GROUP BY cik
  )
  SELECT c.cik, c.company_name, t.ticker, t.exchange
  FROM ck c LEFT JOIN tk t USING (cik);
