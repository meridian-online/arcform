# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = [
#   "duckdb>=1,<2",
# ]
# ///
"""text_embed — arcform typed Python operator (uv-run).

PROVISIONAL. THIS OPERATOR IS ON ITS WAY OUT — DO NOT HARDEN IT. The vectors now come
from the DuckDB embedding extension: this script LOADs it and issues one SQL
statement, and it computes nothing about an embedding itself. What is left is a
Parquet-in/Parquet-out wrapper around `SELECT embed(t)`, which is a step a Protocol
can write for itself. When the extension is installable rather than a file a Protocol
has to carry, this operator is DELETED rather than ported. A knob proposed here
belongs on the extension instead.

WHY IT NO LONGER COMPUTES ANYTHING. It used to tokenise, look ids up in a
`[vocab, dim]` matrix, mean-pool and L2-normalise, in Python. That made one capability
exist twice in two languages, and the two disagreed: measured against the same weights
over the corpus in `tests/text_embed_parity.rs`, the vectors matched to float32
summation order for ordinary text, and departed for text carrying tokens the
vocabulary does not have and for text past the truncation boundary. Against
`model2vec.StaticModel.encode` the extension matched every case in that corpus that is
not NULL and this script did not, so this script was the side that was wrong. Deleting
the computation is what makes a SQL call and a Protocol run agree by construction
rather than by tolerance. No magnitude is quoted, deliberately: the Python path is
gone, so nothing regenerates a difference against it and no test reddens when a figure
written here rots.

WHAT IT DOES. Reads one Parquet, embeds the named text column with the extension's
`embed()`, and writes a Parquet carrying every input column plus a vector column
(`embedding` unless `--vector-column` says otherwise) of FLOAT.

IT WRITES VECTORS AND NOTHING ELSE, which is the point of it being its own step. An
analyst who wants vectors for similarity, clustering, deduplication or as classifier
features stops here and uses the output. An analyst who wants a map feeds that vector
column to `umap_project`, which neither knows nor cares that an embedding produced it.

THE EXTENSION IS AN INPUT, NEVER A DOWNLOAD. `--extension` names the loadable
artifact the Protocol declares as an asset — the same discipline the model directory
used to get. It is loaded with unsigned extensions allowed, because a locally built
artifact is not signed; nothing here installs one, and nothing here opens a socket.

WHAT COMES BACK FOR TEXT WITH NOTHING IN IT. `embed(NULL)` is SQL NULL and a vector
column of NULLs is not what a Parquet consumer wants here, so the text is bridged with
`coalesce(t, '')` — the bridge the extension documents. With that bridge a row whose
text is NULL, empty, whitespace, or made only of tokens outside the model's vocabulary
embeds as a full-width ZERO vector, and the count of those rows is reported on stderr
rather than passed over. That is a change: this script used to average the tokenizer's
`[UNK]` row for the fourth case and hand back a unit-norm vector for text it had
understood nothing of, and it did not count those rows. The count is visible when the
script is run standalone; `arc run` captures a successful step's output and does not
print it today, so a Protocol run does not show it.

LONG TEXT IS TRUNCATED, AT WHICHEVER OF TWO CUTS COMES FIRST. The extension cuts the
raw text to 3072 CHARACTERS before tokenising it, then cuts the token ids to 512 — so
a text can be truncated while still under 512 tokens, and for ordinary English it
usually is. A long description is embedded from its opening either way. This script
inherits the boundary and does not report how many rows it happened to — see the
operator README, and `tests/text_embed_parity.rs` for the probe that pins both cuts.

THE MODEL IS CHECKED, NOT LOADED. The model lives inside the extension binary, so
there is no model directory to read weights from. `--model` is therefore optional and
means something narrower than it used to: it declares WHICH model this Protocol
believes it is embedding with, and the run stops if the extension disagrees. The
extension publishes a content address over its bundled assets through
`staticembed_version()`, and this script recomputes that address from the declared
directory and `--model-release`. Reproducing it needs the three files a published
model2vec/potion release ships:

    model.safetensors   one 2-D float tensor, [vocab, dim]
    tokenizer.json      a HuggingFace `tokenizers` serialisation
    config.json         the release's own configuration

A directory carrying only the first two cannot produce the address, and the refusal
says so rather than checking a weaker thing quietly.

DETERMINISM. There is no seed — the embedding is a lookup and an average, with no
stochastic step to seed. Two things are pinned: DuckDB's THREAD COUNT, to one; and
ROW ORDER, by reading the input with insertion order preserved, carrying an explicit
ordinal through the join, and ordering the output by it. Parquet bytes follow row
order. What is NOT pinned is the dependency set — see README.md.

Run standalone:
    uv run operators/text_embed/text_embed.py \
        --input corpus.parquet --text-column description \
        --extension vendor/staticembed.duckdb_extension \
        --out corpus_embedded.parquet
"""
from __future__ import annotations

