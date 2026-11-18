-- The terminal table the export and describe steps read: one row per NAICS code, the
-- code-list contract's ten columns in order, then the list's own.
--
-- corpus: description_full lowercased and accent-stripped, the field the in-browser
-- search reads, as on every code list. It is derived, not a source attribute.
-- `naics` (from load.sql) is read in full, so arc records it as this step's input.
CREATE OR REPLACE TABLE naics_out AS
SELECT
  code, code_display, level, depth, parent_code, description, description_full,
  release, text_source,
  lower(strip_accents(description_full)) AS corpus,
  title, definition,
  sector_code, subsector_code, industry_group_code, naics_industry_code
FROM naics;
