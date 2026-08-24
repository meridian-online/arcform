# umap_project

Reduces numeric columns that **already exist** to the two coordinates a map is drawn
from. `op: umap_project@1` reads one Parquet, builds a feature matrix from the columns
you name, reduces it to two dimensions with UMAP, and writes a Parquet carrying every
input column plus `projection_x` and `projection_y` as `DOUBLE`.

```yaml
- name: project
  op: umap_project@1
  with:
    input: build/homes.parquet
    columns: [longitude, latitude, median_income]
    out: build/homes_mapped.parquet
    # optional, each omitted from the script's argv when unset:
    metric: euclidean # how distance is measured — euclidean or cosine
    neighbors: 15     # UMAP n_neighbors — low reads local structure, high reads global
    min_dist: 0.1     # UMAP min_dist — how tightly points may pack, in [0, 1)
```

There is **no text column and no model**. A map of longitudes and latitudes needs
neither, and demanding them is what made the merged step this replaced unusable for
that case.

## What counts as a column it can project

`columns:` names columns that are already numbers, in either of two shapes:

| shape | example DuckDB type | features contributed |
|---|---|---|
| a numeric scalar | `BIGINT`, `DOUBLE`, `DECIMAL(9,2)` | one |
| a list or array of numerics | `FLOAT[]`, `DOUBLE[128]` | one per element |

So `columns: [longitude, latitude]` maps a table of places and `columns: [embedding]`
maps whatever wrote a vector column — including `text_embed`, and including a SQL step
once the DuckDB embedding extension can produce one. The operator is not told which
produced it. Mixing the two shapes in one `columns:` list is allowed and they are
concatenated in the order listed.

A fixed-size array survives a Parquet round trip as a plain list (`FLOAT[16]` written,
`FLOAT[]` read back), so both spellings are accepted; a chained Protocol would
otherwise refuse its own previous step's output.

Anything else is refused **naming the column and the type it found**, because "that
did not work" sends an author back to the schema to guess which of several columns was
the problem. A NULL, a NaN or an infinity in a projected column is refused the same
way: a point with no number has no position on a map.

What is decidable from the manifest alone is refused at load rather than an hour into
a run — an empty `columns:`, a column named twice, `neighbors` below 2, a `min_dist`
outside `[0, 1)`, a `metric` this operator does not pass to UMAP.

## It does not scale your columns, and that is deliberate

Under `euclidean`, a column with a wider spread dominates the layout. Whether that is
right is a decision about what the map should *mean*, and it belongs in the SQL step
that selects the columns — where it is visible and arguable — rather than fused into
the projection where it would be neither. One line of DuckDB does it:

```sql
CREATE OR REPLACE TABLE homes_scaled AS
SELECT *,
       (median_income - avg(median_income) OVER ()) / stddev_pop(median_income) OVER ()
           AS median_income_z
FROM homes;
```

`metric: cosine` is the other lever: it reads each row as a direction rather than a
position, which is what an L2-normalised vector wants and what a longitude does not.
`euclidean` is the default because it is umap-learn's own and the right reading of an
arbitrary feature matrix.

## Which tier this sits at, and what the tiers above could not do

arcform builds a capability as a **DuckDB extension** first, then in **Rust or C**, and
reaches a **managed environment** (`uv` for Python) only when the tiers above cannot
carry the work. Reaching past a tier requires naming what in that tier cannot do the
job — so, here it is.

**This is the third tier, and the reason is UMAP itself.** `umap-learn` is the
reference implementation of the algorithm. There is no DuckDB extension implementing
it, and no Rust implementation in this stack. A reimplementation would not be the same
capability at a better tier: UMAP's layout optimisation is stochastic and
floating-point, so a second implementation emits *different coordinates*, and the
value of a map is largely that it can be compared with other maps of the same corpus
drawn the same way. That is a capability absent above, not one that is merely
inconvenient to reach — which is what the escape hatch is for.

The embedding half of this operator's predecessor had no such argument, which is why
it is now a separate step (`text_embed`) explicitly marked as on its way to tier 1.

## Does byte-identity survive a dependency upgrade?

**No, and it is not meant to. Byte-identity is a property of a pinned environment, and
what this operator pins is the code, not the environment.** Three things are frozen:

1. **The seed.** `SEED = 42` lives in the script rather than in `with:`, and `op@1`
   addresses those exact bytes, so no manifest can move it. Removing it is not
   cosmetic: with `random_state=None`, two runs over the same input emit different
   files.
2. **The thread count**, set before numpy, numba or DuckDB are imported — each reads
   its thread count once, at import. umap-learn already takes the single-threaded path
   whenever `random_state` is set, and three runs at 120×48 and three at 2000×384 were
   byte-identical with the pin and without it on 2026-08-23. It stays because the
   spectral initialisation reaches BLAS, where a thread count is free to reorder a
   float sum, and a million-row input is the wrong place to find that out.
3. **The row order.** The input is read single-threaded with DuckDB's insertion order
   preserved, an explicit ordinal is carried through the join, and the output is
   ordered by it. Parquet bytes follow row order.

What is **not** pinned is the dependency set. The PEP-723 header bounds every direct
dependency at both ends — `umap-learn>=0.5,<0.6`, `numpy>=1.26,<3`, `duckdb>=1,<2` —
but a resolve inside those bounds can still pick a newer umap-learn, a newer numba
underneath it, or a newer DuckDB, and any of the three can move the output: UMAP's
layout optimisation is floating-point, and DuckDB writes its own version into the
Parquet footer. Bounds stop a major-version change from silently rewriting the map;
they do not make two machines agree.

So: **the same environment reproduces the same bytes, and the way to hold a Protocol
to a map is to hold it to an environment.** A `uv` lockfile for the script would close
this, and it is not in place yet.

## What the coordinates support, and what they do not

A map's coordinates are a **layout**, not an index. Read them as regions: the shape of
the input, which parts of it are dense, which sit apart, and where a filtered subset
falls relative to the whole.

Do not read them as a nearest-neighbour lookup, and that is measurable rather than a
caveat. Taking each point's ten nearest neighbours in a 256-dimensional embedding and
asking how many are still among its ten nearest in the map: **34.9% on average**
(median 30.0%) over the first 4,000 tokens of `potion-base-8M`'s own vocabulary,
measured 2026-08-23 with the parameters above. Roughly two thirds of each point's true
neighbourhood is somewhere else on the map. That is a property of two dimensions, not
a defect of this operator — 256 dimensions of structure do not fit in two, and a
projection that preserved every neighbourhood would not be a projection.

The measurement inflates on a small input: on this repository's 48-row text fixture
the same k of 10 covers a fifth of the whole corpus and the overlap rises to 66.5%.
The 4,000-point figure is the one to reason from.

The practical rule: **if the question is "what is most like this one", ask the vectors,
not the coordinates.** The map is for seeing the shape of the whole — which is one more
reason the two are separate steps: the answer to that question is the other step's
output, and it is a file you already have.

## Standalone

```bash
uv run operators/umap_project/umap_project.py \
    --input homes.parquet --column longitude --column latitude \
    --metric euclidean --out homes_mapped.parquet
```

The fixture Protocol in `tests/fixtures/umap_project/` is a complete working example
whose input is plain numbers and which contains no model at all.
`tests/fixtures/text_embed/` is the other one: a text column turned into vectors and
only then into a map.

`operators/umap_project/test_umap_project.py` covers the decidable half of the script
— which types can be placed on a map, how many features each contributes, the SQL
quoting — with the standard library alone, so it runs in CI where `uv` does not exist.
