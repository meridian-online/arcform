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
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
RESULTS = HERE / "results.json"
README = REPO / "operators" / "umap_project" / "README.md"
CHANGELOG = REPO / "CHANGELOG.md"


def pct(fraction: float) -> str:
    return f"{round(fraction * 100)}%"


def multiplier(value: float) -> str:
    return f"{round(value)}×"


def main() -> int:
    results = json.loads(RESULTS.read_text())
    readme = README.read_text()
    changelog = CHANGELOG.read_text()

    base_n = results["base_n"]
    problems: list[str] = []

    def require(haystack: str, needle: str, where: str) -> None:
        if needle not in haystack:
            problems.append(f"{where} does not contain {needle!r}")

    # The README's own table: one row per comparison, in the shape the table uses.
    table_rows = {
        "control_B": ("0% (control)", "0", "1.00"),
        "append_05": ("5% (150 rows)", None, None),
        "append_20": ("20% (600 rows)", None, None),
        "append_50": ("50% (1,500 rows)", None, None),
    }
    for tag, (label, disp_override, overlap_override) in table_rows.items():
        c = results["comparisons"][tag]
        disp = disp_override or multiplier(c["displacement_normalised_mean"])
        overlap = overlap_override or f"{c['knn_overlap_mean']:.2f}"
        row = f"| {label} | {base_n:,} | {disp} | {overlap} |"
        require(readme, row, "README.md table")

    # README states the headline percentage once (the 5% case) and the rest of its
    # prose quotes the table's own 2-decimal fraction notation, so only that one
    # figure is checked as a percentage there. CHANGELOG stands alone and spells out
    # all three as percentages, so all three are checked there.
    require(
        readme,
        pct(results["comparisons"]["append_05"]["knn_overlap_mean"]),
        "README.md (headline overlap percentage for append_05)",
    )
    for tag in ("append_05", "append_20", "append_50"):
        overlap_2dp = f"{results['comparisons'][tag]['knn_overlap_mean']:.2f}"
        require(readme, overlap_2dp, f"README.md (overlap fraction for {tag})")
        p = pct(results["comparisons"][tag]["knn_overlap_mean"])
        require(changelog, p, f"CHANGELOG.md (overlap percentage for {tag})")

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
