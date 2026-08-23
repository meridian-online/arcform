# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = [
#   "umap-learn>=0.5,<0.6",
#   "numpy>=1.26,<3",
#   "duckdb>=1,<2",
#   "safetensors>=0.4,<1",
#   "tokenizers>=0.20,<1",
# ]
# ///
"""embed_project — arcform typed Python operator (uv-run).

Turns a text column into the two coordinates a map needs. Reads one Parquet, embeds
the named text column with a LOCAL static-embedding model, reduces the embedding to
two dimensions with UMAP, and writes a Parquet carrying every input column plus
`projection_x` and `projection_y`.

THE MODEL IS AN INPUT, NEVER A DOWNLOAD. `--model` names a directory the Protocol
declares as an asset, holding the two files a model2vec/potion release ships:

    model.safetensors   one 2-D float tensor, [vocab, dim] (key `embeddings`)
    tokenizer.json      a HuggingFace `tokenizers` serialisation

Nothing here opens a socket and nothing here reads a credential: there is no HTTP
client in the import list, no API key is consulted, and the only environment this
script touches it WRITES (the thread pins below). A run's cost is the machine's.

Embedding is a lookup and a mean, which is why it is reproducible: tokenise the text
with the model's own tokenizer, average the rows of the embedding table for the token
ids, then L2-normalise. Measured 2026-08-23 against `minishlab/potion-base-8M`, this
reproduces `model2vec.StaticModel.encode` to a maximum absolute difference of 1.7e-08
— float32 rounding — so any published model2vec/potion model can be used as-is.
A row whose text is NULL, empty, or made only of tokens outside the vocabulary
embeds as a zero vector; the count is reported on stderr rather than passed over.

DETERMINISM — the reason this operator can sit in a Protocol at all. Three things are
pinned, and the third is the one a seed alone does not cover:

  1. SEED, frozen here rather than exposed in `with:` — `op: embed_project@1`
     addresses these exact script bytes, so the seed cannot drift under a manifest.
  2. THREADS, pinned to one before numpy/numba are imported. umap-learn already
     overrides `n_jobs` to 1 when `random_state` is set, so the pin was measured to
     change nothing at 120 and 2,000 rows on 2026-08-23; it stays because the
     spectral initialisation reaches BLAS, and a multi-threaded BLAS reduction is
     free to reorder a float sum. That is not a thing to discover at 10^6 rows.
  3. ROW ORDER, pinned by reading the input with DuckDB single-threaded with
     insertion order preserved, carrying an explicit ordinal through the join, and
     ordering the output by it. Parquet bytes follow row order.

What is NOT pinned is the dependency set: the PEP-723 header above bounds every
direct dependency at both ends, but a resolve inside those bounds can still pick a
newer umap-learn, numba or DuckDB, and any of the three can move the output.
Byte-identity is a property of a pinned environment, not of this script. See
README.md.

Run standalone:
    uv run operators/embed_project/embed_project.py \
        --input corpus.parquet --text-column description \
        --model models/potion-base-8M --out corpus_projected.parquet
"""
from __future__ import annotations

# Thread pins go in BEFORE numpy, numba or DuckDB are imported: each of them reads
# its thread count once, at import, and ignores a later change.
import os

for _var in (
    "OMP_NUM_THREADS",
    "OPENBLAS_NUM_THREADS",
    "MKL_NUM_THREADS",
    "NUMEXPR_NUM_THREADS",
    "NUMBA_NUM_THREADS",
    "VECLIB_MAXIMUM_THREADS",
):
    os.environ[_var] = "1"

import argparse
import sys
from pathlib import Path

import duckdb
import numpy as np
import umap
from safetensors.numpy import load_file
from tokenizers import Tokenizer

SEED = 42  # frozen: the projection's random_state, so a re-run lands on the same map

# The ordinal carried through the embed→join round trip. Underscored and prefixed so
# it cannot collide with a real column by accident; a collision is refused outright.
ROW = "__arc_embed_row"

