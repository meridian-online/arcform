# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = [
#   "umap-learn>=0.5,<0.6",
#   "numpy>=1.26,<3",
#   "duckdb>=1,<2",
# ]
# ///
"""umap_project — arcform typed Python operator (uv-run).

Reduces numeric columns that already exist to the two coordinates a map is drawn
from. Reads one Parquet, builds a feature matrix from the named columns, reduces it
to two dimensions with UMAP, and writes a Parquet carrying every input column plus
`projection_x` and `projection_y` as DOUBLE, and `projection_fit_id` as VARCHAR — a
fingerprint of the exact numbers and knobs this run fit, the same on every row, so a
reader can tell whether two files came from the same fit before comparing positions
between them.

NO TEXT COLUMN AND NO MODEL. `--column` names columns that are already numbers, and
a column is numeric in either of two shapes:

    a numeric scalar          BIGINT, DOUBLE, DECIMAL(…) — one feature
    a list/array of numerics  FLOAT[], DOUBLE[128] — one feature per element

So `--column longitude --column latitude` maps a table of places, and
`--column embedding` maps whatever wrote a vector column, without this script
knowing or caring which. Anything else is refused, naming the column and the DuckDB
type it actually has.

WHY THIS IS A `uv run` OPERATOR — the tier, stated where the next person to change it
will read it. arcform builds a capability as a DuckDB extension first, then in
Rust/C, and reaches a managed environment only when the tiers above cannot carry the
work. This is the third tier and the reason is UMAP itself: `umap-learn` is the
reference implementation, there is no DuckDB extension for it and no Rust one in this
stack, and a reimplementation would emit different coordinates from every published
UMAP map anyone might compare this one against. The capability is absent above, not
merely inconvenient. See README.md.

NOTHING HERE SCALES YOUR COLUMNS, and that is deliberate rather than an omission.
Under `euclidean` a column with a wider spread dominates the layout, which is a
decision about what the map should mean; it belongs in the SQL step that selects the
columns, where it is visible and arguable, not fused into the projection. One line of
DuckDB does it — see README.md.

DETERMINISM — the reason this operator can sit in a Protocol at all. Three things are
pinned, and the third is the one a seed alone does not cover:

  1. SEED, frozen here rather than exposed in `with:` — `op: umap_project@1`
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
    uv run operators/umap_project/umap_project.py \
        --input homes.parquet --column longitude --column latitude \
        --metric euclidean --out homes_mapped.parquet
"""
from __future__ import annotations

# Thread pins go in BEFORE numpy, numba or DuckDB are imported: each of them reads
# its thread count once, at import, and ignores a later change. The heavy imports
# are inside `main` rather than up here, so that the decidable half of this file —
# the type classifier and the SQL quoting below — can be imported and tested by a
# runner that has no numpy, no numba and no DuckDB. `test_umap_project.py` is that
# test and CI runs it; every other line here needs `uv`, which CI has not got.
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
import hashlib
import sys
from pathlib import Path

SEED = 42  # frozen: the projection's random_state, so a re-run lands on the same map

# The ordinal carried through the project→join round trip. Underscored and prefixed so
# it cannot collide with a real column by accident; a collision is refused outright.
ROW = "__arc_project_row"

# The three columns this operator adds. A conflict with an input column is refused
# rather than silently overwritten — the caller asked for both sets of values.
X_COL = "projection_x"
Y_COL = "projection_y"