import argparse
import hashlib
import re
import sys
from pathlib import Path

import duckdb

# The ordinal carried through the embed→join round trip. Underscored and prefixed so
# it cannot collide with a real column by accident; a collision is refused outright.
ROW = "__arc_embed_row"

# The column this operator adds when the caller names none.
DEFAULT_VECTOR_COLUMN = "embedding"

# The three files a published model2vec/potion release ships, in the order the
# extension's content address hashes them. Both facts matter: a missing file makes the
# address unreproducible, and a different order makes it wrong.
MODEL_PARTS = ("tokenizer.json", "model.safetensors", "config.json")

# The domain tag the extension mixes into its model address so the digest cannot be
# confused with a plain SHA-256 of any one asset. Reproduced here rather than read from
# anywhere, which couples this check to version 1 of that derivation: a build using a
# later one will not match, and the refusal below says that is a possible cause.
MODEL_KEY_DOMAIN = b"staticembed/model-key/v1"

# `staticembed 0.1.0 (model minishlab/potion-base-8M@bf8b056651a2, key 1266aa250400, dim 256)`
VERSION_RE = re.compile(
    r"^staticembed (?P<build>\S+) \(model (?P<id>\S+)@(?P<revision>[0-9a-f]+), "
    r"key (?P<key>[0-9a-f]+), dim (?P<dim>\d+)\)$"
)


class Refusal(Exception):
    """A condition the caller can fix, reported as one line rather than a traceback."""


def sql_lit(s: str) -> str:
    """A DuckDB single-quoted string literal: wrap in ' and double any interior '."""
    return "'" + s.replace("'", "''") + "'"


def sql_ident(s: str) -> str:
    """A DuckDB quoted identifier: wrap in " and double any interior "."""
    return '"' + s.replace('"', '""') + '"'


def model_key(model_id: str, revision: str, parts: dict[str, bytes]) -> str:
    """The extension's content address for a model, recomputed from its own files.

    Hex, full width; the caller compares as many leading characters as the extension
    chose to publish. Every argument is hashed — a version of this that dropped one
    would still agree with a test that recomputed the same fields beside it.
    """
    digest = hashlib.sha256()
    digest.update(MODEL_KEY_DOMAIN)
    digest.update(model_id.encode("utf-8"))
    digest.update(b"\x00")
    digest.update(revision.encode("utf-8"))
    digest.update(b"\x00")
    for name in MODEL_PARTS:
        digest.update(parts[name])
    return digest.hexdigest()


def parse_version(reported: str) -> dict[str, str]:
    """Pull the model identity, content address and vector width out of the extension's
    own version line."""
    match = VERSION_RE.match(reported.strip())
    if match is None:
        raise Refusal(
            f"the extension reported a version line this operator cannot read: "
            f"{reported!r}. It expects `staticembed <build> (model <id>@<revision>, "
            f"key <hex>, dim <n>)`; a line saying the model is unavailable means the "
            f"artifact loaded but its weights did not."
        )
    return match.groupdict()


def split_release(release: str) -> tuple[str, str]:
    """`minishlab/potion-base-8M@bf8b0566…` → (id, revision)."""
    model_id, sep, revision = release.rpartition("@")
    if not sep or not model_id or not revision:
        raise Refusal(
            f"`model_release:` is {release!r}, which is not `<model-id>@<revision>`. "
            f"It is the release the declared model directory was taken from, and the "
            f"revision is the full commit the files came from — the extension hashes "
            f"both into the address this operator checks against."
        )
    return model_id, revision


def check_declared_model(model_dir: Path, release: str, version: dict[str, str]) -> str:
    """Refuse unless the declared model directory is the model the extension carries.

    Returns the recomputed address on success. The comparison is over the extension's
    own published prefix, so it moves with any byte of any of the three files, with the
    model id, and with the revision.
    """
    declared_id, declared_revision = split_release(release)
    parts: dict[str, bytes] = {}
    for name in MODEL_PARTS:
        path = model_dir / name
        if not path.is_file():
            raise Refusal(
                f"the declared model asset cannot be checked against the extension: "
                f"{path} is missing. The extension's model address covers "
                f"{', '.join(MODEL_PARTS)}, so a directory carrying fewer than all "
                f"three cannot reproduce it. Fetch the missing file from "
                f"{declared_id} at {declared_revision}, or drop `model:` and "
                f"`model_release:` and let the extension's own version line be the "
                f"record of which model embedded this corpus."
            )
        parts[name] = path.read_bytes()

    recomputed = model_key(declared_id, declared_revision, parts)
    published = version["key"]
    # EVERY character the extension published is compared, and the value compared is
    # the value reported below — a comparison narrowed to a prefix of it would be a
    # check on the first digits of an address rather than on which model this is.
    # `tests/text_embed_parity.rs` declares a model whose address agrees with the
    # published one for its leading characters and diverges after, so narrowing this
    # to fewer than that many characters accepts that model and reddens the test.
    checked = recomputed[: len(published)]
    if checked != published:
        reported_release = f"{version['id']}@{version['revision']}"
        raise Refusal(
            f"the extension does not carry the model this Protocol declares. The "
            f"Protocol declares {declared_id}@{declared_revision} in {model_dir}, "
            f"which addresses to {checked}; the extension "
            f"reports {reported_release} addressing to {published}. Either the "
            f"declared directory holds different bytes from the ones compiled into "
            f"the extension, or the extension derives its address a different way "
            f"from the one this operator reproduces."
        )
    return recomputed