# The two columns this operator adds. A conflict with an input column is refused
# rather than silently overwritten — the caller asked for both sets of values.
X_COL = "projection_x"
Y_COL = "projection_y"

# The one tensor key model2vec writes. A model with a single tensor under any other
# name is accepted; more than one, and the caller has to be told which we would guess.
EMBEDDING_KEY = "embeddings"

DEFAULT_NEIGHBORS = 15
DEFAULT_MIN_DIST = 0.1

# UMAP fits a k-nearest-neighbour graph, so it needs more rows than neighbours. Below
# this there is no neighbourhood structure to find and a map would be noise wearing
# coordinates.
MIN_ROWS = 5


class Refusal(Exception):
    """A condition the caller can fix, reported as one line rather than a traceback."""


def sql_lit(s: str) -> str:
    """A DuckDB single-quoted string literal: wrap in ' and double any interior '."""
    return "'" + s.replace("'", "''") + "'"


def load_embedding_table(model_dir: Path) -> tuple[Tokenizer, np.ndarray]:
    """Load the tokenizer and the static embedding matrix from a model directory."""
    weights = model_dir / "model.safetensors"
    tokenizer_json = model_dir / "tokenizer.json"
    for part in (weights, tokenizer_json):
        if not part.is_file():
            raise Refusal(
                f"the model asset is incomplete: {part} is missing. A model directory "
                f"holds model.safetensors and tokenizer.json — the layout a model2vec "
                f"or potion release ships. This operator does not download one."
            )

    tensors = load_file(weights)
    if EMBEDDING_KEY in tensors:
        table = tensors[EMBEDDING_KEY]
    elif len(tensors) == 1:
        table = next(iter(tensors.values()))
    else:
        raise Refusal(
            f"{weights} carries {len(tensors)} tensors "
            f"({', '.join(sorted(tensors))}) and none is named {EMBEDDING_KEY!r}, so "
            f"which one holds the embeddings is a guess. Publish the table under "
            f"{EMBEDDING_KEY!r}."
        )
    if table.ndim != 2:
        raise Refusal(
            f"the embedding tensor in {weights} has shape {table.shape}; a static "
            f"embedding table is 2-D, [vocab, dim]."
        )
    return Tokenizer.from_file(str(tokenizer_json)), table.astype(np.float32)


def embed(texts: list[str], tokenizer: Tokenizer, table: np.ndarray) -> tuple[np.ndarray, int]:
    """Mean-pool the static token vectors per text, L2-normalised.

    Returns the (n, dim) matrix and how many rows embedded as a zero vector — a text
    that tokenised to nothing in the model's vocabulary.
    """
    vocab_size = table.shape[0]
    encodings = tokenizer.encode_batch(texts, add_special_tokens=False)
    out = np.zeros((len(texts), table.shape[1]), dtype=np.float32)
    empty = 0
    for row, encoding in enumerate(encodings):
        ids = [i for i in encoding.ids if 0 <= i < vocab_size]
        if ids:
            out[row] = table[ids].mean(axis=0)
        else:
            empty += 1
    norms = np.linalg.norm(out, axis=1, keepdims=True)
    # A zero row stays zero: dividing by its own norm would be 0/0. UMAP's cosine
    # metric handles a zero vector without producing a NaN.
    np.divide(out, norms, out=out, where=norms > 0)
    return out, empty


def project(vectors: np.ndarray, neighbors: int, min_dist: float) -> np.ndarray:
    """Reduce to two dimensions. `random_state` is what makes this reproducible —
    umap-learn takes the single-threaded path whenever it is set."""
    rows = vectors.shape[0]
    # n_neighbors must be strictly below the row count; clamp rather than fail, so a
    # small corpus still lands on a map instead of on an exception from inside UMAP.
    k = max(2, min(neighbors, rows - 1))
    reducer = umap.UMAP(
        n_components=2,
        n_neighbors=k,
        min_dist=min_dist,
        metric="cosine",
        random_state=SEED,
        verbose=False,
    )
    return np.asarray(reducer.fit_transform(vectors), dtype=np.float64)