# A refit moves every point — this operator persists nothing between invocations, so
# appending rows means re-fitting the whole map on the next run. What an analyst CAN
# tell from the file alone is whether two projections were ASKED the same question:
# FIT_ID_COL carries a hash of the exact feature matrix this run fed to UMAP together
# with the knobs that shaped the fit (n_neighbors, min_dist, metric, seed), broadcast
# to every row. A DIFFERENT id means the data or a knob changed, and no position in the
# file may be compared position-for-position against an older one — that direction is
# the whole guarantee. The converse does NOT hold: a MATCHING id means the same
# question was asked, not that the same answer came back. This operator's dependency
# resolve is not pinned (see README.md, "Does byte-identity survive a dependency
# upgrade?"), so two machines — or the same machine after `uv` re-resolves umap-learn,
# numba or DuckDB — can share a fit_id and still emit different coordinates. Only on
# one pinned environment does a matching id also mean byte-identical output, and that
# is the existing determinism claim, not a new one this column makes. It answers "did
# the layout change", not "did a particular row's data change" — that second question
# is answerable from the row's own columns without this operator's help.
FIT_ID_COL = "projection_fit_id"

DEFAULT_NEIGHBORS = 15
DEFAULT_MIN_DIST = 0.1

# The metrics this operator will pass to UMAP. umap-learn accepts many more; these two
# are the ones the test suite drives, and a name outside the set is refused at manifest
# load rather than by an exception from inside UMAP an hour into a run. `euclidean` is
# umap-learn's own default and the right reading of an arbitrary feature matrix;
# `cosine` is the right reading of L2-normalised vectors, which is what an embedding
# is. Adding a third is a line here AND a line changing UMAP_METRICS in src/operator.rs
# (which both the authoring schema's `enum` and the operator's config validation read
# from) — `umap_project_metrics_list_agrees_with_the_script` parses this exact tuple out
# of the frozen bytes and reddens if the two lists disagree, so this is not a comment
# asking you to remember, it is a test that fails if you do not.
METRICS = ("euclidean", "cosine")
DEFAULT_METRIC = "euclidean"

# UMAP fits a k-nearest-neighbour graph, so it needs more rows than neighbours. Below
# this there is no neighbourhood structure to find and a map would be noise wearing
# coordinates.
MIN_ROWS = 5

# DuckDB's numeric scalar types, by the name `DESCRIBE` reports. DECIMAL is matched by
# prefix because it reports its precision and scale.
NUMERIC_TYPES = frozenset(
    {
        "TINYINT",
        "SMALLINT",
        "INTEGER",
        "BIGINT",
        "HUGEINT",
        "UTINYINT",
        "USMALLINT",
        "UINTEGER",
        "UBIGINT",
        "UHUGEINT",
        "FLOAT",
        "DOUBLE",
        "REAL",
    }
)


class Refusal(Exception):
    """A condition the caller can fix, reported as one line rather than a traceback."""


def sql_lit(s: str) -> str:
    """A DuckDB single-quoted string literal: wrap in ' and double any interior '."""
    return "'" + s.replace("'", "''") + "'"


def sql_ident(s: str) -> str:
    """A DuckDB quoted identifier: wrap in " and double any interior "."""
    return '"' + s.replace('"', '""') + '"'


def numeric_shape(duckdb_type: str) -> str | None:
    """How a DuckDB type contributes features, or `None` when it contributes none.

    `"scalar"` — one number per row. `"vector"` — a list or array of numbers, one
    feature per element. `None` for everything else, which is what the caller turns
    into a refusal naming the column.

    A fixed-size array survives a Parquet round trip as a LIST (`FLOAT[16]` written,
    `FLOAT[]` read back), so both spellings have to answer the same way or a chained
    Protocol would refuse its own previous step's output.
    """
    remaining = duckdb_type.strip().upper()
    vector = False
    while remaining.endswith("]"):
        opened = remaining.rfind("[")
        if opened == -1:
            return None
        remaining = remaining[:opened].strip()
        vector = True
    if not (remaining in NUMERIC_TYPES or remaining.startswith("DECIMAL")):
        return None
    return "vector" if vector else "scalar"


