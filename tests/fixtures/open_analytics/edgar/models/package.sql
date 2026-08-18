-- Materialise the terminal table the export + describe steps consume. GRAIN: one row
-- per (cik, ticker). The COPY that used to live here is now the first-class
-- parquet_export@1 operator (a graph asset the engine can order + stale-propagate, not
-- an opaque COPY it can't parse); the ORDER BY + zstd move there. This model only adds
-- the derived search corpus and hands off a typed table.
--
-- corpus: a lowered, accent-stripped concatenation of name + ticker + exchange,
-- powering the free-lane in-browser search (single-column ILIKE, pre-lowered).
-- Hidden from the grid; searched, not displayed.
CREATE OR REPLACE TABLE edgar_out AS
SELECT
  cik, company_name, ticker, exchange,
  lower(strip_accents(concat_ws(' ', company_name, ticker, exchange))) AS corpus
FROM edgar;
