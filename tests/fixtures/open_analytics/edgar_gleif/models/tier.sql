-- Build the tiered, multi-grain edge set: one row per asserted identity, keyed by a
-- typed SEC identifier (cik | series | class). Tiers, in precedence order:
--   authoritative  — deterministic, no guessing (SEC registration; N-CEN next)
--   confirmed      — exact normalised-name Splink match (~95% precision)
--   candidate      — fuzzy name match, surfaced not asserted
--   ambiguous      — one exact name matched more than one entity
-- A deterministic edge always wins over a name-match for the same (key_type, key).
-- FIRST CUT — precedence + multi-grain shape are here; N-CEN registrant layer (§1c)
-- and cross-source dedup refinements are the next iteration (see README).

-- 1) Authoritative edges — OFFICIAL sources only (SEC + GLEIF). No crowd-sourced
--    data (Wikidata is kept out of the published pipeline; it's an out-of-band
--    validation cross-check only).
CREATE OR REPLACE TABLE authoritative AS
-- 1a) GLEIF entities registered at SEC EDGAR (RA000665).
SELECT key_type, key, lei, category, 'sec-registration' AS method,
       'authoritative' AS tier, 1.0 AS confidence
FROM gleif_ra_sec
-- 1b) SEC N-CEN — registrant CIK ↔ LEI (the fund company/trust).
UNION ALL BY NAME
SELECT 'cik' AS key_type, key, lei, 'FUND' AS category, 'sec-ncen' AS method,
       'authoritative' AS tier, 1.0 AS confidence
FROM ncen_registrant
-- 1c) SEC N-CEN — series SERIES_ID ↔ LEI (each fund series).
UNION ALL BY NAME
SELECT 'series' AS key_type, key, lei, 'FUND' AS category, 'sec-ncen' AS method,
       'authoritative' AS tier, 1.0 AS confidence
FROM ncen_series;

-- Dedup identical edges (belt-and-braces; RA000665 shouldn't duplicate a key↔lei).
CREATE OR REPLACE TABLE authoritative_d AS
SELECT DISTINCT ON (key_type, key, lei)
       key_type, key, lei, category, method, tier, confidence
FROM authoritative
ORDER BY key_type, key, lei;

-- 2) Probabilistic name-match edges (operating-company tail), CIK grain. The resolver
--    is ticker-grained, so a multi-ticker company yields several identical (cik,lei)
--    rows — dedup to one edge, keeping the highest match probability. Drop any CIK
--    already covered by an authoritative edge (authoritative wins).
CREATE OR REPLACE TABLE probabilistic AS
SELECT DISTINCT ON (key, lei) key_type, key, lei, category, method, tier, confidence
FROM (
  SELECT 'cik' AS key_type, CAST(r.cik AS BIGINT)::VARCHAR AS key, upper(r.lei) AS lei,
         NULL AS category, r.method, r.status AS tier, r.match_probability AS confidence
  FROM read_parquet('build/resolved.parquet') r
  WHERE r.status IN ('confirmed', 'candidate', 'ambiguous')
    AND NOT EXISTS (
      SELECT 1 FROM authoritative_d a
      WHERE a.key_type = 'cik' AND a.key = CAST(r.cik AS BIGINT)::VARCHAR
    )
)
ORDER BY key, lei, confidence DESC;

-- 3) The unified edge set (enriched from the sources in package.sql).
CREATE OR REPLACE TABLE crosswalk_edges AS
SELECT * FROM authoritative_d
UNION ALL BY NAME
SELECT * FROM probabilistic;
