# Arcform

Local-first data pipeline engine for analytical workflows. Part of the [Meridian](https://meridian.online) project.

**Binary:** `arc` | **Version:** 0.1.0 | **Language:** Rust (edition 2024)

## Sprint goal

**Registry (card 0022):** Ship an end-users-first registry of working ArcForm pipelines, organised across three pillars (Practical / Foundational / Investigative). v1 ships two canonical entries — `brewtrend` (Practical) and `gnaf` (Foundational) — fetched on demand via `arc registry list/show/fetch/run` from a hosted index. `fred` (Investigative) deferred to a follow-up spec gated on card 0021 (secrets management). Distribution: monorepo (`meridian-online/registry`) + git fetch with tarball fallback. Architecture supports two-tier ownership (canonical + contributor) from v1.

Delivery: rally coordinating two cards — card 0008 (assets rename) sequenced first, card 0022 (registry capability) second.

## Design principles

1. **Asset-centric, not step-centric.** Nodes are data outputs; edges are data dependencies. The engine understands what data flows where, not just what commands run when. (decision 0001)
2. **Local-first.** Single binary, no cloud dependencies, no ops overhead. (decision 0006)
3. **Structurally transparent.** SQL steps are not black boxes — sqlparser-rs decomposes queries into load operations, CTE dependencies, and export targets. (decision 0007)
4. **Composable by design.** YAML manifests, discrete reusable steps, clear input/output contracts. (decision 0004)

## Decision register

16 decisions in `orbit/decisions/` (MADR format). Browse: `ls orbit/decisions/`. Vocabulary note (decision 0011): forward usage refers to **assets** (not "asset registry") for within-pipeline data declarations, freeing **registry** for the user-facing pipeline catalogue.

## Card roadmap

22 cards in `orbit/cards/`. Shipped/active through 0017; planned from 0018 onward.

- **Shipped:** 0001–0011, 0014–0017 (scaffolding, step execution, manifest validation, preflight, SQL passthrough, shell commands, progress feedback, assets, SQL introspection, run state, local-remote parity, step preconditions, execution resilience, parameterisation, lifecycle hooks)
- **Sprint focus:** 0022 (registry)
- **Planned:** 0012–0013, 0018–0021 (multi-engine dialects, lineage visualisation, parallel execution, typed executors, scheduling, secrets)

## Known issues

- **Tests don't link on this machine** — `cargo test` fails with `cannot find -lduckdb`. The DuckDB shared library is not on the linker search path. Tests compile and run where `libduckdb.so` is available.
- **Dead code warning** — `MockStateBackend::set_step_state` is unused (test helper).

## Tier-2 references — load on demand

**Before modifying the runner, manifest parser, SQL introspection, or asset dependency graph:** Read `docs/ARCHITECTURE.md`.
