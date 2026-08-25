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
the only way a base row's true neighbour SET can change is genuine competition: an
appended row lands close enough in cosine space to displace one of its previous
neighbours. That is why, unlike the map side, the candidate pool here is NOT restricted
to base rows: it is every row present in the comparison experiment (base + appended),
because excluding the appended rows would make this side incapable of ever moving (the
vectors of the base rows are fixed, so a fixed candidate pool could only ever score
1.0) and AC5 exists precisely to catch a measurement that cannot move. Reported per
append tag as `vector_knn_overlap_mean`/`_median` beside the map's own
`knn_overlap_mean`/`_median`, and as `map_vs_vector_overlap_gap_mean`/`_median` —
vector overlap minus map overlap — so a reader gets the gap the finding rests on
without doing the subtraction themselves. A positive gap means the true vector
relationships held together more than the map did: movement the map shows that the
data does not support, i.e. artefact. A gap near zero means the map moved about as
much as the data actually did: information.

DISTRIBUTION-SHIFT CONTROL (AC5 — can this comparison ever say "more"). A vector-space
overlap that always comes back near 1.0 would be indistinguishable from a measurement
that never looks at the appended rows at all. So one more experiment,
`shift_control`, appends — at append_05's own row count, for a same-size comparison —
not the natural next rows in corpus order but VERBATIM DUPLICATES of the base rows
already closest to their own K-th nearest OTHER base row (smallest own-corpus K-NN
cosine distance in control_A — the rows most redundant with the rest of the corpus
already). Concentrating appended mass on the corpus's most crowded existing regions,
rather than spreading it the way a natural append does, measurably lowers vector
overlap further than append_05's own natural rows do at the same count — checked by an
assertion in `main()` that fails loudly, not a number a reader has to notice is
missing, if a future corpus or model change ever makes that stop being true.

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

# AC5's control. Suffix, not a random one, so a re-run is byte-identical: it is
# appended to an existing `name` (the corpus's own unique key), so it must not
# already appear in the corpus — asserted in build_shift_control_slice() rather than
# assumed.
SHIFT_DUP_SUFFIX = "__shift_dup"

# AC5's bar for "measurably more". The measured gap between shift_control and
# append_05's own vector overlap is ~0.073 on the committed corpus/model — every
# figure here is exactly deterministic (text_embed is a pure per-row function, UMAP's
# SEED is pinned), so this is not read against sampling noise. 0.03 is comfortably
# below the measured gap and comfortably above zero, so it catches a comparison that
# stopped moving without being brittle to a small corpus or model change.
MIN_SHIFT_CONTROL_GAP = 0.03

# AC4's verdict thresholds, read against the map-vs-vector overlap gap (vector minus
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


