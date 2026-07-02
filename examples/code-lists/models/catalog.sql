-- catalog.sql — build the frozen `open.ducklake` catalogue for the open zone.
--
-- DuckLake is the queryable index over the published Parquet. The data files are
-- registered BY REFERENCE (ducklake_add_data_files) — they are not rewritten or
-- copied — so the catalogue is a thin metadata layer pointing at ./dist/*.parquet.
-- The license_gate step deletes any prior catalogue before this runs, so each run
-- produces a single, immutable snapshot of the current `as_of` vintage: "frozen".
SET autoinstall_known_extensions = true;
SET autoload_known_extensions = true;
INSTALL ducklake;
LOAD ducklake;

ATTACH 'ducklake:dist/open.ducklake' AS open (DATA_PATH 'dist/');

-- Declare the published schema (idempotent even if a stale catalogue survives).
DROP TABLE IF EXISTS open.naics;
DROP TABLE IF EXISTS open.icd10cm;

CREATE TABLE open.naics   (code VARCHAR, title VARCHAR, level INTEGER, parent VARCHAR, as_of DATE);
CREATE TABLE open.icd10cm (code VARCHAR, title VARCHAR, level INTEGER, parent VARCHAR, as_of DATE);

-- Register the existing Parquet files in place (by reference, no copy).
CALL ducklake_add_data_files('open', 'naics',   'dist/naics.parquet',   ignore_extra_columns => true);
CALL ducklake_add_data_files('open', 'icd10cm', 'dist/icd10cm.parquet', ignore_extra_columns => true);
