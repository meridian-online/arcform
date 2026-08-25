# text_embed

> **Provisional. This operator is on its way out — do not harden it.**
> It no longer computes an embedding: it loads the DuckDB embedding extension and
> issues one SQL statement. What is left is a Parquet-in/Parquet-out wrapper around
> `SELECT embed(t)`, and it survives only because the extension is a file a Protocol
> has to carry rather than something installable. When that changes, this operator is
> **deleted** rather than ported. See *Which tier this sits at* below. A knob proposed
> here belongs on the extension.

Turns a text column into a **vector column**. `op: text_embed@1` reads one Parquet,
embeds the named text column with the extension's `embed()`, and writes a Parquet
carrying every input column plus a vector column of `FLOAT`.

```yaml
- name: embed
  op: text_embed@1
  with:
    input: build/corpus.parquet
    text_column: description
    extension: vendor/staticembed.duckdb_extension
    out: build/corpus_embedded.parquet
    # optional, omitted from the script's argv when unset:
    vector_column: embedding   # name another to embed a second column in a later step
    # optional, and set together or not at all — see "The model is checked, not loaded":
    model: models/potion-base-8M
    model_release: minishlab/potion-base-8M@bf8b056651a2c21b8d2565580b8569da283cab23
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

## One embedder, not two

**A vector this step writes and a vector a SQL session returns for the same text are
the same value, exactly.** They are the same call: this operator does not tokenise,
does not index an embedding table, does not mean-pool and does not normalise. It hands
the text to `embed()` and writes back what comes out.

It used to do all four in Python, and that is worth knowing about because of what it
cost. Measured against byte-identical weights over the corpus in
`tests/text_embed_parity.rs`, the Python path and the extension agreed on ordinary
text to float32 summation order — and departed on three shapes:

| case | why the two differed |
|---|---|
| text made only of tokens outside the vocabulary | one side averaged the tokenizer's unknown-token row into a unit-norm vector for text it had understood nothing of; the other dropped those ids and returned a zero vector |
| such tokens mixed into real words | the same unknown-token row |
| long text | the extension truncates it; the Python path did not |

Nothing on either side went red, because each was correct on its own terms. Against
`model2vec.StaticModel.encode` the extension agreed to float32 summation order on
every case in that corpus that is not NULL, and the Python path did not — so the
Python path was the side that was wrong, and it was deleted rather than corrected.
`tests/text_embed_parity.rs` re-checks the agreement over those same cases whenever a
built extension is available.

**No magnitude is quoted here, deliberately.** The Python path is deleted, so nothing
regenerates a difference against it and no test reddens when a figure written here
goes stale. A number in this file earns its place by having a test that fails when it
is wrong — the truncation boundary below is the worked example.

## The extension is an input, not a download

`extension:` names the **loadable artifact the Protocol puts there**, and the operator
records it as a `reads` asset. Three things follow, and they are the reason this
belongs in an asset-centric engine rather than beside one:

* it is a node in the graph — `arc run` prints it alongside the corpus — so a
  Protocol's dependency on one particular build of the embedder is legible rather than
  implied;
* it is hashed for staleness like any other input, so replacing the artifact re-runs
  the embedding instead of leaving vectors from a different embedder in place;
* the step needs no network. An artifact that is not on disk stops the run in
  milliseconds, naming the file it looked for, rather than reaching for one. It is
  never installed from a registry either — an extension has two ways of arriving by
  itself and the refusal closes both.

It is loaded with unsigned extensions allowed, because a locally built artifact is not
signed.

## The model is checked, not loaded

The weights live **inside** the extension binary. There is no model directory to read
them from, so `model:` is optional and means something narrower than it used to: it
declares *which model this Protocol believes it is embedding with*, and the run stops
if the extension disagrees.

The extension publishes a content address over its bundled assets through
`staticembed_version()`. This operator recomputes that address from the declared
directory and compares. A mismatch names both — what the Protocol declared and what
the extension reports — so the reader can see which of the two to change.

Reproducing the address needs the **three** files a published [model2vec] / potion
release ships, hashed in this order:

```
tokenizer.json      a HuggingFace `tokenizers` serialisation
model.safetensors   one 2-D float tensor, [vocab, dim]
config.json         the release's own configuration
```

and it needs the release's **identity and revision**, which none of those files
carries — hence `model_release: <model-id>@<revision>` beside `model:`. Both are
required together: a directory with no release names bytes whose address cannot be
computed, and a release with no directory names an address with nothing to compute it
from. Either alone would be a check that silently does not happen, so either alone is
refused at load.

A directory holding only `model.safetensors` and `tokenizer.json` — the layout this
operator wanted when it read weights itself — **cannot** produce the address, and the
run stops naming `config.json` rather than checking a weaker digest quietly. Fetch all
three, pinned to the revision rather than to `main`:

```yaml
- name: fetch_model_config
  op: http_fetch@1
  with:
    url: https://huggingface.co/minishlab/potion-base-8M/resolve/bf8b056651a2c21b8d2565580b8569da283cab23/config.json
    out: models/potion-base-8M/config.json