def main() -> int:
    ap = argparse.ArgumentParser(description="Embed a text column and project it to 2-D.")
    ap.add_argument("--input", required=True, help="Parquet to read.")
    ap.add_argument("--text-column", required=True, help="Column to embed.")
    ap.add_argument("--model", required=True, help="Static-embedding model directory.")
    ap.add_argument("--out", required=True, help="Parquet to write.")
    ap.add_argument("--neighbors", type=int, default=DEFAULT_NEIGHBORS)
    ap.add_argument("--min-dist", type=float, default=DEFAULT_MIN_DIST)
    args = ap.parse_args()

    src = Path(args.input)
    if not src.is_file():
        raise Refusal(f"the input Parquet {src} does not exist.")
    tokenizer, table = load_embedding_table(Path(args.model))

    con = duckdb.connect()
    # Single-threaded with insertion order preserved: the scan hands rows back in
    # file order, so the ordinal below is the input's own order and the output's
    # bytes do not move between runs.
    con.execute("SET threads TO 1")
    con.execute("SET preserve_insertion_order TO true")
    con.execute(
        f"CREATE TABLE arc_src AS SELECT *, (row_number() OVER ()) - 1 AS {ROW} "
        f"FROM read_parquet({sql_lit(str(src))})"
    )
    columns = [r[0] for r in con.execute("DESCRIBE arc_src").fetchall()]
    # `columns` ends with the ordinal this step just added, so the input carried it
    # too only if it appears twice.
    clashes = [c for c in (X_COL, Y_COL) if c in columns]
    if columns.count(ROW) > 1:
        clashes.append(ROW)
    if clashes:
        raise Refusal(
            f"{src} already carries a column named {clashes[0]!r}; this operator adds "
            f"{X_COL!r} and {Y_COL!r} and will not overwrite an input column."
        )
    if args.text_column not in columns:
        raise Refusal(
            f"{src} has no column {args.text_column!r}. It carries: "
            f"{', '.join(c for c in columns if c != ROW)}."
        )

    rows = con.execute(
        f'SELECT CAST("{args.text_column}" AS VARCHAR) FROM arc_src ORDER BY {ROW}'
    ).fetchall()
    if len(rows) < MIN_ROWS:
        raise Refusal(
            f"{src} has {len(rows)} rows; a neighbourhood embedding needs at least "
            f"{MIN_ROWS} to have any neighbourhood to describe."
        )
    texts = [("" if r[0] is None else r[0]) for r in rows]

    vectors, empty = embed(texts, tokenizer, table)
    if empty:
        print(
            f"[embed_project] {empty} of {len(texts)} rows tokenised to nothing in the "
            f"model vocabulary and embed as zero vectors",
            file=sys.stderr,
        )
    coordinates = project(vectors, args.neighbors, args.min_dist)

    con.execute(f"CREATE TABLE arc_proj ({ROW} BIGINT, {X_COL} DOUBLE, {Y_COL} DOUBLE)")
    con.executemany(
        "INSERT INTO arc_proj VALUES (?, ?, ?)",
        [(i, float(coordinates[i, 0]), float(coordinates[i, 1])) for i in range(len(texts))],
    )

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    con.execute(
        f"COPY (SELECT s.* EXCLUDE ({ROW}), p.{X_COL}, p.{Y_COL} "
        f"FROM arc_src s JOIN arc_proj p ON s.{ROW} = p.{ROW} ORDER BY s.{ROW}) "
        f"TO {sql_lit(str(out))} (FORMAT parquet, COMPRESSION zstd)"
    )
    print(
        f"[embed_project] {len(texts)} rows · {vectors.shape[1]}-d embedding · "
        f"seed {SEED} → {out}"
    )
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Refusal as refusal:
        print(f"embed_project: {refusal}", file=sys.stderr)
        sys.exit(1)
