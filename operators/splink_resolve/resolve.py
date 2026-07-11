# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = ["splink>=4,<5", "duckdb>=1,<2", "pandas", "pydantic>=2"]
# ///
"""
splink_resolve — arcform typed Python operator (uv-run).

Resolves SEC EDGAR (cik/ticker/name) -> GLEIF (lei) by probabilistic name matching
(Fellegi-Sunter via Splink 4, DuckDB backend), publishing the EDGAR<->GLEIF crosswalk as
an open dataset. Both sources are open: EDGAR (public domain), GLEIF golden copy (CC0).
Emits, per resolved edge:
  cik, ticker, company_name, lei, legal_name, jurisdiction,
  match_weight (log2 Fellegi-Sunter), match_probability (0-1), method, status, as_of.

Signal note: EDGAR carries no address/jurisdiction, so the only comparison is the
normalized company name. GLEIF is pre-filtered to reg_status='ISSUED'; when a name
resolves to multiple ISSUED LEIs, a US-jurisdiction preference breaks the tie and the row
is flagged status='ambiguous'. The model is train-once / frozen (fixed SEED) so the
published snapshot is reproducible.

Run (arcform-Rust does not invoke Python operators yet — run manually via uv):
  uv run operators/splink_resolve/resolve.py \
      --edgar <edgar.parquet> --gleif <gleif.parquet> --out <crosswalk.parquet>
  # add --sample 100000 for a fast smoke test over a GLEIF subset
"""
from __future__ import annotations

import argparse
from typing import Literal

import duckdb
from pydantic import BaseModel, Field

SEED = 42  # frozen: pins u-sampling + EM init so the snapshot is reproducible

# Normalized-name key: uppercase -> strip punctuation -> strip trailing corporate
# suffix (CORP/INC/LLC/LTD/...) -> collapse whitespace.
NORM_SQL = r"""
trim(regexp_replace(regexp_replace(regexp_replace(
    upper(coalesce({col},'')), '[^A-Z0-9 ]', ' ', 'g'),
    '\s+(CORPORATION|CORP|INCORPORATED|INC|COMPANY|CO|LIMITED|LTD|LLC|PLC|LP|HOLDINGS|GROUP)\s*$', '', 'g'),
    '\s+', ' ', 'g'))
"""


class ComparisonCfg(BaseModel):
    column: str = "nname"
    method: Literal["name", "jaro_winkler"] = "name"
    thresholds: list[float] = Field(default_factory=lambda: [0.92, 0.85])
    term_frequency_adjustments: bool = True


class SplinkResolveConfig(BaseModel):
    """The hand-authored operator config (arcform-typed-operators.md §First operator)."""
    left: str = "edgar"
    right: str = "gleif"
    match_key: str = "nname"
    blocking_rules: list[str] = Field(default_factory=lambda: [
        "substr(nname, 1, 6)",          # shared 6-char name prefix
        "split_part(nname, ' ', 1)",    # shared first token
    ])
    comparisons: list[ComparisonCfg] = Field(default_factory=lambda: [ComparisonCfg()])
    match_threshold: float = 0.90       # provisional; calibrated from the run's distribution
    model_ref: str = "edgar_gleif_namecmp_v0"


def build_inputs(con: duckdb.DuckDBPyConnection, edgar_path: str, gleif_path: str, sample: int) -> None:
    n = NORM_SQL.format(col="name")
    con.execute(f"""
        CREATE OR REPLACE TABLE edgar_src AS
        SELECT CAST(cik AS VARCHAR) AS uid, ticker, name AS raw_name, {n} AS nname
        FROM read_parquet('{edgar_path}')
        WHERE {n} <> '';
    """)
    limit = f"USING SAMPLE {sample} ROWS (reservoir, {SEED})" if sample else ""
    con.execute(f"""
        CREATE OR REPLACE TABLE gleif_src AS
        SELECT lei AS uid, name AS raw_name, jurisdiction, {n} AS nname
        FROM read_parquet('{gleif_path}')
        WHERE reg_status = 'ISSUED' AND {n} <> ''
        {limit};
    """)
    # Splink inputs: only uid + the comparison key (identical schema both sides).
    con.execute("CREATE OR REPLACE TABLE edgar_in AS SELECT uid, nname FROM edgar_src;")
    con.execute("CREATE OR REPLACE TABLE gleif_in AS SELECT uid, nname FROM gleif_src;")
    e = con.sql("SELECT count(*) FROM edgar_in").fetchone()[0]
    g = con.sql("SELECT count(*) FROM gleif_in").fetchone()[0]
    print(f"[inputs] edgar={e:,}  gleif(ISSUED{'/sampled' if sample else ''})={g:,}")


