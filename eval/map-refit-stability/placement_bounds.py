# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""The bounds a placement-fidelity measurement has to clear, written down once.

TWO READERS, ONE DEFINITION, AND THE SECOND READER IS THE POINT. `price_transform.py`
asserts these at harness time, where they catch a broken measurement before its numbers
reach a README. `check_findings.py` applies the same functions to the COMMITTED
`transform_pricing.json`, in CI, where they catch a broken measurement whose numbers
already did. Before this file the bounds lived only in the harness, and the harness runs
on a developer's machine when a developer thinks to run it: its greenness was never
observed by a gate, so a run that quietly stopped clearing them looked exactly like a run
nobody had done.

Stdlib-only and importing nothing from the harness, so the CI reader needs no `uv`, no
numpy and no UMAP.

WHAT EACH BOUND IS FOR — three different wrong measurements, three different shapes:

1. FAR BELOW THE CEILING. A 2-D map cannot recover every neighbour a 256-d space has, so
   fidelity is read against this corpus's own ceiling rather than against 1.0. A mean far
   under that ceiling is much more likely a harness comparing the wrong pools than a real
   finding — observed near 0.0-0.07 under a pool swap while writing the harness.

2. ABOVE THE CEILING. No placement of a NEW row can recover more structure than the
   corpus's own base rows do. A mean at or near 1.0 is what a query compared against
   ITSELF reads, and without this bound it reads as an unusually good result.

3. THE TWO ARMS ARE THE SAME MEASUREMENT. This is the flattering failure, and it is the
   one bounding against the ceiling cannot see. Scoring `.transform()`'s placement
   against the FULL REFIT's own base pool — a two-line copy-paste in the harness — makes
   `transform_vs_256d_truth_mean` come back bit-identical to `full_refit_vs_256d_truth_mean`.
   Both land inside the ceiling window at every fraction, nothing asserts, and the page
   then reads that out-of-sample placement is exactly as faithful as a refit, which is
   the strongest possible version of the recommendation and false.

ON THE RELATIONAL BOUND, AND WHY IT IS NOT `transform < full_refit`. The obvious form —
placement should never equal or beat a refit that saw the row — is refused by the
committed measurement itself: at the 50% append `.transform()` reads 0.2720 against the
full refit's 0.2699, marginally AHEAD, and the operator's README already states that. A
refit's base pool moves as the corpus grows while a persisted fit's does not, so at a
large append the frozen layout can place a new row against a more stable pool. Shipping
`<` would have reddened this file against its own committed numbers. What is asserted
instead is the pair that IS true of a correct measurement: the two arms must not be the
same number (case 3 makes them bit-identical), and placement must not beat a refit by a
MARGIN — `TRANSFORM_ADVANTAGE_SLACK`, ten times the largest lead the committed run shows.
"""
from __future__ import annotations

# A mean this far under the ceiling is a broken comparison, not a poor result. Loose on
# purpose: real values land within a few points of the ceiling, not at a fraction of it.
CEILING_FRACTION_FLOOR = 0.5

# How far above the ceiling ordinary measurement noise may reach. Five percentage points,
# chosen to catch a degenerate query-against-itself comparison (which reads 1.0) without
# being brittle.
CEILING_SLACK = 0.05

# How far `.transform()` may lead a full refit before the lead is a finding rather than
# noise. The committed run's largest lead is 0.0021 (append_50); this is ten times it.
TRANSFORM_ADVANTAGE_SLACK = 0.02


def ceiling_problems(tag: str, label: str, mean: float, ceiling: float) -> list[str]:
    """Bounds 1 and 2 for one fidelity mean, as a list of one-line complaints."""
    problems = []
    if not mean >= CEILING_FRACTION_FLOOR * ceiling:
        problems.append(
            f"{tag}: {label} fidelity against the 256-d truth ({mean:.4f}) is below "
            f"{CEILING_FRACTION_FLOOR} of the corpus ceiling ({ceiling:.4f}). A correct "
            f"comparison lands within a few points of the ceiling, so this is far more "
            f"likely a harness comparing the wrong pools than a real finding."
        )
    if not mean <= ceiling + CEILING_SLACK:
        problems.append(
            f"{tag}: {label} fidelity against the 256-d truth ({mean:.4f}) exceeds the "
            f"corpus ceiling ({ceiling:.4f}) by more than the stated slack "
            f"({CEILING_SLACK}). No placement of a new row can recover more structure "
            f"than the corpus's own base rows do, so this is most likely a query "
            f"compared against itself."
        )
    return problems


def separation_problems(tag: str, transform_mean: float, full_refit_mean: float) -> list[str]:
    """Bound 3: the two arms must be two measurements, and placement must not beat a
    refit that saw the row by a margin."""
    problems = []
    if transform_mean == full_refit_mean:
        problems.append(
            f"{tag}: transform_vs_256d_truth_mean and full_refit_vs_256d_truth_mean are "
            f"the same number ({transform_mean!r}). Two placements into two different "
            f"layouts, scored row by row, do not land on the same float; this is what "
            f"scoring .transform()'s rows against the full refit's own base pool "
            f"produces, and it would have the page read that out-of-sample placement is "
            f"exactly as faithful as a refit."
        )
    if transform_mean > full_refit_mean + TRANSFORM_ADVANTAGE_SLACK:
        problems.append(
            f"{tag}: .transform() ({transform_mean:.4f}) leads the full refit "
            f"({full_refit_mean:.4f}) by more than {TRANSFORM_ADVANTAGE_SLACK}. A "
            f"placement into a frozen layout can edge ahead of a refit whose own base "
            f"pool has moved — the committed run shows 0.0021 at the 50% append — but "
            f"not by this much. Check which pool each arm was scored against."
        )
    return problems


def pricing_problems(pricing: dict, tags) -> list[str]:
    """Every bound above, applied to a `transform_pricing.json` payload.

    The one entry point both readers use, so neither can drift into checking a subset.
    """
    problems: list[str] = []
    ceiling = pricing["corpus_ceiling_256d_vs_2d"]
    for tag in tags:
        placement = pricing["placements"][tag]
        transform_mean = placement["transform_vs_256d_truth_mean"]
        full_refit_mean = placement["full_refit_vs_256d_truth_mean"]
        problems += ceiling_problems(tag, "full-refit", full_refit_mean, ceiling)
        problems += ceiling_problems(tag, "transform", transform_mean, ceiling)
        problems += separation_problems(tag, transform_mean, full_refit_mean)
    return problems
