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

A PERSISTED FIT IS OPTIONAL AND CHANGES WHAT AN APPEND MEANS. Without `--fit` this
script fits the whole input every time, so appending a row re-draws the entire map and
every point can move — measured in `eval/map-refit-stability/`, and mostly an artefact
of the drawing rather than the data. With `--fit PATH` the fitted projection is written
to PATH the first time and READ back on every later run: rows the fit already holds keep
their exact coordinates, and rows it has never seen are placed into that same layout
with `UMAP.transform`. `projection_fit_id` then comes from the persisted fit rather than
from the current input, so two files that share an id share a LAYOUT and their positions
may be compared row for row even though one has more rows in it. See README.md,
"Appending rows moves the whole map".

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
import pickle
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

# WITHOUT `--fit`, a refit moves every point: nothing survives between invocations, so
# appending rows means re-fitting the whole map on the next run. WITH `--fit`, the id is
# read back out of the persisted fit and does not move when rows are appended, because
# the layout did not — that is the whole of what a persisted fit buys a reader, and
# `fit_id_source` below is the one line that decides which of the two a file carries.
# What an analyst CAN tell from the file alone is whether two projections were ASKED the
# same question:
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

# The name a persisted fit records for the thing that wrote it, and the shape of the
# record. `--fit` reads back a pickle, and a pickle is executable bytes with no type of
# its own: a file that is not this operator's fit, or is one from a future shape of it,
# is refused by name here rather than unpickled into a plausible-looking map.
OPERATOR_NAME = "umap_project"
FIT_FORMAT = 1

# The header fields a persisted fit and the run trying to use it must agree on, in the
# order they are reported. THE POINT OF THE LIST IS THAT A MISMATCH IS NAMED: a fit for
# different columns, a different vector width, a different knob or a different
# umap-learn will all LOAD, and all place rows somewhere plausible, which is the failure
# that costs the most because nothing about the output looks wrong. `describe_mismatch`
# below turns each into one line the caller can act on.
FIT_COMPARED_FIELDS = (
    "columns",
    "feature_widths",
    "neighbors",
    "min_dist",
    "metric",
    "seed",
    "umap_learn_version",
)

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


def clamp_neighbors(requested: int, n_rows: int) -> int:
    """The `n_neighbors` UMAP is actually given, which is NOT always what was asked.

    UMAP fits a k-nearest-neighbour graph, so `n_neighbors` must be strictly below the
    row count; above that it raises. Clamping rather than failing keeps a small input
    on a map instead of on an exception from inside a library.

    THE COST IS THAT THE KNOB STOPS ABOVE `n_rows - 1` AND SAYS NOTHING. On a 48-row
    input, `neighbors:` of 47, 100 and 200 all produce byte-identical output; 40 moves
    the bytes. That is a real deviation from umap-learn's documented meaning for this
    parameter, and it is arcform's, not umap-learn's — which is why the READMEs and
    the authoring schema state it instead of pointing a reader at umap-learn's own
    docs for what `n_neighbors` does.

    Hoisted out of `main()` deliberately: `main()` imports umap, numpy and duckdb
    inside itself so this module stays importable on a machine with none of the three,
    which is every routine CI runner. A clamp inline in `main()` is a clamp no runner
    can test, and deleting it left the whole suite green.
    """
    return max(2, min(requested, n_rows - 1))

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


def row_digest(row_bytes: bytes) -> str:
    """One feature row's identity, used to ask whether a persisted fit has seen it.

    A row IS its numbers here: the fit records a digest per base row, and a later run
    asks, row by row, whether the fit already holds a position for these exact values.
    Nothing about the input's ORDER is used, deliberately — appended rows sorted into
    the middle of a table are the ordinary case (a SQL step with an `ORDER BY name`
    puts them wherever the name falls), and a rule that assumed appends land at the end
    would silently mis-identify every one of them.
    """
    return hashlib.sha256(row_bytes).hexdigest()


