# text_embed

> **Provisional. This operator is on its way out — do not harden it.**
> Embedding is a table lookup and a mean, and under arcform's implementation order it
> does not belong at this tier at all. A DuckDB scalar function returning a vector is
> being built for exactly this; when it lands a Protocol embeds from a SQL step and
> this operator is **deleted** rather than ported. See *Which tier this sits at* below.
> A knob proposed here belongs on the extension.

Turns a text column into a **vector column**. `op: text_embed@1` reads one Parquet,
embeds the named text column against a **local** static-embedding model, and writes a
Parquet carrying every input column plus a vector column of `FLOAT`.

```yaml
- name: embed
  op: text_embed@1
  with:
    input: build/corpus.parquet
    text_column: description
    model: models/potion-base-8M
    out: build/corpus_embedded.parquet
    # optional, omitted from the script's argv when unset:
    vector_column: embedding   # name another to embed a second column in a later step
```

## It writes vectors and nothing else

That is the point of it being its own step. An analyst who wants vectors for
similarity, clustering, deduplication or as classifier features **stops here**, and the
file they have is the answer. An analyst who wants a map feeds the vector column to
`umap_project`:

```yaml
- name: project
  op: umap_project@1
  with:
    input: build/corpus_embedded.parquet
    columns: [embedding]
    out: build/corpus_mapped.parquet
    metric: cosine    # an L2-normalised vector is a direction; cosine reads it as one
```

`umap_project` is not told that an embedding produced its input — it is handed a
numeric column like any other, and would take a longitude and a latitude just as
readily. Neither half is reachable only through the other, which is what one merged
step made impossible.

## The model is an input, not a download

`model:` names a **directory the Protocol puts there**, and the operator records it as
a `reads` asset of kind Directory. Three things follow, and they are the reason this
belongs in an asset-centric engine rather than beside one:

* it is a node in the graph — `arc run` prints `model [directory] … feeds embed` — so a
  Protocol's dependency on one particular model is legible rather than implied;
* it is hashed for staleness like any other input, so replacing the model re-runs the
  embedding instead of leaving vectors from a different one in place;
* the step needs no network. A model that is not on disk stops the run in
  milliseconds, naming the file it looked for, rather than reaching for one.

A model directory holds the two files a [model2vec] / potion release ships:

```
model.safetensors   one 2-D float tensor, [vocab, dim], under the key `embeddings`
tokenizer.json      a HuggingFace `tokenizers` serialisation
```

Fetch one with `http_fetch`, a step per file, and the fetch becomes something the
Protocol declares rather than something the operator does:

```yaml
- name: fetch_model_weights
  op: http_fetch@1
  with:
    url: https://huggingface.co/minishlab/potion-base-8M/resolve/main/model.safetensors
    out: models/potion-base-8M/model.safetensors

- name: fetch_model_tokenizer
  op: http_fetch@1
  with:
    url: https://huggingface.co/minishlab/potion-base-8M/resolve/main/tokenizer.json
    out: models/potion-base-8M/tokenizer.json
```

**That is not an illustration — it was run verbatim.** `arc run` over a Protocol
carrying those two steps ahead of a projection step reported 5/5 succeeded on
2026-08-24, fetching 28.8 MB of weights and 668 KB of tokenizer and writing the
projected Parquet. Two things a reader should know before adapting it. The
`http_fetch` steps write a `.arcmeta` sidecar beside each file; the operator reads
only the two files it names, so the extra files are harmless, but they are inside the
directory whose contents the staleness hash covers. And `arc run` printed
`models/potion-base-8m [directory] produced by (external source)` alongside separate
nodes for the two fetched files: arcform does not infer that a file written under a
path feeds the directory node above it, so the fetches sit ahead of the embedding by
their position in the manifest and not by a graph edge. Keep them above it.

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

## Which tier this sits at, and why that is a problem rather than a justification

arcform builds a capability as a **DuckDB extension** first, then in **Rust or C**, and
reaches a **managed environment** (`uv` for Python) only when the tiers above cannot
carry the work — and reaching past a tier requires naming what in that tier cannot do
the job.

**Nothing above this tier is incapable of it, so there is no such name to give.** That
is the whole content of the warning at the top of this file. Embedding here is
tokenise, index a `[vocab, dim]` matrix, mean-pool, L2-normalise; a DuckDB scalar
function does that and composes with `WHERE` and `LIMIT` besides, and Rust that loads a
Model2Vec tokenizer and embedding matrix already ships elsewhere in this stack. The
extension is being built. Until it exists, a Protocol has no route from a text column
to a map at all, and this operator is the stopgap that keeps that route open.

The consequence to keep in view: **the same capability is currently implemented twice,
in two languages**, and two implementations of one thing are two places for a model
version, a pooling rule or a normalisation to drift apart — invisibly, because each is
correct on its own terms. That is the cost of the stopgap, and the reason it is
labelled rather than tidied up.

`umap_project` is the opposite case and worth contrasting: UMAP has no equivalent at
any higher tier, so it is at the third tier for a reason it can state. One operator
used to carry both halves, and the justified half concealed the unjustified one.

## Determinism

There is no seed: the embedding is a lookup and an average with no stochastic step.
Two things are pinned — the **thread count**, set before numpy or DuckDB are imported,
and the **row order**, by reading the input single-threaded with DuckDB's insertion
order preserved, carrying an explicit ordinal through the join and ordering the output
by it. Parquet bytes follow row order.

What is **not** pinned is the dependency set. The PEP-723 header bounds every direct
dependency at both ends — `numpy>=1.26,<3`, `duckdb>=1,<2`, `safetensors>=0.4,<1`,
`tokenizers>=0.20,<1` — but a resolve inside those bounds can still pick a newer
DuckDB, which writes its own version into the Parquet footer. The same environment
reproduces the same bytes; two machines are not thereby made to agree.

## Standalone

```bash
uv run operators/text_embed/text_embed.py \
    --input corpus.parquet --text-column description \
    --model models/potion-base-8M --out corpus_embedded.parquet
```

The fixture Protocol in `tests/fixtures/text_embed/` is a complete working example of
the whole chain, including `make_fixture_model.py`, which builds a tiny model in the
same layout so the tests need no download.

[model2vec]: https://github.com/MinishLab/model2vec
