# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = [
#   "duckdb>=1,<2",
#   "numpy>=1.26,<3",
# ]
# ///
"""measure.py — how far umap_project's map moves when rows are appended and refit.

This operator persists nothing between invocations, so appending rows means
refitting the whole map on the next run, and a refit moves every point. Nobody had
measured how much. This does, against a real corpus, through the shipped operators
(`text_embed@1` then `umap_project@1`) rather than a reimplementation of either.

CORPUS. `corpus.parquet` beside this script — 4,500 rows (`name`, `category`,
`description`), COMMITTED, so a clean checkout can re-derive every number below
without touching anything else. It is a frozen, deterministic slice: rows of
`examples/brewtrend/data/ranking.parquet` with a non-empty `description`, ordered by
`name` (the corpus's own unique key), first 4,500. That source file is NOT read here
and could not be — `examples/brewtrend/data/` is gitignored, and even where present is
rebuilt from six unpinned `curl` fetches of live, rolling 30/90-day Homebrew analytics
(`examples/brewtrend/arcform.yaml`), so it is a moving snapshot rather than a fixed
corpus. `corpus.parquet` was taken from it on 2026-08-24 with:

    COPY (SELECT name, category, description
          FROM read_parquet('examples/brewtrend/data/ranking.parquet')
          WHERE description IS NOT NULL AND length(trim(description)) > 0
          ORDER BY name LIMIT 4500)
    TO 'eval/map-refit-stability/corpus.parquet' (FORMAT parquet)

Re-running that query today will not reproduce `corpus.parquet` byte-for-byte — the
source has moved on — which is exactly why the slice is frozen here instead of
re-read. BASE_N of its rows become the "existing" map, and the next rows in the same
order become the appended rows, so append fraction f draws its rows from the same
pool for every f up to the largest.

MODEL. `minishlab/potion-base-8M`, at the revision the embedding extension was built
from. The weights are inside the extension; the three files fetched into `.cache/`
beside this script exist so the harness can DECLARE which model it is embedding with
and have the run refuse if the extension carries a different one — the same
`model:` + `model_release:` pair a Protocol would write. Gitignored: the harness
re-derives them rather than shipping 28.8 MB in git.

EXTENSION. Set `ARC_STATICEMBED_EXTENSION` to a built artifact. It is not fetched here
and there is no default path — it is an input to the measurement like the corpus, and a
harness that quietly found one somewhere would not be able to say which build produced
these numbers.

METHOD. For each experiment, write a Parquet slice, run the real
`operators/text_embed/text_embed.py` over it (--metric is umap_project's, not
text_embed's) to get 256-d vectors, then the real `operators/umap_project/umap_project.py`
(metric cosine, defaults otherwise — SEED=42 is pinned inside that script and cannot be
overridden from here, which is the point: every fit below uses the operator's own
seed). Five fits:

  control_A   — BASE_N rows, the reference map.
  control_B   — the SAME BASE_N rows, embedded and projected again from scratch in a
                separate process. The floor: if refitting identical data twice already
                moves points, every other number here has to be read against that
                floor rather than against zero.
  append_05   — BASE_N rows + the next 5% of rows, refit over the combined table.
  append_20   — BASE_N rows + the next 20%.
  append_50   — BASE_N rows + the next 50%.

For every non-control comparison, only the BASE_N rows that exist in both maps are
scored (joined on `name`, the corpus's own unique key) — an appended row has no "before"
position and is excluded rather than padding the average with an undefined displacement.

Two measures, because either alone misleads:

  displacement — Euclidean distance between a row's (x, y) in control_A and in the
    comparison map. Reported raw (control_A's own coordinate units) and normalised by
    control_A's own median nearest-neighbour spacing, so "how far" is legible as "how
    many typical inter-point gaps."  UMAP's coordinate frame is arbitrary up to
    rotation/reflection/rescaling between independent fits, and that IS part of what an
    analyst sees on screen when the whole map turns or rescales — so this is
    deliberately not Procrustes-aligned away.

  neighbourhood overlap — for each base row, its K nearest OTHER BASE rows in
    control_A compared with its K nearest OTHER BASE rows in the comparison map (the
    appended rows are excluded from the candidate pool in both maps, so this isolates
    whether the refit rearranged the analyst's existing reading rather than measuring
    that new rows are — expectedly — now nearby). Reported as the mean fraction of the
    K shared, matching the form finetype's map-fidelity harness used for the
    neighbouring question (K=20 there; K=20 here too, for comparability).

VECTOR-SPACE COMPARISON (does the DATA move, not just the drawing). Everything above
compares map to map. This also compares each base row's true 256-d neighbourhood in
control_A against its true 256-d neighbourhood in the comparison experiment — over the
SAME base rows and the SAME K=20 the map side uses, so the two overlaps are directly
subtractable. A base row's own EMBEDDING VECTOR never changes when other rows are
appended — `text_embed` is a per-row function of that row's text alone, confirmed here
by `base_vectors_moved_by_append` in results.json, which measures 0.0 every run — so
what changes a base row's true neighbour SET is competition: an appended row lands
close enough in cosine space to displace one of its previous neighbours. Competition
is not the whole of it — TIE-BREAKING below is the other part — but it is the part
this comparison is about. That is why, unlike the map side, the candidate pool here
is NOT restricted to base rows: it is every row present in the comparison experiment
(base + appended), because excluding the appended rows would make this side incapable
of ever moving (the vectors of the base rows are fixed, so a fixed candidate pool
could only ever score 1.0) and the reactivity controls below exist precisely to catch
a measurement that cannot move. Reported per append tag as
`vector_knn_overlap_mean`/`_median` beside the map's own `knn_overlap_mean`/`_median`,
and as `map_vs_vector_overlap_gap_mean`/`_median`
— vector overlap minus map overlap — so a reader gets the gap the finding rests on
without doing the subtraction themselves. A positive gap means the true vector
relationships held together more than the map did: movement the map shows that the
data does not support, i.e. artefact. A gap near zero means the map moved about as
much as the data actually did: information.

TIE-BREAKING, and it is a property of the instrument rather than of the data. Some
base rows have their K-th and (K+1)-th nearest neighbours at exactly the same cosine
distance — the corpus contains exactly-duplicate descriptions — so for those rows
there is no unique set of K nearest neighbours to compare. `cosine_knn_sets` selects
with `np.argpartition`, which resolves such a tie arbitrarily, and the reference pool
and the comparison pool are different arrays, so the two resolve it arbitrarily
DIFFERENTLY. Part of the neighbourhood change every vector overlap below reports is
therefore a tie broken the other way rather than a row genuinely displaced, and each
figure sits at or below what an exact-tie convention would give. How many base rows
are affected is counted per run and written to results.json as
`vector_knn_tie_breaking`. It is disclosed rather than corrected because a stable
convention would move the committed figures without changing the comparison they are
read for — the gaps, the control bars and the verdict all survive it.

REACTIVITY CONTROLS (can this comparison move, in both directions, or is it just
reporting near-1.0 by construction). A vector-space overlap that always comes back
near 1.0 would be indistinguishable from a measurement that never looks at the
appended rows at all, and a measurement that always says "lower" would be just as
uninformative in the other direction. Two more experiments, checked by assertions in
`main()` that fail loudly rather than leave a number a reader has to notice is
missing:

  density_control — appends, at append_05's own row count for a same-size comparison,
    not the natural next rows in corpus order but VERBATIM DUPLICATES of the base rows
    already closest to their own K-th nearest OTHER base row (smallest own-corpus K-NN
    cosine distance in control_A — the rows most redundant with the rest of the corpus
    already). Concentrating appended mass on the corpus's most crowded existing
    regions, rather than spreading it the way a natural append does, must measurably
    LOWER vector overlap below append_05's own natural rows at the same count — proof
    the comparison can say "the neighbourhoods changed more" when they genuinely did.

  out_of_distribution_control — appends, at the same row count, real English prose
    from domains the corpus has nothing to do with (legal, medical, culinary, literary
    — nothing about the corpus's own subject matter). Genuinely unrelated content is
    MOSTLY too far away in cosine distance to compete for a base row's true top-K
    neighbourhood — the typical base row's own K-th-nearest-neighbour distance among
    its own kind is well inside the typical out-of-distribution row's distance to its
    nearest base row — so this control's vector overlap must come back AT OR ABOVE
    append_05's own natural figure. Mostly, not entirely: a row that far away can
    still land inside some base row's own top-K radius, which is why the figure is
    measured and reported rather than asserted to be 1.0. A run reporting it BELOW
    would mean the comparison is responding to something other than genuine
    neighbourhood competition, which is the observable failure this control exists to
    catch. That the metric stays put under unrelated content is not a control that
    failed to move — it is the metric behaving correctly, and it strengthens the
    map-vs-vector reading rather than weakening it.

  Each control also WITNESSES ITS OWN INPUT, separately from the number it reports,
    because the second of them cannot do it any other way. Its criterion is that the
    overlap stays HIGH, and an append that never happened produces exactly that: drop
    the appended rows and every base row keeps the neighbours it had, so the overlap
    comes back 1.0 and the assertion passes on a comparison that was never run. An
    assertion in the "stays high" direction cannot pin its own input. So the builders
    return the row count read back from the file they wrote rather than the count they
    were asked for, and `checked_pool_size()` requires each control's candidate pool to
    hold BASE_N + n rows, all of the base rows, and no vector that is zero or
    non-finite — a row that embeds to the zero vector is in the pool and still absent
    from the measurement, since cosine distance is undefined for it and it can never be
    selected. Both counts reach results.json as `vector_pool_size`.

Run: `uv run eval/map-refit-stability/measure.py`. Writes results.json beside this
file. No flags — the corpus, split and fractions are the measurement, not parameters
to explore; change the constants below and re-run to ask a different question.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import time
import urllib.request
from pathlib import Path
from typing import NamedTuple

import duckdb
import numpy as np

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
CACHE = HERE / ".cache"
BUILD = HERE / "build"
MODEL_DIR = CACHE / "potion-base-8M"
CORPUS = HERE / "corpus.parquet"  # committed — see the CORPUS section above
TEXT_EMBED = REPO / "operators" / "text_embed" / "text_embed.py"
UMAP_PROJECT = REPO / "operators" / "umap_project" / "umap_project.py"

# Pinned to the revision the extension was built from, not to `main`. The model check
# is over exact bytes, so a branch that moves would make the declaration uncheckable
# the next time someone re-derived the cache.
MODEL_ID = "minishlab/potion-base-8M"
MODEL_REVISION = "bf8b056651a2c21b8d2565580b8569da283cab23"
MODEL_RELEASE = f"{MODEL_ID}@{MODEL_REVISION}"
MODEL_URLS = {
    name: f"https://huggingface.co/{MODEL_ID}/resolve/{MODEL_REVISION}/{name}"
    for name in ("model.safetensors", "tokenizer.json", "config.json")
}

BASE_N = 3000
APPEND_FRACTIONS = {"append_05": 0.05, "append_20": 0.20, "append_50": 0.50}
K_NEIGHBOURS = 20  # matches finetype's static-embedding-map-fidelity kNN-overlap K

# density_control's suffix. Not a random one, so a re-run is byte-identical: it is
# appended to an existing `name` (the corpus's own unique key), so it must not
# already appear in the corpus — asserted in build_density_control_slice() rather
# than assumed.
DENSITY_DUP_SUFFIX = "__density_dup"

# density_control's bar for "measurably lower". The measured gap between it and
# append_05's own vector overlap is ~0.073 on the committed corpus/model — every
# figure here is exactly deterministic (text_embed is a pure per-row function, UMAP's
# SEED is pinned), so this is not read against sampling noise. 0.03 is comfortably
# below the measured gap and comfortably above zero, so it catches a comparison that
# stopped moving without being brittle to a small corpus or model change.
MIN_DENSITY_CONTROL_GAP = 0.03

# out_of_distribution_control's bar for "at or above", and what it separates is worth
# being exact about, because it is narrower than it looks. It separates appended
# content that competes for the base rows' own neighbourhoods from content that does
# not. It does NOT discriminate among kinds of non-competing content: an appended
# corpus far enough away to leave the neighbourhoods alone clears this bar whatever it
# is made of, INCLUDING one that embeds to the zero vector and is therefore incapable
# of being anyone's neighbour at all. That last case is refused before this bar is
# read — by checked_pool_size(), not by tightening the number — because a control
# corpus that embeds to nothing is not a control. The tolerance sits below zero rather
# than at zero because the bar compares two separately measured means.
MAX_OOD_CONTROL_DEFICIT = 0.01

# out_of_distribution_control's corpus: real English prose from domains this corpus's
# own subject matter has nothing to do with — legal, medical, culinary, literary. Real
# prose, not duplicated corpus rows and not gibberish (gibberish would tokenise to
# near-nothing and land near the zero vector, a different and less interesting failure
# mode than genuinely unrelated content landing far away). Cycled to reach whatever row count the
# comparison needs.
OOD_SENTENCES = [
    "Preheat the oven to 190 degrees and cream the butter with the sugar until pale and fluffy.",
    "The tenant shall indemnify the landlord against any claim arising from a breach of this covenant.",
    "Administer the medication twice daily with food and monitor closely for gastrointestinal distress.",
    "It was the best of times, it was the worst of times, it was the age of wisdom and the age of foolishness.",
    "The defendant's motion to dismiss was denied for lack of proper venue and insufficient jurisdiction.",
    "Fold the whipped cream gently into the melted chocolate until no streaks of white remain visible.",
    "The patient presented with elevated blood pressure and a persistent cough lasting three weeks.",
    "She wandered lonely as a cloud that floats on high o'er vales and hills, when all at once she saw a crowd.",
    "The board of directors convened an emergency session to discuss the terms of the pending merger.",
    "Season the lamb shanks generously with rosemary and thyme before searing them on all sides.",
    "The plaintiff alleges a breach of fiduciary duty and seeks both compensatory and punitive damages.",
    "A gentle rain fell over the moor as the shepherd counted his flock before the light failed entirely.",
    "The clinical trial enrolled several thousand participants across a dozen independent sites.",
    "Whisk the flour, baking soda and salt together before folding in the wet ingredients by hand.",
    "The treaty was ratified by both legislatures after eighteen months of difficult negotiation.",
    "Braise the short ribs low and slow until the meat falls easily away from the bone.",
    "The appellate court reversed the lower court's ruling and remanded the case for further proceedings.",
    "He measured out his life in coffee spoons and watched the evening spread out against the sky.",
    "The surgeon reviewed the imaging carefully before scheduling the procedure for the following week.",
    "Simmer the stock for several hours, skimming the surface occasionally, until it turns rich and clear.",
]
OOD_NAME_PREFIX = "__ood_control_"

# The written-answer verdict thresholds, read against the map-vs-vector overlap gap (vector minus
# map) at EVERY append fraction. 0.15 is a large fraction of the 0-1 overlap scale —
# comfortably below the ~0.34-0.54 gaps measured on the committed corpus — chosen so
# "mostly artefact" requires the vectors to visibly outperform the map at every
# fraction, not just on average. 0.05 mirrors control_B's own measured floor (0.0),
# giving "mostly information" a small allowance for noise before requiring the two to
# be indistinguishable.
MOSTLY_ARTEFACT_MIN_GAP = 0.15
MOSTLY_INFORMATION_MAX_GAP = 0.05


class Fit(NamedTuple):
    """Where one experiment's two outputs landed — the 256-d vectors `text_embed`
    wrote, and the 2D coordinates `umap_project` derived from them."""

    embedded: Path
    projected: Path


def ensure_model() -> None:
    MODEL_DIR.mkdir(parents=True, exist_ok=True)
    for name, url in MODEL_URLS.items():
        dest = MODEL_DIR / name
        if dest.is_file() and dest.stat().st_size > 0:
            continue
        print(f"[measure] fetching {name} …", file=sys.stderr)
        with urllib.request.urlopen(url, timeout=120) as resp, open(dest, "wb") as f:
            f.write(resp.read())


def extension() -> Path:
    raw = os.environ.get("ARC_STATICEMBED_EXTENSION")
    if not raw:
        raise SystemExit(
            "measure.py: set ARC_STATICEMBED_EXTENSION to a built embedding-extension "
            "artifact. The vectors come from it, so which build was used is part of "
            "the measurement rather than something to discover at run time."
        )
    path = Path(raw)
    if not path.is_file():
        raise SystemExit(f"measure.py: ARC_STATICEMBED_EXTENSION={raw} is not a file.")
    return path


def run(cmd: list[str]) -> None:
    result = subprocess.run(cmd, cwd=REPO, capture_output=True, text=True)
    if result.returncode != 0:
        raise RuntimeError(
            f"command failed ({result.returncode}): {' '.join(cmd)}\n"
            f"stdout: {result.stdout}\nstderr: {result.stderr}"
        )


def build_corpus_slices(con: duckdb.DuckDBPyConnection) -> dict[str, int]:
    """Write one Parquet per experiment. Returns {experiment: row_count}."""
    BUILD.mkdir(parents=True, exist_ok=True)
    con.execute(
        f"""
        CREATE OR REPLACE TABLE ordered AS
        SELECT name, category, description, row_number() OVER (ORDER BY name) - 1 AS ord
        FROM read_parquet('{CORPUS.as_posix()}')
        WHERE description IS NOT NULL AND length(trim(description)) > 0
        """
    )
    total = con.execute("SELECT count(*) FROM ordered").fetchone()[0]

    counts: dict[str, int] = {}

    def write_slice(tag: str, upper: int) -> None:
        dest = BUILD / f"{tag}.parquet"
        con.execute(
            f"COPY (SELECT name, category, description FROM ordered "
            f"WHERE ord < {upper} ORDER BY ord) TO '{dest.as_posix()}' (FORMAT parquet)"
        )
        counts[tag] = upper

    if BASE_N + max(round(BASE_N * f) for f in APPEND_FRACTIONS.values()) > total:
        raise RuntimeError(
            f"corpus has only {total} usable rows; BASE_N={BASE_N} plus the largest "
            f"append fraction needs more than that."
        )

    write_slice("control_A", BASE_N)
    write_slice("control_B", BASE_N)  # identical rows, refit from scratch
    for tag, frac in APPEND_FRACTIONS.items():
        write_slice(tag, BASE_N + round(BASE_N * frac))
    return counts


def slice_rows_written(con: duckdb.DuckDBPyConnection, dest: Path) -> int:
    """How many rows the COPY that just ran actually put in `dest`, read back from
    the file. A builder that returns the count its caller asked for witnesses
    nothing about the SQL it ran: `assert n_written == n_requested` then reduces to
    `assert n == n` and stays green with the append half of the query deleted."""
    return int(
        con.execute(f"SELECT count(*) FROM read_parquet('{dest.as_posix()}')").fetchone()[0]
    )


def checked_pool_size(
    tag: str,
    pool_vectors: dict[str, np.ndarray],
    base_names: list[str],
    expected_appended: int,
) -> int:
    """The size of `tag`'s candidate pool, returned only after checking that the
    rows it is supposed to contain are in it and are vectors the cosine metric can
    act on. Raises rather than returning a number nobody can trust.

    This exists because a control whose success criterion is that an overlap STAYS
    HIGH is satisfied by an input that never arrived. Drop the appended rows and
    every base row keeps exactly the neighbours it had, so the overlap comes back
    1.0, the "at or above" assertion passes, and the run reports a control that was
    never exercised. Presence one step further in is not enough either: a row whose
    text embeds to the zero vector normalises to NaN, sorts behind everything in
    cosine_knn_sets, and cannot be selected as anyone's neighbour — it is in the
    pool and still inert. Neither failure shows up in the number the control
    reports, which is why it is checked here instead of read off the result."""
    expected_total = len(base_names) + expected_appended
    if len(pool_vectors) != expected_total:
        raise RuntimeError(
            f"{tag}: candidate pool holds {len(pool_vectors)} rows, expected "
            f"{expected_total} ({len(base_names)} base + {expected_appended} "
            f"appended). The appended rows did not reach the comparison, so any "
            f"overlap it reports is a measurement of the base rows against "
            f"themselves."
        )
    missing = [n for n in base_names if n not in pool_vectors]
    if missing:
        raise RuntimeError(
            f"{tag}: {len(missing)} base row(s) are absent from the candidate pool "
            f"(e.g. {missing[0]!r}), so the comparison is not over the same rows the "
            f"reference is."
        )
    inert = [
        n
        for n, v in pool_vectors.items()
        if not np.isfinite(v).all() or float(np.linalg.norm(v)) == 0.0
    ]
    if inert:
        raise RuntimeError(
            f"{tag}: {len(inert)} row(s) in the candidate pool embed to a zero or "
            f"non-finite vector (e.g. {inert[0]!r}). Cosine distance is undefined "
            f"for them and cosine_knn_sets can never select one as a neighbour, so "
            f"they are present in the pool and absent from the measurement — a "
            f"control made of them would report the same near-1.0 overlap as a "
            f"control that was never appended at all."
        )
    return len(pool_vectors)


def kth_distance_ties(
    vectors_by_name: dict[str, np.ndarray], names: list[str], k: int
) -> int:
    """How many of `names` have their k-th and (k+1)-th nearest OTHER `names` at
    exactly the same cosine distance — the rows for which "which rows are the k
    nearest" has no unique answer, because the corpus contains exactly-duplicate
    descriptions. Reported so the tie-breaking caveat in this file's docstring
    carries a figure from this run rather than a remembered one."""
    vecs = np.array([vectors_by_name[n] for n in names])
    vecs = vecs / np.linalg.norm(vecs, axis=1, keepdims=True)
    dist = 1.0 - (vecs @ vecs.T)
    np.fill_diagonal(dist, np.inf)
    k = min(k, len(names) - 2)
    ordered = np.sort(dist, axis=1)
    return int((ordered[:, k - 1] == ordered[:, k]).sum())


def build_density_control_slice(con: duckdb.DuckDBPyConnection, target_names: list[str]) -> int:
    """density_control's slice: BASE_N base rows plus a VERBATIM DUPLICATE (same
    category, same description, a suffixed name) of each row in `target_names` —
    appended mass concentrated on the corpus's own most-redundant rows rather than
    append_05's natural next-in-order sample. Returns the number of rows the COPY
    actually landed in the file, read back from it — see slice_rows_written()."""
    if any(DENSITY_DUP_SUFFIX in n for n in target_names):
        raise RuntimeError(
            f"a target name already contains {DENSITY_DUP_SUFFIX!r} — the suffix is "
            "no longer safe to use as a uniqueness guarantee for the duplicate rows."
        )
    dest = BUILD / "density_control.parquet"
    con.execute(
        f"""
        COPY (
            SELECT name, category, description FROM ordered WHERE ord < {BASE_N}
            UNION ALL
            SELECT name || '{DENSITY_DUP_SUFFIX}' AS name, category, description
            FROM ordered
            WHERE ord < {BASE_N} AND name = ANY(?)
        ) TO '{dest.as_posix()}' (FORMAT parquet)
        """,
        [target_names],
    )
    return slice_rows_written(con, dest)


def build_ood_control_slice(con: duckdb.DuckDBPyConnection, n_rows: int) -> int:
    """out_of_distribution_control's slice: BASE_N base rows plus n_rows rows of real
    English prose from domains this corpus's own subject matter has nothing to do
    with — OOD_SENTENCES cycled to reach n_rows, each given a unique prefixed name.
    Returns the number of rows the COPY actually landed in the file, read back from
    it — see slice_rows_written()."""
    existing = con.execute(
        f"SELECT count(*) FROM ordered WHERE ord < {BASE_N} AND "
        f"name LIKE '{OOD_NAME_PREFIX}%'"
    ).fetchone()[0]
    if existing:
        raise RuntimeError(
            f"{existing} base row name(s) already start with {OOD_NAME_PREFIX!r} — "
            "the prefix is no longer safe to use as a uniqueness guarantee for the "
            "out-of-distribution rows."
        )
    con.execute(
        "CREATE OR REPLACE TABLE ood_rows (name VARCHAR, category VARCHAR, description VARCHAR)"
    )
    rows = [
        (f"{OOD_NAME_PREFIX}{i:04d}", "out_of_distribution", OOD_SENTENCES[i % len(OOD_SENTENCES)])
        for i in range(n_rows)
    ]
    con.executemany("INSERT INTO ood_rows VALUES (?, ?, ?)", rows)
    dest = BUILD / "ood_control.parquet"
    con.execute(
        f"""
        COPY (
            SELECT name, category, description FROM ordered WHERE ord < {BASE_N}
            UNION ALL
            SELECT name, category, description FROM ood_rows
        ) TO '{dest.as_posix()}' (FORMAT parquet)
        """
    )
    return slice_rows_written(con, dest)


def embed(tag: str) -> Path:
    """Run the real text_embed@1 operator over `{tag}.parquet`, writing
    `{tag}_embedded.parquet` (adds a 256-d `embedding` column) and returning its
    path."""
    src = BUILD / f"{tag}.parquet"
    embedded = BUILD / f"{tag}_embedded.parquet"
    run(
        [
            "uv", "run", str(TEXT_EMBED),
            "--input", str(src),
            "--text-column", "description",
            "--extension", str(extension()),
            "--out", str(embedded),
            # Declared and checked: if the artifact carries a different model, the run
            # stops instead of writing numbers nobody could attribute.
            "--model", str(MODEL_DIR),
            "--model-release", MODEL_RELEASE,
        ]
    )
    return embedded


def project(tag: str, embedded: Path) -> Path:
    """Run the real umap_project@1 operator over an embedded Parquet, writing
    `{tag}_projected.parquet` (adds `projection_x`/`projection_y`) and returning its
    path."""
    projected = BUILD / f"{tag}_projected.parquet"
    run(
        [
            "uv", "run", str(UMAP_PROJECT),
            "--input", str(embedded),
            "--column", "embedding",
            "--metric", "cosine",
            "--out", str(projected),
        ]
    )
    return projected


def embed_and_project(tag: str) -> Fit:
    embedded = embed(tag)
    projected = project(tag, embedded)
    return Fit(embedded=embedded, projected=projected)


def load_xy(con: duckdb.DuckDBPyConnection, parquet: Path) -> dict[str, tuple[float, float]]:
    rows = con.execute(
        f"SELECT name, projection_x, projection_y FROM read_parquet('{parquet.as_posix()}')"
    ).fetchall()
    return {name: (x, y) for name, x, y in rows}


def load_vectors(con: duckdb.DuckDBPyConnection, parquet: Path) -> dict[str, np.ndarray]:
    rows = con.execute(
        f"SELECT name, embedding FROM read_parquet('{parquet.as_posix()}')"
    ).fetchall()
    return {name: np.asarray(vec, dtype=np.float64) for name, vec in rows}


def cosine_knn_sets(
    query_vectors: dict[str, np.ndarray],
    query_names: list[str],
    pool_vectors: dict[str, np.ndarray],
    pool_names: list[str],
    k: int,
) -> dict[str, frozenset[str]]:
    """Each query row's k nearest OTHER pool rows by cosine distance, keyed by name
    rather than position — unlike knn_overlap()'s 2D version, query and pool here are
    genuinely different sets (the pool may include appended rows the query set does
    not), so nothing here can assume they line up positionally.

    `np.argpartition` resolves a tie at the k-th distance arbitrarily, and this is
    called on pools that are different arrays, so the same tie can resolve differently
    between two calls. See TIE-BREAKING in this file's docstring for what that costs
    and why it is reported rather than removed."""
    q = np.array([query_vectors[n] for n in query_names])
    p = np.array([pool_vectors[n] for n in pool_names])
    q = q / np.linalg.norm(q, axis=1, keepdims=True)
    p = p / np.linalg.norm(p, axis=1, keepdims=True)
    dist = 1.0 - (q @ p.T)
    k = min(k, len(pool_names) - 1)
    pool_index = {n: i for i, n in enumerate(pool_names)}
    result: dict[str, frozenset[str]] = {}
    for qi, qname in enumerate(query_names):
        row = dist[qi].copy()
        self_i = pool_index.get(qname)
        if self_i is not None:
            row[self_i] = np.inf
        nn = np.argpartition(row, k)[:k]
        result[qname] = frozenset(pool_names[i] for i in nn)
    return result


def vector_overlap(
    ref_nn: dict[str, frozenset[str]],
    cmp_nn: dict[str, frozenset[str]],
    names: list[str],
    k: int,
) -> tuple[float, float]:
    """Mean and median fraction of each name's k reference neighbours also present in
    its comparison neighbours."""
    fractions = np.array([len(ref_nn[n] & cmp_nn[n]) / k for n in names])
    return float(fractions.mean()), float(np.median(fractions))


def own_neighbour_distances(
    vectors_by_name: dict[str, np.ndarray], names: list[str], k: int
) -> dict[str, float]:
    """Each name's cosine distance to its own k-th nearest OTHER name in `names` — how
    redundant that row already is with the rest of the corpus. Small distance means
    many close relatives already exist; used by density_control to pick which rows to
    duplicate, not by anything else here."""
    vecs = np.array([vectors_by_name[n] for n in names])
    vecs = vecs / np.linalg.norm(vecs, axis=1, keepdims=True)
    dist = 1.0 - (vecs @ vecs.T)
    np.fill_diagonal(dist, np.inf)
    k = min(k, len(names) - 1)
    kth = np.sort(dist, axis=1)[:, k - 1]
    return {n: float(d) for n, d in zip(names, kth)}


def median_nn_spacing(coords: np.ndarray) -> float:
    """Median nearest-neighbour distance within one map — the "typical gap" scale
    displacement is normalised against."""
    diff = coords[:, None, :] - coords[None, :, :]
    dist = np.sqrt((diff**2).sum(axis=-1))
    np.fill_diagonal(dist, np.inf)
    nn = dist.min(axis=1)
    return float(np.median(nn))


def knn_overlap(coords_a: np.ndarray, coords_b: np.ndarray, k: int) -> tuple[float, float]:
    """Mean and median fraction of each point's k nearest OTHER points shared between
    two coordinate sets over the SAME point set in the same order."""
    n = coords_a.shape[0]
    k = min(k, n - 1)

    def neighbours(coords: np.ndarray) -> np.ndarray:
        diff = coords[:, None, :] - coords[None, :, :]
        dist = np.sqrt((diff**2).sum(axis=-1))
        np.fill_diagonal(dist, np.inf)
        return np.argsort(dist, axis=1)[:, :k]

    nn_a = neighbours(coords_a)
    nn_b = neighbours(coords_b)
    fractions = np.empty(n, dtype=np.float64)
    for i in range(n):
        fractions[i] = len(set(nn_a[i]) & set(nn_b[i])) / k
    return float(fractions.mean()), float(np.median(fractions))


def compare(
    reference: dict[str, tuple[float, float]],
    other: dict[str, tuple[float, float]],
    base_names: list[str],
    scale: float,
) -> dict:
    ref_xy = np.array([reference[n] for n in base_names])
    other_xy = np.array([other[n] for n in base_names])
    disp = np.sqrt(((ref_xy - other_xy) ** 2).sum(axis=1))
    overlap_mean, overlap_median = knn_overlap(ref_xy, other_xy, K_NEIGHBOURS)
    return {
        "n_shared_rows": len(base_names),
        "displacement_raw_mean": float(disp.mean()),
        "displacement_raw_median": float(np.median(disp)),
        "displacement_normalised_mean": float(disp.mean() / scale),
        "displacement_normalised_median": float(np.median(disp) / scale),
        "knn_overlap_k": K_NEIGHBOURS,
        "knn_overlap_mean": overlap_mean,
        "knn_overlap_median": overlap_median,
    }


def main() -> int:
    started = time.time()
    extension()  # fail before the fetch if the artifact is missing
    ensure_model()
    con = duckdb.connect()
    counts = build_corpus_slices(con)
    print(f"[measure] slices: {counts}", file=sys.stderr)

    fits: dict[str, Fit] = {}
    for tag in ("control_A", "control_B", *APPEND_FRACTIONS):
        t0 = time.time()
        fits[tag] = embed_and_project(tag)
        print(f"[measure] {tag} fit in {time.time() - t0:.1f}s", file=sys.stderr)

    xy = {tag: load_xy(con, fit.projected) for tag, fit in fits.items()}
    vectors = {tag: load_vectors(con, fit.embedded) for tag, fit in fits.items()}

    base_names = sorted(xy["control_A"].keys())
    assert len(base_names) == BASE_N
    ref_coords = np.array([xy["control_A"][n] for n in base_names])
    scale = median_nn_spacing(ref_coords)

    base_vectors = vectors["control_A"]
    checked_pool_size("control_A", base_vectors, base_names, 0)
    ref_ties = kth_distance_ties(base_vectors, base_names, K_NEIGHBOURS)
    ref_nn = cosine_knn_sets(base_vectors, base_names, base_vectors, base_names, K_NEIGHBOURS)

    results = {
        "corpus": str(CORPUS.relative_to(REPO)),
        "corpus_provenance": (
            "frozen 2026-08-24 from examples/brewtrend/data/ranking.parquet "
            "(gitignored, rebuilt from live rolling Homebrew analytics) — see the "
            "CORPUS section of this file's docstring for the exact query"
        ),
        "corpus_rows_committed": int(
            con.execute(
                f"SELECT count(*) FROM read_parquet('{CORPUS.as_posix()}')"
            ).fetchone()[0]
        ),
        "base_n": BASE_N,
        "append_fractions": APPEND_FRACTIONS,
        "model": MODEL_ID,
        "model_release": MODEL_RELEASE,
        "umap_project_params": {"metric": "cosine", "neighbors": 15, "min_dist": 0.1, "seed": 42},
        "base_map_median_nn_spacing": scale,
        "vector_knn_overlap_k": K_NEIGHBOURS,
        "vector_knn_tie_breaking": {
            "reference_rows_scored": len(base_names),
            "reference_rows_with_tied_kth_distance": ref_ties,
            "note": (
                f"{ref_ties} of the {len(base_names)} base rows have their "
                f"{K_NEIGHBOURS}th and {K_NEIGHBOURS + 1}th nearest other base rows "
                "at exactly the same cosine distance, because the corpus contains "
                "exactly-duplicate descriptions. For those rows there is no unique "
                "set of nearest neighbours, and the selection here picks one "
                "arbitrarily (np.argpartition) — differently in the reference pool "
                "and in each comparison pool, since they are different arrays. Part "
                "of the neighbourhood change every vector_knn_overlap figure below "
                "reports is therefore a tie resolved the other way rather than a row "
                "genuinely displaced, and each figure sits at or below what an "
                "exact-tie convention would give. It is disclosed rather than "
                "corrected: a stable convention would move the committed figures "
                "without changing the comparison they are read for."
            ),
        },
        "comparisons": {},
        "runtime_seconds": None,
    }

    # The map-space comparison (unchanged) plus, beside it, the same comparison run
    # in the 256-d vector space over the same base rows and the same k, and the gap
    # between the two — vector overlap minus map overlap, so a reader does not have
    # to subtract two figures to get the number the fix decides on.
    max_base_vector_drift = 0.0
    for tag in ("control_B", *APPEND_FRACTIONS):
        entry = compare(xy["control_A"], xy[tag], base_names, scale)

        tag_vectors = vectors[tag]
        drift = max(float(np.max(np.abs(tag_vectors[n] - base_vectors[n]))) for n in base_names)
        max_base_vector_drift = max(max_base_vector_drift, drift)

        entry["vector_pool_size"] = checked_pool_size(
            tag, tag_vectors, base_names, counts[tag] - BASE_N
        )
        cmp_nn = cosine_knn_sets(
            tag_vectors, base_names, tag_vectors, list(tag_vectors.keys()), K_NEIGHBOURS
        )
        v_mean, v_median = vector_overlap(ref_nn, cmp_nn, base_names, K_NEIGHBOURS)
        entry["vector_knn_overlap_mean"] = v_mean
        entry["vector_knn_overlap_median"] = v_median
        entry["map_vs_vector_overlap_gap_mean"] = v_mean - entry["knn_overlap_mean"]
        entry["map_vs_vector_overlap_gap_median"] = v_median - entry["knn_overlap_median"]

        results["comparisons"][tag] = entry

    # base_vectors_moved_by_append: a base row's own 256-d vector is a per-row
    # function of its text alone, so appending other rows should never move it. This
    # confirms that at every append fraction rather than assuming it — see the
    # VECTOR-SPACE COMPARISON section of this file's docstring.
    results["base_vectors_moved_by_append"] = max_base_vector_drift

    # density_control: same row count as append_05, drawn from a deliberately
    # concentrated distribution — duplicates of the base rows already closest to
    # their own K-th nearest neighbour, not the natural next rows.
    n_density = round(BASE_N * APPEND_FRACTIONS["append_05"])
    own_dist = own_neighbour_distances(base_vectors, base_names, K_NEIGHBOURS)
    density_targets = sorted(base_names, key=lambda n: (own_dist[n], n))[:n_density]
    density_rows_written = build_density_control_slice(con, density_targets)
    assert density_rows_written == BASE_N + n_density, (
        f"density_control's slice holds {density_rows_written} rows, expected "
        f"{BASE_N + n_density} ({BASE_N} base rows + {n_density} duplicates). The "
        "duplicate rows never reached the file, so the control below would score "
        "the base rows against themselves."
    )

    t0 = time.time()
    density_embedded = embed("density_control")
    print(f"[measure] density_control embedded in {time.time() - t0:.1f}s", file=sys.stderr)
    density_vectors = load_vectors(con, density_embedded)
    density_pool_size = checked_pool_size(
        "density_control", density_vectors, base_names, n_density
    )

    density_cmp_nn = cosine_knn_sets(
        density_vectors, base_names, density_vectors, list(density_vectors.keys()), K_NEIGHBOURS
    )
    density_mean, density_median = vector_overlap(ref_nn, density_cmp_nn, base_names, K_NEIGHBOURS)
    append_05_vector_mean = results["comparisons"]["append_05"]["vector_knn_overlap_mean"]

    # The self-check density_control exists to run: if it does not move the vector
    # neighbourhoods measurably more than append_05's own natural rows do at the same
    # count, the vector-space comparison above cannot be trusted to distinguish a
    # real "movement is information" reading from a probe that never really looks —
    # fail loudly rather than let the written reading below rest on an unproven
    # instrument.
    assert append_05_vector_mean - density_mean >= MIN_DENSITY_CONTROL_GAP, (
        "density_control self-check failed: its vector overlap "
        f"({density_mean:.4f}) is not at least {MIN_DENSITY_CONTROL_GAP} below "
        f"append_05's own vector overlap ({append_05_vector_mean:.4f}) at the same "
        f"row count ({n_density}). The vector-space comparison above would then be "
        "unable to tell a genuine 'movement is information' reading from a "
        "measurement that cannot register a neighbourhood change at all — see the "
        "REACTIVITY CONTROLS section of this file's docstring."
    )

    results["density_control"] = {
        "description": (
            f"duplicates the {n_density} base rows with the smallest own-corpus "
            f"{K_NEIGHBOURS}-NN cosine distance (the rows already most redundant "
            "with the rest of control_A) instead of append_05's natural "
            "next-in-order sample, at the same row count, so the comparison "
            "isolates which rows were appended rather than how many. Must drive "
            "vector overlap measurably BELOW append_05's own — proof the "
            "comparison can say 'the neighbourhoods changed more' when they "
            "genuinely did. vector_pool_size is the separate witness that these "
            "rows reached the comparison at all."
        ),
        "n_appended_rows": n_density,
        "vector_pool_size": density_pool_size,
        "knn_overlap_k": K_NEIGHBOURS,
        "vector_knn_overlap_mean": density_mean,
        "vector_knn_overlap_median": density_median,
        "append_05_vector_knn_overlap_mean": append_05_vector_mean,
        "gap_below_append_05_vector_overlap": append_05_vector_mean - density_mean,
    }

    # out_of_distribution_control: same row count as append_05, drawn from real
    # English prose the corpus's own subject matter has nothing to do with — the
    # opposite direction from density_control. Genuinely unrelated rows are mostly
    # too far away to compete for an existing row's true top-K neighbourhood, so this
    # control's own vector overlap must land AT OR ABOVE append_05's own natural
    # figure — at or above, not at exactly 1.0, which is why the bar is a comparison
    # against append_05 rather than an equality.
    n_ood = round(BASE_N * APPEND_FRACTIONS["append_05"])
    ood_rows_written = build_ood_control_slice(con, n_ood)
    assert ood_rows_written == BASE_N + n_ood, (
        f"out_of_distribution_control's slice holds {ood_rows_written} rows, "
        f"expected {BASE_N + n_ood} ({BASE_N} base rows + {n_ood} out-of-"
        "distribution rows). The out-of-distribution rows never reached the file, "
        "and this control's own criterion — that the overlap stays HIGH — is "
        "satisfied by exactly that failure, so nothing downstream would notice."
    )

    t0 = time.time()
    ood_embedded = embed("ood_control")
    print(f"[measure] ood_control embedded in {time.time() - t0:.1f}s", file=sys.stderr)
    ood_vectors = load_vectors(con, ood_embedded)
    ood_pool_size = checked_pool_size(
        "out_of_distribution_control", ood_vectors, base_names, n_ood
    )

    ood_cmp_nn = cosine_knn_sets(
        ood_vectors, base_names, ood_vectors, list(ood_vectors.keys()), K_NEIGHBOURS
    )
    ood_mean, ood_median = vector_overlap(ref_nn, ood_cmp_nn, base_names, K_NEIGHBOURS)

    # The self-check out_of_distribution_control exists to run: content this far
    # away in cosine space cannot enter any base row's true top-K neighbourhood, so
    # if this control's overlap comes back measurably BELOW append_05's own natural
    # figure, the vector-space comparison above is responding to something other
    # than genuine neighbourhood competition — fail loudly rather than let the
    # written reading below rest on an unproven instrument.
    assert ood_mean >= append_05_vector_mean - MAX_OOD_CONTROL_DEFICIT, (
        "out_of_distribution_control self-check failed: its vector overlap "
        f"({ood_mean:.4f}) is more than {MAX_OOD_CONTROL_DEFICIT} below append_05's "
        f"own vector overlap ({append_05_vector_mean:.4f}) at the same row count "
        f"({n_ood}). Genuinely unrelated content should rarely displace an "
        "existing row's true neighbours, so a deficit this large means "
        "the vector-space comparison above is responding to something other than "
        "genuine neighbourhood competition — see the REACTIVITY CONTROLS section of "
        "this file's docstring."
    )

    results["out_of_distribution_control"] = {
        "description": (
            f"appends {n_ood} rows of real English prose from domains this "
            "corpus's own subject matter has nothing to do with (legal, medical, "
            "culinary, literary), at append_05's own row count, so the comparison "
            "is exercised in the opposite direction from density_control. Must "
            "leave vector overlap AT OR ABOVE append_05's own — content this far "
            "away mostly cannot compete for an existing row's true top-K "
            "neighbourhood, so a run reporting it below would be responding to "
            "something other than genuine neighbourhood change. Mostly rather than "
            "entirely: a row that far away can still land inside a base row's own "
            "top-K radius, so the figure is measured rather than assumed to be 1.0. "
            "That the metric stays put here is not a control that failed to move; "
            "it is the metric behaving correctly, and it strengthens rather than "
            "weakens the map-vs-vector reading below. vector_pool_size is the "
            "separate witness that these rows reached the comparison at all — an "
            "overlap that stays high is also exactly what an append that never "
            "happened would report."
        ),
        "n_appended_rows": n_ood,
        "vector_pool_size": ood_pool_size,
        "knn_overlap_k": K_NEIGHBOURS,
        "vector_knn_overlap_mean": ood_mean,
        "vector_knn_overlap_median": ood_median,
        "append_05_vector_knn_overlap_mean": append_05_vector_mean,
        "margin_at_or_above_append_05_vector_overlap": ood_mean - append_05_vector_mean,
    }

    # A short written answer, built from this run's own numbers so it cannot go
    # stale the way prose committed once and never revisited does (the reason
    # check_findings.py exists for the figures above).
    append_tags = list(APPEND_FRACTIONS)
    gap_means = [results["comparisons"][t]["map_vs_vector_overlap_gap_mean"] for t in append_tags]
    if min(gap_means) > MOSTLY_ARTEFACT_MIN_GAP:
        verdict = "mostly_artefact"
    elif max(gap_means) < MOSTLY_INFORMATION_MAX_GAP:
        verdict = "mostly_information"
    else:
        verdict = "mixed"

    gap_summary = ", ".join(
        f"{t} gap={results['comparisons'][t]['map_vs_vector_overlap_gap_mean']:.3f} "
        f"(map={results['comparisons'][t]['knn_overlap_mean']:.3f} vs "
        f"vector={results['comparisons'][t]['vector_knn_overlap_mean']:.3f})"
        for t in append_tags
    )
    reactivity_summary = (
        f"the reactivity controls confirm this comparison can move in both "
        f"directions: a same-size, deliberately concentrated append (density_control) "
        f"drops vector overlap to {density_mean:.3f} against append_05's "
        f"{append_05_vector_mean:.3f}, while a same-size append of content unrelated "
        f"to the corpus (out_of_distribution_control) leaves it at {ood_mean:.3f} — "
        "at or above append_05's own figure, as it should, since content that far "
        "away mostly cannot compete for an existing row's true neighbours. Mostly "
        "rather than entirely: a row that far away can still land inside a base "
        "row's own top-K radius, and the tie-breaking recorded under "
        "vector_knn_tie_breaking contributes as well, which is why that figure is "
        "measured rather than assumed to be 1.0. Each control's vector_pool_size is "
        "the separate witness that the rows it is supposed to append actually "
        "reached the comparison, which the number alone cannot show for a control "
        "whose criterion is that the overlap stays high."
    )
    explanation = (
        f"At every append fraction measured — {gap_summary} — the map-space overlap "
        "is well below the vector-space overlap for the same "
        f"{BASE_N} base rows at k={K_NEIGHBOURS}. A base row's own 256-d vector never "
        f"moves when other rows are appended (base_vectors_moved_by_append="
        f"{max_base_vector_drift}); what moves, and moves far less than the map "
        f"does, is which OTHER rows count as its true nearest neighbours — "
        f"{reactivity_summary} So the map's rearrangement at every fraction tested "
        "is mostly a projection artefact, not the data changing underneath it."
        if verdict == "mostly_artefact"
        else f"At every append fraction measured — {gap_summary} — the map-space and "
        "vector-space overlaps track closely, so the map's rearrangement mostly "
        f"reflects genuine change in the underlying data rather than an artefact of "
        f"the projection. {reactivity_summary}"
        if verdict == "mostly_information"
        else f"At every append fraction measured — {gap_summary} — the gap between "
        "map-space and vector-space overlap is neither consistently large nor "
        "consistently small, so the map's movement reads as genuinely mixed: part "
        f"artefact, part real change in the data, and the fractions do not point the "
        f"same way. {reactivity_summary}"
    )
    results["movement_reading"] = {"verdict": verdict, "explanation": explanation}
    print(f"[measure] movement reading: {verdict}", file=sys.stderr)
    print(f"[measure] {explanation}", file=sys.stderr)

    results["runtime_seconds"] = round(time.time() - started, 1)

    out = HERE / "results.json"
    out.write_text(json.dumps(results, indent=2) + "\n")
    print(f"[measure] wrote {out}", file=sys.stderr)
    print(json.dumps(results, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
