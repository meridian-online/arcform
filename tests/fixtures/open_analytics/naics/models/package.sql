-- Materialise the terminal table the export + describe steps consume. GRAIN: one row
-- per NAICS code. The COPY that used to live here is now the first-class
-- parquet_export@1 operator (a graph asset the engine orders + stale-propagates, not an
-- opaque COPY it can't parse); the ORDER BY code + zstd move there. This model only adds
-- the derived search corpus and hands off a typed table.
--
-- corpus: a lowered, accent-stripped concatenation of title + description, powering
-- the free-lane in-browser search (concat_ws skips a NULL description, so no stray
-- 'null' text leaks in). Hidden from the grid; searched, not displayed.
CREATE OR REPLACE TABLE naics_out AS
SELECT
  code, title, level, description,
  lower(strip_accents(concat_ws(' ', title, description))) AS corpus
FROM naics;