def run(edgar_path: str, gleif_path: str, out_path: str, sample: int) -> None:
    import splink
    from splink import DuckDBAPI, Linker, SettingsCreator, block_on

    print(f"[splink] version {splink.__version__}")
    cfg = SplinkResolveConfig()

    con = duckdb.connect()
    build_inputs(con, edgar_path, gleif_path, sample)

    # Fully-specified Fellegi-Sunter name comparison with expert m/u priors (v0).
    # Frozen + reproducible by construction (no stochastic EM); EM/TF refinement is a
    # later enhancement. m = P(level | true match); u = P(level | random pair).
    name_cmp = {
        "output_column_name": "nname",
        "comparison_description": "normalized company name",
        "comparison_levels": [
            {"sql_condition": '"nname_l" IS NULL OR "nname_r" IS NULL',
             "label_for_charts": "Null", "is_null_level": True},
            {"sql_condition": '"nname_l" = "nname_r"',
             "label_for_charts": "Exact", "m_probability": 0.75, "u_probability": 5e-7},
            {"sql_condition": 'jaro_winkler_similarity("nname_l", "nname_r") >= 0.92',
             "label_for_charts": "JW>=0.92", "m_probability": 0.15, "u_probability": 5e-5},
            {"sql_condition": 'jaro_winkler_similarity("nname_l", "nname_r") >= 0.85',
             "label_for_charts": "JW>=0.85", "m_probability": 0.06, "u_probability": 1e-3},
            {"sql_condition": "ELSE",
             "label_for_charts": "All other", "m_probability": 0.04, "u_probability": 0.998949},
        ],
    }

    settings = SettingsCreator(
        link_type="link_only",
        unique_id_column_name="uid",
        probability_two_random_records_match=1e-4,
        comparisons=[name_cmp],
        blocking_rules_to_generate_predictions=[block_on(r) for r in cfg.blocking_rules],
    )
    db_api = DuckDBAPI(connection=con)
    linker = Linker(["edgar_in", "gleif_in"], settings, db_api,
                    input_table_aliases=["edgar", "gleif"])

    # --- predict (model fully specified above; no training needed) ---
    pred = linker.inference.predict(threshold_match_probability=0.05)
    con.register("pred", pred.as_pandas_dataframe())

    # Orient edgar<-side, dedupe to best match per cik, US-preference tie-break, flag ambiguity.
    con.execute("""
        CREATE OR REPLACE TABLE edges AS
        WITH oriented AS (
          SELECT
            CASE WHEN source_dataset_l='edgar' THEN uid_l ELSE uid_r END AS cik,
            CASE WHEN source_dataset_l='edgar' THEN uid_r ELSE uid_l END AS lei,
            match_weight, match_probability
          FROM pred
        ),
        joined AS (
          SELECT o.cik, e.ticker, e.raw_name AS company_name,
                 o.lei, g.raw_name AS legal_name, g.jurisdiction,
                 o.match_weight, o.match_probability,
                 (g.jurisdiction LIKE 'US%') AS is_us
          FROM oriented o
          JOIN edgar_src e ON e.uid=o.cik
          JOIN gleif_src g ON g.uid=o.lei
        ),
        ranked AS (
          SELECT *,
            row_number() OVER (PARTITION BY cik
              ORDER BY match_probability DESC, is_us DESC, lei) AS rn,
            count(*)     OVER (PARTITION BY cik) AS n_cand,
            count(*)     OVER (PARTITION BY cik, match_probability) AS n_at_top
          FROM joined
        )
        SELECT cik, ticker, company_name, lei, legal_name, jurisdiction,
               match_weight, match_probability,
               'splink_probabilistic' AS method,
               CASE WHEN n_cand>1 AND n_at_top>1 THEN 'ambiguous' ELSE 'confirmed' END AS status
        FROM ranked WHERE rn=1;
    """)

    total = con.sql("SELECT count(*) FROM edges").fetchone()[0]
    print(f"\n[result] best-match rows (one per resolved cik): {total:,} of "
          f"{con.sql('SELECT count(*) FROM edgar_in').fetchone()[0]:,} EDGAR filers")

    print("\n[distribution] best match_probability by band:")
    print(con.sql("""
        SELECT CASE
                 WHEN match_probability>=0.99 THEN '1  >=0.99'
                 WHEN match_probability>=0.95 THEN '2  0.95-0.99'
                 WHEN match_probability>=0.90 THEN '3  0.90-0.95'
                 WHEN match_probability>=0.70 THEN '4  0.70-0.90'
                 WHEN match_probability>=0.50 THEN '5  0.50-0.70'
                 ELSE '6  <0.50' END AS band,
               count(*) AS n,
               sum(CASE WHEN status='ambiguous' THEN 1 ELSE 0 END) AS ambiguous
        FROM edges GROUP BY band ORDER BY band;
    """).df().to_string(index=False))

    hero = ("320193", "1679788", "789019", "1045810", "1318605")  # AAPL COIN MSFT NVDA TSLA
    print("\n[hero tickers] resolved edge:")
    print(con.sql(f"""
        SELECT ticker, company_name, lei, legal_name, jurisdiction,
               round(match_probability,4) AS p, round(match_weight,2) AS wt, status
        FROM edges WHERE cik IN {hero} ORDER BY ticker;
    """).df().to_string(index=False))

    con.execute(f"COPY (SELECT * FROM edges ORDER BY company_name) TO '{out_path}' (FORMAT parquet);")
    print(f"\n[out] wrote {out_path}")


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--edgar", required=True, help="EDGAR company-tickers Parquet (cik, ticker, name)")
    ap.add_argument("--gleif", required=True, help="GLEIF golden-copy Parquet (lei, name, jurisdiction, reg_status)")
    ap.add_argument("--out", default="edgar_gleif_crosswalk.parquet")
    ap.add_argument("--sample", type=int, default=0, help="cap GLEIF ISSUED rows for a smoke test (0 = full)")
    a = ap.parse_args()
    run(a.edgar, a.gleif, a.out, a.sample)
