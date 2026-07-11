# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = ["splink>=4,<5", "duckdb>=1,<2", "pandas", "pydantic>=2"]
# ///
"""
splink_resolve — arcform typed Python operator (uv-run).

Resolves SEC EDGAR (cik/ticker/name) -> GLEIF (lei) by probabilistic name matching
(Fellegi-Sunter via Splink 4, DuckDB backend), publishing the EDGAR<->GLEIF crosswalk as
an open dataset. Both sources are open: EDGAR (public domain), GLEIF golden copy (CC0).
Emits, per resolved edge (ticker grain):
  cik, ticker, company_name, lei, legal_name, jurisdiction, reg_status,
  match_weight (log2 Fellegi-Sunter), match_probability (0-1), method, status.

Signal note: EDGAR carries no address/jurisdiction, so the only comparison is the
normalized company name. All GLEIF registration statuses are matched (a LAPSED/RETIRED
registration does not change the entity's LEI) and reg_status is carried through; ties are
broken by ISSUED > LAPSED > RETIRED then a US-jurisdiction preference, and any residual
tie is flagged status='ambiguous'. The model is a fully-specified Fellegi-Sunter with
expert m/u priors (fixed SEED) so the published snapshot is reproducible.

Run (arcform-Rust does not invoke Python operators yet — run manually via uv):
  uv run operators/splink_resolve/resolve.py \
      --edgar <edgar.parquet> --gleif <gleif.parquet> --out <crosswalk.parquet>
  # add --sample 200000 for a fast smoke test over a GLEIF subset
"""
from __future__ import annotations

import argparse

import duckdb
from pydantic import BaseModel, Field

SEED = 42  # frozen: fixed model + deterministic tie-break so the snapshot is reproducible

# Normalized-name key: uppercase -> strip punctuation -> strip trailing corporate
# suffix (CORP/INC/LLC/LTD/...) -> collapse whitespace.
NORM_SQL = r"""
trim(regexp_replace(regexp_replace(regexp_replace(
    upper(coalesce({col},'')), '[^A-Z0-9 ]', ' ', 'g'),
    '\s+(CORPORATION|CORP|INCORPORATED|INC|COMPANY|CO|LIMITED|LTD|LLC|PLC|LP|HOLDINGS|GROUP)\s*$', '', 'g'),
    '\s+', ' ', 'g'))
"""


class SplinkResolveConfig(BaseModel):
    """Hand-authored operator config (typed-operator design: config-not-code)."""
    match_key: str = "nname"
    blocking_rules: list[str] = Field(default_factory=lambda: [
        "substr(nname, 1, 6)",          # shared 6-char name prefix (catches exact + near-miss)
        "split_part(nname, ' ', 1)",    # shared first token
    ])
    # Fellegi-Sunter name-comparison priors: (sql, label, m, u). m = P(level | true match),
    # u = P(level | random pair). Tuned so exact/JW>=0.95 -> confirmed, JW>=0.92 -> candidate.
    prob_two_random_match: float = 0.01
    confirmed_threshold: float = 0.95
    candidate_threshold: float = 0.50
    model_ref: str = "edgar_gleif_namecmp_v1"


def build_inputs(con: duckdb.DuckDBPyConnection, edgar_path: str, gleif_path: str, sample: int) -> None:
    n = NORM_SQL.format(col="name")
    con.execute(f"""
        CREATE OR REPLACE TABLE edgar_src AS
        SELECT 'e' || CAST(row_number() OVER () AS VARCHAR) AS uid,
               cik, ticker, name AS raw_name, {n} AS nname
        FROM read_parquet('{edgar_path}')
        WHERE {n} <> '';
    """)
    limit = f"USING SAMPLE {sample} ROWS (reservoir, {SEED})" if sample else ""
    con.execute(f"""
        CREATE OR REPLACE TABLE gleif_src AS
        SELECT lei AS uid, name AS raw_name, jurisdiction, reg_status, {n} AS nname
        FROM read_parquet('{gleif_path}')
        WHERE {n} <> '' AND reg_status IN ('ISSUED', 'LAPSED', 'RETIRED')
        {limit};
    """)
    con.execute("CREATE OR REPLACE TABLE edgar_in AS SELECT uid, nname FROM edgar_src;")
    con.execute("CREATE OR REPLACE TABLE gleif_in AS SELECT uid, nname FROM gleif_src;")
    e = con.sql("SELECT count(*) FROM edgar_in").fetchone()[0]
    g = con.sql("SELECT count(*) FROM gleif_in").fetchone()[0]
    print(f"[inputs] edgar(ticker-grain)={e:,}  gleif(all statuses{'/sampled' if sample else ''})={g:,}")


