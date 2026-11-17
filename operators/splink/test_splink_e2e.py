# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = ["splink==5.0.0", "duckdb==2.0.0", "pyarrow==26.0.1"]
# ///
"""Run splink.py end to end over small Parquet inputs: `uv run operators/splink/test_splink_e2e.py`."""
from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import duckdb

SCRIPT = Path(__file__).resolve().parent / "splink.py"

MODEL = {
    "link_type": "link_only",
    "unique_id_column_name": "unique_id",
    "probability_two_random_records_match": 0.01,
    "blocking_rules_to_generate_predictions": ["l.postcode = r.postcode"],
    "comparisons": [
        {
            "output_column_name": "name",
            "comparison_levels": [
                {"sql_condition": '"name_l" IS NULL OR "name_r" IS NULL', "is_null_level": True},
                {
                    "sql_condition": '"name_l" = "name_r"',
                    "label_for_charts": "exact",
                    "m_probability": 0.9,
                    "u_probability": 0.05,
                },
                {"sql_condition": "ELSE", "m_probability": 0.1, "u_probability": 0.95},
            ],
        }
    ],
}


def write_parquet(path: Path, rows: list[tuple]) -> None:
    con = duckdb.connect()
    con.execute("CREATE TABLE t (unique_id BIGINT, name VARCHAR, postcode VARCHAR)")
    if rows:
        con.executemany("INSERT INTO t VALUES (?, ?, ?)", rows)
    con.execute("COPY t TO ? (FORMAT parquet)", [str(path)])


