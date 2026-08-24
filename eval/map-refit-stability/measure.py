# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = [
#   "duckdb>=1,<2",
#   "numpy>=1.26,<3",
# ]
# ///
"""measure.py — how far umap_project's map moves when rows are appended and refit.

Answers AC1 of "the map reshuffles when rows are added": the projection has no
out-of-sample transform, so appending rows means refitting the whole map, and a refit
moves every point. Nobody had measured how much. This does, against a real corpus,
through the shipped operators (`text_embed@1` then `umap_project@1`) rather than a
reimplementation of either.

CORPUS. `examples/brewtrend/data/ranking.parquet` — this repository's own reference
Protocol output, real Homebrew package descriptions (13,332 rows with a non-empty
`description` after filtering; committed, not fetched). Rows are ordered by `name`
(the corpus's own unique key) for a deterministic, non-cherry-picked split: the first
BASE_N become the "existing" map, and the next rows in that same order become the
appended rows, so append fraction f draws its rows from the same pool for every f up
to the largest.

MODEL. `minishlab/potion-base-8M`, fetched into `.cache/` beside this script on first
run (gitignored — the harness re-derives it rather than shipping 28.8 MB of weights
in git). Same model and fetch URLs as operators/text_embed/README.md's worked example.

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

Run: `uv run eval/map-refit-stability/measure.py`. Writes results.json beside this
file. No flags — the corpus, split and fractions are the measurement, not parameters
to explore; change the constants below and re-run to ask a different question.
"""
from __future__ import annotations

import json
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

import duckdb
import numpy as np

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
CACHE = HERE / ".cache"
BUILD = HERE / "build"
MODEL_DIR = CACHE / "potion-base-8M"
CORPUS = REPO / "examples" / "brewtrend" / "data" / "ranking.parquet"
TEXT_EMBED = REPO / "operators" / "text_embed" / "text_embed.py"
UMAP_PROJECT = REPO / "operators" / "umap_project" / "umap_project.py"

MODEL_URLS = {
    "model.safetensors": "https://huggingface.co/minishlab/potion-base-8M/resolve/main/model.safetensors",
    "tokenizer.json": "https://huggingface.co/minishlab/potion-base-8M/resolve/main/tokenizer.json",
}

BASE_N = 3000
APPEND_FRACTIONS = {"append_05": 0.05, "append_20": 0.20, "append_50": 0.50}
K_NEIGHBOURS = 20  # matches finetype's static-embedding-map-fidelity kNN-overlap K


def ensure_model() -> None:
    MODEL_DIR.mkdir(parents=True, exist_ok=True)
    for name, url in MODEL_URLS.items():
        dest = MODEL_DIR / name
        if dest.is_file() and dest.stat().st_size > 0:
            continue
        print(f"[measure] fetching {name} …", file=sys.stderr)
        with urllib.request.urlopen(url, timeout=120) as resp, open(dest, "wb") as f:
            f.write(resp.read())


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


def embed_and_project(tag: str) -> Path:
    src = BUILD / f"{tag}.parquet"
    embedded = BUILD / f"{tag}_embedded.parquet"
    projected = BUILD / f"{tag}_projected.parquet"
    run(
        [
            "uv", "run", str(TEXT_EMBED),
            "--input", str(src),
            "--text-column", "description",
            "--model", str(MODEL_DIR),
            "--out", str(embedded),
        ]
    )
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


def load_xy(con: duckdb.DuckDBPyConnection, parquet: Path) -> dict[str, tuple[float, float]]:
    rows = con.execute(
        f"SELECT name, projection_x, projection_y FROM read_parquet('{parquet.as_posix()}')"
    ).fetchall()
    return {name: (x, y) for name, x, y in rows}


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
    ensure_model()
    con = duckdb.connect()
    counts = build_corpus_slices(con)
    print(f"[measure] slices: {counts}", file=sys.stderr)

    projected = {}
    for tag in ("control_A", "control_B", *APPEND_FRACTIONS):
        t0 = time.time()
        projected[tag] = embed_and_project(tag)
        print(f"[measure] {tag} fit in {time.time() - t0:.1f}s", file=sys.stderr)

    xy = {tag: load_xy(con, path) for tag, path in projected.items()}

    base_names = sorted(xy["control_A"].keys())
    assert len(base_names) == BASE_N
    ref_coords = np.array([xy["control_A"][n] for n in base_names])
    scale = median_nn_spacing(ref_coords)

    results = {
        "corpus": str(CORPUS.relative_to(REPO)),
        "corpus_rows_available": int(
            con.execute(
                f"SELECT count(*) FROM read_parquet('{CORPUS.as_posix()}') "
                f"WHERE description IS NOT NULL AND length(trim(description)) > 0"
            ).fetchone()[0]
        ),
        "base_n": BASE_N,
        "append_fractions": APPEND_FRACTIONS,
        "model": "minishlab/potion-base-8M",
        "umap_project_params": {"metric": "cosine", "neighbors": 15, "min_dist": 0.1, "seed": 42},
        "base_map_median_nn_spacing": scale,
        "comparisons": {},
        "runtime_seconds": None,
    }

    for tag in ("control_B", *APPEND_FRACTIONS):
        results["comparisons"][tag] = compare(xy["control_A"], xy[tag], base_names, scale)

    results["runtime_seconds"] = round(time.time() - started, 1)

    out = HERE / "results.json"
    out.write_text(json.dumps(results, indent=2) + "\n")
    print(f"[measure] wrote {out}", file=sys.stderr)
    print(json.dumps(results, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
