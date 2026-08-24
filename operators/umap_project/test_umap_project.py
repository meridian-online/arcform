# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Cover the half of umap_project.py that decides, without running a projection.

Everything this operator does at run time needs `uv`, umap-learn and DuckDB, and CI
has none of them — so the end-to-end tests in `tests/umap_project.rs` skip there and
the script's own lines are reached by nothing. That is why umap_project.py imports
duckdb, numpy and umap INSIDE `main()` rather than at module scope: the decidable
half above it — which column types can be placed on a map, how many features each
contributes, and the SQL quoting that carries a column name into a query — imports
with the standard library alone and is covered here.

The classifier is the load-bearing piece. It is what turns "you asked to project a
VARCHAR" into one line the caller can act on instead of an exception from inside
UMAP, and it is what lets a vector column written by another step be projected at
all: a fixed-size array survives a Parquet round trip as a LIST, so `FLOAT[16]`
written and `FLOAT[]` read back have to answer the same way.

Stdlib-only (unittest), matching the shape of describe.py's tests.
"""
from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent


def _load():
    spec = importlib.util.spec_from_file_location(
        "umap_project_under_test", HERE / "umap_project.py"
    )
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


up = _load()


class NumericShapeTest(unittest.TestCase):
    def test_numeric_scalars_are_one_feature_each(self) -> None:
        for duckdb_type in (
            "TINYINT",
            "SMALLINT",
            "INTEGER",
            "BIGINT",
            "HUGEINT",
            "UTINYINT",
            "USMALLINT",
            "UINTEGER",
            "UBIGINT",
            "UHUGEINT",
            "FLOAT",
            "DOUBLE",
        ):
            self.assertEqual(
                up.numeric_shape(duckdb_type), "scalar", f"{duckdb_type} is a number"
            )

    def test_decimal_carries_its_precision_and_is_still_a_number(self) -> None:
        # DESCRIBE reports DECIMAL with its precision and scale, so an exact-match
        # set would miss every decimal column there is.
        self.assertEqual(up.numeric_shape("DECIMAL(18,3)"), "scalar")
        self.assertEqual(up.numeric_shape("DECIMAL(4,1)"), "scalar")

    def test_a_list_or_array_of_numbers_is_a_vector(self) -> None:
        # FLOAT[16] is what text_embed writes; FLOAT[] is what comes back after the
        # Parquet round trip. Both have to be projectable or a chained Protocol
        # would refuse its own previous step's output.
        for duckdb_type in ("FLOAT[]", "FLOAT[16]", "DOUBLE[]", "DOUBLE[128]", "INTEGER[]"):
            self.assertEqual(
                up.numeric_shape(duckdb_type), "vector", f"{duckdb_type} is a vector"
            )

    def test_a_nested_list_of_numbers_is_still_a_vector(self) -> None:
        self.assertEqual(up.numeric_shape("DOUBLE[][]"), "vector")

    def test_types_that_are_not_numbers_are_refused(self) -> None:
        for duckdb_type in (
            "VARCHAR",
            "BOOLEAN",
            "DATE",
            "TIMESTAMP",
            "BLOB",
            "UUID",
            "INTERVAL",
            "VARCHAR[]",
            "STRUCT(a INTEGER)",
            "MAP(VARCHAR, INTEGER)",
        ):
            self.assertIsNone(
                up.numeric_shape(duckdb_type),
                f"{duckdb_type} is not a number this operator can place on a map",
            )

    def test_classification_ignores_case_and_surrounding_space(self) -> None:
        self.assertEqual(up.numeric_shape("  double  "), "scalar")
        self.assertEqual(up.numeric_shape("float[16]"), "vector")

    def test_a_malformed_bracket_is_not_a_vector(self) -> None:
        self.assertIsNone(up.numeric_shape("DOUBLE]"))
        # The DECIMAL case is the one that pins the guard rather than merely agreeing
        # with it. A closing bracket with no opening one has to end the unwrapping
        # loop, and `break` terminates it just as `return None` does — for "DOUBLE]"
        # the two are indistinguishable, because the leftover "DOUBLE]" is not in the
        # numeric set either way. DECIMAL is matched by PREFIX, so "DECIMAL(9,2)]"
        # would come back a number under `break` and does not under the refusal. This
        # test was green against both until this line was added.
        self.assertIsNone(up.numeric_shape("DECIMAL(9,2)]"))


class FeatureWidthsTest(unittest.TestCase):
    def test_a_scalar_column_contributes_one_feature(self) -> None:
        rows = [(1.0, 2.0), (3.0, 4.0)]
        self.assertEqual(
            up.feature_widths(rows, ["lon", "lat"], ["scalar", "scalar"]), [1, 1]
        )

    def test_a_vector_column_contributes_one_feature_per_element(self) -> None:
        rows = [([1.0, 2.0, 3.0],), ([4.0, 5.0, 6.0],)]
        self.assertEqual(up.feature_widths(rows, ["v"], ["vector"]), [3])

    def test_a_scalar_and_a_vector_side_by_side(self) -> None:
        rows = [(1.0, [1.0, 2.0]), (2.0, [3.0, 4.0])]
        self.assertEqual(
            up.feature_widths(rows, ["lon", "v"], ["scalar", "vector"]), [1, 2]
        )

    def test_a_ragged_vector_column_is_refused_naming_the_widths(self) -> None:
        rows = [([1.0, 2.0],), ([1.0, 2.0, 3.0],)]
        with self.assertRaises(up.Refusal) as ctx:
            up.feature_widths(rows, ["v"], ["vector"])
        said = str(ctx.exception)
        self.assertIn("'v'", said)
        self.assertIn("2, 3", said)

    def test_a_null_vector_is_refused_with_a_count(self) -> None:
        rows = [([1.0, 2.0],), (None,), (None,)]
        with self.assertRaises(up.Refusal) as ctx:
            up.feature_widths(rows, ["v"], ["vector"])
        said = str(ctx.exception)
        self.assertIn("2 of 3", said)
        self.assertIn("'v'", said)

    def test_an_empty_vector_has_nothing_to_project(self) -> None:
        rows = [([],), ([],)]
        with self.assertRaises(up.Refusal) as ctx:
            up.feature_widths(rows, ["v"], ["vector"])
        self.assertIn("empty", str(ctx.exception))


class ComputeFitIdTest(unittest.TestCase):
    """`compute_fit_id` is stdlib-only, hoisted out of `main()` so this file — the one
    test CI runs for this operator without `uv` — can pin its value-sensitivity
    directly.

    This does NOT cover the call site in `main()` — whether `main()` actually passes
    `matrix.tobytes()` rather than something shape-only still needs `uv` and numpy to
    exercise, and stays covered only by
    tests/umap_project.rs::projection_fit_id_moves_when_a_value_changes_with_the_shape_held_fixed.
    What this pins is narrower and still real: the hash function itself is sensitive
    to its payload's CONTENT, not merely its length.
    """

    KNOBS = (15, 0.1, "cosine", 42)

    def test_the_same_payload_and_knobs_give_the_same_id(self) -> None:
        payload = b"\x00\x01\x02\x03" * 8
        first = up.compute_fit_id(payload, *self.KNOBS)
        second = up.compute_fit_id(payload, *self.KNOBS)
        self.assertEqual(first, second)

    def test_a_different_payload_of_the_same_length_moves_the_id(self) -> None:
        # Same LENGTH (the byte-string analogue of "shape") on both sides, one byte
        # different — this is the case `str(matrix.shape).encode()` collapses: a
        # fingerprint built from shape alone cannot move here, because the shape
        # never changes; only a fingerprint built from the actual bytes can.
        payload_a = b"\x00\x01\x02\x03" * 8
        payload_b = b"\x00\x01\x02\x04" * 8
        self.assertEqual(len(payload_a), len(payload_b))
        self.assertNotEqual(payload_a, payload_b)
        self.assertNotEqual(
            up.compute_fit_id(payload_a, *self.KNOBS),
            up.compute_fit_id(payload_b, *self.KNOBS),
        )

    def test_a_different_knob_moves_the_id_even_with_the_same_payload(self) -> None:
        payload = b"\x00\x01\x02\x03" * 8
        baseline = up.compute_fit_id(payload, 15, 0.1, "cosine", 42)
        self.assertNotEqual(baseline, up.compute_fit_id(payload, 40, 0.1, "cosine", 42))
        self.assertNotEqual(baseline, up.compute_fit_id(payload, 15, 0.9, "cosine", 42))
        self.assertNotEqual(
            baseline, up.compute_fit_id(payload, 15, 0.1, "euclidean", 42)
        )

    def test_the_id_is_a_short_hex_string(self) -> None:
        fit_id = up.compute_fit_id(b"anything", *self.KNOBS)
        self.assertEqual(len(fit_id), 16)
        int(fit_id, 16)  # raises ValueError if it is not hex


class SqlQuotingTest(unittest.TestCase):
    def test_an_identifier_is_double_quoted_and_interior_quotes_are_doubled(self) -> None:
        self.assertEqual(up.sql_ident("median income"), '"median income"')
        self.assertEqual(up.sql_ident('od"d'), '"od""d"')

    def test_a_literal_is_single_quoted_and_interior_quotes_are_doubled(self) -> None:
        self.assertEqual(up.sql_lit("a/b.parquet"), "'a/b.parquet'")
        self.assertEqual(up.sql_lit("it's"), "'it''s'")


class DefaultsTest(unittest.TestCase):
    def test_the_default_metric_is_one_the_operator_accepts(self) -> None:
        # argparse would reject its own default otherwise, and the failure would land
        # on whichever Protocol first omitted `metric:`.
        self.assertIn(up.DEFAULT_METRIC, up.METRICS)

    def test_the_ordinal_cannot_be_mistaken_for_a_real_column(self) -> None:
        # It is added to the input's own columns and excluded again on the way out;
        # a plain name would collide with a real one and be silently dropped.
        self.assertTrue(up.ROW.startswith("__arc"))
        self.assertNotIn(up.ROW, (up.X_COL, up.Y_COL, up.FIT_ID_COL))

    def test_the_three_added_columns_are_pairwise_distinct(self) -> None:
        # Each is checked for a clash against the input separately (see
        # `clashes = [c for c in (X_COL, Y_COL, FIT_ID_COL) ...]` in main()); if two of
        # them were equal, a Parquet carrying one of the names would be refused for the
        # wrong reason, or the CREATE TABLE that adds all three would collide with
        # itself.
        added = (up.X_COL, up.Y_COL, up.FIT_ID_COL)
        self.assertEqual(len(added), len(set(added)))


if __name__ == "__main__":
    unittest.main()
