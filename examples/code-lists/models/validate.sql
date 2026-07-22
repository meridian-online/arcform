-- validate.sql — self-validation gate. Blocks the pipeline (non-zero exit via
-- error()) on any regression, so a bad build can never reach publish. Checks:
--   1. row-count delta vs committed baseline (catastrophic drift guard)
--   2. referential integrity (every non-null parent exists as a code)
--   3. golden rows (structural anchors present and exact)
--   4. catalogue agreement (frozen open.ducklake matches the Parquet)
SET autoinstall_known_extensions = true;
SET autoload_known_extensions = true;
INSTALL ducklake;
LOAD ducklake;
ATTACH 'ducklake:dist/open.ducklake' AS open (DATA_PATH 'dist/', READ_ONLY);

-- Committed baselines (last known-good counts). A routine source refresh may
-- drift within +/-10%; an empty or partial fetch trips the delta guard.
CREATE OR REPLACE TEMP TABLE baseline(list, expected_rows) AS
    VALUES ('naics', 2125), ('icd10cm', 97584);

CREATE OR REPLACE TEMP TABLE actual AS
    SELECT 'naics'   AS list, count(*) AS n FROM read_parquet('dist/naics.parquet')
    UNION ALL
    SELECT 'icd10cm' AS list, count(*) AS n FROM read_parquet('dist/icd10cm.parquet');

-- 1) row-count delta guard
CREATE OR REPLACE TEMP TABLE rowcount_check AS
    SELECT b.list, b.expected_rows, a.n AS actual_rows
    FROM baseline b JOIN actual a USING (list)
    WHERE a.n < b.expected_rows * 0.90
       OR a.n > b.expected_rows * 1.10;

-- 2) referential integrity
CREATE OR REPLACE TEMP TABLE ri_check AS
    SELECT 'naics' AS list, count(*) AS orphans
    FROM read_parquet('dist/naics.parquet') c
    WHERE c.parent IS NOT NULL
      AND NOT EXISTS (SELECT 1 FROM read_parquet('dist/naics.parquet') p WHERE p.code = c.parent)
    UNION ALL
    SELECT 'icd10cm', count(*)
    FROM read_parquet('dist/icd10cm.parquet') c
    WHERE c.parent IS NOT NULL
      AND NOT EXISTS (SELECT 1 FROM read_parquet('dist/icd10cm.parquet') p WHERE p.code = c.parent);

-- 3) golden rows (structural anchors)
CREATE OR REPLACE TEMP TABLE golden(list, code, title, level, parent) AS VALUES
    ('naics',   '111110',  'Soybean Farming',    5, '11111'),
    ('naics',   '31-33',   'Manufacturing',      1, NULL),
    ('naics',   '311',     'Food Manufacturing', 2, '31-33'),
    ('icd10cm', 'A00',     'Cholera',            1, NULL),
    ('icd10cm', 'S020XXA', 'Fracture of vault of skull, initial encounter for closed fracture', 5, 'S020');

CREATE OR REPLACE TEMP TABLE golden_check AS
    SELECT g.*
    FROM golden g
    LEFT JOIN (
        SELECT 'naics'   AS list, code, title, level, parent FROM read_parquet('dist/naics.parquet')
        UNION ALL
        SELECT 'icd10cm', code, title, level, parent FROM read_parquet('dist/icd10cm.parquet')
    ) d
      ON  d.list  = g.list
      AND d.code  = g.code
      AND d.title = g.title
      AND d.level = g.level
      AND d.parent IS NOT DISTINCT FROM g.parent
    WHERE d.code IS NULL;

-- 4) catalogue agreement
CREATE OR REPLACE TEMP TABLE catalog_check AS
    SELECT 'naics'   AS list, (SELECT count(*) FROM open.naics)   AS cat_n, (SELECT n FROM actual WHERE list = 'naics')   AS par_n
    UNION ALL
    SELECT 'icd10cm', (SELECT count(*) FROM open.icd10cm), (SELECT n FROM actual WHERE list = 'icd10cm');

-- Block on the first failing check; otherwise report success.
SELECT CASE
    WHEN (SELECT count(*) FROM rowcount_check) > 0
        THEN error('validate: row-count delta out of tolerance — ' ||
             (SELECT string_agg(list || ' expected~' || expected_rows || ' got ' || actual_rows, '; ') FROM rowcount_check))
    WHEN (SELECT sum(orphans) FROM ri_check) > 0
        THEN error('validate: referential integrity broken — ' ||
             (SELECT string_agg(list || '=' || orphans, '; ') FROM ri_check WHERE orphans > 0))
    WHEN (SELECT count(*) FROM golden_check) > 0
        THEN error('validate: golden-row mismatch — ' ||
             (SELECT string_agg(list || ':' || code, '; ') FROM golden_check))
    WHEN (SELECT count(*) FROM catalog_check WHERE cat_n <> par_n) > 0
        THEN error('validate: catalogue/Parquet row-count mismatch')
    ELSE 'validation passed: row counts, referential integrity, golden rows, and catalogue all OK'
END AS validation_result;
