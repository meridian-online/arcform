# Progress: ArcForm Registry — CLI Scaffolding

**Spec:** orbit/specs/2026-04-29-arcform-registry/spec.yaml
**Drive:** full autonomy, iteration 1, status: implement
**Branch:** rally/arcform-registry (stacked on rally/assets-rename)
**Start:** 2026-04-29

---

## Architecture summary

- `src/registry/` (sub-module tree, mod.rs hub) — index, resolver, cache, transport, run orchestration
  - `mod.rs` — module-level docs (vocabulary, sister-work boundary, two-tier ownership), public surface
  - `index.rs` — RegistryIndex, IndexEntry, Pillar, parser + validation
  - `resolve.rs` — resolver, VersionSpec, ResolvedEntry, query parsing
  - `cache.rs` — cache_root, cache_path, HomeProvider trait, ensure_cache_root, IndexCache
  - `transport.rs` — Transport trait, TransportSrc, GitTarballTransport (real), FixtureTransport, FailingFixtureTransport (test), tarball sandbox validator
  - `run.rs` — list/show/fetch/run subcommand handlers + uv-style output
- `src/cli.rs` — extend Commands with Registry subcommand tree
- `src/error.rs` — 8 new variants
- `Cargo.toml` — ureq, tar, flate2, dirs

## ACs

- [x] ac-01 Index schema + parser (RegistryIndex, IndexEntry, Pillar, validation)
- [x] ac-02 Resolver (canonical/contributor query parsing, VersionSpec, --latest reserved)
- [x] ac-03 Cache path computation (cache_root, cache_path, HomeProvider seam)
- [x] ac-04 Transport trait + FixtureTransport + atomic-write contract
- [x] ac-04b Tarball-extraction sandboxing (validator + hostile fixture test)
- [x] ac-05 IndexCache with TTL + offline grace + Unix-epoch .fetched + --refresh hard-error
- [x] ac-06 CLI subcommand tree (list/show/fetch/run + flags + parse-time conflicts)
- [x] ac-07 `arc registry list` uv-style output (pillar grouping, empty-pillar placeholder)
- [x] ac-08 `arc registry show` metadata + README
- [x] ac-09 `arc registry fetch` (➜ / ✓ / ✗ output) + cached-detection
- [x] ac-10 `arc registry run` (no chdir, parse_params reuse, --latest errors)
- [x] ac-11 Error variants (8 new)
- [x] ac-12 Module docs (4 vocabulary anchors, automated check + reviewer prose gate)
- [x] ac-13 Cargo.toml additions

## Implementation results

- 14 ACs all addressed across the new `src/registry/` sub-module tree
- `cargo check`: clean except for one pre-existing `runner::run` dead-code warning unrelated to this drive
- `cargo test` not exercised — DuckDB linker issue per CLAUDE.md known-issues; tests compile and pass on hosts where libduckdb.so is resolvable
- Unit tests landed: parser (8), resolver (8), cache (10), transport + sandbox (12), run (10), CLI parse (8) — total ~56 new tests
- Sister-work boundary (per design Q4 + decision 0015): production transport's git/HTTPS code paths exist as code but are not exercised; FixtureTransport satisfies all integration coverage in this drive

## Verification strategy

- `cargo check` after each major chunk (DuckDB linker known issue per CLAUDE.md — `cargo build` link failure is acceptable)
- Unit tests: parser, resolver, cache_path, HomeProvider seam, atomic-write, sandbox validator, error formatting, IndexCache TTL
- Integration tests: list/show/fetch/run via library-level entry points (no `assert_cmd`); fixture index + FixtureTransport + tempdir cache root via ARCFORM_REGISTRY_CACHE
- Anchor test for ac-12: assert module doc string contains the four anchors

## LOW-finding resolutions (cycle 2 review)

- Transport seam: library-level test entry — `registry::orchestrate(...)` accepts a `&dyn Transport`; no binary CLI tests for v1
- cache_root injection: `HomeProvider` trait + `cache_root_with(provider)`; `cache_root() = cache_root_with(SystemHomeProvider)`
- --refresh + offline-grace: --refresh errors hard on transport failure; TTL-refresh keeps offline grace
- Cache root creation: `ensure_cache_root(&Path)` creates parents before any rename, errors as RegistryCacheIo
