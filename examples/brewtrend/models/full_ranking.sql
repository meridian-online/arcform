-- Create a table for details with descriptions and homepages
CREATE OR REPLACE TABLE details AS
SELECT
    'cask' AS category,
    token AS name,
    "desc" AS description,
    homepage,
FROM casks
UNION ALL
SELECT
    'formula' AS category,
    name,
    "desc" AS description,
    homepage,
FROM core;

-- Create a table for the final ranking of items based on trend + volume
CREATE OR REPLACE TABLE ranking AS
SELECT
    ROW_NUMBER() OVER (ORDER BY distance ASC) AS rank,
    measures.category,
    measures.name,
    CAST(d30 AS INTEGER) AS installs_30d,
    trend_index,
    description,
    homepage,
FROM measures
LEFT JOIN details ON measures.name = details.name AND measures.category = details.category
ORDER BY rank ASC;

-- Export the ranking as the pipeline's portable output artifact.
COPY ranking TO 'data/ranking.parquet';
