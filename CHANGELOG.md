# Changelog

All notable changes to arcform will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

Rationale for each change is recorded in the project's design notes and commit history.

## [Unreleased]

### Added

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
