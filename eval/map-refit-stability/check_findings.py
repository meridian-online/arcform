# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""check_findings.py — the committed prose about map-refit stability must agree with
`results.json`, or a future re-run of `measure.py` can silently outdate every number
in `operators/umap_project/README.md` and `CHANGELOG.md` while `results.json` moves
underneath them. Stdlib-only, no `uv` needed, so CI can run it every time (wired into
the `operators` job in .github/workflows/ci.yml, which already runs without `uv`).

This does not re-derive the numbers — that needs `uv`, DuckDB, UMAP and the fetched
model, and is `measure.py`'s job, not this one's. It only checks that the two places a
human reads the numbers (a README table meant to be skimmed, a CHANGELOG entry meant
to stand alone) still say what the committed JSON says. A mismatch means someone
edited the prose, or re-ran the harness and forgot the prose, and either is a bug this
script exists to catch rather than to explain.

EVERY figure checked below is DERIVED from `results.json` — none is a literal typed
here that happens to match today's numbers. Round three of this card's review found
exactly that mistake already made once: `control_B`'s table row was hardcoded as
`("0", "1.00")` instead of read from the file, so a `results.json` edit that broke the
no-new-rows control — the single number every other row is read against — would have
left this checker green. It is the reason for this paragraph, not just a comment on
one line.

THREE FIGURES ARE NOT CHECKED, DELIBERATELY, AND SAY SO: the sibling embedder-swap
kNN-overlap numbers (0.13 / 0.28 / 0.40) come from a measurement in a different
repository. There is no copy of that source here to check against, so this script
checks only that the prose DISCLOSES that fact (rather than silently presenting the
three numbers as if they were this repo's own), not that the numbers themselves are
current — nothing here could tell.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
RESULTS = HERE / "results.json"
PRICING = HERE / "transform_pricing.json"
README = REPO / "operators" / "umap_project" / "README.md"
CHANGELOG = REPO / "CHANGELOG.md"


def pct(fraction: float) -> str:
    return f"{round(fraction * 100)}%"


def multiplier(value: float) -> str:
    return f"{round(value)}×"


import re


def normalise_whitespace(text: str) -> str:
    """Markdown prose is hard-wrapped in this file for readability, and a soft line
    break (a single newline inside a paragraph) renders as a space, not a break — so
    a multi-word phrase this script checks for can legitimately straddle a wrap point
    in the source bytes. Collapse every run of whitespace to one space before
    searching, on both the haystack and the needle, so a check does not fail merely
    because a sentence wrapped differently than the day this script was written."""
    return re.sub(r"\s+", " ", text)


def main() -> int:
    results = json.loads(RESULTS.read_text())
    readme = normalise_whitespace(README.read_text())
    changelog = normalise_whitespace(CHANGELOG.read_text())

    base_n = results["base_n"]
    corpus_rows = results["corpus_rows_committed"]
    comparisons = results["comparisons"]
    problems: list[str] = []

    def require(haystack: str, needle: str, where: str) -> None:
        needle = normalise_whitespace(needle)
        if needle not in haystack:
            problems.append(f"{where} does not contain {needle!r}")

    # The README's own table: one row per comparison, EVERY cell read from
    # results.json — no override, no exception for the control row.
    table_labels = {
        "control_B": "0% (control)",
        "append_05": "5% (150 rows)",
        "append_20": "20% (600 rows)",
        "append_50": "50% (1,500 rows)",
    }
    for tag, label in table_labels.items():
        c = comparisons[tag]
        disp = multiplier(c["displacement_normalised_mean"])
        overlap = f"{c['knn_overlap_mean']:.2f}"
        row = f"| {label} | {base_n:,} | {disp} | {overlap} |"
        require(readme, row, f"README.md table ({tag})")

    # The "38 to 268 times" sentence: the min and max normalised displacement across
    # the three append fractions, in the direction the prose states them (smallest
    # append first).
    append_tags = ("append_05", "append_20", "append_50")
    normalised = [comparisons[t]["displacement_normalised_mean"] for t in append_tags]
    low = multiplier(min(normalised)).rstrip("×")
    high = multiplier(max(normalised)).rstrip("×")
    require(
        readme,
        f"displacement grows from {low} to {high} times",
        "README.md (displacement-range sentence)",
    )

    # Corpus size and base size, stated in prose in both files.
    require(readme, f"{corpus_rows:,}-row slice", "README.md (corpus size)")
    require(readme, f"{base_n:,} rows of Homebrew", "README.md (base corpus size)")
    require(changelog, f"{base_n:,} real rows", "CHANGELOG.md (base corpus size)")

    # README states the headline percentage once (the 5% case) and the rest of its
    # prose quotes the table's own 2-decimal fraction notation, so only that one
    # figure is checked as a percentage there. CHANGELOG stands alone and spells out
    # all three as percentages, so all three are checked there.
    require(
        readme,
        pct(comparisons["append_05"]["knn_overlap_mean"]),
        "README.md (headline overlap percentage for append_05)",
    )
    for tag in append_tags:
        overlap_2dp = f"{comparisons[tag]['knn_overlap_mean']:.2f}"
        require(readme, overlap_2dp, f"README.md (overlap fraction for {tag})")
        p = pct(comparisons[tag]["knn_overlap_mean"])
        require(changelog, p, f"CHANGELOG.md (overlap percentage for {tag})")

    # The three sibling-repo figures are not derivable from anything committed here —
    # see the module docstring — so what IS checked is that both files disclose that
    # rather than presenting them as this repo's own.
    require(
        readme,
        "this figure comes from a measurement in a different repository",
        "README.md (sibling-figure disclosure)",
    )
    require(
        changelog,
        "a different repository",
        "CHANGELOG.md (sibling-figure disclosure)",
    )

    # transform_pricing.json — the out-of-sample-placement pricing round three of
    # this card's review asked for. Same rule: every figure quoted in prose is
    # derived here, not retyped.
    if PRICING.is_file():
        pricing = json.loads(PRICING.read_text())
        mb = f"{pricing['pickled_reducer_bytes'] / 1_000_000:.1f} MB"
        require(readme, mb, "README.md (pickled model size)")
        require(changelog, mb, "CHANGELOG.md (pickled model size)")

        fidelity_values = [
            pricing["placements"][t]["knn_overlap_with_full_refit_mean"]
            for t in ("append_05", "append_20", "append_50")
        ]
        fidelities = [f"{v:.2f}" for v in fidelity_values]
        require(
            readme,
            f"{fidelities[0]} / {fidelities[1]} / {fidelities[2]}",
            "README.md (transform placement fidelity, per-fraction)",
        )
        low = f"{min(fidelity_values):.2f}"
        high = f"{max(fidelity_values):.2f}"
        require(
            readme,
            f"{low}–{high} fidelity",
            "README.md (transform placement fidelity range)",
        )
        require(
            changelog,
            f"{low}-{high} placement fidelity",
            "CHANGELOG.md (transform placement fidelity range)",
        )
    else:
        problems.append(
            "transform_pricing.json is missing — the out-of-sample pricing numbers "
            "in README.md and CHANGELOG.md cannot be checked at all"
        )

    if problems:
        print("check_findings: the committed prose disagrees with results.json:", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        print(
            "\nEither the prose drifted from a re-run, or results.json moved and the "
            "prose was not updated to match. Re-read eval/map-refit-stability/results.json "
            "and fix operators/umap_project/README.md and/or CHANGELOG.md.",
            file=sys.stderr,
        )
        return 1

    print("check_findings: README.md and CHANGELOG.md agree with results.json.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
