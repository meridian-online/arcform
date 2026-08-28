# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""The self-test for this directory's two checkers, run in CI beside them.

A CHECKER IS GREEN OVER WORKING CODE AND GREEN OVER BROKEN CODE, AND THE TWO RUNS LOOK
IDENTICAL. So "the committed prose agrees with the JSON" means nothing until "the checker
still bites" has been established, and both of the defects below were live in this
directory before this file existed:

  * `check_findings.py` asked whether a correct rendering of a figure existed ANYWHERE in
    a file. The README stated the `.transform()` gap correctly in one paragraph and
    staled it to "3 to 5" in its conclusion three paragraphs down, and the checker was
    green. Every test in `EveryStatementIsCheckedTest` drives a stale SECOND statement.

  * `price_transform.py`'s bounds put both fidelity arms inside a 20-point window around
    the corpus ceiling and compared them to nothing else. Setting `.transform()`'s
    fidelity equal to the full refit's — which is exactly what scoring its rows against
    the full refit's own base pool produces — cleared every bound at all three fractions,
    and the page would then have read that out-of-sample placement is as faithful as a
    refit. `TheFlatteringFailureTest` is that value, and it now reddens.

Stdlib-only, no `uv`: it runs in ci.yml's `operators` job alongside the checkers it
tests.
"""
from __future__ import annotations

import copy
import importlib.util
import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
APPEND_TAGS = ("append_05", "append_20", "append_50")


def _load(name: str):
    spec = importlib.util.spec_from_file_location(f"{name}_under_test", HERE / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


bounds = _load("placement_bounds")
check_findings = _load("check_findings")
COMMITTED = json.loads((HERE / "transform_pricing.json").read_text())


class TheFlatteringFailureTest(unittest.TestCase):
    """The bound the residual on this card is about: two arms that are one measurement."""

    def test_the_committed_measurement_clears_every_bound(self) -> None:
        self.assertEqual(bounds.pricing_problems(COMMITTED, APPEND_TAGS), [])

    def test_placement_equal_to_the_full_refit_is_refused(self) -> None:
        """Set `.transform()`'s fidelity to the full refit's own, at every fraction —
        what a two-line copy-paste in the harness produces — and the bounds must report
        it at every fraction."""
        broken = copy.deepcopy(COMMITTED)
        for tag in APPEND_TAGS:
            placement = broken["placements"][tag]
            placement["transform_vs_256d_truth_mean"] = placement["full_refit_vs_256d_truth_mean"]
        problems = bounds.pricing_problems(broken, APPEND_TAGS)
        self.assertEqual(len(problems), len(APPEND_TAGS), problems)
        for tag in APPEND_TAGS:
            self.assertTrue(any(tag in p for p in problems), f"{tag} was not reported")

    def test_the_ceiling_bounds_ALONE_are_green_over_that_same_value(self) -> None:
        """Why the relational bound had to be added rather than the window tightened.
        Every one of those flattering values sits comfortably inside the ceiling window,
        so the bounds that existed before it saw nothing."""
        ceiling = COMMITTED["corpus_ceiling_256d_vs_2d"]
        for tag in APPEND_TAGS:
            flattering = COMMITTED["placements"][tag]["full_refit_vs_256d_truth_mean"]
            self.assertEqual(
                bounds.ceiling_problems(tag, "transform", flattering, ceiling),
                [],
                "if the ceiling window caught this, the relational bound would be "
                "redundant and this test is asserting the wrong thing",
            )

    def test_the_relational_bound_is_not_transform_less_than_full_refit(self) -> None:
        """The committed measurement refuses the obvious form. At the 50% append
        `.transform()` reads AHEAD of the full refit — a frozen layout scores a new row
        against a pool that has not moved, while a refit's has — so a `<` bound would
        redden this file against its own numbers. Pinned so nobody re-derives it."""
        at_50 = COMMITTED["placements"]["append_50"]
        self.assertGreater(
            at_50["transform_vs_256d_truth_mean"],
            at_50["full_refit_vs_256d_truth_mean"],
            "if a re-run moved this, the bound in placement_bounds.py can be tightened "
            "to `<` and its docstring rewritten",
        )
        self.assertEqual(bounds.separation_problems("append_50", **{
            "transform_mean": at_50["transform_vs_256d_truth_mean"],
            "full_refit_mean": at_50["full_refit_vs_256d_truth_mean"],
        }), [])

    def test_a_lead_bigger_than_the_slack_is_refused(self) -> None:
        problems = bounds.separation_problems("t", 0.30, 0.30 - bounds.TRANSFORM_ADVANTAGE_SLACK - 0.001)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("leads the full refit", problems[0])

    def test_a_fidelity_at_one_is_refused_as_a_query_against_itself(self) -> None:
        problems = bounds.ceiling_problems("t", "transform", 1.0, 0.303)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("exceeds the corpus ceiling", problems[0])

    def test_a_fidelity_near_zero_is_refused_as_a_broken_comparison(self) -> None:
        problems = bounds.ceiling_problems("t", "full-refit", 0.02, 0.303)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("below 0.5 of the corpus ceiling", problems[0])


class EveryStatementIsCheckedTest(unittest.TestCase):
    """`check_findings.py` over a copy of the real tree, with one figure staled."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        root = Path(self._tmp.name)
        (root / "operators" / "umap_project").mkdir(parents=True)
        self.readme = root / "operators" / "umap_project" / "README.md"
        self.changelog = root / "CHANGELOG.md"
        shutil.copy(HERE.parent.parent / "operators" / "umap_project" / "README.md", self.readme)
        shutil.copy(HERE.parent.parent / "CHANGELOG.md", self.changelog)
        self._saved = (check_findings.README, check_findings.CHANGELOG)
        check_findings.README, check_findings.CHANGELOG = self.readme, self.changelog

    def tearDown(self) -> None:
        check_findings.README, check_findings.CHANGELOG = self._saved
        self._tmp.cleanup()

    def run_checker(self) -> int:
        return check_findings.main()

    def test_the_committed_prose_passes(self) -> None:
        self.assertEqual(self.run_checker(), 0)

    def test_a_stale_SECOND_statement_of_a_figure_reddens(self) -> None:
        """THE DEFECT THAT SHIPPED. The README states the gap twice; the first statement
        stays correct, so an exists-anywhere check is satisfied — asserted below so this
        test cannot pass for the wrong reason — and the second is staled."""
        text = self.readme.read_text()
        first = "3 to 4 points below the ceiling"
        second = "3 to 4 points below this corpus's own ceiling"
        self.assertEqual(text.count(first), 1)
        self.assertEqual(text.count(second), 1)
        self.readme.write_text(text.replace(second, "3 to 9 points below this corpus's own ceiling"))
        self.assertIn(
            first,
            self.readme.read_text(),
            "the correct first statement must survive, or this test proves nothing about "
            "the SECOND site being covered",
        )
        self.assertEqual(self.run_checker(), 1)

    def test_a_stale_FIRST_statement_of_a_figure_reddens(self) -> None:
        text = self.readme.read_text()
        self.readme.write_text(text.replace("3 to 4 points below the ceiling", "3 to 9 points below the ceiling"))
        self.assertEqual(self.run_checker(), 1)

    def test_the_same_figure_staled_in_the_CHANGELOG_reddens(self) -> None:
        text = self.changelog.read_text()
        self.assertIn("3 to 4 points under the ceiling", text)
        self.changelog.write_text(text.replace("3 to 4 points under the ceiling", "3 to 9 points under the ceiling"))
        self.assertEqual(self.run_checker(), 1)

    def test_a_figure_that_stops_being_stated_at_all_reddens(self) -> None:
        """A shape that matches nothing is a check that has stopped checking, and it must
        not read as agreement.

        BOTH statements have to go, which is itself the point: removing one leaves the
        other matching, so the file still states the figure and the check still has
        something to compare. It reddens only when the README stops saying it at all."""
        text = self.readme.read_text()
        stripped = text.replace(
            "points below the ceiling", "points below the roof"
        ).replace(
            "points below this corpus's own ceiling", "points below this corpus's own roof"
        )
        self.assertNotIn("points below the ceiling", stripped)
        self.readme.write_text(stripped)
        self.assertEqual(self.run_checker(), 1)

    def test_a_stale_extra_fidelity_triple_reddens(self) -> None:
        """The set rule. Two triples are legitimate; a third, wherever it came from, is
        a figure nothing in the JSON supports."""
        text = self.readme.read_text()
        self.readme.write_text(text + "\n\nAn older draft said 31.4% / 30.2% / 29.9%.\n")
        self.assertEqual(self.run_checker(), 1)

    def test_the_pickled_model_size_is_checked_in_both_files(self) -> None:
        for path in (self.readme, self.changelog):
            with self.subTest(path=path.name):
                text = path.read_text()
                self.assertIn("3.6 MB", text)
                path.write_text(text.replace("3.6 MB", "9.9 MB"))
                self.assertEqual(self.run_checker(), 1)
                path.write_text(text)

    def test_the_committed_pricing_bounds_are_read_by_this_checker(self) -> None:
        """The bounds are not merely importable from CI — this checker applies them. A
        pricing file whose two arms are one measurement must redden the CI gate, not just
        a harness run nobody observes."""
        broken = copy.deepcopy(COMMITTED)
        for tag in APPEND_TAGS:
            placement = broken["placements"][tag]
            placement["transform_vs_256d_truth_mean"] = placement["full_refit_vs_256d_truth_mean"]
        staged = Path(self._tmp.name) / "transform_pricing.json"
        staged.write_text(json.dumps(broken, indent=2) + "\n")
        saved = check_findings.PRICING
        check_findings.PRICING = staged
        try:
            self.assertEqual(self.run_checker(), 1)
        finally:
            check_findings.PRICING = saved


if __name__ == "__main__":
    unittest.main()