```

…and the same for `model.safetensors` and `tokenizer.json`. `resolve/main` moves; the
address is over exact bytes, so a Protocol that fetches from a branch is declaring
something it cannot check twice.

Declaring no model at all is a reasonable choice, and not a weaker one in the way it
looks: the extension artifact is itself a hashed input, so the vectors are already a
function of bytes the Protocol names. What `model:` adds is a statement, checked, about
*what those bytes are* — worth making when the model matters to whoever reads the
Protocol. Either way the operator prints the model identity and address it embedded
with on stdout, so the run's own output says which model produced the vectors.

## What comes back for text with nothing in it

`embed(NULL)` is SQL NULL, and a vector column of NULLs is not what a Parquet consumer
wants here, so the text is bridged with `coalesce(t, '')` — the bridge the extension
documents. With that bridge, a row whose text is **NULL, empty, whitespace, or made
only of tokens outside the model's vocabulary** embeds as a full-width **zero vector**,
and the count of those rows is reported on stderr rather than passed over — visible
when the script is run standalone. `arc run` captures a successful step's output and
does not print it today, so a Protocol run does not show that line; that is a gap in
the engine rather than in this operator.

**That is a change.** The Python path returned a zero vector for the first three cases
only. For the fourth it averaged the tokenizer's unknown-token row and handed back a
unit-norm vector for text it had understood nothing of — and did not count those rows,
so nothing on screen said it had happened.

## Long text is truncated

The extension cuts **twice**, and the boundary is **whichever comes first**: the raw
text is cut to **3,072 characters** before it is tokenised, and the token ids are then
cut to **512**. A long description is embedded from its opening either way.

The character cut is `512 × the model's median token length`, so it moves with the
model the extension carries — 3,072 is the value for the bundled `potion-base-8M`,
whose median token is six characters. **For ordinary English the character cut usually
wins**, because English runs shorter than six characters a token. 450 words of this
operator's own test filler come to 3,404 characters and are cut there, while what
survives is comfortably under 512 tokens. So *"under 512 tokens and your text is
whole"* is wrong at a length a real description reaches — which matters here, because
this operator does **not** report how many rows it happened to. That count belongs in
the SQL surface, where the person who cannot otherwise tell is sitting, and is being
added there.

`tests/text_embed_parity.rs` pins both cuts against the built extension, each so that
it can fail:

* **the character cut** — a text longer than 3,072 characters must embed to the same
  vector as its first 3,072 characters, and to a *different* vector from its first
  3,071. Both halves are needed: the first alone passes for any boundary at or below
  3,072, the second alone for any boundary above it, and together they hold only at
  exactly 3,072;
* **the token cut** — a text of 512 one-token words must not move when a 513th is
  appended, and a text of 511 must move when a 512th is. Every one of those four texts
  is under 3,072 characters, so the character cut cannot be what is acting;
* **that the character cut really does bite first** — the 3,404-character text above is
  truncated, and a 3,060-character prefix of it still takes a new word into its vector,
  which a text sitting at the token cut could not.

