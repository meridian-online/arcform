-- The points the map is drawn from: 48 properties in two districts, described by four
-- numbers each and no text at all. Typed explicitly rather than left to CSV sniffing,
-- so the column types the projected Parquet is checked against are the ones this file
-- states.
--
-- `district` is carried through as VARCHAR and is NOT projected. It is here for two
-- reasons: a column the projection does not name has to survive into the output
-- untouched, and asking to project it is how the "that is not a number" refusal is
-- driven (see not_numeric.yaml).
CREATE OR REPLACE TABLE homes AS
SELECT
    CAST(id AS INTEGER)                   AS id,
    CAST(district AS VARCHAR)             AS district,
    CAST(longitude AS DOUBLE)             AS longitude,
    CAST(latitude AS DOUBLE)              AS latitude,
    CAST(median_income AS DOUBLE)         AS median_income,
    CAST(rooms_per_household AS DOUBLE)   AS rooms_per_household
FROM read_csv_auto('homes.csv');
