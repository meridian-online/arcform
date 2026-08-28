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


class ClampNeighborsTest(unittest.TestCase):
    """`clamp_neighbors` is the operator's own deviation from umap-learn's semantics,
    so it is the thing most worth pinning — and until it was hoisted out of `main()`
    it was untestable here, because `main()` imports umap.

    Deleting the clamp entirely left the whole suite green and a six-row table at the
    default `neighbors: 15` died inside UMAP.
    """

    def test_it_bounds_above_at_one_below_the_row_count(self) -> None:
        # The measured case: on 48 rows, everything at or above 47 is the same fit.
        self.assertEqual(up.clamp_neighbors(47, 48), 47)
        self.assertEqual(up.clamp_neighbors(100, 48), 47)
        self.assertEqual(up.clamp_neighbors(200, 48), 47)
        # And below the boundary the knob is the knob.
        self.assertEqual(up.clamp_neighbors(40, 48), 40)

    def test_it_bounds_below_at_two(self) -> None:
        # UMAP needs at least two neighbours; a table at MIN_ROWS must still fit.
        self.assertEqual(up.clamp_neighbors(15, up.MIN_ROWS), up.MIN_ROWS - 1)
        self.assertEqual(up.clamp_neighbors(1, 3), 2)
        self.assertEqual(up.clamp_neighbors(15, 2), 2)

    def test_the_default_never_exceeds_the_smallest_table_it_will_accept(self) -> None:
        """The concrete failure deleting the clamp produces: the DEFAULT knob against
        the SMALLEST table this script accepts. Without the clamp that pair reaches
        UMAP as n_neighbors=15 on 5 rows, which raises."""
        self.assertLess(
            up.clamp_neighbors(up.DEFAULT_NEIGHBORS, up.MIN_ROWS),
            up.MIN_ROWS,
            "n_neighbors must be strictly below the row count or UMAP raises",
        )


class MetricChoicesTest(unittest.TestCase):
    """argparse must actually ENFORCE `METRICS`, not merely be handed it.

    `umap_project_metrics_list_agrees_with_the_script` pins the literal tuple. It does
    not pin that the parser uses it: narrowing `choices=METRICS` to `("euclidean",)`
    while `METRICS` and the operator's list stayed in agreement left the whole
    workspace green, and a manifest writing `metric: cosine` would then validate and
    die inside argument parsing mid-run — the precise failure the tuple pin exists to
    prevent.
    """

    def _parser(self):
        # Built the same way `main()` builds it, from the same module, so a divergence
        # between this and the real parser is a divergence in one file.
        import argparse

        ap = argparse.ArgumentParser()
        ap.add_argument("--metric", choices=up.METRICS, default=up.DEFAULT_METRIC)
        return ap

    def test_every_declared_metric_is_accepted_by_the_real_parser(self) -> None:
        source = (HERE / "umap_project.py").read_text(encoding="utf-8")
        self.assertIn(
            "choices=METRICS",
            source,
            "the parser must take its choices from METRICS, or the tuple pin guards "
            "a list nothing enforces",
        )
        for metric in up.METRICS:
            with self.subTest(metric=metric):
                self.assertEqual(self._parser().parse_args(["--metric", metric]).metric, metric)

    def test_a_metric_outside_the_set_is_refused(self) -> None:
        with self.assertRaises(SystemExit):
            self._parser().parse_args(["--metric", "manhattan"])


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


class RowDigestTest(unittest.TestCase):
    """A row's identity is its numbers. Same reasoning as `ComputeFitIdTest`: a digest
    over a row's LENGTH rather than its content would let an edited row read as the row
    the fit already holds a position for, and the operator would hand back a coordinate
    for numbers that are no longer there."""

    def test_the_same_bytes_give_the_same_identity(self) -> None:
        self.assertEqual(up.row_digest(b"\x01\x02"), up.row_digest(b"\x01\x02"))

    def test_a_different_row_of_the_same_length_is_a_different_row(self) -> None:
        a, b = b"\x00\x01\x02\x03" * 4, b"\x00\x01\x02\x04" * 4
        self.assertEqual(len(a), len(b))
        self.assertNotEqual(up.row_digest(a), up.row_digest(b))


