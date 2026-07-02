-- transform_icd10cm.sql — normalise ICD-10-CM into code/title/level/parent/as_of
-- and export a well-sized Parquet under ./dist. Reads icd10cm_raw (from load.sql).
SET VARIABLE as_of = CAST(getenv('ARC_PARAM_AS_OF') AS DATE);

-- Level from code length: 3-char category = 1 ... 7-char code = 5. Precompute the
-- candidate prefixes so the parent lookup is a set of plain equijoins (a hash
-- join per prefix length) rather than a correlated / range join.
CREATE OR REPLACE TABLE icd10cm_norm AS
SELECT
    code,
    long_title                  AS title,
    (length(code) - 2)::INTEGER AS level,
    CASE WHEN length(code) > 6 THEN substr(code, 1, 6) END AS pre6,
    CASE WHEN length(code) > 5 THEN substr(code, 1, 5) END AS pre5,
    CASE WHEN length(code) > 4 THEN substr(code, 1, 4) END AS pre4,
    CASE WHEN length(code) > 3 THEN substr(code, 1, 3) END AS pre3
FROM icd10cm_raw;

CREATE OR REPLACE TABLE icd10cm_codeset AS
SELECT DISTINCT code FROM icd10cm_norm;

-- parent = the longest strict prefix that is itself a code. This guarantees
-- referential integrity even for 7-char extension codes whose immediate 6-char
-- prefix does not exist (e.g. S020XXA -> S020). 3-char categories are roots (NULL).
COPY (
    SELECT
        b.code,
        b.title,
        b.level,
        COALESCE(p6.code, p5.code, p4.code, p3.code) AS parent,
        getvariable('as_of') AS as_of
    FROM icd10cm_norm b
    LEFT JOIN icd10cm_codeset p6 ON p6.code = b.pre6
    LEFT JOIN icd10cm_codeset p5 ON p5.code = b.pre5
    LEFT JOIN icd10cm_codeset p4 ON p4.code = b.pre4
    LEFT JOIN icd10cm_codeset p3 ON p3.code = b.pre3
    ORDER BY b.code
) TO 'dist/icd10cm.parquet' (FORMAT parquet, COMPRESSION zstd, ROW_GROUP_SIZE 122880);