def feature_widths(rows: list, columns: list[str], shapes: list[str]) -> list[int]:
    """How many features each requested column contributes — 1 for a scalar, the
    element count for a vector. A vector column has to be the same width in every row,
    because a feature matrix is rectangular."""
    widths = []
    for index, (name, shape) in enumerate(zip(columns, shapes)):
        if shape == "scalar":
            widths.append(1)
            continue
        lengths = set()
        nulls = 0
        for row in rows:
            value = row[index]
            if value is None:
                nulls += 1
            else:
                lengths.add(len(value))
        if nulls:
            raise Refusal(
                f"{nulls} of {len(rows)} rows hold NULL in the vector column {name!r}. "
                f"A row with no vector has no position; filter them out or fill them in "
                f"the step that produces this input."
            )
        if len(lengths) != 1:
            raise Refusal(
                f"the vector column {name!r} is not one width: it holds vectors of "
                f"{', '.join(str(n) for n in sorted(lengths))} elements. A feature "
                f"matrix is rectangular."
            )
        width = lengths.pop()
        if width == 0:
            raise Refusal(
                f"the vector column {name!r} holds empty vectors — there is nothing to "
                f"project."
            )
        widths.append(width)
    return widths


def compute_fit_id(
    payload: bytes, neighbors: int, min_dist: float, metric: str, seed: int
) -> str:
    """The whole fingerprint contract in one place: a fit_id depends on the exact
    VALUES a fit consumed (`payload`, the feature matrix's own bytes — not its shape,
    not a summary of it) and the knobs that shaped the fit. Hoisted out of `main()` so
    it needs nothing beyond the standard library: `test_umap_project.py` — stdlib-only,
    and the one test file CI runs for this operator without `uv` — can pin that the id
    moves when the payload's CONTENT changes, not only when its length does, directly
    rather than only through the `uv`-dependent end-to-end test in
    tests/umap_project.rs.
    """
    return hashlib.sha256(
        payload + f"|{neighbors}|{min_dist}|{metric}|{seed}".encode()
    ).hexdigest()[:16]


