# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""check_findings.py — the committed prose about map-refit stability must agree with
`results.json` and `transform_pricing.json`, or a future re-run of `measure.py` /
`price_transform.py` can silently outdate every number in
`operators/umap_project/README.md` and `CHANGELOG.md` while the JSON moves underneath
them. Stdlib-only, no `uv` needed, so CI can run it every time (wired into the
`operators` job in .github/workflows/ci.yml, which already runs without `uv`).

This does not re-derive the numbers — that needs `uv`, DuckDB, UMAP and the fetched
model, and is `measure.py`'s and `price_transform.py`'s job, not this one's. It
checks two things: that the two places a human reads the numbers (a README table
meant to be skimmed, a CHANGELOG entry meant to stand alone) still say what the
committed JSON says, and that the JSON's own internal invariants — the same k used
throughout, the same row count scored in every comparison — actually hold rather than
being assumed. A mismatch means someone edited the prose, or re-ran a harness and
forgot the prose, or a harness itself regressed; all three are bugs this script exists
to catch rather than to explain.

THREE FIGURES ARE NOT CHECKED, DELIBERATELY, AND SAY SO: the sibling embedder-swap
kNN-overlap numbers (0.13 / 0.28 / 0.40) come from a measurement in a different
repository. There is no copy of that source here to check against, so this script
checks only that the prose DISCLOSES that fact (rather than silently presenting the
three numbers as if they were this repo's own), not that the numbers themselves are
current — nothing here could tell.
"""
from __future__ import annotations

import json
import re
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
    append_fractions = results["append_fractions"]
    append_tags = ("append_05", "append_20", "append_50")
    problems: list[str] = []

    def require(haystack: str, needle: str, where: str) -> None:
        needle = normalise_whitespace(needle)
        if needle not in haystack:
            problems.append(f"{where} does not contain {needle!r}")

    # Internal consistency of results.json itself, independent of any prose: every
    # comparison scored the same base rows, at the same k, and against the same
    # base_n the file states — none of this is asserted by construction, so a
    # regression in measure.py that broke one of them without changing the other
    # would otherwise pass silently.
    for tag in ("control_B", *append_tags):
        c = comparisons[tag]
        if c["n_shared_rows"] != base_n:
            problems.append(
                f"results.json: comparisons.{tag}.n_shared_rows "
                f"({c['n_shared_rows']}) != base_n ({base_n})"
            )
    knn_ks = {comparisons[tag]["knn_overlap_k"] for tag in ("control_B", *append_tags)}
    if len(knn_ks) != 1:
        problems.append(f"results.json: knn_overlap_k is not the same across comparisons: {knn_ks}")
    k = knn_ks.pop()

    # The README's own table: one row per comparison. Every cell — including the
    # label — is derived from results.json; none is a literal that happens to match
    # today's numbers. control_B's "0% (control)" label is the one exception, and it
    # is not a drift risk the way the append labels are: it names the zero-append
    # case by the measurement's own definition, not a figure `measure.py` could
    # silently move — there is no `append_fractions` entry for it to drift from.
    table_labels = {"control_B": "0% (control)"}
    for tag in append_tags:
        frac = append_fractions[tag]
        rows_added = round(base_n * frac)  # matches measure.py's own write_slice math
        table_labels[tag] = f"{pct(frac)} ({rows_added:,} rows)"

    for tag, label in table_labels.items():
        c = comparisons[tag]
        disp = multiplier(c["displacement_normalised_mean"])
        overlap = f"{c['knn_overlap_mean']:.2f}"
        row = f"| {label} | {base_n:,} | {disp} | {overlap} |"
        require(readme, row, f"README.md table ({tag})")

    # Every place "20" (or whatever k actually is) appears as the neighbourhood size
    # in prose is checked against the JSON's own knn_overlap_k, not hardcoded here.
    require(readme, f"{k} nearest", "README.md (k, 'N nearest' phrasing)")
    require(readme, f"{k}-NN overlap", "README.md (k, 'N-NN overlap' table header)")
    require(readme, f"k={k}", "README.md (k, 'k=N' phrasing)")

    # The "38 to 268 times" sentence: the min and max normalised displacement across
    # the three append fractions, in the direction the prose states them (smallest
    # append first).
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

    # transform_pricing.json — the out-of-sample-placement pricing this card's review
    # asked for. Same rule: every pricing figure below is computed from this file
    # and required as a string, not typed a second time by hand — including the
    # derived ones (the gap below the ceiling, the ceiling's share recovered, the
    # gap below the full refit), not only the figures read straight off the JSON.
    # The file's own internal invariants are checked whether or not prose quotes
    # them.
    if PRICING.is_file():
        pricing = json.loads(PRICING.read_text())

        if pricing["base_rows"] != base_n:
            problems.append(
                f"transform_pricing.json: base_rows ({pricing['base_rows']}) != "
                f"results.json base_n ({base_n}) — the two harnesses ran against "
                f"different bases"
            )
        if pricing["knn_overlap_k"] != k:
            problems.append(
                f"transform_pricing.json: knn_overlap_k ({pricing['knn_overlap_k']}) "
                f"!= results.json's ({k})"
            )
        for tag in append_tags:
            drift = pricing["placements"][tag]["transform_repeat_max_abs_diff"]
            if drift != 0.0:
                problems.append(
                    f"transform_pricing.json: placements.{tag}.transform_repeat_max_abs_diff "
                    f"is {drift}, not 0.0 — .transform() is no longer deterministic on "
                    f"repeat calls, and nothing in the committed prose says so"
                )
        base_drift = pricing["base_rows_moved_by_transform"]
        if base_drift != 0.0:
            problems.append(
                f"transform_pricing.json: base_rows_moved_by_transform is {base_drift}, "
                f"not 0.0 — the README's claim that the base rows stay exactly put is "
                f"no longer true of this run"
            )

        mb = f"{pricing['pickled_reducer_bytes'] / 1_000_000:.1f} MB"
        require(readme, mb, "README.md (pickled model size)")
        require(changelog, mb, "CHANGELOG.md (pickled model size)")

        ceiling = f"{pricing['corpus_ceiling_256d_vs_2d'] * 100:.1f}%"
        require(readme, ceiling, "README.md (corpus ceiling)")

        truth_fidelities = {
            "transform": [
                pricing["placements"][t]["transform_vs_256d_truth_mean"] for t in append_tags
            ],
            "full_refit": [
                pricing["placements"][t]["full_refit_vs_256d_truth_mean"] for t in append_tags
            ],
        }
        for kind, values in truth_fidelities.items():
            joined_2dp = " / ".join(f"{v * 100:.1f}%" for v in values)
            require(
                readme,
                joined_2dp,
                f"README.md ({kind} fidelity vs 256-d truth, per-fraction)",
            )

        # The gap-below-the-ceiling and share-of-the-ceiling sentences — the ones a
        # prior round of this card's review had to correct once already, because
        # "points below the ceiling" and "points below the full refit" are different
        # numbers and the wrong one was pinned first. Both are computed here, from
        # the same pricing values checked above, rather than typed a second time.
        ceiling_pct = pricing["corpus_ceiling_256d_vs_2d"] * 100
        transform_pcts = [v * 100 for v in truth_fidelities["transform"]]
        full_refit_pcts = [v * 100 for v in truth_fidelities["full_refit"]]

        gaps_below_ceiling = [ceiling_pct - t for t in transform_pcts]
        require(
            readme,
            f"{round(min(gaps_below_ceiling))} to {round(max(gaps_below_ceiling))} "
            f"points below the ceiling",
            "README.md (.transform() gap below the ceiling)",
        )

        ceiling_share = [f / ceiling_pct * 100 for f in full_refit_pcts]
        require(
            readme,
            f"{round(min(ceiling_share))}-{round(max(ceiling_share))}% of the ceiling",
            "README.md (full refit's share of the ceiling)",
        )
        require(
            changelog,
            f"{round(min(ceiling_share))}-{round(max(ceiling_share))}% of the ceiling",
            "CHANGELOG.md (full refit's share of the ceiling)",
        )

        gaps_below_full_refit = [f - t for f, t in zip(full_refit_pcts, transform_pcts)]
        require(
            changelog,
            f"{min(gaps_below_full_refit):.1f}-{max(gaps_below_full_refit):.1f} points "
            f"under the full refit",
            "CHANGELOG.md (.transform() gap below the full refit)",
        )
        require(
            readme,
            " / ".join(f"{v:.1f}" for v in gaps_below_full_refit),
            "README.md (.transform() gap below the full refit, per-fraction)",
        )

        # CHANGELOG's own "30.4-27.3%" / "27.4-25.7%" range notation, distinct from
        # the README's " / "-joined one checked above.
        for kind, values in (("full_refit", full_refit_pcts), ("transform", transform_pcts)):
            require(
                changelog,
                f"{max(values):.1f}-{min(values):.1f}%",
                f"CHANGELOG.md ({kind} fidelity vs 256-d truth, hyphen range)",
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
            "\nEither the prose drifted from a re-run, a harness itself regressed, or "
            "results.json/transform_pricing.json moved and the prose was not updated to "
            "match. Re-read the JSON and fix operators/umap_project/README.md and/or "
            "CHANGELOG.md — or, if a harness invariant broke, fix the harness.",
            file=sys.stderr,
        )
        return 1

    print("check_findings: README.md and CHANGELOG.md agree with the committed JSON.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
