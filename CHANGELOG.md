# Changelog

All notable changes to arcform will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

Rationale for each change is recorded in the project's design notes and commit history.

## [Unreleased]

### Added

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

### Changed

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
