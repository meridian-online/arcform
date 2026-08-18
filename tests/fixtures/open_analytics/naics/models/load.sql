-- Load the Census 2022 NAICS descriptions workbook into the published 4-field schema.
-- Run by the `load` step (needs DuckDB's excel extension). Two source quirks handled:
--   • the Census flattens a superscript 'T' (trilateral-comparable marker) onto some
--     titles — strip a trailing 'T';
--   • codes without their own description carry the literal string 'null' (they defer
--     to a child code) — map 'null'/blank to a real NULL.
-- `level` is derived from the code shape: 2-digit codes and the hyphenated 31-33 /
-- 44-45 / 48-49 sector ranges are sectors, then 3/4/5/6 digits descend to national
-- industry.
INSTALL excel; LOAD excel;

CREATE OR REPLACE TABLE naics AS
WITH src AS (
  SELECT
    "Code" AS code,
    regexp_replace(trim("Title"), 'T$', '') AS title,
    CASE WHEN lower(trim(coalesce("Description", ''))) IN ('', 'null') THEN NULL
         ELSE trim(regexp_replace("Description", '\s+', ' ', 'g')) END AS description
  FROM read_xlsx('build/naics_desc.xlsx', header = true, all_varchar = true)
)
SELECT
  code, title,
  CASE WHEN code LIKE '%-%' OR length(code) = 2 THEN 'sector'
       WHEN length(code) = 3 THEN 'subsector'
       WHEN length(code) = 4 THEN 'industry_group'
       WHEN length(code) = 5 THEN 'naics_industry'
       WHEN length(code) = 6 THEN 'national_industry' END AS level,
  description
FROM src;
