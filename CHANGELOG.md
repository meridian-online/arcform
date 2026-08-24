# Changelog

All notable changes to arcform will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

Rationale for each change is recorded in the project's design notes and commit history.

## [Unreleased]

### Added

- **`embed_project` is split into `umap_project` and `text_embed`, because one name
  over two jobs made each one reachable only through the other.** An analyst who
  wanted vectors — for similarity, clustering, deduplication, or as classifier
  features — could not get them from a Protocol without also computing a 2-D map they
  had not asked for, and an analyst who wanted a map of columns that were ALREADY
  numbers could not use the step at all, because it insisted on embedding text first.
  The published Embedding Atlas gallery makes the second case concrete: its housing
  example draws a map from a longitude and a latitude with no embedding anywhere
  behind it, and the merged step could not serve it.

  `umap_project@1` takes `columns:` — a list of columns that are already numbers — and
  writes `projection_x` and `projection_y`. A numeric scalar contributes one feature; a
  list or array of numerics (a vector column) contributes one per element, so
  `[longitude, latitude]` maps a table of places and `[embedding]` maps whatever wrote
  a vector column, without the operator knowing which. A fixed-size array survives a
  Parquet round trip as a plain list, so `FLOAT[16]` and `FLOAT[]` are both accepted; a
  chained Protocol would otherwise refuse its own previous step's output. A column that
  is not a number is refused naming the column AND the type it found. A new `metric:`
  (`euclidean` or `cosine`) is how a Protocol says what distance between two rows
  means — `euclidean` by default, which is umap-learn's own and the right reading of an
  arbitrary feature matrix. Nothing scales your columns, deliberately: under euclidean
  a wider-spread column dominates the layout, and that decision belongs in the SQL step
  that selects them, where it is visible.

  `text_embed@1` writes vectors and nothing else, into a `FLOAT[]` column named by
  `vector_column:` (default `embedding`). It carries no projection knob, and
  `umap_project` carries no text column and no model — `deny_unknown_fields` makes a
  manifest that mixes them stop at load rather than silently ignore the field.
  **`text_embed` is marked PROVISIONAL in its own source and README, naming the DuckDB
  embedding extension as where the capability is going**: embedding is a table lookup
  and a mean, it has no business at the `uv` tier, and when the extension lands a
  Protocol embeds from a SQL step and the operator is deleted rather than ported. The
  same capability is currently implemented twice in two languages, which is a cost
  worth naming rather than tidying away.

  `embed_project` is GONE, not aliased — nothing outside arcform referenced it, so the
  rename is free today and would have been a breaking change the moment a Protocol
  depended on it. `op: embed_project@1` now fails manifest validation with the
  unknown-operator refusal, which is a better answer than an alias quietly resolving to
  one of the two halves. Both new names start at `@1`: neither is the old operator at a
  later version, because each does strictly less than it did.

  Everything the merged operator pinned is preserved. `op@1` still addresses exact
  script bytes; the projection's seed is still frozen in the script rather than exposed
  in `with:`; threads are still pinned before numpy/numba import and row order through
  an explicit ordinal, so two runs over the same input still emit byte-identical
  Parquet within one resolved environment. The model is still a DECLARED READ of kind
  Directory rather than a download — a node in the asset graph, hashed for staleness,
  and a model that is not on disk still stops the step non-retryably before `uv` is
  spawned, naming the file it looked for. Both operators are registered
  unconditionally, beside the other uv-run ops: arcform validates a manifest against
  the same catalog it executes from, so a feature gate named for the transport would
  force a consumer that only wants to READ a Protocol naming these steps to claim the
  capability to run them.

  `umap_project.py` imports duckdb, numpy and umap inside `main()` rather than at module
  scope, so the half of the script that DECIDES — the column-type classifier, the
  feature-width check, the SQL quoting — is importable with the standard library alone
  and is covered by `operators/umap_project/test_umap_project.py` in CI. The
  end-to-end tests need `uv` and skip on every runner here, so without that the script
  had no CI coverage at all.

