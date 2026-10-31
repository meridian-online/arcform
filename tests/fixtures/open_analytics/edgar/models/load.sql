-- Flatten the SEC company_tickers_exchange.json into the published 4-field schema.
-- Run by the `load` step. The SEC file is columnar: a `fields` header naming the
-- columns and a `data` array of positional rows. The field order is stable and
-- documented as ["cik","name","ticker","exchange"] — read `data` as a list of JSON
-- arrays and pick each column by position.
--
-- CIK IS AN INTEGER, and the double cast that used to end `::VARCHAR` is why it was not.
-- The BIGINT cast normalises: the SEC file carries the CIK unpadded already, and the cast
-- refuses anything non-numeric rather than passing it through. Re-casting the result back
-- to text then threw that type away, so the built column was VARCHAR while the descriptor
-- declared `integer` — a disagreement `scripts/check_descriptors.py` reports twice, once
-- for the repo's descriptor and once for the copy the object carries about itself.
--
-- The measurement behind `integer`, from the 2026-08-12 crosswalk-join work: of the
-- cik-typed keys in the crosswalk none begins with `0`, all are purely numeric, the values
-- run 1,750 to 2,142,762 with no nulls, and `CAST(cik AS VARCHAR) = edgar_gleif.key`
-- round-trips with no mismatches. An integer loses nothing here. The crosswalk join still
-- renders it as text, which is what that package's `x-joins` declares.
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
  CAST(json_extract_string(r, '$[0]') AS BIGINT)              AS cik,
  json_extract_string(r, '$[1]')                          AS company_name,
  json_extract_string(r, '$[2]')                          AS ticker,
  json_extract_string(r, '$[3]')                          AS exchange
FROM data_rows;