def main() -> int:
    import duckdb
    import numpy as np
    import umap

    ap = argparse.ArgumentParser(
        description="Reduce existing numeric columns to two coordinates."
    )
    ap.add_argument("--input", required=True, help="Parquet to read.")
    ap.add_argument(
        "--column",
        required=True,
        action="append",
        dest="columns",
        help="A numeric column to project. Repeat for each one.",
    )
    ap.add_argument("--out", required=True, help="Parquet to write.")
    ap.add_argument("--neighbors", type=int, default=DEFAULT_NEIGHBORS)
    ap.add_argument("--min-dist", type=float, default=DEFAULT_MIN_DIST)
    ap.add_argument("--metric", choices=METRICS, default=DEFAULT_METRIC)
    args = ap.parse_args()

    src = Path(args.input)
    if not src.is_file():
        raise Refusal(f"the input Parquet {src} does not exist.")

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
    described = con.execute("DESCRIBE arc_src").fetchall()
    types = {row[0]: row[1] for row in described}
    names = [row[0] for row in described]

    # `names` ends with the ordinal this step just added, so the input carried it too
    # only if it appears twice.
    clashes = [c for c in (X_COL, Y_COL, FIT_ID_COL) if c in types]
    if names.count(ROW) > 1:
        clashes.append(ROW)
    if clashes:
        raise Refusal(
            f"{src} already carries a column named {clashes[0]!r}; this operator adds "
            f"{X_COL!r}, {Y_COL!r} and {FIT_ID_COL!r} and will not overwrite an input "
            f"column."
        )

    columns: list[str] = args.columns
    missing = [c for c in columns if c not in types]
    if missing:
        raise Refusal(
            f"{src} has no column {missing[0]!r}. It carries: "
            f"{', '.join(c for c in names if c != ROW)}."
        )
    duplicated = sorted({c for c in columns if columns.count(c) > 1})
    if duplicated:
        raise Refusal(
            f"column {duplicated[0]!r} is named more than once; each column "
            f"contributes its features once."
        )

    shapes = []
    for name in columns:
        shape = numeric_shape(types[name])
        if shape is None:
            raise Refusal(
                f"column {name!r} is {types[name]}, which is not a number this "
                f"operator can place on a map. Project columns that are numeric, or a "
                f"list/array of numerics; to map text, produce a vector column from it "
                f"first."
            )
        shapes.append(shape)

    selected = ", ".join(sql_ident(c) for c in columns)
    rows = con.execute(
        f"SELECT {selected} FROM arc_src ORDER BY {ROW}"
    ).fetchall()
    if len(rows) < MIN_ROWS:
        raise Refusal(
            f"{src} has {len(rows)} rows; a neighbourhood projection needs at least "
            f"{MIN_ROWS} to have any neighbourhood to describe."
        )

    widths = feature_widths(rows, columns, shapes)
    matrix = np.empty((len(rows), sum(widths)), dtype=np.float64)
    owners: list[str] = []
    at = 0
    for index, (name, shape, width) in enumerate(zip(columns, shapes, widths)):
        owners.extend([name] * width)
        for row_index, row in enumerate(rows):
            value = row[index]
            if shape == "scalar":
                matrix[row_index, at] = np.nan if value is None else float(value)
            else:
                for offset in range(width):
                    element = value[offset]
                    matrix[row_index, at + offset] = (
                        np.nan if element is None else float(element)
                    )
        at += width

    # NULL arrived as NaN above, and a DOUBLE column may hold a NaN or an infinity of
    # its own. Either way UMAP would propagate it and the map would carry coordinates
    # that are not positions, so name the columns responsible instead.
    unusable = ~np.isfinite(matrix)
    if unusable.any():
        blamed = sorted({owners[j] for j in np.nonzero(unusable.any(axis=0))[0]})
        raise Refusal(
            f"{int(unusable.any(axis=1).sum())} of {len(rows)} rows hold a NULL, a NaN "
            f"or an infinity in {', '.join(repr(c) for c in blamed)}. A point with no "
            f"number has no position on a map."
        )

    # n_neighbors must be strictly below the row count; clamp rather than fail, so a
    # small input still lands on a map instead of on an exception from inside UMAP.
    k = max(2, min(args.neighbors, len(rows) - 1))
    reducer = umap.UMAP(
        n_components=2,
        n_neighbors=k,
        min_dist=args.min_dist,
        metric=args.metric,
        random_state=SEED,
        verbose=False,
    )
    coordinates = np.asarray(reducer.fit_transform(matrix), dtype=np.float64)

    # Everything that determines the fit, not just the numbers: the same matrix under
    # a different `neighbors:`/`min_dist:`/`metric:` is a different map, and a fit_id
    # that only hashed the matrix would claim two such maps were comparable when they
    # are not. SEED is a constant, included anyway so the id is a complete fingerprint
    # of the call rather than one that happens to be complete only while SEED stays 42.
    fit_id = compute_fit_id(matrix.tobytes(), k, args.min_dist, args.metric, SEED)

    con.execute(
        f"CREATE TABLE arc_proj ({ROW} BIGINT, {X_COL} DOUBLE, {Y_COL} DOUBLE, "
        f"{FIT_ID_COL} VARCHAR)"
    )
    con.executemany(
        "INSERT INTO arc_proj VALUES (?, ?, ?, ?)",
        [
            (i, float(coordinates[i, 0]), float(coordinates[i, 1]), fit_id)
            for i in range(len(rows))
        ],
    )

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    con.execute(
        f"COPY (SELECT s.* EXCLUDE ({ROW}), p.{X_COL}, p.{Y_COL}, p.{FIT_ID_COL} "
        f"FROM arc_src s JOIN arc_proj p ON s.{ROW} = p.{ROW} ORDER BY s.{ROW}) "
        f"TO {sql_lit(str(out))} (FORMAT parquet, COMPRESSION zstd)"
    )
    print(
        f"[umap_project] {len(rows)} rows · {matrix.shape[1]} features from "
        f"{len(columns)} column(s) · {args.metric} · seed {SEED} · fit_id {fit_id} → {out}"
    )
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Refusal as refusal:
        print(f"umap_project: {refusal}", file=sys.stderr)
        sys.exit(1)
