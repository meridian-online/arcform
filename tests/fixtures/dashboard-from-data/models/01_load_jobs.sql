CREATE OR REPLACE TABLE jobs AS
SELECT * FROM read_csv('jobs.csv');
