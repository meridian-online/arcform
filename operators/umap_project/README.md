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
    # optional, each omitted from the script's argv when unset. Each is PROVEN to
    # reach this script's own `uv run` invocation carrying the exact value set here —
    # see "What each knob is proven to do" below for what that does and does not cover:
    metric: euclidean # forwarded to UMAP's `metric=` — euclidean or cosine, per umap-learn's own docs
    neighbors: 15     # UMAP's `n_neighbors=`, CLAMPED to one below the row count — see "The neighbours clamp" below
    min_dist: 0.1     # forwarded to UMAP's `min_dist=` — how tightly points may pack, in [0, 1), per umap-learn's own docs
```

There is **no text column and no model**. A map of longitudes and latitudes needs
neither, and demanding them is what made the merged step this replaced unusable for
that case.

## What each knob is proven to do, and what it is not

`umap_project_invocation_materialises_the_frozen_script_and_names_it`
(`src/operator.rs`) sets `metric:`, `neighbors:` and `min_dist:` in a manifest and
asserts the built `uv run` invocation carries each value verbatim. That closes a real
gap: it used to be possible for a knob to be silently dropped between the manifest and
the subprocess argv **with every test still green under CI conditions** — that is, on
a runner with no `uv`, which is every routine runner here. With a real `uv` the
pre-existing byte test already reddened for `metric:`. The qualifier is the whole
claim: what was missing was cover on the machines that actually run the suite.

**What it does not prove is that, once inside the frozen script, each value is wired to
the UMAP parameter its name promises rather than to a different one.**
`operators/umap_project/umap_project.py` passes them straight through —
`min_dist=args.min_dist`, `metric=args.metric` — in the one call to `umap.UMAP(...)`
inside `main()`.

There **is** an existing check aimed at exactly this —
`the_knobs_a_manifest_sets_change_the_map_not_just_the_argv` in `tests/umap_project.rs`
runs four real projections through a real `uv` and `umap-learn` and asserts each
knobbed run's output bytes differ from the default run's — and it is not enough,
measured rather than assumed. Mutating the call above to
`min_dist=DEFAULT_MIN_DIST, metric=args.metric, set_op_mix_ratio=args.min_dist` — the
manifest's `min_dist:` silently controlling `set_op_mix_ratio` instead, a real UMAP
keyword that also takes a float in `[0, 1)` — and re-running that one test leaves it
`ok`, `1 passed; 0 failed`, exit 0. The bytes still move (a different keyword still
changes the fit), so `assert_ne!` is satisfied for the wrong reason, and nothing in
this repository's suite would fail if that swap actually happened.

A test that told the two apart would need to assert something about *which way* the
layout moved as the knob moved — mean pairwise distance rising as `min_dist` rises is
the obvious candidate for that knob — rather than only that it moved at all. That test
does not exist, and adding it would not close it as a CI gate regardless: it needs real
`umap-learn`, `main()` imports `umap`, `numpy` and `duckdb` inside itself precisely so
the rest of this script (and `test_umap_project.py`) stays importable and runnable on a
machine with none of the three, which is every CI runner (see "Which tier this sits at"
below) — and the one workflow that does stage a real environment
(`text-embed-parity.yml`) runs on a schedule, not on a pull request. Until a test like
that exists and runs somewhere a defect in it is guaranteed to be seen, this
repository does not independently verify that a manifest's `metric:`, `neighbors:` or
`min_dist:` moves the layout the way its name says. What it verifies is that the value
the manifest set is what reaches the script.

**Two sentences that used to stand here have been deleted rather than rewritten, and
what replaced them is smaller on purpose.** One offered umap-learn's own documentation
as the reading of these three knobs. The other offered, as the guarantee a reader still
had after every other one was withdrawn, that the layout moves *somehow* when a knob
changes. Both are false for `neighbors:` — see below — and a residual guarantee that
does not hold is worse than no guarantee, because it is the one a reader relies on
precisely when they have been told to trust nothing else.

## The neighbours clamp

**`neighbors:` stops working above one below the input's row count, the run still
succeeds, and nothing prints.** `clamp_neighbors` in `umap_project.py` is
`max(2, min(requested, n_rows - 1))`, and UMAP is given the clamped value. The clamp is
there because UMAP raises when `n_neighbors` reaches the row count, and a small table
landing on a map beats a small table landing on a traceback.

Measured end to end through `arc run` on a real 48-row input with a real `uv` and
`umap-learn`: `neighbors: 47`, `100` and `200` all produce `sha256 29e51cc98b16974f`
and `fit_id cae3df77263a1dff` — byte-identical maps — while `neighbors: 40` on the same
input moves the bytes. The step's stdout prints rows, features, metric, seed and
`fit_id`, and never `k`.

**This is arcform's deviation, not umap-learn's**, which is why umap-learn's
documentation for `n_neighbors` does not describe what happens above the boundary, and
why this file no longer sends you there. `projection_fit_id` is computed from the
clamped `k` rather than from the value the manifest set, so two manifests that differ
only above the boundary produce the same fingerprint — which is correct, since they
produce the same fit.

**Nothing refuses `neighbors:` above the boundary.** The authoring schema declares
`"minimum": 2` and no maximum, and `validate()` bounds it only below. Refusing at load
is impossible — the manifest cannot know the row count — and refusing at run needs a
mechanism this repository has not decided on yet. Until then this is disclosed rather
than prevented, in three places that stay in step: here, in `clamp_neighbors`'s own
docstring, and in the `neighbors` description the authoring schema emits, which is what
an editor and `arc mcp`'s `operator_describe` show and which no edit to this file
reaches. `umap_project_schema_discloses_the_neighbors_clamp` reddens if that third one
loses the disclosure.

## What counts as a column it can project

`columns:` names columns that are already numbers, in either of two shapes:

| shape | example DuckDB type | features contributed |
|---|---|---|
| a numeric scalar | `BIGINT`, `DOUBLE`, `DECIMAL(9,2)` | one |
| a list or array of numerics | `FLOAT[]`, `DOUBLE[128]` | one per element |

So `columns: [longitude, latitude]` maps a table of places and `columns: [embedding]`
maps whatever wrote a vector column — including `text_embed`, and including a SQL step
calling the DuckDB embedding extension's `embed()` directly. The operator is not told which
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
it is now a separate step (`text_embed`) — and why the embedding itself has since moved
to tier 1, leaving that step a wrapper around one SQL call.

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

## Appending rows moves the whole map, and it was measured before it was decided

**This operator has no memory between invocations.** It is a fresh `uv run` per
step, given one Parquet and told to fit and project it — nothing about a previous
fit survives to the next call. So appending rows and running the step again today
means a full `fit_transform` over everything, old rows and new, and every point can
move. That is a fact about what THIS OPERATOR does now, not about what the
underlying library can do — see "Pricing the alternative" below.

An analyst who appends rows and reruns cannot tell, from the map alone, whether the
picture changed because the data did or because the layout was refit around it.

**How much, re-measured 2026-08-25, is `eval/map-refit-stability/`
in this repository — `uv run eval/map-refit-stability/measure.py`, committed output in
`results.json`.** 3,000 rows of Homebrew package descriptions, embedded with
`minishlab/potion-base-8M` through this operator's own sibling `text_embed@1` (which
takes its vectors from the DuckDB embedding extension), then
projected through this operator itself — no reimplementation of either. The corpus is
`eval/map-refit-stability/corpus.parquet`, COMMITTED (not `examples/brewtrend`'s own
output, which is gitignored and rebuilt from live, rolling analytics on every `arc
run` — a moving snapshot that would silently change these numbers): a frozen,
deterministic 4,500-row slice, ordered by `name` so nothing is cherry-picked, of which
the first 3,000 are the "existing" map and the next rows are the appended ones. See
`measure.py`'s own docstring for the exact query that produced it, and re-run the
harness rather than trusting this table — that is what re-derivable means.

**These numbers moved on 2026-08-25 and the reason is worth stating, because it is the
same effect the table measures.** `text_embed` stopped computing its own vectors and
started taking them from the DuckDB embedding extension, so the input to every fit
below changed — not by much (ordinary text agreed to float32 noise; package
descriptions carrying tokens outside the vocabulary moved further), but a refit is
chaotic in exactly the way this eval exists to show. The 5% append's mean displacement
went from 38× to 61× the median gap and its neighbourhood overlap from 0.46 to 0.42;
the 50% append landed at the same 268× and moved from 0.35 to 0.36. **The finding is
unchanged and the control still holds** — refitting identical rows still reproduces
identical coordinates — so what moved is the size of the effect on one corpus, not the
conclusion. Do not read a difference between this table and an older copy of it as a
change in `umap_project`; nothing in this operator changed.

Two measures, because either alone misleads: raw+normalised displacement of a row's
(x, y) between fits (UMAP's frame is arbitrary up to rotation/scale between
independent fits, and that arbitrariness is itself part of what an analyst sees as
"the map turned", so it is deliberately not aligned away), and the fraction of a
row's 20 nearest *other* existing rows shared between the two fits — which isolates
whether the refit rearranged the analyst's existing reading, as opposed to new rows
simply now being nearby, which is expected. This is CHURN: both sides of the
comparison are the same 3,000 base rows, so its ceiling is 1.00 and the no-new-rows
control below proves that ceiling is reached. It is a different quantity from the
placement FIDELITY numbers further down, whose query is a new row and whose ceiling
is not 1.00 — the two are not read against each other anywhere in this file.

The **no-new-rows control matters more than any other number here**: refitting the
identical 3,000 rows from scratch, in a separate process, reproduces the prior run's
coordinates exactly — 0.0 displacement, 100% neighbourhood overlap — confirming this
operator's documented determinism (pinned seed, thread count, row order) is the floor
every other number below is read against, not zero.

| append | shared rows scored | mean displacement (× median gap) | 20-NN overlap, mean |
|---|---|---|---|
| 0% (control) | 3,000 | 0× | 1.00 |
| 5% (150 rows) | 3,000 | 61× | 0.42 |
| 20% (600 rows) | 3,000 | 168× | 0.39 |
| 50% (1,500 rows) | 3,000 | 268× | 0.36 |

A modest, realistic append already destroys about half of a point's visual
neighbourhood: **a 5% append shares only 42% of a point's 20 nearest map-neighbours
with the pre-append layout**, and displacement grows from 61 to 268 times the map's
own typical point spacing as the append grows — points do not drift, they land
somewhere else on the map.

Read against a sibling measurement of how much neighbourhood structure survives
*swapping the embedding model entirely* on a comparable text corpus — **0.13
long-form, 0.28 short, 0.40 very-short, higher is more retained; this figure comes
from a measurement in a different repository, has no source committed here, and is
NOT checked by `check_findings.py` for that reason — read it as context, not as a
pinned claim here** — on the same kind of kNN-overlap scale: **the two
disturbances are the same order of magnitude, not one uniformly worse than the
other.** Ranked by how much structure survives, highest to lowest: 5% append (0.42) >
swapping the embedder on very-short text (0.40) > 20% append (0.39) ≈ 50% append
(0.36) > swapping the embedder on short text (0.28) > swapping the embedder on
long-form text (0.13). A 5% append preserves *more* structure than any embedder swap
measured; by 20%, an append has already lost slightly more than swapping the embedder
does on its easiest (very-short-text) case, though every append fraction here still
preserves more than swapping on short or long-form text. Refitting on an ordinary
append is not a small, forgivable jitter — whether it is gentler or harsher than
changing the embedder depends on the fraction and the corpus, but it sits in the same
range.

### Pricing the alternative: `UMAP.transform` is real, and here is what it costs

**`umap-learn`'s `UMAP.transform(X)` is public in the pinned bound
(`umap-learn>=0.5,<0.6`, resolved 0.5.12 as of 2026-08-24), and places new rows into
an existing fitted embedding without moving the rows already in it.** Verified by
actually calling it — `eval/map-refit-stability/price_transform.py`,
`uv run eval/map-refit-stability/price_transform.py`, committed output in
`transform_pricing.json` — against the same 3,000-row base and the same 5/20/50%
append pools the headline table above uses, so the pricing below is comparable to
it rather than a separate, smaller demo.

Four costs, measured rather than assumed:

1. **Model persistence.** `.transform()` needs the FITTED reducer object, not just
   its output coordinates — the k-NN graph and the optimised embedding it walks to
   place a new point have to survive between the process that ran `fit()` and a
   later process that runs `.transform()`, and this operator today persists nothing
   between invocations. Pickled, the fitted reducer for the 3,000-row base is
   **3.6 MB**. It would need a place in a Protocol's asset graph — most naturally a
   `produces` asset the first fit writes and a `reads` asset a later append-only run
   reads back, the same shape `text_embed`'s declared extension artifact already uses
   for a different kind of frozen input.

2. **A compatibility rule that does not exist yet.** A persisted model is only valid
   for the SAME base rows under the SAME knobs it was fit with — if a base row's
   values changed, or a row was removed, or `neighbors:`/`min_dist:`/`metric:`
   moved, the persisted model no longer describes the current input and
   `.transform()` would silently place new rows against a mapping that has quietly
   gone stale. Nothing prices or designs that check here; it is named as a real,
   unbuilt requirement, not assumed away.

3. **Whether the base rows actually stay put — measured, not assumed.**
   `reducer.embedding_` was captured right after `fit()` and compared, by max
   absolute difference, against `reducer.embedding_` after every `.transform()` call
   in the pricing script: **0.0**. `.transform()`'s documented contract says it does
   not re-optimise the training embedding; this is that claim executed rather than
   taken on faith.

4. **Placement fidelity, read against this corpus's own ceiling rather than against
   1.00 or against the churn number above.** A 2D map cannot recover every
   neighbour a 256-d embedding space has — this operator's own README already
   measures that loss for a different corpus (34.9% at k=10, "What the coordinates
   support" above). For THIS corpus, at the same k=20 used throughout: a BASE row's
   own base fit recovers **30.3%** of its 256-d neighbourhood — the ceiling every
   number below is read against. Scored the same way, against the 256-d embeddings
   as ground truth (cosine, k=20, base rows as the candidate pool on both sides): a
   **full refit** places a NEW row at 29.6% / 28.1% / 27.0% (5% / 20% / 50%
   appends) — 89-97% of the ceiling, never AT it, and least close where the
   recommendation is most exposed (50% appended). **`.transform()`** places the
   same new rows at 27.0% / 26.4% / 27.2% — 3 to 4 points below the ceiling, which
   is the gap against the ceiling, not against the full refit (the gap against the
   full refit itself is smaller: 2.6 / 1.7 / -0.2 points, and at the 50% append
   `.transform()` is marginally AHEAD of the full refit). On this corpus,
   out-of-sample placement is close to as faithful as a full refit, not
   indistinguishable from it.

   (The pricing script also self-checks this: a full refit's fidelity against the
   256-d truth should stay close to the ceiling, and an assertion fails loudly if it
   does not — swapping the full refit's own base coordinates for the original base
   fit's, a mistake made once while writing this measurement, drops that number
   below 0.1 and the assertion catches it before the numbers reach this file.)

### Pricing the other alternative: refit only on an explicit user action

**This is not implementable in arcform today**, and saying so needs a mechanism
named, not asserted. `compute_staleness` (`src/runner.rs`) marks a step stale
whenever `is_hash_stale` finds its declared inputs have changed — appending a row to
this step's input Parquet is exactly that — and where `preconditions:` exist,
staleness is `hash_stale || !preconditions_fresh`: a precondition can only ADD
staleness, never suppress a hash-driven one. There is no branch anywhere in that
function, and no flag on `arc run` (`--force` and `--param` are the only ones; there
is no per-step selector), that lets a step whose input changed stay un-run pending a
separate, later user action. `runner::tests::test_glob_read_change_forces_the_reading_step_stale`
pins exactly this: appending bytes to a step's read glob is one of three mutations
the test proves forces that step stale, and it is asserted, not incidental.
Building "hold this step's output until the user asks" would be a genuine addition to
arcform's step model — every step today is a pure function of its declared,
hash-checked inputs — and that is a different surface and separate work; it is not
built here.

### The choice, priced rather than assumed

**Pin the existing rows by persisting the fitted model and placing appended rows with
`.transform()`.** Not because the alternative above is free — it is not implementable
at all today — and not because out-of-sample placement is a small compromise assumed
away: it is 3 to 4 points below this corpus's own ceiling for the new rows, measured
against the 256-d truth rather than against the churn number, which is a different
quantity. The case for it is the asymmetry between what it protects and what it
risks: the base rows — which is most of the corpus at every fraction measured here,
and the ones an analyst has already built a reading around — are held EXACTLY, by
construction and confirmed by measurement (§3 above), with zero drift. The rows
placed a few points under the ceiling are exactly the rows the analyst has no prior
reading of yet, because they are new. A full refit corrupts everyone's reading to
place new rows barely better than `.transform()` does; pinning corrupts nothing and
places the new rows almost as well. That asymmetry, not a claim that either technique
is free, is the argument.

**Where the asymmetry stops being obvious.** The argument assumes an analyst reading
the BASE rows, whose positions are exactly held — not an analyst appending rows
specifically to see where the NEW ones land, who gets a placement a few points off a
full refit's own, with nothing in the interface to say so; that discrimination is
part of what pinning would still need to design, not something this measurement
provides. And it assumes appended rows stay a minority of what is on screen. At the
20% fraction measured here, new rows are 600 of 3,600 — 17% of the map, still
plainly a minority. At 50%, they are 1,500 of 4,500 — a third of the map, not "a
handful". Somewhere between those two measured points the map stops being "mostly
exact positions plus some approximate ones" and becomes a map where a substantial
share of what is on screen carries the fidelity gap above; nothing measured here
locates that point more precisely than the two fractions it is bracketed by.

**None of this is implemented here.** A strategy is chosen and its
trade-off stated, not built. What would be needed: model persistence and a
compatibility check inside `operators/umap_project/umap_project.py` (this operator's own
surface, no runner change required for this half), and, if "the map does not move at
all until asked" is wanted on top of that, the runner capability named above (a
different surface). Neither is built here — persisting the fit and computing
`projection_fit_id` from it rather than from the current input is real design work
of its own and is tracked separately.

**Telling a refit from an append, as this operator ships today.** Every output row
carries `projection_fit_id` — a hash of the exact feature matrix and knobs
(`neighbors`, `min_dist`, `metric`, seed) that fit consumed, the same value on every
row. It moves whenever the data or a knob changes, and it cannot tell you which —
today, every run is a full refit, so "the data changed" and "the layout changed" are
the same event and the id does not need to distinguish them. A DIFFERENT id means no
row's position may be compared position-for-position against an older file. The
converse does **not** hold: a MATCHING id means the same data and knobs were used,
not that the coordinates are guaranteed identical — this operator's dependency
resolve is not pinned (see "Does byte-identity survive a dependency upgrade?" above),
so two machines, or the same machine after a resolve moves, can share a fit_id and
still emit different coordinates. Discriminating "the data changed" from "the layout
changed" arrives when pinning does, not before — computing the id from a persisted
fit rather than from the current input is part of that future work, not this one. A
downstream renderer (`brightfield`) that wants to warn an analyst a map has changed
reads this column today for that coarser signal; distinguishing why it changed is not
available from it yet.

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
