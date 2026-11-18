//! End to end for the `splink` operator: a real `arc run` of a one-step Protocol, so
//! the whole route is exercised, arc to `uv` to the frozen script to Splink. The unit
//! tests in `src/operator.rs` read the argv; this is the test that would have caught a
//! script that cannot import Splink once arc has put it in the temp directory.
//!
//! It needs `uv` (Splink, DuckDB and pyarrow resolve on first use) and skips where `uv`
//! is not on PATH. CI runs it in the job that installs `uv`.

use std::path::Path;
use std::process::Command;

fn have_uv() -> bool {
    Command::new("uv")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

const MANIFEST: &str = "\
name: splink_fixture
engine: duckdb
db: build/fixture.duckdb

steps:
  - name: score_pairs
    op: splink@1
    with:
      inputs:
        - role: left
          path: build/left.parquet
        - role: right
          path: build/right.parquet
      model: model.json
      out: build/pairs.parquet
";

const MODEL: &str = r#"{
  "link_type": "link_only",
  "unique_id_column_name": "unique_id",
  "probability_two_random_records_match": 0.01,
  "blocking_rules_to_generate_predictions": ["l.postcode = r.postcode"],
  "comparisons": [{
    "output_column_name": "name",
    "comparison_levels": [
      {"sql_condition": "\"name_l\" IS NULL OR \"name_r\" IS NULL", "is_null_level": true},
      {"sql_condition": "\"name_l\" = \"name_r\"", "m_probability": 0.9, "u_probability": 0.05},
      {"sql_condition": "ELSE", "m_probability": 0.1, "u_probability": 0.95}
    ]
  }]
}"#;

fn write_parquet(path: &Path, values: &str) {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE t AS SELECT * FROM (VALUES {values}) v(unique_id, name, postcode);\
         COPY t TO '{}' (FORMAT parquet)",
        path.display()
    ))
    .unwrap();
}

#[test]
fn a_two_input_protocol_writes_the_scored_pairs() {
    if !have_uv() {
        eprintln!("skipping a_two_input_protocol_writes_the_scored_pairs: no `uv` on PATH");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("build")).unwrap();
    write_parquet(
        &dir.join("build/left.parquet"),
        "(1, 'ann', 'x'), (2, 'bob', 'y')",
    );
    write_parquet(
        &dir.join("build/right.parquet"),
        "(10, 'ann', 'x'), (20, 'zed', 'z')",
    );
    std::fs::write(dir.join("model.json"), MODEL).unwrap();
    std::fs::write(dir.join("arcform.yaml"), MANIFEST).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(dir)
        .env("ARCFORM_DB_DIR", std::env::temp_dir().join("arc-tests-db"))
        .arg("run")
        .output()
        .expect("spawn arc run");
    assert!(
        out.status.success(),
        "arc run failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let conn = duckdb::Connection::open_in_memory().unwrap();
    let pairs: Vec<(i64, i64)> = conn
        .prepare(&format!(
            "SELECT unique_id_l, unique_id_r FROM read_parquet('{}') \
             ORDER BY unique_id_l, unique_id_r",
            dir.join("build/pairs.parquet").display()
        ))
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    // Only the x/x pair shares a postcode, so it is the one blocked pair.
    assert_eq!(pairs, vec![(1, 10)]);
}
