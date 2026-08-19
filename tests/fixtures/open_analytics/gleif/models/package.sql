-- Materialise the terminal table the export + describe steps consume. GRAIN: one row
-- per LEI. The COPY that used to live here is now the first-class parquet_export@1
-- operator (a graph asset the engine orders + stale-propagates, not an opaque COPY it
-- can't parse); the ORDER BY + zstd move there. This model only adds the derived search
-- corpus and hands off a typed table.
--
-- corpus: a lowered, accent-stripped concatenation of the key text fields, powering
-- the free-lane in-browser search (single-column ILIKE, pre-lowered) and the future
-- embedding field. Hidden from the grid; searched, not displayed.
CREATE OR REPLACE TABLE gleif_out AS
SELECT
  lei, legal_name, country, city, jurisdiction, legal_form, entity_status,
  registration_status, initial_registration, next_renewal,
  lower(strip_accents(concat_ws(' ', legal_name, city, country))) AS corpus
FROM gleif;
