//! ArcForm registry — discovery, fetch, and run for curated pipelines.
//!
//! # Vocabulary
//!
//! The term *asset* (singular) names a data output produced by a
//! pipeline step (see `crate::asset`). The term *registry* — owned by this module — names
//! the catalogue of working ArcForm pipelines that end users browse, fetch, and run via
//! `arc registry {list,show,fetch,run}`. The two concepts share no code or data; they are
//! distinct domains that historically collided on the word "registry".
//!
//! # Two-tier ownership
//!
//! Following the Docker Hub model, every registry entry is either:
//!
//! - **canonical** — `owner: null`, resolves unprefixed (`brewtrend`); or
//! - **contributor** — `owner: <name>`, resolves as `<owner>/<name>`
//!   (`someone/myproject`).
//!
//! Canonical names cannot be shadowed by contributors; the index parser hard-rejects
//! any contributor entry whose `name` collides with a canonical entry.
//!
//! # Transport contract
//!
//! Fetches go through the [`transport::Transport`] trait. The production implementation
//! [`transport::GitTarballTransport`] prefers `git clone --depth=1 --filter=blob:none --sparse`
//! and falls back to a sandboxed HTTPS tarball extractor when git is unavailable. Tests
//! use [`transport::FixtureTransport`] which copies from a local directory tree — no
//! shell-out, no network. **All transports honour an atomic-rename contract**: writes go
//! to a sibling temp directory and rename into the resolved-ref path on success; partial
//! writes leave no `<ref>/` directory.
//!
//! # Cache layout
//!
//! ```text
//! ~/.arcform/registry/
//!   index.yaml                       # cached index document
//!   index.yaml.fetched               # Unix epoch seconds of last fetch
//!   <name>/<resolved-ref>/           # canonical entries
//!   <owner>/<name>/<resolved-ref>/   # contributor entries
//! ```
//!
//! Cache root resolves from `$ARCFORM_REGISTRY_CACHE` if set, else
//! `~/.arcform/registry/` via the `dirs` crate. When neither is available, the registry
//! errors with [`crate::error::Error::RegistryCacheRootMissing`].
//!
//! # Sister work
//!
//! This module ships the **arcform-side CLI scaffolding** for the registry capability.
//! The companion `meridian-online/registry` monorepo (the hosted index plus the
//! canonical entries `brewtrend` and `gnaf`) is delivered as **sister work** — a
//! separate, later effort. Until that ships,
//! the production transport's git/HTTPS code paths are exercisable in development
//! but not covered by this crate's automated tests; the FixtureTransport satisfies
//! all integration coverage in this drive.
//!
//! The FRED entry (Investigative pillar)
//! is also deferred: it depends on secrets management. v1 ships two
//! entries (brewtrend in Practical, gnaf in Foundational) and an empty Investigative
//! pillar that renders with a `(no entries yet)` placeholder.

pub mod cache;
pub mod index;
pub mod resolve;
pub mod run;
pub mod transport;

// Re-exports: callers (e.g. `crate::cli`) can either go through `crate::registry::cache_root`
// or `crate::registry::cache::cache_root` — both are intentionally public surface.
#[allow(unused_imports)]
pub use cache::{
    HomeProvider, IndexCache, SystemHomeProvider, cache_path, cache_root, cache_root_with,
    ensure_cache_root,
};
#[allow(unused_imports)]
pub use index::{IndexEntry, Pillar, RegistryIndex};
#[allow(unused_imports)]
pub use resolve::{ResolvedEntry, VersionSpec, resolve};
#[allow(unused_imports)]
pub use run::{RunOptions, handle_fetch, handle_list, handle_run, handle_show};
#[cfg(test)]
#[allow(unused_imports)]
pub use transport::FixtureTransport;
#[allow(unused_imports)]
pub use transport::{GitTarballTransport, Transport, TransportSrc};
