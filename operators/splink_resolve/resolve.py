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

# --- Blocking-key derivation (scaling only — does NOT touch the frozen F-S model) -----
# Leading "stopword" tokens that would otherwise pool into one giant block. Measured on
# the 3.36M GLEIF golden copy: first-token "THE" = 66k rows, "STICHTING" = 9.8k, and the
# "SOCIETE"/"STIFTUNG" prefixes ~8k/3k. They are stripped into a `core` name so that
# neither the name prefix nor the first token of these rows can form a hotspot block.
BLOCK_STOPWORDS = (
    "THE", "STICHTING", "SOCIETE", "STIFTUNG",
    "FONDATION", "ASSOCIATION", "FUNDACION", "FONDAZIONE",
)


def blocking_select(src: str, country_sql: str) -> str:
    """SQL that projects a source table down to (uid, nname) + the compound-blocking keys.

    `country_sql` is the per-source domicile expression (a literal for EDGAR, the GLEIF
    `country` column for GLEIF). All name keys derive from `core` = nname with a single
    leading stopword token removed, so THE.../STICHTING... rows disperse instead of pooling.
    Each key is NULL when its guard fails, which drops the row from that block (NULL never
    equi-joins) — this is how the min-length guard and the short-name routing are enforced.
    """
    stop = ", ".join(f"'{w}'" for w in BLOCK_STOPWORDS)
    core = (f"CASE WHEN split_part(nname, ' ', 1) IN ({stop}) "
            f"THEN substr(nname, length(split_part(nname, ' ', 1)) + 2) ELSE nname END")
    return f"""
        WITH base AS (
            SELECT uid, nname, {country_sql} AS country, {core} AS core FROM {src}
        )
        SELECT uid, nname, country,
               -- primary US-core key: 6-char prefix of the stopword-stripped name (names >=6)
               CASE WHEN length(core) >= 6 THEN substr(core, 1, 6) END        AS blk_p6,
               -- global near-miss key (no country): selective 8-char prefix for longer names
               CASE WHEN length(core) >= 8 THEN substr(core, 1, 8) END        AS blk_p8,
               -- significant first token; >=4 chars kills single-letter/short-token megablocks
               CASE WHEN length(split_part(core, ' ', 1)) >= 4
                    THEN split_part(core, ' ', 1) END                         AS blk_tok,
               -- global exact-name key (no country): recovers foreign ADRs of ANY length that
               -- the country/8-char rules would otherwise miss. Exact full-name collisions are
               -- rare, so this block stays tiny (worst observed ~40 rows) with no hotspot.
               CASE WHEN length(core) >= 2 THEN core END                      AS blk_exact
        FROM base
    """


class SplinkResolveConfig(BaseModel):
    """Hand-authored operator config (typed-operator design: config-not-code)."""
    match_key: str = "nname"
    # Compound blocking keys (union of rules; each rule is an AND of its columns). Country is
    # the strong pruning dimension: only ~355k of 3.36M GLEIF are US-domiciled and every SEC
    # filer is US, so the US<->US rules cut the comparison space ~9.5x. Two country-free rules
    # (8-char prefix + exact name) keep foreign ADRs reachable by name alone. Keys are built in
    # build_inputs via blocking_select() (min-length guards + stopword-stripped `core`
    # neutralise the THE/STICHTING/single-letter-token hotspots).
    blocking_rules: list[list[str]] = Field(default_factory=lambda: [
        ["country", "blk_p6"],      # US<->US core: domicile + 6-char stopword-stripped prefix
        ["blk_p8"],                 # global 8-char prefix — foreign near-miss recall
        ["country", "blk_tok"],     # US<->US significant first token (>=4 chars)
        ["blk_exact"],              # global exact name — foreign ADRs of any length
    ])
    # Fellegi-Sunter name-comparison priors: (sql, label, m, u). m = P(level | true match),
    # u = P(level | random pair). Tuned so exact/JW>=0.95 -> confirmed, JW>=0.92 -> candidate.
    prob_two_random_match: float = 0.01
    confirmed_threshold: float = 0.95
    candidate_threshold: float = 0.50
    model_ref: str = "edgar_gleif_namecmp_v1"


def build_inputs(con: duckdb.DuckDBPyConnection, edgar_path: str, gleif_path: str, sample: int) -> None:
    n_e = NORM_SQL.format(col="company_name")   # EDGAR published name column
    n_g = NORM_SQL.format(col="legal_name")     # GLEIF published name column
    con.execute(f"""
        CREATE OR REPLACE TABLE edgar_src AS
        SELECT 'e' || CAST(row_number() OVER () AS VARCHAR) AS uid,
               cik, ticker, company_name AS raw_name, {n_e} AS nname
        FROM read_parquet('{edgar_path}')
        WHERE {n_e} <> '';
    """)
    limit = f"USING SAMPLE {sample} ROWS (reservoir, {SEED})" if sample else ""
    # GLEIF now publishes a domicile `country` column (used for blocking) alongside the
    # `jurisdiction` used by the frozen US tie-break; carry both. reg_status = registration_status.
    con.execute(f"""
        CREATE OR REPLACE TABLE gleif_src AS
        SELECT lei AS uid, legal_name AS raw_name, jurisdiction, country,
               registration_status AS reg_status, {n_g} AS nname
        FROM read_parquet('{gleif_path}')
        WHERE {n_g} <> '' AND registration_status IN ('ISSUED', 'LAPSED', 'RETIRED')
        {limit};
    """)
    # Blocking inputs: nname + domicile + derived compound-blocking keys. EDGAR filers are all
    # US-listed, so EDGAR's blocking domicile is the literal 'US' (a proxy — see blocking_select).
    edgar_in_sql = blocking_select("edgar_src", "'US'")
    gleif_in_sql = blocking_select("gleif_src", "country")
    con.execute(f"CREATE OR REPLACE TABLE edgar_in AS {edgar_in_sql};")
    con.execute(f"CREATE OR REPLACE TABLE gleif_in AS {gleif_in_sql};")
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
        blocking_rules_to_generate_predictions=[block_on(*r) for r in cfg.blocking_rules],
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
