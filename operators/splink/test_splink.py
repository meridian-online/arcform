# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Cover splink.py's decision half (`model_problems`) with the standard library alone.

Splink, DuckDB and pyarrow load inside `main()`, so every refusal the operator makes
before building a Linker is testable here without them.
"""
from __future__ import annotations

import copy
import importlib.util
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent


def _load():
    spec = importlib.util.spec_from_file_location("splink_op_under_test", HERE / "splink.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


op = _load()

MODEL = {
    "link_type": "link_only",
    "unique_id_column_name": "unique_id",
    "blocking_rules_to_generate_predictions": ["l.postcode = r.postcode"],
    "additional_columns_to_retain": ["note"],
    "comparisons": [
        {
            "output_column_name": "name",
            "comparison_levels": [
                {"sql_condition": '"name_l" IS NULL OR "name_r" IS NULL', "is_null_level": True},
                {
                    "sql_condition": '"name_l" = "name_r"',
                    "label_for_charts": "exact",
                    "m_probability": 0.9,
                    "u_probability": 0.01,
                },
                {"sql_condition": "ELSE", "m_probability": 0.1, "u_probability": 0.99},
            ],
        }
    ],
}
COLS = {"unique_id": "BIGINT", "name": "VARCHAR", "postcode": "VARCHAR", "note": "VARCHAR"}
TWO = {"a": dict(COLS), "b": dict(COLS)}


def model(**changes):
    m = copy.deepcopy(MODEL)
    m.update(changes)
    return m


class ModelProblemsTest(unittest.TestCase):
    def test_a_complete_two_input_link_model_has_no_problems(self) -> None:
        self.assertEqual(op.model_problems(MODEL, TWO), [])

    def test_a_level_without_m_or_u_is_refused_naming_it(self) -> None:
        m = copy.deepcopy(MODEL)
        del m["comparisons"][0]["comparison_levels"][1]["m_probability"]
        out = op.model_problems(m, TWO)
        self.assertEqual(len(out), 1)
        self.assertIn("exact", out[0])
        self.assertIn("m_probability", out[0])

    def test_the_null_level_needs_no_probabilities(self) -> None:
        self.assertEqual(op.model_problems(MODEL, TWO), [])

    def test_missing_comparisons_is_refused(self) -> None:
        m = copy.deepcopy(MODEL)
        del m["comparisons"]
        self.assertTrue(any("comparisons" in p for p in op.model_problems(m, TWO)))

    def test_dedupe_only_with_two_inputs_is_refused(self) -> None:
        out = op.model_problems(model(link_type="dedupe_only"), TWO)
        self.assertTrue(any("dedupe_only" in p and "2" in p for p in out))

    def test_link_only_with_one_input_is_refused(self) -> None:
        out = op.model_problems(model(link_type="link_only"), {"a": COLS})
        self.assertTrue(any("link_only" in p and "at least two" in p for p in out))

    def test_link_and_dedupe_with_one_input_is_refused(self) -> None:
        out = op.model_problems(model(link_type="link_and_dedupe"), {"a": COLS})
        self.assertTrue(any("link_and_dedupe" in p for p in out))

    def test_an_input_missing_the_unique_id_column_is_refused(self) -> None:
        cols = {k: v for k, v in COLS.items() if k != "unique_id"}
        out = op.model_problems(MODEL, {"a": COLS, "b": cols})
        self.assertTrue(any("'b'" in p and "'unique_id'" in p for p in out))

    def test_an_input_missing_a_comparison_column_is_refused(self) -> None:
        cols = {k: v for k, v in COLS.items() if k != "name"}
        out = op.model_problems(MODEL, {"a": COLS, "b": cols})
        self.assertTrue(any("'b'" in p and "'name'" in p for p in out))

    def test_an_input_missing_a_blocking_column_is_refused(self) -> None:
        cols = {k: v for k, v in COLS.items() if k != "postcode"}
        out = op.model_problems(MODEL, {"a": cols, "b": COLS})
        self.assertTrue(any("'a'" in p and "'postcode'" in p for p in out))

    def test_an_input_missing_a_retained_column_is_refused(self) -> None:
        cols = {k: v for k, v in COLS.items() if k != "note"}
        out = op.model_problems(MODEL, {"a": COLS, "b": cols})
        self.assertTrue(any("'b'" in p and "'note'" in p for p in out))

    def test_a_column_with_different_types_is_refused_naming_roles_and_types(self) -> None:
        out = op.model_problems(MODEL, {"a": COLS, "b": {**COLS, "name": "BIGINT"}})
        (msg,) = out
        for part in ("'name'", "a is VARCHAR", "b is BIGINT"):
            self.assertIn(part, msg)

    def test_blocking_rules_given_as_dicts_are_read(self) -> None:
        m = model(blocking_rules_to_generate_predictions=[{"blocking_rule": "l.town = r.town"}])
        out = op.model_problems(m, TWO)
        self.assertTrue(any("'town'" in p for p in out))


if __name__ == "__main__":
    unittest.main()
