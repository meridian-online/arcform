# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = [
#   "numpy>=1.26,<3",
#   "duckdb>=1,<2",
#   "safetensors>=0.4,<1",
#   "tokenizers>=0.20,<1",
# ]
# ///
"""text_embed — arcform typed Python operator (uv-run).

PROVISIONAL. THIS OPERATOR IS ON ITS WAY OUT — DO NOT HARDEN IT.

Embedding is a table lookup and a mean. It does not need a Python process, and under
arcform's implementation order it does not belong at this tier at all: a capability is
built as a DuckDB extension first, then in Rust/C, and only then in a managed
environment like `uv`. A DuckDB scalar function returning a vector is being built for
exactly this, and Rust that loads a Model2Vec tokenizer and embedding matrix already
ships elsewhere in the stack. When the extension lands, a Protocol embeds from a SQL
step — `SELECT …, embed(description) AS embedding FROM …` — and this operator is
deleted rather than maintained.

It exists now because that extension does not exist yet, and without it a Protocol has
no route from a text column to a map at all. So: a stopgap, named as one. If you are
about to add a knob here, add it to the extension instead. See README.md.

WHAT IT DOES. Reads one Parquet, embeds the named text column against a LOCAL
static-embedding model, and writes a Parquet carrying every input column plus a vector
column (`embedding` unless `--vector-column` says otherwise) of FLOAT.

IT WRITES VECTORS AND NOTHING ELSE, which is the point of it being its own step. An
analyst who wants vectors for similarity, clustering, deduplication or as classifier
features stops here and uses the output. An analyst who wants a map feeds that vector
column to `umap_project`, which neither knows nor cares that an embedding produced it.

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

DETERMINISM. There is no seed here — the embedding is a lookup and an average, with
no stochastic step to seed. Two things are pinned: THREADS, to one, before numpy or
DuckDB are imported; and ROW ORDER, by reading the input single-threaded with
insertion order preserved, carrying an explicit ordinal through the join, and ordering
the output by it. Parquet bytes follow row order. What is NOT pinned is the dependency
set — see README.md.

Run standalone:
    uv run operators/text_embed/text_embed.py \
        --input corpus.parquet --text-column description \
        --model models/potion-base-8M --out corpus_embedded.parquet
"""
from __future__ import annotations

# Thread pins go in BEFORE numpy or DuckDB are imported: each of them reads its thread
# count once, at import, and ignores a later change.
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
from safetensors.numpy import load_file
from tokenizers import Tokenizer

# The ordinal carried through the embed→join round trip. Underscored and prefixed so
# it cannot collide with a real column by accident; a collision is refused outright.
ROW = "__arc_embed_row"

# The column this operator adds when the caller names none.
DEFAULT_VECTOR_COLUMN = "embedding"

# The one tensor key model2vec writes. A model with a single tensor under any other
# name is accepted; more than one, and the caller has to be told which we would guess.
EMBEDDING_KEY = "embeddings"


class Refusal(Exception):
    """A condition the caller can fix, reported as one line rather than a traceback."""


def sql_lit(s: str) -> str:
    """A DuckDB single-quoted string literal: wrap in ' and double any interior '."""
    return "'" + s.replace("'", "''") + "'"


def sql_ident(s: str) -> str:
    """A DuckDB quoted identifier: wrap in " and double any interior "."""
    return '"' + s.replace('"', '""') + '"'


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
    # A zero row stays zero: dividing by its own norm would be 0/0. A cosine metric
    # downstream handles a zero vector without producing a NaN.
    np.divide(out, norms, out=out, where=norms > 0)
    return out, empty


def main() -> int:
    ap = argparse.ArgumentParser(description="Embed a text column into a vector column.")
    ap.add_argument("--input", required=True, help="Parquet to read.")
    ap.add_argument("--text-column", required=True, help="Column to embed.")
    ap.add_argument("--model", required=True, help="Static-embedding model directory.")
    ap.add_argument("--out", required=True, help="Parquet to write.")
    ap.add_argument(
        "--vector-column",
        default=DEFAULT_VECTOR_COLUMN,
        help="Column to write the vector into.",
    )
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
    vector_column = args.vector_column
    clashes = [c for c in (vector_column,) if c in columns]
    if columns.count(ROW) > 1:
        clashes.append(ROW)
    if clashes:
        raise Refusal(
            f"{src} already carries a column named {clashes[0]!r}; this operator adds "
            f"{vector_column!r} and will not overwrite an input column. Name another "
            f"with `vector_column:`."
        )
    if args.text_column not in columns:
        raise Refusal(
            f"{src} has no column {args.text_column!r}. It carries: "
            f"{', '.join(c for c in columns if c != ROW)}."
        )

    rows = con.execute(
        f"SELECT CAST({sql_ident(args.text_column)} AS VARCHAR) FROM arc_src "
        f"ORDER BY {ROW}"
    ).fetchall()
    if not rows:
        raise Refusal(f"{src} has no rows to embed.")
    texts = [("" if r[0] is None else r[0]) for r in rows]

    vectors, empty = embed(texts, tokenizer, table)
    if empty:
        print(
            f"[text_embed] {empty} of {len(texts)} rows tokenised to nothing in the "
            f"model vocabulary and embed as zero vectors",
            file=sys.stderr,
        )

    dim = vectors.shape[1]
    con.execute(
        f"CREATE TABLE arc_vec ({ROW} BIGINT, {sql_ident(vector_column)} FLOAT[{dim}])"
    )
    con.executemany(
        "INSERT INTO arc_vec VALUES (?, ?)",
        [(i, [float(v) for v in vectors[i]]) for i in range(len(texts))],
    )

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    con.execute(
        f"COPY (SELECT s.* EXCLUDE ({ROW}), v.{sql_ident(vector_column)} "
        f"FROM arc_src s JOIN arc_vec v ON s.{ROW} = v.{ROW} ORDER BY s.{ROW}) "
        f"TO {sql_lit(str(out))} (FORMAT parquet, COMPRESSION zstd)"
    )
    print(
        f"[text_embed] {len(texts)} rows · {dim}-d vectors → "
        f"{vector_column} in {out}"
    )
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Refusal as refusal:
        print(f"text_embed: {refusal}", file=sys.stderr)
        sys.exit(1)