def build_shift_control_slice(con: duckdb.DuckDBPyConnection, target_names: list[str]) -> int:
    """AC5's control slice: BASE_N base rows plus a VERBATIM DUPLICATE (same category,
    same description, a suffixed name) of each row in `target_names` — a distribution
    concentrated on the corpus's own most-redundant rows rather than append_05's
    natural next-in-order sample. Returns the number of duplicate rows written (equal
    to len(target_names), asserted below the call site against append_05's own
    count)."""
    if any(SHIFT_DUP_SUFFIX in n for n in target_names):
        raise RuntimeError(
            f"a target name already contains {SHIFT_DUP_SUFFIX!r} — the suffix is no "
            "longer safe to use as a uniqueness guarantee for the duplicate rows."
        )
    dest = BUILD / "shift_control.parquet"
    con.execute(
        f"""
        COPY (
            SELECT name, category, description FROM ordered WHERE ord < {BASE_N}
            UNION ALL
            SELECT name || '{SHIFT_DUP_SUFFIX}' AS name, category, description
            FROM ordered
            WHERE ord < {BASE_N} AND name = ANY(?)
        ) TO '{dest.as_posix()}' (FORMAT parquet)
        """,
        [target_names],
    )
    return len(target_names)


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
    not), so nothing here can assume they line up positionally."""
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
    many close relatives already exist; used by the AC5 control to pick which rows to
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
        "comparisons": {},
        "runtime_seconds": None,
    }

    # AC1 + AC2: the map-space comparison (unchanged) plus, beside it, the same
    # comparison run in the 256-d vector space over the same base rows and the same
    # k, and the gap between the two — vector overlap minus map overlap, so a reader
    # does not have to subtract two figures to get the number the fix decides on.
    max_base_vector_drift = 0.0
    for tag in ("control_B", *APPEND_FRACTIONS):
        entry = compare(xy["control_A"], xy[tag], base_names, scale)

        tag_vectors = vectors[tag]
        drift = max(float(np.max(np.abs(tag_vectors[n] - base_vectors[n]))) for n in base_names)
        max_base_vector_drift = max(max_base_vector_drift, drift)

        cmp_nn = cosine_knn_sets(
            tag_vectors, base_names, tag_vectors, list(tag_vectors.keys()), K_NEIGHBOURS
        )
        v_mean, v_median = vector_overlap(ref_nn, cmp_nn, base_names, K_NEIGHBOURS)
        entry["vector_pool_size"] = len(tag_vectors)
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

    # AC5: the distribution-shift control. Same row count as append_05, drawn from a
    # deliberately different distribution — duplicates of the base rows already
    # closest to their own K-th nearest neighbour, not the natural next rows.
    n_shift = round(BASE_N * APPEND_FRACTIONS["append_05"])
    own_dist = own_neighbour_distances(base_vectors, base_names, K_NEIGHBOURS)
    shift_targets = sorted(base_names, key=lambda n: (own_dist[n], n))[:n_shift]
    n_dup = build_shift_control_slice(con, shift_targets)
    assert n_dup == n_shift

    t0 = time.time()
    shift_embedded = embed("shift_control")
    print(f"[measure] shift_control embedded in {time.time() - t0:.1f}s", file=sys.stderr)
    shift_vectors = load_vectors(con, shift_embedded)

    shift_cmp_nn = cosine_knn_sets(
        shift_vectors, base_names, shift_vectors, list(shift_vectors.keys()), K_NEIGHBOURS
    )
    shift_mean, shift_median = vector_overlap(ref_nn, shift_cmp_nn, base_names, K_NEIGHBOURS)
    append_05_vector_mean = results["comparisons"]["append_05"]["vector_knn_overlap_mean"]

    # The self-check this control exists to run: if it does not move the vector
    # neighbourhoods measurably more than append_05's own natural rows do at the same
    # count, the vector-space comparison above cannot be trusted to distinguish a
    # real "movement is information" reading from a probe that never really looks —
    # fail loudly rather than let AC4's conclusion rest on an unproven instrument.
    assert append_05_vector_mean - shift_mean >= MIN_SHIFT_CONTROL_GAP, (
        "AC5 self-check failed: the distribution-shift control's vector overlap "
        f"({shift_mean:.4f}) is not at least {MIN_SHIFT_CONTROL_GAP} below append_05's "
        f"own vector overlap ({append_05_vector_mean:.4f}) at the same row count "
        f"({n_dup}). The vector-space comparison above would then be unable to tell "
        "a genuine 'movement is information' reading from a measurement that cannot "
        "register a distribution shift at all — see AC5 in the card and the "
        "DISTRIBUTION-SHIFT CONTROL section of this file's docstring."
    )

    results["ac5_distribution_shift_control"] = {
        "description": (
            f"duplicates the {n_dup} base rows with the smallest own-corpus "
            f"{K_NEIGHBOURS}-NN cosine distance (the rows already most redundant "
            "with the rest of control_A) instead of append_05's natural "
            "next-in-order sample, at the same row count, so the comparison "
            "isolates which rows were appended rather than how many"
        ),
        "n_appended_rows": n_dup,
        "knn_overlap_k": K_NEIGHBOURS,
        "vector_knn_overlap_mean": shift_mean,
        "vector_knn_overlap_median": shift_median,
        "append_05_vector_knn_overlap_mean": append_05_vector_mean,
        "gap_below_append_05_vector_overlap": append_05_vector_mean - shift_mean,
    }

    # AC4: a short written answer, built from this run's own numbers so it cannot go
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
    explanation = (
        f"At every append fraction measured — {gap_summary} — the map-space overlap "
        "is well below the vector-space overlap for the same "
        f"{BASE_N} base rows at k={K_NEIGHBOURS}. A base row's own 256-d vector never "
        f"moves when other rows are appended (base_vectors_moved_by_append="
        f"{max_base_vector_drift}); what moves, and moves far less than the map "
        "does, is which OTHER rows count as its true nearest neighbours. AC5's "
        f"control shows this comparison is not just reporting near-1.0 by "
        f"construction: a same-size, deliberately concentrated append drops vector "
        f"overlap to {shift_mean:.3f} against append_05's {append_05_vector_mean:.3f}. "
        "So the map's rearrangement at every fraction tested is mostly a projection "
        "artefact, not the data changing underneath it."
        if verdict == "mostly_artefact"
        else f"At every append fraction measured — {gap_summary} — the map-space and "
        "vector-space overlaps track closely, so the map's rearrangement mostly "
        "reflects genuine change in the underlying data rather than an artefact of "
        "the projection."
        if verdict == "mostly_information"
        else f"At every append fraction measured — {gap_summary} — the gap between "
        "map-space and vector-space overlap is neither consistently large nor "
        "consistently small, so the map's movement reads as genuinely mixed: part "
        "artefact, part real change in the data, and the fractions do not point the "
        "same way."
    )
    results["ac4_movement_reading"] = {"verdict": verdict, "explanation": explanation}
    print(f"[measure] AC4 reading: {verdict}", file=sys.stderr)
    print(f"[measure] {explanation}", file=sys.stderr)

    results["runtime_seconds"] = round(time.time() - started, 1)

    out = HERE / "results.json"
    out.write_text(json.dumps(results, indent=2) + "\n")
    print(f"[measure] wrote {out}", file=sys.stderr)
    print(json.dumps(results, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
