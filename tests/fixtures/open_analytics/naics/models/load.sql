-- Load Census's 2022 NAICS descriptions workbook as a ladder of codes, in the code-list
-- contract's shape (docs/code-lists.md) less `corpus`, which package.sql adds.
-- Run by the `load` step, and by the NAICS index's and concordance's `load_naics` steps,
-- which read `code`, `level` and `title` from the table it writes.
--
-- Source quirks handled:
--   • Census flattens a superscript 'T' (the trilateral-comparable marker) onto some
--     titles; a trailing 'T' is stripped.
--   • Codes without a paragraph of their own carry the literal string 'null' (they defer
--     to a child code); 'null' or blank becomes a real NULL.
--
-- Levels, from the code's shape: two digits, or one of Census's three sector ranges
-- (31-33, 44-45, 48-49), is a sector; then 3, 4, 5 and 6 digits descend to the national
-- industry. Below the subsector a code's parent is the code with its last digit dropped.
-- A subsector's parent is the sector whose two digits it starts with, or whose range
-- covers them: 311 sits under 31-33. The run fails on a code of any other shape, a
-- repeated code, a row with no title, a row below the top with no parent at the level
-- above, and a subsector that two sectors cover.
--
-- `description` is Census's title, the row's own text the contract asks for; `title`
-- keeps the same value under the name the table published before the contract, and
-- `definition` is Census's paragraph about the industry. `release` is the edition: the
-- workbook states no revision (see "The NAICS edition" in docs/code-lists.md).
INSTALL excel; LOAD excel;

CREATE OR REPLACE TABLE naics AS
WITH src AS (
  SELECT
    CASE WHEN regexp_full_match(coalesce("Code", ''), '[0-9]{2,6}|[0-9]{2}-[0-9]{2}') THEN "Code"
         ELSE error('NAICS code is neither 2 to 6 digits nor a sector range: ' || coalesce("Code", 'NULL')) END AS code,
    CASE WHEN trim(coalesce("Title", '')) = ''
         THEN error('NAICS code has no title: ' || coalesce("Code", 'NULL'))
         ELSE regexp_replace(trim("Title"), 'T$', '') END AS title,
    CASE WHEN lower(trim(coalesce("Description", ''))) IN ('', 'null') THEN NULL
         ELSE trim(regexp_replace("Description", '\s+', ' ', 'g')) END AS definition
  FROM read_xlsx('build/naics_desc.xlsx', header = true, all_varchar = true)
), leveled AS (
  SELECT *,
    CAST(CASE WHEN code LIKE '%-%' OR length(code) = 2 THEN 0 ELSE length(code) - 2 END AS INTEGER) AS depth,
    -- The two-digit span a sector covers: one value, or a range's two ends.
    substr(code, 1, 2) AS span_lo,
    CASE WHEN code LIKE '%-%' THEN substr(code, 4, 2) ELSE substr(code, 1, 2) END AS span_hi,
    count(*) OVER (PARTITION BY code) AS times
  FROM src
), sectors AS (
  SELECT code, span_lo, span_hi FROM leveled WHERE depth = 0
), sector_of AS (
  -- Each subsector's covering sectors, counted, so a subsector under none or under two is
  -- found here and refused below, on that subsector's own row.
  SELECT l.code, count(s.code) AS sectors, min(s.code) AS sector
  FROM leveled l LEFT JOIN sectors s ON substr(l.code, 1, 2) BETWEEN s.span_lo AND s.span_hi
  WHERE l.depth = 1
  GROUP BY l.code
), parented AS (
  SELECT l.*,
    CASE l.depth WHEN 0 THEN 'sector' WHEN 1 THEN 'subsector' WHEN 2 THEN 'industry_group'
                 WHEN 3 THEN 'naics_industry' ELSE 'national_industry' END AS level,
    CASE
      WHEN l.times > 1 THEN error('NAICS code is repeated: ' || l.code)
      WHEN l.depth = 0 THEN NULL
      WHEN l.depth = 1 THEN
        CASE so.sectors
          WHEN 1 THEN so.sector
          WHEN 0 THEN error('NAICS subsector ' || l.code || ' has no sector above it')
          ELSE error('NAICS subsector ' || l.code || ' falls under more than one sector') END
      WHEN up.code IS NULL
        THEN error('NAICS code ' || l.code || ' has no parent ' || substr(l.code, 1, length(l.code) - 1))
      ELSE up.code
    END AS parent_code
  FROM leveled l
  LEFT JOIN sector_of so ON so.code = l.code
  -- Below the subsector, the code less its last digit, from the distinct codes so a
  -- repeated parent is reported as repeated, not as two parents.
  LEFT JOIN (SELECT DISTINCT code FROM leveled) up
    ON l.depth >= 2 AND up.code = substr(l.code, 1, length(l.code) - 1)
)
-- Ancestors: a row at depth 1 or below has a subsector (itself at depth 1), the first
-- three digits of its code, whose parent is the row's sector; the industry group and
-- NAICS industry are the code's first four and five digits.
SELECT
  c.code,
  c.code AS code_display,
  c.level,
  c.depth,
  c.parent_code,
  c.title AS description,
  concat_ws(' › ', s.title, ss.title, ig.title, ni.title, c.title) AS description_full,
  '2022' AS release,
  'census' AS text_source,
  c.title,
  c.definition,
  s.code AS sector_code,
  ss.code AS subsector_code,
  ig.code AS industry_group_code,
  ni.code AS naics_industry_code
FROM parented c
LEFT JOIN parented sub ON c.depth >= 1 AND sub.code = substr(c.code, 1, 3)
LEFT JOIN parented s ON s.code = sub.parent_code
LEFT JOIN parented ss ON c.depth >= 2 AND ss.code = sub.code
LEFT JOIN parented ig ON c.depth >= 3 AND ig.code = substr(c.code, 1, 4)
LEFT JOIN parented ni ON c.depth >= 4 AND ni.code = substr(c.code, 1, 5)
ORDER BY c.code;