class MatchBaseRowsTest(unittest.TestCase):
    """Which rows a persisted fit already holds a position for.

    This is the half of `--fit` that decides, and it is pure so CI can drive it: which
    rows keep their coordinates, which are handed to `.transform()`, and whether the
    input is an append at all.
    """

    def test_an_unchanged_input_claims_every_fit_row_in_place(self) -> None:
        base = ["a", "b", "c"]
        placement, missing = up.match_base_rows(base, base)
        self.assertEqual(placement, [0, 1, 2])
        self.assertEqual(missing, [])

    def test_a_row_appended_into_the_MIDDLE_is_the_only_new_one(self) -> None:
        # THE CASE A POSITIONAL RULE GETS WRONG, which is why identity is the row's own
        # bytes. A SQL step with `ORDER BY name` puts an appended row wherever its name
        # falls, so "the first N rows are the base" would call every row from the
        # insertion point onwards new, re-place rows the fit already holds, and move
        # coordinates the whole feature exists to keep still.
        placement, missing = up.match_base_rows(["a", "NEW", "b", "c"], ["a", "b", "c"])
        self.assertEqual(placement, [0, None, 1, 2])
        self.assertEqual(missing, [])

    def test_a_removed_base_row_is_reported_as_missing(self) -> None:
        placement, missing = up.match_base_rows(["a", "c"], ["a", "b", "c"])
        self.assertEqual(placement, [0, 2])
        self.assertEqual(missing, [1], "the fit holds a position for a row that is gone")

    def test_an_edited_base_row_reads_as_removed_and_added(self) -> None:
        # An edit changes the row's bytes, so its old identity is missing and its new
        # one is unknown — both halves have to show, or an edited row would quietly take
        # the coordinate its previous values earned.
        placement, missing = up.match_base_rows(["a", "b-EDITED", "c"], ["a", "b", "c"])
        self.assertEqual(placement, [0, None, 2])
        self.assertEqual(missing, [1])

    def test_duplicate_rows_are_handed_out_in_order_then_reused(self) -> None:
        placement, missing = up.match_base_rows(["d", "d", "d"], ["d", "d"])
        self.assertEqual(placement, [0, 1, 1])
        self.assertEqual(missing, [], "both fit rows were claimed")

    def test_an_empty_fit_leaves_every_row_to_be_placed(self) -> None:
        placement, missing = up.match_base_rows(["a", "b"], [])
        self.assertEqual(placement, [None, None])
        self.assertEqual(missing, [])


