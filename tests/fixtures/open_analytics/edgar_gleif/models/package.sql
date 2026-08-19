-- Enrich every edge from the sources (by LEI → GLEIF; by CIK → EDGAR), stamp the
-- as_of, build a search corpus, and MATERIALISE the terminal table the `parquet_export`
-- operator writes to build/edgar_gleif.parquet (the Parquet the out-of-repo publish
-- seam consumes). Funds (key_type=series) don't join to EDGAR, so their display name
-- comes from GLEIF's legal_name via coalesce.
--
-- GRAIN: one row per (key_type, key, lei) edge. A CIK can carry several tickers, so
-- EDGAR is deduped to one row per CIK with the tickers aggregated (a comma list) —
-- otherwise the join fans an edge out into one row per ticker. (Ticker-grain vs
-- CIK-grain is a deliberate modelling choice; see README.)
--
-- Why a TABLE (not a COPY, not a VIEW): the terminal write is now the typed
-- `parquet_export` operator (arcform), so this step just builds the row set and the
-- operator owns the file — the export becomes a first-class produced asset instead of
-- a COPY the engine can't parse (which was a graph ISLAND that could silently ship a
-- stale Parquet on a green run). A TABLE, not a VIEW, because `as_of` reads
-- `getenv('ARC_PARAM_AS_OF')`, which must be evaluated HERE (this step's env) — a lazy
-- view would defer it to the export step's context and stamp NULL. The row ORDER is
-- applied by `parquet_export`'s `order_by` (a total order — see arcform.yaml).
CREATE OR REPLACE TABLE edgar_gleif_out AS
  SELECT
    x.key_type,
    x.key,
    e.ticker,
    coalesce(e.company_name, g.legal_name) AS company_name,
    x.lei,
    g.legal_name,
    coalesce(x.category, 'GENERAL')        AS category,
    g.jurisdiction,
    g.reg_status,
    x.method,
    x.tier,
    round(x.confidence, 6)                 AS confidence,
    CAST(nullif(getenv('ARC_PARAM_AS_OF'), '') AS DATE) AS as_of,
    lower(strip_accents(concat_ws(' ',
      coalesce(e.company_name, g.legal_name), g.legal_name, e.ticker, x.lei))) AS corpus
  FROM crosswalk_edges x
  LEFT JOIN gleif g ON g.lei = x.lei
  LEFT JOIN (
    SELECT cik,
           string_agg(DISTINCT ticker, ',' ORDER BY ticker) AS ticker,
           -- A CIK carries several name variants (former names). `min` picks one
           -- DETERMINISTICALLY; `any_value` returned an arbitrary variant that varied
           -- run-to-run (DuckDB parallel scan), making both the displayed name AND the
           -- export's sort key non-reproducible. (The row SET is unchanged either way.)
           min(company_name)                                AS company_name
    FROM edgar GROUP BY cik
  ) e ON x.key_type = 'cik' AND e.cik = x.key;

-- GATE the terminal table on the identifier, and stop the Run rather than export an
-- edge whose LEI names nothing. An edge is an assertion that a filer IS a given legal
-- entity, so an identifier that fails ISO 7064 MOD 97-10 and that GLEIF does not
-- publish is not a weak edge — it is not an edge, and no `tier` value makes it one.
--
-- models/load.sql filters the one source known to report placeholders (N-CEN, whose
-- LEI field is filer-reported free text). This covers the routes it does not: the
-- RA000665 register and the resolver, where a value failing both tests means something
-- upstream is broken and the honest response is to fail. `lei_mod97` is the macro
-- models/load.sql declares in this database.
--
-- `IS DISTINCT FROM` rather than `<>`: a NULL remainder is a malformed identifier, and
-- `NULL <> 1` would let it through as unknown.
SELECT CASE WHEN v.edges > 0 THEN error(
         'edgar_gleif: ' || v.edges || ' edge(s) carry an LEI that fails ISO 7064 MOD 97-10 '
         || 'and is not published by GLEIF — ' || v.distinct_leis || ' distinct value(s), '
         || 'e.g. ' || v.examples) END
FROM (
  SELECT count(*) AS edges,
         count(DISTINCT o.lei) AS distinct_leis,
         array_to_string(list_slice(list_sort(list(DISTINCT o.lei)), 1, 5), ', ') AS examples
  FROM edgar_gleif_out o
  WHERE lei_mod97(o.lei) IS DISTINCT FROM 1
    AND NOT EXISTS (SELECT 1 FROM gleif g WHERE g.lei = o.lei)
) v;