class SplinkE2ETest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)
        self.a = self.dir / "a.parquet"
        self.b = self.dir / "b.parquet"
        write_parquet(self.a, [(2, "ann", "x"), (1, "bob", "x"), (3, "cy", "y")])
        write_parquet(self.b, [(20, "ann", "x"), (10, "zed", "x"), (30, "cy", "y")])
        self.model = self.dir / "model.json"
        self.model.write_text(json.dumps(MODEL))
        self.out = self.dir / "out.parquet"

    def run_op(self, *extra, a=None, model=None, out=None, role="a"):
        cmd = [
            sys.executable, str(SCRIPT),
            "--input", f"{role}={a or self.a}", "--input", f"b={self.b}",
            "--model", str(model or self.model), "--out", str(out or self.out), *extra,
        ]
        return subprocess.run(cmd, capture_output=True, text=True)

    def rows(self, path=None):
        return duckdb.sql(
            "SELECT unique_id_l, unique_id_r, round(match_probability, 6) "
            "FROM read_parquet(?)", params=[str(path or self.out)]
        ).fetchall()

    def test_scores_the_blocked_pairs_and_sorts_them(self) -> None:
        r = self.run_op()
        self.assertEqual(r.returncode, 0, r.stderr)
        got = self.rows()
        self.assertEqual([g[:2] for g in got], [(1, 10), (1, 20), (2, 10), (2, 20), (3, 30)])
        # prior 0.01, exact: bf = 0.9/0.05 = 18, else 0.1/0.95
        odds = 0.01 / 0.99
        for (_, _, p), exact in zip(got, [False, False, False, True, True]):
            bf = 0.9 / 0.05 if exact else 0.1 / 0.95
            self.assertAlmostEqual(p, odds * bf / (1 + odds * bf), places=6)

    def test_two_runs_write_identical_bytes(self) -> None:
        second = self.dir / "second.parquet"
        self.assertEqual(self.run_op().returncode, 0)
        self.assertEqual(self.run_op(out=second).returncode, 0)
        self.assertEqual(self.out.read_bytes(), second.read_bytes())

    def test_no_pair_above_threshold_writes_an_empty_file_with_the_schema(self) -> None:
        full = self.dir / "full.parquet"
        self.assertEqual(self.run_op(out=full).returncode, 0)
        r = self.run_op("--threshold-match-probability", "0.99")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.rows(), [])
        cols = lambda p: duckdb.sql("DESCRIBE SELECT * FROM read_parquet(?)", params=[str(p)]).fetchall()
        self.assertEqual(cols(self.out), cols(full))

    def test_a_path_with_a_space_and_a_quote_is_read(self) -> None:
        odd = self.dir / "o'dd name.parquet"
        write_parquet(odd, [(1, "bob", "x"), (2, "ann", "x")])
        r = self.run_op(a=odd)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(len(self.rows()), 4)

    def test_an_untrained_model_is_refused_with_exit_2(self) -> None:
        bad = json.loads(json.dumps(MODEL))
        del bad["comparisons"][0]["comparison_levels"][1]["u_probability"]
        self.model.write_text(json.dumps(bad))
        r = self.run_op()
        self.assertEqual(r.returncode, 2)
        self.assertIn("u_probability", r.stderr)
        self.assertFalse(self.out.exists())

    def test_a_model_that_does_not_parse_is_refused(self) -> None:
        self.model.write_text("{not json")
        r = self.run_op()
        self.assertEqual(r.returncode, 2)
        self.assertIn(str(self.model), r.stderr)

    def test_a_role_given_twice_is_refused(self) -> None:
        cmd = [sys.executable, str(SCRIPT), "--input", f"a={self.a}", "--input", f"a={self.b}",
               "--model", str(self.model), "--out", str(self.out)]
        r = subprocess.run(cmd, capture_output=True, text=True)
        self.assertEqual(r.returncode, 2)
        self.assertIn("'a'", r.stderr)

    def test_a_type_mismatch_across_roles_is_refused_naming_the_column(self) -> None:
        con = duckdb.connect()
        con.execute("CREATE TABLE t AS SELECT 1::BIGINT AS unique_id, 5::BIGINT AS name, 'x' AS postcode")
        odd = self.dir / "odd.parquet"
        con.execute("COPY t TO ? (FORMAT parquet)", [str(odd)])
        r = self.run_op(a=odd)
        self.assertEqual(r.returncode, 2)
        self.assertIn("'name'", r.stderr)
        self.assertIn("BIGINT", r.stderr)

    def test_a_role_that_is_not_a_plain_name_is_refused(self) -> None:
        r = self.run_op(role="sec-edgar")
        self.assertEqual(r.returncode, 2)
        self.assertIn("sec-edgar", r.stderr)

    def test_a_reserved_word_role_works(self) -> None:
        r = self.run_op(role="order")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(len(self.rows()), 5)

    def test_a_non_default_source_dataset_column_name_is_sorted_on(self) -> None:
        # Splink 5 only names its own source column `source_dataset`; any other name
        # must already be a column of the inputs, so the inputs carry `src`.
        for path, tag in ((self.a, "A"), (self.b, "B")):
            rel = duckdb.read_parquet(str(path))
            rel = duckdb.sql("SELECT *, ? AS src FROM rel", params=[tag])
            rel.write_parquet(str(path))
        self.model.write_text(json.dumps({**MODEL, "source_dataset_column_name": "src"}))
        r = self.run_op()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual([g[:2] for g in self.rows()], [(1, 10), (1, 20), (2, 10), (2, 20), (3, 30)])
        got = duckdb.sql("SELECT DISTINCT src_l, src_r FROM read_parquet(?)", params=[str(self.out)]).fetchall()
        self.assertEqual(got, [("A", "B")])

    def test_duplicate_unique_ids_in_an_input_are_refused(self) -> None:
        dup = self.dir / "dup.parquet"
        write_parquet(dup, [(1, "bob", "x"), (1, "ann", "x")])
        r = self.run_op(a=dup)
        self.assertEqual(r.returncode, 2)
        for part in ("'a'", "'unique_id'", "1 duplicate"):
            self.assertIn(part, r.stderr)
        self.assertFalse(self.out.exists())

    def test_a_model_without_comparisons_names_the_model_path(self) -> None:
        bad = {k: v for k, v in MODEL.items() if k != "comparisons"}
        self.model.write_text(json.dumps(bad))
        r = self.run_op()
        self.assertEqual(r.returncode, 2)
        self.assertIn(str(self.model), r.stderr)
        self.assertIn("comparisons", r.stderr)

    def test_a_probability_threshold_of_one_is_refused(self) -> None:
        r = self.run_op("--threshold-match-probability", "1")
        self.assertEqual(r.returncode, 2)
        self.assertIn("threshold_match_probability", r.stderr)

    def test_both_thresholds_are_refused(self) -> None:
        r = self.run_op("--threshold-match-probability", "0.5", "--threshold-match-weight", "1")
        self.assertEqual(r.returncode, 2)
        self.assertIn("not both", r.stderr)

    def test_an_input_with_zero_rows_writes_the_schema_and_no_rows(self) -> None:
        empty = self.dir / "empty.parquet"
        write_parquet(empty, [])
        full = self.dir / "full.parquet"
        self.assertEqual(self.run_op(out=full).returncode, 0)
        r = self.run_op(a=empty)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.rows(), [])
        cols = lambda p: duckdb.sql("DESCRIBE SELECT * FROM read_parquet(?)", params=[str(p)]).fetchall()
        self.assertEqual(cols(self.out), cols(full))

    def test_null_unique_ids_are_refused_and_not_counted_as_duplicates(self) -> None:
        nulls = self.dir / "nulls.parquet"
        write_parquet(nulls, [(None, "bob", "x"), (None, "ann", "x"), (1, "cy", "y")])
        r = self.run_op(a=nulls)
        self.assertEqual(r.returncode, 2)
        self.assertIn("2 rows have a NULL 'unique_id'", r.stderr)
        self.assertNotIn("duplicate", r.stderr)

    def test_duplicates_are_counted_among_non_null_ids_only(self) -> None:
        mixed = self.dir / "mixed.parquet"
        write_parquet(mixed, [(None, "bob", "x"), (1, "ann", "x"), (1, "cy", "y")])
        r = self.run_op(a=mixed)
        self.assertEqual(r.returncode, 2)
        self.assertIn("1 rows have a NULL", r.stderr)
        self.assertIn("1 duplicate", r.stderr)

    def test_a_custom_source_column_missing_from_an_input_is_refused(self) -> None:
        self.model.write_text(json.dumps({**MODEL, "source_dataset_column_name": "src"}))
        r = self.run_op()
        self.assertEqual(r.returncode, 2)
        self.assertIn("'src'", r.stderr)
        self.assertIn("'a'", r.stderr)
        self.assertIn("'b'", r.stderr)

    def test_a_missing_probability_refusal_names_the_model_path(self) -> None:
        bad = json.loads(json.dumps(MODEL))
        del bad["comparisons"][0]["comparison_levels"][1]["m_probability"]
        self.model.write_text(json.dumps(bad))
        r = self.run_op()
        self.assertEqual(r.returncode, 2)
        self.assertIn(str(self.model), r.stderr)


if __name__ == "__main__":
    unittest.main()