- **`datapackage_describe` stamps `x-finetype-version` into every descriptor it
  writes** — the dotted version reported by the SAME `finetype` binary the step
  already resolves and runs to type the columns, so a descriptor names the engine
  that produced it and a stale one is visible by reading the file rather than by
  trusting whatever produced it. The stamp is written after the curated
  `descriptor.overrides.json` sidecar is merged in, so a sidecar cannot supply or
  overwrite it — the field is machine-derived, not hand-curated. A new optional
  `expect_finetype_version` on the operator's `with:` block (piped to describe.py's
  `--expect-finetype-version`) pins a run to one exact release: unlike the existing
  `--min-finetype-version` floor, which passes anything at or above it, a pin
  refuses a NEWER binary too if it is not the one asked for, naming both versions
  in the refusal. This replaces the shape of a per-dataset `stamp_finetype_version.py`
  script that recorded the same fact after the fact, outside the step that ran
  finetype — one change here now covers every descriptor the operator produces.
- **A `tool:` precondition declares the external binary or artifact a step depends on**
  — a fourth precondition kind, beside `modified_after`, `fresh` and `command`. A
  step's staleness hash covers what the manifest says and nothing about the machine it
  runs on, so a step whose output is decided by a binary somewhere on that machine is
  clean for ever while the binary moves underneath it. The declaration has two halves,
  each explicit: where the tool is (exactly one of `name:` on `PATH`, `path:`, or
  `env:` naming a variable that holds the path) and what identifies it (exactly one of
  `version:`, a shell command with `$ARC_TOOL` bound to the resolved path, or
  `contents: true`, the sha256 of the resolved file's bytes). Both shapes are needed:
  a released binary announces a version, while an artifact rebuilt in place keeps its
  path and its version and changes only its bytes. `arc` refuses at manifest load when
  either half is missing or given two ways.

  Same identity as the step's last successful run = skip, any difference = run, no
  prior identity = run. The identity is observed at plan time and recorded after the
  step succeeds — `evaluate` writes nothing, so asking whether a step is stale cannot
  change the answer, and a step un-skipped by this gate and then never reached runs
  again. Identities live in `build/.arcform/tools.json` beside the run records, so
  deleting `build/` forgets them and the step goes stale. A skip yields the new
  `precondition_tool` reason in the run contract, distinct from `hash_clean` and from
  `precondition_fresh`.

  **A tool that cannot be identified halts the run** — it is never "fresh", and not
  "stale" either. `modified_after` calls a missing file stale because the step itself
  produces that file, so running it is the remedy; nothing a step does creates the tool
  it was told to depend on. The run stops at plan time naming the step and the path the
  lookup reached, rather than surfacing later from whichever operator tripped over the
  absence.

- **`parquet_export` can stamp key-value metadata into the Parquet footer** — a new
  optional `metadata:` mapping on the operator's `with:` block, written through
  DuckDB's `KV_METADATA` copy option, so a dataset arcform writes can carry a
  description inside the file rather than only in a sidecar. Keys and values are
  strings written as their UTF-8 bytes; Parquet's footer map is untyped, so DuckDB
  reads them back as `BLOB` and `decode(key)` / `decode(value)` over
  `parquet_kv_metadata()` recovers the text — a `::VARCHAR` cast does not, it yields
  DuckDB's escaped rendering. The operator takes no view on what the keys mean.
  Entries are emitted in sorted key order (the config is a `BTreeMap`), because
  DuckDB writes the map into the footer in the order given and an unordered map
  would move the output bytes between runs.

  **Effect on output bytes, measured rather than assumed.** An export declaring no
  `metadata:` — or an empty map — emits no `KV_METADATA` option at all and is
  byte-identical to what the operator produced before, so no existing output moves.
  An export that does declare metadata necessarily changes the file and therefore
  its hash, but the change is confined to the footer: every data page is
  byte-identical, and the same stamp twice is the same file, so the `order_by`
  clause still buys the reproducibility it was added for. A publish step that pins
  the hash of a file that starts being stamped re-pins it once, not on every run.
  An empty map has to take the no-metadata path in any case: DuckDB rejects
  `KV_METADATA {}` as a syntax error.

- **Local history + machine-edit checkpoints** — the middle tier between editor undo
  and version control, and the tier that makes machine edits safe to accept. Saves
  record debounced, bounded snapshots of `arcform.yaml` under `~/.arcform/history`
  (override: `$ARCFORM_HISTORY_DIR`) — outside the protocol directory, so nothing
  appears in `git status` or a diff; at most 50 entries are kept per spec (oldest
  pruned first) and saves within 10 seconds of the newest save merge into it. Machine
  edits go through checkpointed roads — `edit_spec_with_history`,
  `record_step_with_history`, now used by `arc create-protocol` / `arc edit-protocol` —
  that snapshot the state being replaced *before* writing: no checkpoint, no write.
  New `arc history list|show|restore` verbs list the recorded states (with the
  retention policy printed under them), print an entry's exact bytes, and roll a spec
  back — restore checkpoints the state it replaces, so a rollback is itself
  reversible, and none of it needs a git repository or an account. Nothing is ever
  promoted to git: the only automatic promotion is undo → local history at the save
  boundary.
- **The record path** — `arc::spec` can now promote an exploration into a step:
  `record_step` writes the captured SQL as a new numbered model under `models/` (create
  mode, opened by a one-line `-- generated:` provenance marker) and splices a
  `name:` + `sql:` step onto the manifest through the same gated, byte-preserving write
  path — refusal-first, so a promotion that cannot apply leaves the directory untouched.
  The step name must record faithfully: it is spliced as a plain YAML scalar, so a name
  that would not read back as itself (newlines or other control characters, `#`, `:`,
  surrounding whitespace, a leading YAML indicator) is refused up front, and the
  reloaded document is checked to carry exactly the step that was asked for.
  `amend_step_sql` regenerates a model only when it carries the marker: the marker is
  the license to regenerate, and a hand-authored model is refused with the remedy in
  the reason (record a new step downstream instead). Recording never runs anything —
  a recorded step's outputs exist only after `arc run` says so, proven by a parity test
  that grows a spec purely by recording and executes it under the bare binary.
- **CLI authoring verbs** — `arc create-protocol` writes a fresh `arcform.yaml` from
  scratch and `arc edit-protocol` amends an existing one (`replace`, `rewrite`, `add`,
  `append`, `delete`, `reorder`, addressed as `steps[2].command`-style paths). Both are
  argument surface over the spec write path — the same splice, the same validation gate,
  the same atomic write the library gives every caller, so an agent can author and amend
  a runnable protocol with nothing but the binary in the loop. An edit that cannot apply,
  or whose result would not load, is refused with the reason before the file is touched;
  every untargeted byte is preserved verbatim, and no verb reformats as a side effect.
- **The spec write path** — `arc::spec` now edits specs as well as loading them:
  `SpecEdit` describes a change as an inspectable value (`Replace`, `RewriteFragment`,
  `Add`, `Append`, `Delete`, `Reorder`), `apply_edits` applies it to the original bytes
  and gates the result through the real loader in memory, and `edit_spec` is the
  one-shot apply → validate → atomic-write against a protocol directory. Edits splice
  raw bytes at tree-sitter node spans (via `yamlpath`), so every byte an edit does not
  target — comments, blank lines, key order, quote style — survives verbatim; the only
  normalisation is a final newline. A result that will not load is refused with the
  loader's reason and the file on disk is untouched; writes go through a temp file +
  rename, so an interrupt can never leave a truncated spec. Delete/reorder follow a
  documented comment-ownership convention (a `#` block flush against an item is that
  item's header and travels with it). `create_spec` covers the other mode: a brand-new
  spec serialises directly — no preservation machinery — through the same gate and the
  same atomic write. New corpus example `examples/almanac` exercises inline
  `command: |` block scalars, which the rest of the corpus lacked.
- **Execution resilience** — step `retry` with exponential backoff, plus step- and
  pipeline-level `timeout_sec`. Transient API failures retry; stuck processes are killed.
- **Pipeline parameterisation** — runtime `arc run --param KEY=VALUE` with manifest defaults,
  `dotenv` loading, and command-step output capture. Values reach shell steps as `ARC_PARAM_*`
  env vars and SQL via DuckDB `getenv()`; SQL passthrough is preserved.
- **Lifecycle hooks** — `on_init`, `on_success`, `on_failure`, and `on_exit` handlers for
  pipeline setup, teardown, and notification. Hook failures are non-fatal to the exit code.
- **Step preconditions** — typed freshness checks (`modified_after`, `command`) that skip a
  step when all pass.
- **Asset registry & SQL introspection** — SQL steps auto-discover their inputs/outputs via
  sqlparser-rs; command steps declare assets manually. Unparseable SQL degrades to an opaque
  step with a warning.
- **Run state tracking** — step hashes and run metadata persisted in the DuckDB database for
  staleness detection across runs.
- **Registry CLI scaffolding** — `arc registry list/show/fetch/run` command tree, index/resolver/
  cache/transport modules, and sandboxed tarball extraction. Production transport is not yet
  wired to a live index.
- **brewtrend reference pipeline** — the first complete runnable example under
  `examples/brewtrend/`, exercising command + SQL steps, preconditions, retries, and a runtime
  parameter; ships a Frictionless Data Package describing its output.
- **`umap_project` writes `projection_fit_id`** (1.1.0). There is no out-of-sample
  transform, so appending rows and re-running means the whole map is refit and every
  point can move — measured at 3,000 real rows (`eval/map-refit-stability/`): a 5%
  append already shares only 46% of a point's 20 nearest map-neighbours with the
  pre-append layout, worse than swapping the entire embedding model does on two of
  three text corpora in a sibling measurement of neighbourhood survival on the same
  kind of 2D map. `projection_fit_id` is a hash of the exact feature matrix and knobs
  (`neighbors`, `min_dist`, `metric`, seed) one fit consumed, the same value on every
  row of that fit's output — so a reader comparing two Parquets can tell whether they
  came from the same fit before trusting that a shared row's position means the same
  thing in both. See `operators/umap_project/README.md`, "Telling a refit from an
  append."

### Changed

- **`datapackage_describe` no longer runs Python.** A Frictionless Data Package is a
  specification, not a runtime: the operator used to wrap `describe.py` — a frozen
  script, run via `uv run --script`, that shelled to `finetype profile … -o
  datapackage` and merged the curated `descriptor.overrides.json` sidecar over it in
  a Python dict — and there was no library behind that script to justify the Python
  runtime it needed. The JSON merge (`merge_datapackage` + `check_relations`) is now
  native Rust over `serde_json::Value`; the machine-decidable half is UNCHANGED — the
  operator still shells the `finetype` CLI directly (no longer through `uv`/Python)
  and forms no opinion of its own about column types. Verified byte-identical to the
  retired path against the real Parquet + `descriptor.overrides.json` for all four
  published datasets (`edgar`, `naics`, `gleif`, `edgar_gleif`) — the only field that
  differs between the two is `created`, the per-run timestamp `finetype profile`
  itself stamps on every invocation, which was already non-deterministic before this
  change. Confirmed separately with no Python interpreter or `uv` anywhere on PATH
  (only `finetype` and the `duckdb` CLI every `arc run` already needs): the operator
  still describes the dataset; the retired uv-run substrate could not have. `serde_json`
  carries no `preserve_order` feature in this crate's own dependency edge (confirmed
  via `cargo tree`), so its `Map` is BTreeMap-backed and keys serialize sorted at
  every nesting level with no explicit sort step — matching Python's
  `json.dump(..., indent=2, sort_keys=True, ensure_ascii=False)` byte for byte.
  `operators/datapackage_describe/{describe.py,test_describe.py}` are deleted. The
  operator's version stays `1.0.0`: the `with:` contract and the produced bytes are
  unchanged, the same precedent set when `x-finetype-version` was added (also not a
  version bump). The `op@<version>` guarantee `materialize_frozen_script`'s
  write-if-changed cache gave the embedded script — a behaviour change is a rebuild,
  never a silent edit — now holds by a simpler mechanism: there is no separate script
  materialized at runtime anymore, so the operator's behaviour is entirely the
  compiled binary, exactly like every other in-process operator in this catalog
  (`parquet_export`, `archive_extract`, `finetype_validate`) that never needed
  `materialize_frozen_script` to make that same claim.
- **Raised the minimum `finetype` to 0.6.54 in both places that gate it.** finetype
  0.6.54 stopped eight-digit numbers typing as confident dates. Up to and including
  0.6.53, the year-first and day-first compact date leaves both validated on `^\d{8}$`,
  so any eight-digit token — a financial figure, a surrogate key — came back a
  high-confidence date *with a `strptime` transform attached*. That is a worse failure
  than the mislabelling the previous bump addressed: a consumer that follows the
  transform does not get a wrong name for a correct column, it gets a corrupted one. So
  0.6.53 is now refused as firmly as 0.6.52 was. Both gates name the superseded releases
  in one place — `SUPERSEDED_RELEASES` in the operator's tests and in the `arc mcp`
  module — and each is asserted refused *and* asserted to sit below the floor, so the
  constant cannot drift back onto either one unnoticed. Verified against the real 0.6.53
  binary, not only fixtures: the operator exits non-zero on it, and the `taxonomy` tool
  returns `isError` naming both versions, while the same binary reporting 0.6.54 passes
  and serves the call.
- **Raised the minimum `finetype` to 0.6.53 in both places that gate it.** The
  `datapackage_describe` operator and the `arc mcp` finetype proxies each shell out to
  whatever `finetype` is on PATH, and each asserted 0.6.52. But 0.6.53 is the release
  that corrected three labels the published datasets depend on — the ticker column, the
  industry-code level column, and the resolved legal-name column — and the consuming
  website has stopped suppressing the older, wrong labels at display time. An 0.6.52
  binary therefore passed the gate, re-emitted superseded labels, and nothing downstream
  caught it: precisely the stale-binary-on-PATH failure the gate exists to stop, which
  has silently mis-described a published dataset once already. The operator's tests now
  read `MIN_FINETYPE_VERSION` instead of restating a literal, and a new case in each
  language refuses an 0.6.52 fixture outright and asserts the floor sits above it, so
  the constant cannot drift back unnoticed. CI now runs the operator's Python tests —
  `cargo test` never executed them, so the gate had been shipping untested.
- **Feature-gated the HTTP ingress path behind `http-fetch`.** The ureq-backed
  `http_fetch` and `html_link_discover` operators, and the `fresh` precondition's remote
  HEAD probe, now compile only under the new `http-fetch` feature, which `cli` pulls in.
  A pipeline is only ever run through the `arc` CLI and the published library surface is
  `spec` alone, so a crate that links arc with `default-features = false` cannot reach
  this path — yet it previously still compiled `ureq` (and, as ureq's only runtime
  consumer, the whole rustls/ring TLS stack) into its binary as dead weight. With the
  gate, such a consumer drops `ureq` and its TLS stack from the dependency graph
  entirely; `ureq` is now an optional dependency. The default build and the `arc` binary
  are unchanged.
- **Adopted Frictionless Data Package as the data-description standard.** A Data Package
  *describes* data (Table Schema, `foreignKeys`, provenance); it does not execute — the runnable
  artifact stays the arcform manifest. Pipelines may ship a `datapackage.json` describing their
  IO.
- **Engine version assertion** — manifests may pin an `engine_version` semver constraint, checked
  at preflight for local/remote parity.
- **Vocabulary: "assets" not "asset registry"** for within-pipeline data declarations, freeing
  "registry" for the user-facing pipeline catalogue.

## [0.1.0] - 2026-03-31

### Added

- Initial release: the asset-centric step-execution foundation.
- `arc init` scaffolds a new project; `arc run` executes the pipeline in `arcform.yaml`.
- YAML manifest parsing and validation.
- Engine preflight — detects the DuckDB CLI before running.
- SQL passthrough — SQL files are handed to the engine unmodified.
- Shell command steps with real-time stdout streaming.
- Sequential step execution with per-step progress feedback.

[Unreleased]: https://github.com/meridian-online/arcform/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/meridian-online/arcform/releases/tag/v0.1.0