def main() -> int:
    ap = argparse.ArgumentParser(description="Embed a text column into a vector column.")
    ap.add_argument("--input", required=True, help="Parquet to read.")
    ap.add_argument("--text-column", required=True, help="Column to embed.")
    ap.add_argument("--extension", required=True, help="staticembed extension artifact.")
    ap.add_argument("--out", required=True, help="Parquet to write.")
    ap.add_argument(
        "--vector-column",
        default=DEFAULT_VECTOR_COLUMN,
        help="Column to write the vector into.",
    )
    ap.add_argument("--model", help="Model directory to check the extension against.")
    ap.add_argument("--model-release", help="`<model-id>@<revision>` that directory is.")
    args = ap.parse_args()

    src = Path(args.input)
    if not src.is_file():
        raise Refusal(f"the input Parquet {src} does not exist.")
    extension = Path(args.extension)
    if not extension.is_file():
        raise Refusal(
            f"the embedding extension {extension} is not on disk. It is a declared "
            f"input, so a step in this Protocol has to put it there; this operator "
            f"does not download one."
        )
    if bool(args.model) != bool(args.model_release):
        raise Refusal(
            "`model:` and `model_release:` are checked against each other and against "
            "the extension, so neither means anything alone. Set both, or neither."
        )

    # Unsigned, because a locally built artifact is not signed. This is the only
    # configuration this script sets on the connection.
    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.execute(f"LOAD {sql_lit(str(extension))}")
    version = parse_version(con.execute("SELECT staticembed_version()").fetchone()[0])
    dim = int(version["dim"])
    if args.model:
        check_declared_model(Path(args.model), args.model_release, version)

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
    rows = con.execute("SELECT count(*) FROM arc_src").fetchone()[0]
    if not rows:
        raise Refusal(f"{src} has no rows to embed.")

    # The one place a text becomes a vector, and it is a call into the extension.
    # `coalesce` is the bridge the extension documents: embed(NULL) is NULL, and a
    # NULL where a vector belongs is not what a Parquet consumer can use.
    con.execute(
        f"CREATE TABLE arc_vec AS SELECT {ROW}, "
        f"embed(coalesce(CAST({sql_ident(args.text_column)} AS VARCHAR), '')) AS vec "
        f"FROM arc_src"
    )
    empty = con.execute(
        "SELECT count(*) FROM arc_vec "
        "WHERE list_sum(list_transform(vec, x -> abs(x))) = 0"
    ).fetchone()[0]
    if empty:
        print(
            f"[text_embed] {empty} of {rows} rows tokenised to nothing in the "
            f"model vocabulary and embed as zero vectors",
            file=sys.stderr,
        )

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    # The CAST is a WIDTH GUARD and nothing else. `embed()` returns FLOAT[] and DuckDB
    # writes a Parquet LIST from either type, so the output file is byte-for-byte the
    # same with the cast and without it — measured 2026-08-25. What it buys is a stop
    # if `embed()` ever hands back a width other than the one `staticembed_version()`
    # reported a moment earlier: `Cannot cast list with length N to array with length`.
    # IT IS NOT PINNED BY A TEST, and it cannot be from this repo — firing it needs an
    # extension whose `embed()` disagrees with its own version line, which is a build
    # nothing here can produce. The width actually written is covered instead, by the
    # parity comparison: two vectors of different widths are different values there.
    con.execute(
        f"COPY (SELECT s.* EXCLUDE ({ROW}), "
        f"CAST(v.vec AS FLOAT[{dim}]) AS {sql_ident(vector_column)} "
        f"FROM arc_src s JOIN arc_vec v ON s.{ROW} = v.{ROW} ORDER BY s.{ROW}) "
        f"TO {sql_lit(str(out))} (FORMAT parquet, COMPRESSION zstd)"
    )
    print(
        f"[text_embed] {rows} rows · {dim}-d vectors → {vector_column} in {out} "
        f"· {version['id']}@{version['revision']} key {version['key']}"
    )
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Refusal as refusal:
        print(f"text_embed: {refusal}", file=sys.stderr)
        sys.exit(1)
