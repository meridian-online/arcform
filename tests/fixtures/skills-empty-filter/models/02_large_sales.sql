-- generated: filter-rows on sales
CREATE OR REPLACE TABLE "large_sales" AS
SELECT *
FROM sales
WHERE amount > 1000;
