# embed_project

Turns a text column into the two coordinates a map is drawn from. `op: embed_project@1`
reads one Parquet, embeds the named text column against a **local** static-embedding
model, reduces that embedding to two dimensions with UMAP, and writes a Parquet
carrying every input column plus `projection_x` and `projection_y` as `DOUBLE`.

```yaml
- name: project
  op: embed_project@1
  with:
    input: build/corpus.parquet
    text_column: description
    model: models/potion-base-8M
    out: build/corpus_mapped.parquet
    # optional, both omitted from the script's argv when unset:
    neighbors: 15     # UMAP n_neighbors — low reads local structure, high reads global
    min_dist: 0.1     # UMAP min_dist — how tightly points may pack, in [0, 1)
```

## The model is an input, not a download

`model:` names a **directory the Protocol puts there**, and the operator records it as
a `reads` asset of kind Directory. Three things follow, and they are the reason this
belongs in an asset-centric engine rather than beside one:

* it is a node in the graph — `arc run` prints `model [directory] … feeds project` —
  so a Protocol's dependency on one particular model is legible rather than implied;
* it is hashed for staleness like any other input, so replacing the model re-runs the
  projection instead of leaving a map built by a different one in place;
* the step needs no network. A model that is not on disk stops the run in
  milliseconds, naming the file it looked for, rather than reaching for one.

A model directory holds the two files a [model2vec] / potion release ships:

```
model.safetensors   one 2-D float tensor, [vocab, dim], under the key `embeddings`
tokenizer.json      a HuggingFace `tokenizers` serialisation
```

Fetch one with the operators arcform already has, and the fetch becomes a step the
Protocol declares:

```yaml
- name: fetch_model
  op: http_fetch@1
  with: { url: https://…/potion-base-8M.tar.gz, out: build/model.tar.gz }
- name: unpack_model
  op: archive_extract@1
  with: { archive: build/model.tar.gz, dest: models/potion-base-8M }
```

Embedding is a lookup and a mean: tokenise with the model's own `tokenizer.json`,
average the embedding-table rows for the token ids, L2-normalise. Measured on
2026-08-23 against the published `minishlab/potion-base-8M`, that reproduces
`model2vec.StaticModel.encode` to a maximum absolute difference of 1.7e-08 — float32
rounding — so a released model2vec or potion model can be pointed at as it comes. A
row whose text is NULL, empty, or made only of out-of-vocabulary tokens embeds as a
zero vector, and the count is reported on stderr rather than passed over.

There is no HTTP client in the script's imports and it reads no environment variable —
the only environment it touches, it writes (the thread pins). A run's cost is the
machine it runs on.

## Does byte-identity survive a dependency upgrade?

**No, and it is not meant to. Byte-identity is a property of a pinned environment, and
what this operator pins is the code, not the environment.** Three things are frozen:

1. **The seed.** `SEED = 42` lives in the script rather than in `with:`, and `op@1`
   addresses those exact bytes, so no manifest can move it. Removing it is not
   cosmetic: with `random_state=None`, two runs over the fixture corpus emitted
   different files.
2. **The thread count**, set before numpy, numba or DuckDB are imported — each reads
   its thread count once, at import. umap-learn already takes the single-threaded path
   whenever `random_state` is set, and three runs at 120×48 and three at 2000×384 were
   byte-identical with the pin and without it on 2026-08-23. It stays because the
   spectral initialisation reaches BLAS, where a thread count is free to reorder a
   float sum, and a million-row corpus is the wrong place to find that out.
3. **The row order.** The input is read single-threaded with DuckDB's insertion order
   preserved, an explicit ordinal is carried through the join, and the output is
   ordered by it. Parquet bytes follow row order.

What is **not** pinned is the dependency set. The PEP-723 header bounds every direct
dependency at both ends — `umap-learn>=0.5,<0.6`, `numpy>=1.26,<3`, `duckdb>=1,<2`,
`safetensors>=0.4,<1`, `tokenizers>=0.20,<1` — but a resolve inside those bounds can
still pick a newer umap-learn, a newer numba underneath it, or a newer DuckDB, and any
of the three can move the output: UMAP's layout optimisation is floating-point, and
DuckDB writes its own version into the Parquet footer. Bounds stop a major-version
change from silently rewriting the map; they do not make two machines agree.

So: **the same environment reproduces the same bytes, and the way to hold a Protocol
to a map is to hold it to an environment.** A `uv` lockfile for the script would close
this, and it is not in place yet.

## What the coordinates support, and what they do not

A map's coordinates are a **layout**, not an index. Read them as regions: the shape of
the corpus, which parts of it are dense, which sit apart, and where a filtered subset
falls relative to the whole.

Do not read them as a nearest-neighbour lookup, and that is measurable rather than a
caveat. Taking each point's ten nearest neighbours in the embedding and asking how
many are still among its ten nearest in the map: **34.9% on average** (median 30.0%)
over the first 4,000 tokens of `potion-base-8M`'s own vocabulary, measured 2026-08-23
with the parameters above. Roughly two thirds of each point's true neighbourhood is
somewhere else on the map. That is a property of two dimensions, not a defect of this
operator — 256 dimensions of structure do not fit in two, and a projection that
preserved every neighbourhood would not be a projection.

The measurement inflates on a small corpus: on this repository's 48-row fixture the
same k of 10 covers a fifth of the whole corpus and the overlap rises to 66.5%. The
4,000-point figure is the one to reason from.

The practical rule: **if the question is "what is most like this one", ask the
embedding, not the coordinates.** The map is for seeing the shape of the whole.

## Standalone

```bash
uv run operators/embed_project/embed_project.py \
    --input corpus.parquet --text-column description \
    --model models/potion-base-8M --out corpus_mapped.parquet
```

The fixture Protocol in `tests/fixtures/embed_project/` is a complete working example,
including `make_fixture_model.py`, which builds a tiny model in the same layout so the
tests need no download.

[model2vec]: https://github.com/MinishLab/model2vec
