-- Choose one GLEIF entity per EDGAR row from the pairs `score_pairs` scored, and give
-- each edge its method and status. Writes build/resolved.parquet, which tier.sql reads.
--
-- Ties on match probability go to the better registration (ISSUED > LAPSED > RETIRED),
-- then to a US jurisdiction, then to the lower LEI. Precision first: only an exact
-- normalised-name match is `confirmed` (a name-only fuzzy match is ~55% precise, so it
-- is a `candidate`), and an exact name still tied with another entity after the
-- tie-break is `ambiguous`.
CREATE OR REPLACE TABLE resolved_edges AS
WITH oriented AS (
  SELECT CASE WHEN source_dataset_l = 'edgar' THEN uid_l ELSE uid_r END AS e_uid,
         CASE WHEN source_dataset_l = 'edgar' THEN uid_r ELSE uid_l END AS g_uid,
         match_weight, match_probability
  FROM read_parquet('build/scored_pairs.parquet')
),
joined AS (
  SELECT o.e_uid, e.cik, e.ticker, e.raw_name AS company_name, e.nname AS e_nn,
         o.g_uid AS lei, g.raw_name AS legal_name, g.jurisdiction, g.reg_status, g.nname AS g_nn,
         o.match_weight, o.match_probability,
         CASE g.reg_status WHEN 'ISSUED' THEN 0 WHEN 'LAPSED' THEN 1
                           WHEN 'RETIRED' THEN 2 ELSE 3 END AS reg_rank,
         (g.jurisdiction LIKE 'US%') AS is_us
  FROM oriented o
  JOIN edgar_src e ON e.uid = o.e_uid
  JOIN gleif_src g ON g.uid = o.g_uid
),
ranked AS (
  SELECT *,
    row_number() OVER (PARTITION BY e_uid
      ORDER BY match_probability DESC, reg_rank ASC, is_us DESC, lei) AS rn,
    count(*) OVER (PARTITION BY e_uid, match_probability, reg_rank, is_us) AS n_equiv
  FROM joined
)
SELECT cik, ticker, company_name, lei, legal_name, jurisdiction, reg_status,
       match_weight, match_probability,
       CASE WHEN e_nn = g_nn THEN 'exact_name' ELSE 'jaro_winkler' END AS method,
       CASE WHEN e_nn = g_nn AND n_equiv > 1 THEN 'ambiguous'
            WHEN e_nn = g_nn THEN 'confirmed'
            ELSE 'candidate' END AS status
FROM ranked
WHERE rn = 1;

-- A total order, so the file is the same bytes on a re-run.
COPY (
  SELECT * FROM resolved_edges
  WHERE status IN ('confirmed', 'ambiguous', 'candidate')
  ORDER BY company_name, cik, ticker, lei
) TO 'build/resolved.parquet' (FORMAT parquet);