def match_base_rows(
    current_digests: list[str], base_digests: list[str]
) -> tuple[list[int | None], list[int]]:
    """Which rows of this input the persisted fit already holds a position for.

    Returns `(placement, missing)`. `placement[i]` is the index into the fit's stored
    coordinates for current row `i`, or `None` when the fit has never seen that row and
    it must be placed with `.transform()`. `missing` is every fit row no current row
    claims — a row the fit was built from that is no longer in the input, which means
    the input was not appended to but EDITED, and the caller refuses rather than drawing
    a map from a layout that describes rows which are gone.

    DUPLICATE FEATURE ROWS ARE HANDED OUT IN ORDER AND THEN REUSED. Two input rows with
    identical numbers are one point as far as a projection is concerned; if the fit holds
    two such rows, the first two claimants take one each, and a third takes the last
    again. That keeps the mapping a pure function of the input's own content rather than
    of how many duplicates happen to arrive, and identical numbers landing on identical
    coordinates is the answer a reader expects either way.
    """
    by_digest: dict[str, list[int]] = {}
    for index, digest in enumerate(base_digests):
        by_digest.setdefault(digest, []).append(index)

    claimed_count: dict[str, int] = {}
    placement: list[int | None] = []
    for digest in current_digests:
        candidates = by_digest.get(digest)
        if not candidates:
            placement.append(None)
            continue
        taken = claimed_count.get(digest, 0)
        placement.append(candidates[min(taken, len(candidates) - 1)])
        claimed_count[digest] = taken + 1

    claimed = {index for index in placement if index is not None}
    missing = [index for index in range(len(base_digests)) if index not in claimed]
    return placement, missing


def describe_mismatch(stored: dict, current: dict) -> str | None:
    """The one line naming why a persisted fit does not describe this input, or `None`.

    THIS IS THE FUNCTION AC4 IS ABOUT AND IT IS PURE ON PURPOSE. A persisted fit outlives
    the data it was built from. A fit for different columns, for a vector of a different
    width, under a different `neighbors:`/`min_dist:`/`metric:`, or from a different
    umap-learn will all unpickle without complaint and all place rows at coordinates that
    look like coordinates — which is the expensive failure, because nothing downstream
    can tell. Every one of those is a field compared here, and the answer names the field
    and both values so the caller can decide whether to fix the input or discard the fit.

    Hoisted out of `main()` alongside the classifier for the same reason: `main()` imports
    umap, numpy and duckdb, so a check left inline is a check no routine CI runner can
    execute. `test_umap_project.py` drives this one directly.
    """
    if not isinstance(stored, dict) or "operator" not in stored:
        kind = type(stored).__name__
        return (
            f"it holds a {kind} that records no operator name — every fit this operator "
            f"writes names {OPERATOR_NAME!r} as the thing that wrote it, so this file is "
            f"not one. Point --fit at a file {OPERATOR_NAME} produced."
        )
    if stored["operator"] != OPERATOR_NAME:
        return (
            f"it was written by {stored['operator']!r}, not by {OPERATOR_NAME!r}. "
            f"Point --fit at a file this operator produced."
        )
    if stored.get("fit_format") != FIT_FORMAT:
        return (
            f"it is fit format {stored.get('fit_format')!r} and this operator writes "
            f"and reads format {FIT_FORMAT}. Delete it and let this run fit again."
        )
    for field in FIT_COMPARED_FIELDS:
        was, now = stored.get(field), current.get(field)
        if was == now:
            continue
        if field == "feature_widths":
            return (
                f"it was fit on {sum(was)} features ({_widths_by_column(stored)}) and "
                f"this input offers {sum(now)} ({_widths_by_column(current)}). A layout "
                f"built for one vector width cannot place a row of another."
            )
        if field == "columns":
            return (
                f"it was fit on columns {was!r} and this run projects {now!r}. A layout "
                f"built from different numbers places rows against a structure this "
                f"input does not have."
            )
        if field == "umap_learn_version":
            return (
                f"it was fit by umap-learn {was}, and this run has umap-learn {now}. "
                f"The fitted object loads across versions and places rows either way, "
                f"so the version is compared rather than trusted."
            )
        return (
            f"it was fit with {field}={was!r} and this run asks for {field}={now!r}. "
            f"The same numbers under a different knob are a different map."
        )
    return None


