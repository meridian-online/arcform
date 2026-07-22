-- load.sql — stage the two fetched code-list sources into DuckDB tables.
--
-- Reads the NAICS 2022 structure workbook (US Census) and the ICD-10-CM order
-- file (CMS) into raw tables. The downstream transform steps normalise these
-- into the shared code/title/level/parent/as_of shape and export Parquet.
--
-- The Census source is an .xlsx, so enable the DuckDB `excel` extension. Autoload
-- lets a first run install it, matching the same fetch-then-run model the fetch
-- steps rely on (cached thereafter, no network on later runs).
SET autoinstall_known_extensions = true;
SET autoload_known_extensions = true;
INSTALL excel;
LOAD excel;

-- NAICS 2022 structure: the workbook has merged title/notes cells at the top and
-- lays the data out with the code in column B and the title in column C. Read an
-- explicit A:D range (all text) and keep only rows whose column B is a real NAICS
-- code: 2–6 digits, or a sector range such as 31-33. This also drops the lone
-- stray footnote row that happens to begin with a digit.
CREATE OR REPLACE TABLE naics_raw AS
SELECT
    trim(B) AS code,
    trim(C) AS title
FROM read_xlsx(
    'data/naics/naics_structure.xlsx',
    header      = false,
    all_varchar = true,
    range       = 'A1:D3000',
    stop_at_empty = false
)
WHERE B IS NOT NULL
  AND regexp_matches(trim(B), '^(\d{2}-\d{2}|\d{2,6})$');

-- ICD-10-CM FY order file: a fixed-width text file, one code per line. Columns are
-- positional (1-indexed): 7–13 code, 15 billable flag, 17–76 short title, 78– long
-- title. Read each whole line as a single field by choosing a delimiter byte that
-- never occurs in the data (unit separator, chr(31)); slice by position here.
CREATE OR REPLACE TABLE icd10cm_raw AS
SELECT
    trim(substr(line, 7, 7))                AS code,
    substr(line, 15, 1)                     AS billable,
    trim(substr(line, 17, 60))              AS short_title,
    trim(rtrim(substr(line, 78)), chr(13))  AS long_title
FROM read_csv(
    'data/icd10cm/icd10cm_order.txt',
    delim         = chr(31),
    header        = false,
    quote         = '',
    escape        = '',
    columns       = {'line': 'VARCHAR'},
    ignore_errors = true
)
WHERE trim(substr(line, 7, 7)) <> '';