def run(edgar_path: str, gleif_path: str, out_path: str, sample: int) -> None:
    import splink
    from splink import DuckDBAPI, Linker, SettingsCreator, block_on

    print(f"[splink] version {splink.__version__}")
    cfg = SplinkResolveConfig()

    con = duckdb.connect()
    build_inputs(con, edgar_path, gleif_path, sample)

    # Fully-specified Fellegi-Sunter name comparison (v1 expert priors). Frozen/reproducible.
    name_cmp = {
        "output_column_name": "nname",
        "comparison_description": "normalized company name",
        "comparison_levels": [
            {"sql_condition": '"nname_l" IS NULL OR "nname_r" IS NULL',
             "label_for_charts": "Null", "is_null_level": True},
            {"sql_condition": '"nname_l" = "nname_r"',
             "label_for_charts": "Exact", "m_probability": 0.80, "u_probability": 1e-6},
            {"sql_condition": 'jaro_winkler_similarity("nname_l", "nname_r") >= 0.95',
             "label_for_charts": "JW>=0.95", "m_probability": 0.12, "u_probability": 1e-5},
            {"sql_condition": 'jaro_winkler_similarity("nname_l", "nname_r") >= 0.92',
             "label_for_charts": "JW>=0.92", "m_probability": 0.04, "u_probability": 1e-4},
            {"sql_condition": 'jaro_winkler_similarity("nname_l", "nname_r") >= 0.88',
             "label_for_charts": "JW>=0.88", "m_probability": 0.02, "u_probability": 1e-3},
            {"sql_condition": "ELSE",
             "label_for_charts": "All other", "m_probability": 0.02, "u_probability": 0.998889},
        ],
    }
    settings = SettingsCreator(
        link_type="link_only",
        unique_id_column_name="uid",
        probability_two_random_records_match=cfg.prob_two_random_match,
        comparisons=[name_cmp],
        blocking_rules_to_generate_predictions=[block_on(r) for r in cfg.blocking_rules],
    )
    db_api = DuckDBAPI(connection=con)
    linker = Linker(["edgar_in", "gleif_in"], settings, db_api, input_table_aliases=["edgar", "gleif"])
    pred = linker.inference.predict(threshold_match_probability=0.30)
    con.register("pred", pred.as_pandas_dataframe())

    # Best edge per EDGAR row; reg_status + US preference tie-break; residual ties -> ambiguous.
    con.execute(f"""
        CREATE OR REPLACE TABLE edges AS
        WITH oriented AS (
          SELECT CASE WHEN source_dataset_l='edgar' THEN uid_l ELSE uid_r END AS e_uid,
                 CASE WHEN source_dataset_l='edgar' THEN uid_r ELSE uid_l END AS g_uid,
                 match_weight, match_probability
          FROM pred
        ),
        joined AS (
          SELECT o.e_uid, e.cik, e.ticker, e.raw_name AS company_name, e.nname AS e_nn,
                 o.g_uid AS lei, g.raw_name AS legal_name, g.jurisdiction, g.reg_status, g.nname AS g_nn,
                 o.match_weight, o.match_probability,
                 CASE g.reg_status WHEN 'ISSUED' THEN 0 WHEN 'LAPSED' THEN 1
                                   WHEN 'RETIRED' THEN 2 ELSE 3 END AS reg_rank,
                 (g.jurisdiction LIKE 'US%') AS is_us
          FROM oriented o
          JOIN edgar_src e ON e.uid=o.e_uid
          JOIN gleif_src g ON g.uid=o.g_uid
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
               CASE WHEN e_nn=g_nn THEN 'exact_name' ELSE 'jaro_winkler' END AS method,
               -- Precision-first: only exact-normalized-name matches auto-confirm (name-only
               -- fuzzy is ~55% precise -> 'candidate', not 'confirmed'). Exact-name collisions
               -- across >1 entity (after reg_status + US tie-break) -> 'ambiguous'.
               CASE WHEN e_nn=g_nn AND n_equiv>1 THEN 'ambiguous'
                    WHEN e_nn=g_nn THEN 'confirmed'
                    ELSE 'candidate' END AS status
        FROM ranked WHERE rn=1;
    """)

    edgar_rows = con.sql("SELECT count(*) FROM edgar_src").fetchone()[0]
    print(f"\n[coverage] of {edgar_rows:,} EDGAR ticker-rows:")
    print(con.sql(f"""
        SELECT status, count(*) AS n,
               round(100.0*count(*)/{edgar_rows}, 1) AS pct,
               round(avg(match_probability), 3) AS avg_p,
               sum(CASE WHEN method='exact_name' THEN 1 ELSE 0 END) AS exact,
               sum(CASE WHEN method='jaro_winkler' THEN 1 ELSE 0 END) AS fuzzy
        FROM edges GROUP BY status
        ORDER BY array_position(['confirmed','ambiguous','candidate','weak'], status);
    """).df().to_string(index=False))
    conf = con.sql("SELECT count(*) FROM edges WHERE status IN ('confirmed','ambiguous')").fetchone()[0]
    print(f"\n[confident universe] confirmed+ambiguous = {conf:,} "
          f"({round(100.0*conf/edgar_rows,1)}% of ticker-rows)")

    hero = (320193, 1679788, 789019, 1045810, 1318605)  # AAPL COIN MSFT NVDA TSLA
    print("\n[hero tickers]:")
    print(con.sql(f"""
        SELECT ticker, company_name, lei, legal_name, jurisdiction, reg_status,
               round(match_probability,4) AS p, method, status
        FROM edges WHERE cik IN {hero} ORDER BY ticker;
    """).df().to_string(index=False))

    con.execute(f"""
        COPY (SELECT * FROM edges WHERE status IN ('confirmed','ambiguous','candidate')
              ORDER BY company_name) TO '{out_path}' (FORMAT parquet);
    """)
    print(f"\n[out] wrote {out_path}")


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--edgar", required=True, help="EDGAR company-tickers Parquet (cik, ticker, name)")
    ap.add_argument("--gleif", required=True, help="GLEIF golden-copy Parquet (lei, name, jurisdiction, reg_status)")
    ap.add_argument("--out", default="edgar_gleif_crosswalk.parquet")
    ap.add_argument("--sample", type=int, default=0, help="cap GLEIF rows for a smoke test (0 = full)")
    a = ap.parse_args()
    run(a.edgar, a.gleif, a.out, a.sample)
