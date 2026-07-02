-- transform_naics.sql — normalise NAICS into code/title/level/parent/as_of and
-- export a well-sized Parquet under ./dist. Reads naics_raw (from load.sql).
--
-- `as_of` is the snapshot date (arcform param, default 2025-01-01), injected into
-- the child process as ARC_PARAM_AS_OF and read here via getenv().
SET VARIABLE as_of = CAST(getenv('ARC_PARAM_AS_OF') AS DATE);

-- Level from code shape: sectors (2-digit, or a range like 31-33) are level 1;
-- otherwise level = length - 1 (3-digit subsector = 2 ... 6-digit national = 5).
-- Titles carry trailing change/trilateral markers (T, *) — strip them.
CREATE OR REPLACE TABLE naics_norm AS
SELECT
    code,
    regexp_replace(title, '[T*]+$', '') AS title,
    CASE WHEN code LIKE '%-%' OR length(code) = 2
         THEN 1 ELSE length(code) - 1 END AS level
FROM naics_raw;

-- parent: sectors are roots (NULL). A 3-digit subsector hangs off its sector,
-- which may be a range (e.g. 311 -> 31-33), so match either the 2-digit prefix or
-- the covering range. Deeper codes hang off their (length-1) prefix, which always
-- exists in the structure — giving referential integrity by construction.
COPY (
    SELECT
        n.code,
        n.title,
        n.level::INTEGER AS level,
        CASE
            WHEN n.level = 1 THEN NULL
            WHEN length(n.code) = 3 THEN (
                SELECT s.code
                FROM naics_norm s
                WHERE s.level = 1
                  AND ( s.code = substr(n.code, 1, 2)
                        OR ( s.code LIKE '%-%'
                             AND substr(n.code, 1, 2)
                                 BETWEEN split_part(s.code, '-', 1)
                                     AND split_part(s.code, '-', 2) ) )
                LIMIT 1)
            ELSE substr(n.code, 1, length(n.code) - 1)
        END AS parent,
        getvariable('as_of') AS as_of
    FROM naics_norm n
    ORDER BY n.code
) TO 'dist/naics.parquet' (FORMAT parquet, COMPRESSION zstd, ROW_GROUP_SIZE 122880);
