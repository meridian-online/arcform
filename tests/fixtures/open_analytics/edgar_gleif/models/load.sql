-- Load the fetched sources into typed tables and normalise the join keys.
-- Run by the `load` step (depends on all fetches). CIK representations differ across
-- sources (EDGAR unpadded, GLEIF zero-padded) — normalise all to unpadded.

-- ISO 7064 MOD 97-10 — the arithmetic ISO 17442 defines over an LEI's twenty
-- characters, of which the last two are the check digits. Expand each character to its
-- base-36 value and fold left, so nothing overflows a fixed-width integer: a digit
-- contributes one decimal place, a letter (10…35) two. The identifier is well-formed
-- when the remainder is 1. A character outside [0-9A-Z] yields NULL, which is not 1 —
-- the macro is fail-closed on both a malformed value and a NULL one.
--
-- A `pattern` cannot do this job. `00000000000000000000` and `6GG1425B5X0SPG36P158`
-- both satisfy ISO 17442's eighteen-alphanumeric-plus-two-digit shape, and only the
-- arithmetic separates them from an identifier that was actually issued.
--
-- Declared here because this is the first model to read a self-reported LEI. The
-- catalog entry lives in build/edgar_gleif.db, so models/package.sql — which the
-- manifest runs against the same database, after this one — uses it too.
--
-- `list_transform(…, lambda c: …)`, not DuckDB's `[… FOR c IN …]` comprehension. This
-- model declares no `produces:`, so if the engine's SQL parser could not parse this
-- expression, the model would degrade to an OPAQUE step — no assets, no lineage —
-- models/tier.sql would stop depending on it, and an incremental run would skip tier
-- as clean while load rebuilt beneath it: the stale-Parquet hazard arcform.yaml names
-- in its own comments. `lambda c: …` / `lambda acc, v: …` need an `arc` built from
-- arcform commit 25ea222 or later; an older build's vendored parser rejects this form
-- and silently degrades the model instead of failing the run.
CREATE OR REPLACE MACRO lei_mod97(s) AS (
  list_reduce(
    list_prepend(0,
      list_transform(
        regexp_split_to_array(upper(trim(s)), ''),
        lambda c: CASE WHEN c BETWEEN '0' AND '9' THEN ascii(c) - 48
                  WHEN c BETWEEN 'A' AND 'Z' THEN ascii(c) - 55
             END)),
    lambda acc, v: (acc * (CASE WHEN v >= 10 THEN 100 ELSE 10 END) + v) % 97
  )
);

-- The SEC entity universe (the resolution left side + name/ticker enrichment source):
-- ~1.05M named filers incl. former names, tickers where present. cik is already
-- unpadded VARCHAR from the build. Multiple rows per CIK (former names) are collapsed
-- downstream — package.sql dedups EDGAR to one row per CIK, tier.sql dedups edges.
CREATE OR REPLACE TABLE edgar AS
  SELECT cik, company_name, ticker, exchange
  FROM read_parquet('build/sec_entities.parquet');

CREATE OR REPLACE TABLE gleif AS
  SELECT upper(trim(lei)) AS lei, legal_name, country, city, jurisdiction,
         registration_status AS reg_status
  FROM read_parquet('build/gleif.parquet');

-- Deterministic authoritative pairs — GLEIF SEC registrations. Type the SEC
-- identifier by its scheme (SEC-defined: S…=fund series, C…=share class, digits=CIK);
-- unpad CIKs to match EDGAR, keep series/class verbatim. `category` is GLEIF's own
-- authoritative entity type (FUND | GENERAL | BRANCH | …).
CREATE OR REPLACE TABLE gleif_ra_sec AS
  SELECT
    upper(trim(lei)) AS lei,
    CASE WHEN registered_as LIKE 'S%' THEN 'series'
         WHEN registered_as LIKE 'C%' THEN 'class'
         WHEN regexp_full_match(registered_as, '[0-9]+') THEN 'cik'
         ELSE 'other' END AS key_type,
    CASE WHEN regexp_full_match(registered_as, '[0-9]+')
         THEN CAST(registered_as AS BIGINT)::VARCHAR
         ELSE trim(registered_as) END AS key,
    category
  FROM read_csv('build/gleif_ra_sec.csv', header=true, all_varchar=true)
  WHERE lei IS NOT NULL AND registered_as IS NOT NULL AND registered_as <> ''
    -- Keep only the three EDGAR identifier schemes. RA000665 also carries a handful
    -- of SEC investment-adviser file numbers (`805-…`, Form ADV) — a different scheme
    -- that maps to no EDGAR CIK/series/class, so they'd be orphan edges. Drop them.
    AND (registered_as LIKE 'S%' OR registered_as LIKE 'C%'
         OR regexp_full_match(registered_as, '[0-9]+'));

-- SEC Form N-CEN (the annual fund filing itself — most direct authority). Glob over
-- the fetched quarters; union_by_name is robust to column-order drift across releases.
-- REGISTRANT.tsv: registrant CIK ↔ LEI. FUND_REPORTED_INFO.tsv: series SERIES_ID ↔ LEI.
--
-- The LEI on an N-CEN is filer-reported free text, not a validated key: the column
-- carries all-zero placeholders written where a null belongs, and single-character
-- transcriptions of a real identifier (letter O for zero, letter I for one). Neither
-- is an identity, and both pass the ISO 17442 shape, so admit a value only when the
-- MOD 97-10 arithmetic accepts it or GLEIF itself publishes it. The second arm is a
-- forward guard, not slack: `gleif` holds LEIs whose check digits are wrong at the
-- register, and an entity GLEIF publishes is a real one whatever our arithmetic says of
-- its digits. Measured against the published snapshot that arm changes no row; it is
-- here so the first filing to report such an entry keeps it.
--
-- The membership test is written out with both sides qualified; as a macro parameter
-- `s` binds to the subquery's own `lei` column and the EXISTS is then true of every row.
CREATE OR REPLACE TABLE ncen_registrant AS
  SELECT DISTINCT CAST(n.CIK AS BIGINT)::VARCHAR AS key, upper(trim(n.LEI)) AS lei
  FROM read_csv('build/ncen/*/REGISTRANT.tsv', delim='\t', header=true, all_varchar=true, union_by_name=true) n
  WHERE n.LEI IS NOT NULL AND n.LEI <> '' AND n.CIK IS NOT NULL
    AND (lei_mod97(n.LEI) = 1
         OR EXISTS (SELECT 1 FROM gleif g WHERE g.lei = upper(trim(n.LEI))));
CREATE OR REPLACE TABLE ncen_series AS
  SELECT DISTINCT trim(n.SERIES_ID) AS key, upper(trim(n.LEI)) AS lei
  FROM read_csv('build/ncen/*/FUND_REPORTED_INFO.tsv', delim='\t', header=true, all_varchar=true, union_by_name=true) n
  WHERE n.LEI IS NOT NULL AND n.LEI <> '' AND n.SERIES_ID IS NOT NULL AND n.SERIES_ID <> ''
    AND (lei_mod97(n.LEI) = 1
         OR EXISTS (SELECT 1 FROM gleif g WHERE g.lei = upper(trim(n.LEI))));
