-- Prepare the two sides of the name match for `score_pairs` (op: splink@1), which takes
-- the model from models/edgar_gleif_model.json and everything else from these files.
-- This SQL and resolved.sql are the crosswalk's own half of the match, moved out of the
-- one-source `splink_resolve` operator unchanged, so the published crosswalk is the same
-- on the same inputs.
--
-- Normalised name (`nname`): uppercase, punctuation to spaces, one trailing corporate
-- suffix (CORP/INC/LLC/LTD/...) stripped, whitespace collapsed.
--
-- EDGAR is every named SEC filer at ticker grain (former names included). Its `uid` is
-- numbered in a fixed order so a re-run numbers the same rows the same way; it is an
-- internal join key and never reaches the published file.
CREATE OR REPLACE TABLE edgar_src AS
WITH n AS (
  SELECT cik, ticker, company_name,
         trim(regexp_replace(regexp_replace(regexp_replace(
             upper(coalesce(company_name, '')), '[^A-Z0-9 ]', ' ', 'g'),
             '\s+(CORPORATION|CORP|INCORPORATED|INC|COMPANY|CO|LIMITED|LTD|LLC|PLC|LP|HOLDINGS|GROUP)\s*$', '', 'g'),
             '\s+', ' ', 'g')) AS nname
  FROM read_parquet('build/sec_entities.parquet')
)
SELECT 'e' || CAST(row_number() OVER (ORDER BY cik, ticker, company_name) AS VARCHAR) AS uid,
       cik, ticker, company_name AS raw_name, nname
FROM n
WHERE nname <> '';

-- GLEIF: every ISSUED, LAPSED or RETIRED registration (a lapsed registration does not
-- change the entity's LEI), keyed by its LEI. `country` (domicile) is a blocking key;
-- `jurisdiction` and `reg_status` feed resolved.sql's tie-break.
CREATE OR REPLACE TABLE gleif_src AS
WITH n AS (
  SELECT lei, legal_name, jurisdiction, country, registration_status,
         trim(regexp_replace(regexp_replace(regexp_replace(
             upper(coalesce(legal_name, '')), '[^A-Z0-9 ]', ' ', 'g'),
             '\s+(CORPORATION|CORP|INCORPORATED|INC|COMPANY|CO|LIMITED|LTD|LLC|PLC|LP|HOLDINGS|GROUP)\s*$', '', 'g'),
             '\s+', ' ', 'g')) AS nname
  FROM read_parquet('build/gleif.parquet')
)
SELECT lei AS uid, legal_name AS raw_name, jurisdiction, country,
       registration_status AS reg_status, nname
FROM n
WHERE nname <> '' AND registration_status IN ('ISSUED', 'LAPSED', 'RETIRED');

-- The blocking keys, the same for both sides. Every name key derives from `core`, the
-- name with one leading stopword token removed (THE, STICHTING, ...: on the full GLEIF
-- copy "THE" alone leads 66k names), so those rows disperse instead of pooling into one
-- block. A key is NULL when its guard fails, which keeps the row out of that block.
--   blk_p6    6-character prefix, paired with country in the model (names >= 6)
--   blk_p8    8-character prefix, no country: foreign near-misses (names >= 8)
--   blk_tok   first token of 4 or more characters, paired with country
--   blk_exact the whole core name, no country: foreign ADRs of any length
-- EDGAR filers are US-listed, so EDGAR's `country` is the literal 'US'.
CREATE OR REPLACE TABLE edgar_in AS
WITH base AS (
  SELECT uid, nname, 'US' AS country,
         CASE WHEN split_part(nname, ' ', 1) IN ('THE', 'STICHTING', 'SOCIETE', 'STIFTUNG', 'FONDATION', 'ASSOCIATION', 'FUNDACION', 'FONDAZIONE')
              THEN substr(nname, length(split_part(nname, ' ', 1)) + 2) ELSE nname END AS core
  FROM edgar_src
)
SELECT uid, nname, country,
       CASE WHEN length(core) >= 6 THEN substr(core, 1, 6) END AS blk_p6,
       CASE WHEN length(core) >= 8 THEN substr(core, 1, 8) END AS blk_p8,
       CASE WHEN length(split_part(core, ' ', 1)) >= 4 THEN split_part(core, ' ', 1) END AS blk_tok,
       CASE WHEN length(core) >= 2 THEN core END AS blk_exact
FROM base;

CREATE OR REPLACE TABLE gleif_in AS
WITH base AS (
  SELECT uid, nname, country,
         CASE WHEN split_part(nname, ' ', 1) IN ('THE', 'STICHTING', 'SOCIETE', 'STIFTUNG', 'FONDATION', 'ASSOCIATION', 'FUNDACION', 'FONDAZIONE')
              THEN substr(nname, length(split_part(nname, ' ', 1)) + 2) ELSE nname END AS core
  FROM gleif_src
)
SELECT uid, nname, country,
       CASE WHEN length(core) >= 6 THEN substr(core, 1, 6) END AS blk_p6,
       CASE WHEN length(core) >= 8 THEN substr(core, 1, 8) END AS blk_p8,
       CASE WHEN length(split_part(core, ' ', 1)) >= 4 THEN split_part(core, ' ', 1) END AS blk_tok,
       CASE WHEN length(core) >= 2 THEN core END AS blk_exact
FROM base;

-- splink@1 reads Parquet, and both inputs must carry the same columns with the same types.
COPY edgar_in TO 'build/edgar_in.parquet' (FORMAT parquet);
COPY gleif_in TO 'build/gleif_in.parquet' (FORMAT parquet);