def _widths_by_column(header: dict) -> str:
    return ", ".join(
        f"{name}={width}"
        for name, width in zip(header.get("columns", []), header.get("feature_widths", []))
    )


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
    ap.add_argument(
        "--fit",
        default=None,
        help=(
            "Where the fitted projection lives. Written on the first run and READ on "
            "every later one: rows the fit already holds keep their exact coordinates "
            "and new rows are placed into that layout with UMAP.transform. Omit it and "
            "every run refits the whole input, which moves every point."
        ),
    )
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

    # See `clamp_neighbors` for what this costs and why it is disclosed rather than
    # hidden: above `len(rows) - 1` the knob stops moving and nothing says so.
    k = clamp_neighbors(args.neighbors, len(rows))

    # The header a persisted fit is compared against. `neighbors` is what the CALLER
    # asked for, not the clamped `k`: an append makes the table longer, so the clamp can
    # legitimately land somewhere else on the second run while the manifest is untouched,
    # and comparing the clamped value would refuse every append of a small table.
    header = {
        "operator": OPERATOR_NAME,
        "fit_format": FIT_FORMAT,
        "columns": list(columns),
        "feature_widths": list(widths),
        "neighbors": args.neighbors,
        "min_dist": args.min_dist,
        "metric": args.metric,
        "seed": SEED,
        "umap_learn_version": getattr(umap, "__version__", "unknown"),
    }

    fit_path = Path(args.fit) if args.fit else None
    digests = [row_digest(matrix[i].tobytes()) for i in range(len(rows))]
    reused = 0

    if fit_path is not None and fit_path.is_file():
        with open(fit_path, "rb") as handle:
            stored = pickle.load(handle)
        # The header is what is compared, but an unrecognisable file has no header, and
        # naming the object actually on disk is more use than naming the None that
        # reading a missing key returns.
        record = (
            stored["header"]
            if isinstance(stored, dict) and isinstance(stored.get("header"), dict)
            else stored
        )
        mismatch = describe_mismatch(record, header)
        if mismatch is not None:
            raise Refusal(
                f"the persisted fit {fit_path} does not describe this input: {mismatch}"
            )

        placement, gone = match_base_rows(digests, stored["base_row_digests"])
        if gone:
            raise Refusal(
                f"{len(gone)} of the {len(stored['base_row_digests'])} rows the "
                f"persisted fit {fit_path} was built from are not in {src}. A fit places "
                f"appended rows into an existing layout; it cannot describe an input "
                f"rows were removed from or edited in. Delete the fit to re-fit, or "
                f"restore the rows."
            )

        base_embedding = np.asarray(stored["base_embedding"], dtype=np.float64)
        coordinates = np.empty((len(rows), 2), dtype=np.float64)
        appended = [i for i, at in enumerate(placement) if at is None]
        for i, at in enumerate(placement):
            if at is not None:
                coordinates[i] = base_embedding[at]
        reused = len(rows) - len(appended)
        if appended:
            # The whole point: the rows already on the map keep the coordinates the fit
            # gave them, byte for byte, and only the new ones are placed.
            coordinates[appended] = np.asarray(
                stored["reducer"].transform(matrix[appended]), dtype=np.float64
            )

        # AC3's surface. The id names the LAYOUT, not this run's input, so a file with
        # appended rows carries the same id as the file before the append and the two
        # may be read row for row against each other. A refit — no `--fit`, or a fit
        # this run had to build — carries a different one, which is what says the
        # positions are not comparable.
        fit_id = stored["header"]["fit_id"]
        fit_id_source = f"persisted fit {fit_path}"
    else:
        reducer = umap.UMAP(
            n_components=2,
            n_neighbors=k,
            min_dist=args.min_dist,
            metric=args.metric,
            random_state=SEED,
            verbose=False,
        )
        coordinates = np.asarray(reducer.fit_transform(matrix), dtype=np.float64)

        # Everything that determines the fit, not just the numbers: the same matrix
        # under a different `neighbors:`/`min_dist:`/`metric:` is a different map, and a
        # fit_id that only hashed the matrix would claim two such maps were comparable
        # when they are not. SEED is a constant, included anyway so the id is a complete
        # fingerprint of the call rather than one that happens to be complete only while
        # SEED stays 42.
        fit_id = compute_fit_id(matrix.tobytes(), k, args.min_dist, args.metric, SEED)
        fit_id_source = "this run's own fit"

        if fit_path is not None:
            fit_path.parent.mkdir(parents=True, exist_ok=True)
            with open(fit_path, "wb") as handle:
                pickle.dump(
                    {
                        "header": {**header, "fit_id": fit_id},
                        "base_row_digests": digests,
                        "base_embedding": coordinates,
                        "reducer": reducer,
                    },
                    handle,
                    protocol=5,
                )
            fit_id_source = f"this run's own fit, written to {fit_path}"

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
    placed = (
        f"{reused} held + {len(rows) - reused} placed"
        if reused
        else f"{len(rows)} fitted"
    )
    print(
        f"[umap_project] {len(rows)} rows ({placed}) · {matrix.shape[1]} features from "
        f"{len(columns)} column(s) · {args.metric} · seed {SEED} · fit_id {fit_id} "
        f"from {fit_id_source} → {out}"
    )
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Refusal as refusal:
        print(f"umap_project: {refusal}", file=sys.stderr)
        sys.exit(1)
