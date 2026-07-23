# Changelog

All notable changes to arcform will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

Rationale for each change is recorded in the project's design notes and commit history.

## [Unreleased]

### Added

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
