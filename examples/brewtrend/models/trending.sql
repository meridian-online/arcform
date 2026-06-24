-- Software must be installed at least `trend_threshold` times in the past 30
-- days to be ranked. Sourced from the arcform runtime parameter (default 10);
-- override with `arc run --param trend_threshold=25`.
SET VARIABLE trend_threshold = CAST(getenv('ARC_PARAM_TREND_THRESHOLD') AS INTEGER);

-- Create a function to normalize values using Min-Max scaling
CREATE OR REPLACE FUNCTION MINMAX(v) AS
    COALESCE((v - MIN(v) OVER()) / (MAX(v) OVER() - MIN(v) OVER()), 0);

-- Create a table for categorized items with their install counts
CREATE OR REPLACE TABLE categories AS
WITH item_array AS (
    SELECT
        start_date,
        end_date,
        CASE
            WHEN category = 'cask_install' THEN 'cask'
            WHEN category = 'formula_install_on_request' THEN 'formula'
        END AS category,
        CONCAT('d', REGEXP_EXTRACT(filename, '(\d+)d.json', 1)) AS days,
        formulae ->> '$.*[*]' AS item_group
    FROM brew_analytics
),
items AS (
    SELECT
        category,
        start_date,
        end_date,
        days,
        UNNEST(item_group) AS item
    FROM item_array
)
SELECT
    category,
    start_date,
    end_date,
    days,
    CASE
        WHEN category = 'cask' THEN item ->> '$.cask'
        WHEN category = 'formula' THEN item ->> '$.formula'
    END AS name,
    CAST(REPLACE(item ->> '$.count', ',', '') AS INTEGER) AS installs
FROM items;

-- Create a table for install counts pivoted by days
CREATE OR REPLACE TABLE installs AS
WITH install_counts AS (
    SELECT
        category,
        name,
        days,
        installs
    FROM categories
)
PIVOT install_counts ON days USING SUM(installs);

-- Create a table for measures including average installs and trend change
CREATE OR REPLACE TABLE measures AS
SELECT
    *,
    IFNULL(d30 / 30, 0) AS d30_avg,
    IFNULL((d90 - d30) / 60, 0) AS p60_avg,
    CASE
        WHEN d30_avg < getvariable('trend_threshold') THEN 0
        ELSE p60_avg / d30_avg
    END AS trend_index,
    MINMAX(trend_index) AS trend_scaled,
    MINMAX(d30) AS d30_scaled,
    array_distance(
        CAST([trend_scaled, d30_scaled] AS FLOAT[2]),
        CAST([1, 1] AS FLOAT[2])
    ) AS distance
FROM installs;

-- Create a combined table with all package details
CREATE OR REPLACE TABLE all_packages AS
SELECT
    'cask' AS category,
    token AS name,
    "desc" AS description,
    homepage
FROM casks
UNION ALL
SELECT
    'formula' AS category,
    name,
    "desc" AS description,
    homepage
FROM core;
