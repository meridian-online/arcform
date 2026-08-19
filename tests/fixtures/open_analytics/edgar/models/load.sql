-- Flatten the SEC company_tickers_exchange.json into the published 4-field schema.
-- Run by the `load` step. The SEC file is columnar: a `fields` header naming the
-- columns and a `data` array of positional rows. The field order is stable and
-- documented as ["cik","name","ticker","exchange"] — read `data` as a list of JSON
-- arrays and pick each column by position. CIK is normalised to an unpadded numeric
-- string (matching how EDGAR reports it).
--
-- TODO: robustify against a `fields` reordering by resolving each position from the
-- `fields` header at runtime rather than assuming the documented order.
CREATE OR REPLACE TABLE edgar AS
WITH data_rows AS (
  SELECT unnest(data) AS r
  FROM read_json('build/edgar.json',
                 columns = {'fields': 'VARCHAR[]', 'data': 'JSON[]'})
)
SELECT
  CAST(json_extract_string(r, '$[0]') AS BIGINT)::VARCHAR AS cik,
  json_extract_string(r, '$[1]')                          AS company_name,
  json_extract_string(r, '$[2]')                          AS ticker,
  json_extract_string(r, '$[3]')                          AS exchange
FROM data_rows;