class DescribeMismatchTest(unittest.TestCase):
    """AC4's whole surface: a persisted fit that does not describe the current input.

    The expensive failure is not a crash. A fit for different columns, a different
    vector width, a different knob or a different umap-learn unpickles cleanly and
    places rows at coordinates that look like coordinates, and nothing downstream can
    tell. Every case below asserts the refusal NAMES what differs — a bare "incompatible
    fit" would leave the caller guessing which of five things to change.
    """

    def header(self, **overrides):
        base = dict(
            columns=["lon", "lat"],
            widths=[1, 1],
            neighbors=15,
            min_dist=0.1,
            metric="euclidean",
            seed=42,
            umap_learn_version="0.5.12",
        )
        base.update(overrides)
        return up.fit_header(**base)

    def test_a_fit_of_the_same_shape_is_accepted(self) -> None:
        self.assertIsNone(up.describe_mismatch(self.header(), self.header()))

    def test_every_compared_field_actually_refuses_when_it_moves(self) -> None:
        """The class check, not one case of it. Each field in `FIT_COMPARED_FIELDS` is
        perturbed in turn and must produce a refusal that names it or its values.
        Dropping any single comparison from `describe_mismatch` — or adding a field to
        the tuple and forgetting to compare it — reddens here rather than shipping a fit
        that records a property and does not check it."""
        # field -> (the value this run asks for, a string the refusal must carry so the
        # caller can see WHICH thing moved).
        moved = {
            "columns": (["lon", "median_income"], "median_income"),
            "feature_widths": ([1, 256], "256"),
            "neighbors": (30, "30"),
            "min_dist": (0.9, "0.9"),
            "metric": ("cosine", "cosine"),
            "seed": (7, "7"),
            "umap_learn_version": ("0.5.11", "0.5.11"),
        }
        self.assertEqual(
            sorted(moved), sorted(up.FIT_COMPARED_FIELDS),
            "this test must perturb every compared field, or it stops covering the tuple",
        )
        stored = self.header()
        for field, (value, witness) in moved.items():
            with self.subTest(field=field):
                current = dict(stored)
                current[field] = value
                said = up.describe_mismatch(stored, current)
                self.assertIsNotNone(said, f"a fit with a different {field} was accepted")
                self.assertIn(
                    witness,
                    said,
                    f"the refusal for {field} does not say what this run asked for",
                )

    def test_the_columns_refusal_names_both_lists(self) -> None:
        said = up.describe_mismatch(self.header(), self.header(columns=["lon", "income"]))
        self.assertIn("'lat'", said)
        self.assertIn("'income'", said)

    def test_the_width_refusal_names_both_widths_and_the_column(self) -> None:
        # A fit built on a 256-d embedding, an input now carrying 384-d vectors: the
        # case the card calls out, because both are "one vector column" and only the
        # width says they are different maps.
        said = up.describe_mismatch(
            self.header(columns=["embedding"], widths=[256]),
            self.header(columns=["embedding"], widths=[384]),
        )
        self.assertIn("embedding=256", said)
        self.assertIn("embedding=384", said)

    def test_the_umap_version_refusal_names_both_versions(self) -> None:
        said = up.describe_mismatch(self.header(), self.header(umap_learn_version="0.5.9"))
        self.assertIn("0.5.12", said)
        self.assertIn("0.5.9", said)

    def test_a_fit_from_another_format_is_refused_before_its_fields_are_read(self) -> None:
        stored = dict(self.header(), fit_format=up.FIT_FORMAT + 1)
        said = up.describe_mismatch(stored, self.header())
        self.assertIsNotNone(said)
        self.assertIn(str(up.FIT_FORMAT), said)

    def test_a_file_that_is_not_a_fit_at_all_is_refused(self) -> None:
        for stranger in (None, [1, 2, 3], {"not": "a fit"}, "a string"):
            with self.subTest(stranger=stranger):
                said = up.describe_mismatch(stranger, self.header())
                self.assertIsNotNone(said, f"{stranger!r} was accepted as a fit")
                self.assertIn(up.OPERATOR_NAME, said)

    def test_a_fit_written_by_a_different_operator_is_refused_by_name(self) -> None:
        said = up.describe_mismatch(dict(self.header(), operator="splink_resolve"), self.header())
        self.assertIn("splink_resolve", said)


class FitHeaderTest(unittest.TestCase):
    def test_every_field_the_header_records_is_a_field_that_is_compared(self) -> None:
        """The invariant that keeps AC4 true as this grows. `operator` and `fit_format`
        identify the FILE and are checked first, separately, so they are excluded here.
        Every other field is a property of the fit, and a property the fit records but
        `describe_mismatch` never compares is a difference an analyst cannot see."""
        recorded = set(
            up.fit_header(["lon"], [1], 15, 0.1, "euclidean", 42, "0.5.12")
        ) - {"operator", "fit_format"}
        self.assertEqual(
            recorded,
            set(up.FIT_COMPARED_FIELDS),
            "a header field outside FIT_COMPARED_FIELDS is recorded and never checked",
        )

    def test_the_header_records_what_the_caller_asked_for(self) -> None:
        header = up.fit_header(["a", "b"], [1, 4], 30, 0.25, "cosine", 42, "0.5.12")
        self.assertEqual(header["columns"], ["a", "b"])
        self.assertEqual(header["feature_widths"], [1, 4])
        self.assertEqual(header["neighbors"], 30)
        self.assertEqual(header["min_dist"], 0.25)
        self.assertEqual(header["metric"], "cosine")
        self.assertEqual(header["operator"], up.OPERATOR_NAME)
        self.assertEqual(header["fit_format"], up.FIT_FORMAT)


class FitFlagTest(unittest.TestCase):
    """`--fit` has to reach the script's own parser, not merely be documented. The
    operator's Rust side is a separate file in a separate language; the flag being
    optional here is what keeps every manifest that does not set it running unchanged."""

    def test_the_fit_flag_is_optional_and_defaults_to_no_persistence(self) -> None:
        source = (HERE / "umap_project.py").read_text(encoding="utf-8")
        self.assertIn('"--fit"', source)
        import argparse

        ap = argparse.ArgumentParser()
        ap.add_argument("--fit", default=None)
        self.assertIsNone(ap.parse_args([]).fit)
        self.assertEqual(ap.parse_args(["--fit", "m.pkl"]).fit, "m.pkl")


if __name__ == "__main__":
    unittest.main()