## Which tier this sits at, and why that is a problem rather than a justification

arcform builds a capability as a **DuckDB extension** first, then in **Rust or C**, and
reaches a **managed environment** (`uv` for Python) only when the tiers above cannot
carry the work — and reaching past a tier requires naming what in that tier cannot do
the job.

The embedding itself is now at the first tier, which is where it belonged. What is
left at the third tier is a wrapper: open a connection, load an artifact, run one
`COPY`. **Nothing above this tier is incapable of that either** — arcform links DuckDB
in Rust already. It stays for now because the artifact is a file a Protocol has to
declare and stage, and moving the wrapper into the engine is a separate change with
its own tests. That is a reason to postpone, not a justification, which is what the
warning at the top of this file is for.

`umap_project` is the opposite case and worth contrasting: UMAP has no equivalent at
any higher tier, so it is at the third tier for a reason it can state.

## Determinism

There is no seed: the embedding is a lookup and an average with no stochastic step.
Two things are pinned — DuckDB's **thread count**, set to one, and the **row order**,
by reading the input with insertion order preserved, carrying an explicit ordinal
through the join and ordering the output by it. Parquet bytes follow row order.

What is **not** pinned is the dependency set. The PEP-723 header bounds the one direct
dependency at both ends — `duckdb>=1,<2` — but a resolve inside those bounds can still
pick a newer DuckDB, which writes its own version into the Parquet footer. The same
environment reproduces the same bytes; two machines are not thereby made to agree. The
vectors themselves are the extension's and do not move with the resolve.

## What CI actually checks, and what it does not

There are two gates, and they cover different guarantees.

**The routine gate** — `ci.yml`'s `build` job, every push and every PR — has no
extension artifact and no `uv`. It compiles `tests/text_embed.rs` and
`tests/text_embed_parity.rs`, and it runs the handful of tests in them that need
neither: that a Protocol declaring an extension asset which was never staged stops the
run naming the file, and that the value-comparison predicate itself still tells two
vectors apart (`the_comparison_notices_a_single_float_out_of_place`). Every other test
in those two files is `#[ignore]`d, which is a structural difference from returning
early: `cargo test`'s own summary reports them as `ignored`, distinct from the `ok` a
test that ran and passed gets, and the routine gate's job summary repeats that count
under its own heading so a PR reader sees it without opening a log. **What the routine
gate does not check, on any given PR: that a Protocol run and a SQL session agree on a
single vector.**

**The staged gate** — `.github/workflows/text-embed-parity.yml`, on a daily schedule
and on `workflow_dispatch` — builds the extension from a pinned `staticembed` commit,
stages the model directory it bundles, installs `uv`, and runs both files again with
`--include-ignored`. There every test above actually executes: the Protocol/SQL
parity comparison over the full corpus, the zero-vector count on stderr, the
truncation-boundary pins, and all four of the model-address checks. Its job summary
names the `staticembed` commit and the extension's own `staticembed_version()` line, so
a pass there names the build it passed against rather than implying whatever
`staticembed` currently is. A change that breaks the vectors is caught here, not on
the PR that made it — up to a day later, or on demand.

## Standalone

```bash
uv run operators/text_embed/text_embed.py \
    --input corpus.parquet --text-column description \
    --extension vendor/staticembed.duckdb_extension \
    --out corpus_embedded.parquet
```

The fixture Protocol in `tests/fixtures/text_embed/` is a complete working example of
the whole chain. The extension artifact is not committed — it is tens of megabytes,
most of them weights — so the tests that need one are staged from
`ARC_STATICEMBED_EXTENSION` and `#[ignore]`d without it (see *What CI actually checks*
above). Running the fixture by hand means putting a built artifact beside
`arcform.yaml` yourself; running the ignored tests by hand means setting
`ARC_STATICEMBED_EXTENSION` (and, for the model checks, `ARC_STATICEMBED_MODEL` and
`ARC_STATICEMBED_MODEL_RELEASE`) and passing `cargo test -- --include-ignored`.

[model2vec]: https://github.com/MinishLab/model2vec
