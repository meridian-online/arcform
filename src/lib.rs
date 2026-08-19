//! Arcform — a local-first data pipeline engine.
//!
//! This crate is a binary first: `arc` runs pipelines. The library target exists for
//! one reason — a sibling tool that edits the same `arcform.yaml` must not carry its
//! own copy of the schema. Two schemas drift; one schema cannot. So the spec loader
//! and its validation are published here, and everything else stays private.
//!
//! **The published surface is [`spec`].** Read it for the contract, including what it
//! guarantees, what it deliberately withholds, and why a consumer should pin an exact
//! version.
//!
//! One other item exists at the crate root, and it is not published: [`cli_main`], the
//! `arc` binary's entry point. It is compiled only under the `cli` feature, which is on
//! by default so that `cargo build` produces the binary. **Link this crate with
//! `default-features = false`** and the crate root exports [`spec`] and nothing else —
//! which is what a library consumer wants, since [`cli_main`] parses the *caller's*
//! `argv` and can terminate the caller's process.
//!
//! ```toml
//! [dependencies]
//! arc = { version = "=0.1.0", default-features = false }
//! ```
//!
//! The engine itself — the runner, the DuckDB bridge, SQL introspection, the asset
//! graph, the operator catalog, the run contract, the registry client, the state
//! backend, the CLI — is private and is expected to change without notice. If you
//! need something from behind that line, ask for it to be added to [`spec`] rather
//! than reaching around; an item that is exported is an item this crate has to keep.

// Without the CLI the engine has no entry point, so every module behind the private
// line is dead by construction — `cargo build --no-default-features` would otherwise
// emit ~200 dead-code warnings for code that is doing exactly what it should. The
// default build still lints the whole tree.
#![cfg_attr(not(feature = "cli"), allow(dead_code))]

mod asset;
mod asset_kind;
mod bridge;
#[cfg(feature = "cli")]
mod cli;
mod contract;
mod edit;
mod engine;
mod error;
mod fetch_cache;
mod history;
mod ingress_meta;
mod introspect;
mod manifest;
#[cfg(feature = "mcp")]
mod mcp;
mod operator;
mod precondition;
mod record;
mod registry;
mod runner;
mod state;

pub mod spec;

/// The `arc` binary's entry point.
///
/// **Not part of the published contract**, and gated behind the `cli` feature so a
/// library consumer can compile it out entirely. It exists so `src/main.rs` can be a
/// three-line shim over this crate rather than a second compilation of the whole
/// tree.
///
/// Calling it is not a library operation: it parses **the calling process's**
/// `std::env::args` with `clap`, so an unrecognised argument prints `arc` usage to
/// stderr and exits; on a dispatch error it prints to stderr and calls
/// `std::process::exit`, with the code `Error::exit_code` gives that error — 1 for
/// "cannot run", 2 for "found a problem" (see that method's doc for the split).
/// Nothing but the binary should call it, and it may change or disappear at any time.
#[cfg(feature = "cli")]
#[doc(hidden)]
pub fn cli_main() {
    use clap::Parser;

    let cli = cli::Cli::parse();

    if let Err(e) = cli::dispatch(cli) {
        eprintln!("error: {}", e);
        std::process::exit(e.exit_code());
    }
}
