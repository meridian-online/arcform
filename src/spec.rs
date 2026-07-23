//! The published spec contract — the whole public surface of the `arc` library.
//!
//! # What this is for
//!
//! An `arcform.yaml` is hand-authored, hand-edited, and machine-edited. A tool that
//! edits one has to know whether the result is still a valid spec, and the only
//! answer that means anything is the one `arc` itself would give. Reimplementing the
//! schema in the editor produces two schemas that agree until the day they don't.
//! So the loader is published, and callers link it.
//!
//! Validation here is a **gate, not a transform**. [`Manifest::from_yaml_str`] parses
//! raw YAML solely to decide whether it is valid; it never gives you bytes back to
//! write. An editor that wants to preserve comments, key order and formatting should
//! edit the original bytes, pass them through this gate, and — if the gate passes —
//! write the bytes it already had.
//!
//! That discipline is packaged as **the write path**: [`SpecEdit`] describes a
//! change as a plain value, [`apply_edits`] splices it into the original bytes and
//! gates the result (in memory — inspectable and refusable before any file is
//! touched), and [`edit_spec`] is the whole apply → validate → atomic-write
//! operation against a protocol directory. Every byte an edit does not target is
//! preserved verbatim; a result that will not load is refused with the loader's
//! reason, never written. [`create_spec`] is the other mode: a spec authored from
//! scratch has no prior authorship to protect, so it serialises a [`Manifest`]
//! directly — through the same gate and the same atomic write.
//!
//! **Do not round-trip a spec through `serde` yourself.** The exported types derive
//! `serde::Serialize` because generated-manifest emission ([`create_spec`], and
//! `arc`'s own `arc init` scaffolding) serialises into a place that has no manifest
//! yet — the one case where there are no author bytes to lose. That derive is **not
//! part of this contract** for any other purpose: load-serialise-write-back destroys
//! the author's comments, key order and blank lines, and [`Manifest::from_yaml_str`]
//! would bless the result, because the mangled bytes are still a valid spec. Editing
//! an existing spec goes through [`edit_spec`], which exists so that no consumer
//! ever has a reason to re-serialise one.
//!
//! What you *can* do is build a [`Manifest`] in memory: every field is `pub`, because
//! the exported types are the schema and a consumer has to be able to read all of it.
//! Nothing stops you assembling one with a struct literal — [`create_spec`] takes
//! exactly that and turns it into a validated file.
//!
//! # The contract
//!
//! Exported types (the spec schema, exactly as `arc` parses it):
//!
//! - [`Manifest`] — the top-level `arcform.yaml`
//! - [`Step`] — one pipeline step
//! - [`Param`] — a declared runtime parameter
//! - [`Defaults`] — manifest-level step defaults
//! - [`RetryPolicy`] — retry/backoff configuration
//! - [`Hooks`] — the lifecycle hook block
//! - [`AssetOverride`] — an entry in the top-level `assets:` block
//! - [`Precondition`], [`ModifiedAfterConfig`], [`FreshConfig`] — step freshness checks
//!
//! Exported entry points:
//!
//! - [`Manifest::load`] — read and validate `arcform.yaml` from a directory
//! - [`Manifest::from_yaml_str`] — validate YAML text that is not (yet) on disk
//! - [`Precondition::validate`] — the well-formedness check `load` applies per step
//! - [`MANIFEST_FILENAME`] — the filename [`Manifest::load`] looks for
//!
//! The write path (see the discussion above):
//!
//! - [`SpecEdit`], [`PathPart`] — an edit as an inspectable value, and its address
//! - [`apply_edits`] — apply + validate in memory; returns a [`ValidatedSpec`]
//! - [`ValidatedSpec`] — gated candidate bytes; [`ValidatedSpec::write_to`] is atomic
//! - [`edit_spec`] — the one-shot apply → validate → atomic-write against a directory
//! - [`create_spec`] — direct serialisation for a brand-new spec, same gate
//!
//! The record path (the [`record`](crate::record) module's docs carry the full
//! discussion — recording never runs; the marker is the license to regenerate):
//!
//! - [`RecordedStep`] — an exploration ready to become a step, as a plain value
//! - [`record_step`] — new numbered model under `models/` + step splice, refusal-first
//! - [`amend_step_sql`] — rewrite a *generated* model; hand-authored files are refused
//! - [`GENERATED_MARKER`], [`sql_is_generated`] — the ownership line, readable by anyone
//!
//! Exported error types:
//!
//! - [`Error`] — `#[non_exhaustive]`; new variants are not a breaking change
//! - [`Result`] — `std::result::Result<T, Error>`
//!
//! That is the list. `tests/public_surface.rs` enumerates it and fails if it grows.
//!
//! # What is deliberately not here
//!
//! Execution. There is no way through this surface to run a pipeline, open a DuckDB
//! connection, resolve a database path, introspect SQL, evaluate a precondition,
//! build an asset graph, reach the operator catalog, read the run contract, or touch
//! the registry. Those are the engine's business and they move; the spec does not.
//!
//! The crate root carries one further item, `cli_main`, the `arc` binary's entry
//! point. It is `#[doc(hidden)]` and gated behind the default `cli` feature, and it
//! is not a library function: it parses the **calling process's** `argv` and can end
//! the calling process. Depend on `arc` with `default-features = false` and it does
//! not exist at all — which also drops `clap` and the command dispatch from the
//! build:
//!
//! ```toml
//! [dependencies]
//! arc = { version = "=0.1.0", default-features = false }
//! ```
//!
//! # Stability
//!
//! **Pin an exact version.** `arc` is pre-1.0 and the spec schema is still growing;
//! treat every version bump as potentially breaking and move deliberately. A
//! consumer inside the same organisation should pin a git revision; one outside
//! should pin `version = "=x.y.z"`. The guarantee offered is narrow and real:
//! within a version, what is listed above is what exists, it behaves as `arc`
//! itself behaves, and nothing else is reachable.
//!
//! One wart to know about: [`Step::with`] — the typed config block of an `op:` step —
//! is a `serde_yaml::Value`, so inspecting it means depending on `serde_yaml` 0.9.
//! That crate is unmaintained. It is load-bearing for `arc`'s own parsing today, and
//! replacing it is a change to how specs parse, not a change to this surface, so it
//! is recorded here rather than papered over.
//!
//! Two build-time facts a consumer inherits, recorded here because they bite at
//! resolve or link time rather than in this API:
//!
//! - `arc` parses SQL with its **vendored `sqlparser` fork** (see `vendor/`), wired
//!   in with a `[patch.crates-io]` entry — and Cargo patch tables do not propagate
//!   through dependencies. The workspace that links this crate must carry the same
//!   patch entry, pointing at the `vendor/sqlparser-0.55.0` package inside the same
//!   pinned `arc` source; without it the build fails on missing
//!   `SetExpr::Pivot`/`SetExpr::Unpivot` variants.
//! - The private engine still compiles into the library, so the **DuckDB C library**
//!   must be present to build (`DUCKDB_LIB_DIR`/`DUCKDB_INCLUDE_DIR`, or a system
//!   install the linker finds), even for a consumer that only loads specs.
//!   `default-features = false` drops `clap`, not the engine's dependency graph.
//!
//! # Example
//!
//! ```no_run
//! use arc::spec::{Manifest, Result, SpecEdit, edit_spec};
//!
//! # fn main() -> Result<()> {
//! // Load and validate a spec that is on disk.
//! let dir = std::path::Path::new("examples/brewtrend");
//! let manifest = Manifest::load(dir)?;
//! println!("{} — {} steps", manifest.name, manifest.steps.len());
//!
//! // Edit it through the write path: the change is applied to the original
//! // bytes, validated by the same loader, and written atomically — or refused
//! // with a reason, leaving the file untouched.
//! let edit = SpecEdit::Replace {
//!     path: vec!["params".into(), "trend_threshold".into(), "default".into()],
//!     value: "\"25\"".into(),
//! };
//! edit_spec(dir, &[edit])?;
//! # Ok(())
//! # }
//! ```

pub use crate::edit::{PathPart, SpecEdit, ValidatedSpec, apply_edits, create_spec, edit_spec};
pub use crate::error::{Error, Result};
pub use crate::manifest::{
    AssetOverride, Defaults, Hooks, MANIFEST_FILENAME, Manifest, Param, RetryPolicy, Step,
};
pub use crate::precondition::{FreshConfig, ModifiedAfterConfig, Precondition};
pub use crate::record::{
    GENERATED_MARKER, RecordedStep, amend_step_sql, record_step, sql_is_generated,
};
