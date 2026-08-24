-- The corpus the projection is built from: 48 short texts on two subjects, a
-- working harbour and company results. Typed explicitly rather than left to CSV
-- sniffing, so the column types the projected Parquet is checked against are the
-- ones this file states.
CREATE OR REPLACE TABLE corpus AS
SELECT
    CAST(id AS INTEGER)      AS id,
    CAST(title AS VARCHAR)   AS title,
    CAST(description AS VARCHAR) AS description
FROM read_csv_auto('corpus.csv');
