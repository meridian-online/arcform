# Arcform

Local-first data pipeline engine for analytical workflows. Part of the [Meridian](https://meridian.online) project.

**Binary:** `arc` | **Version:** 0.1.0 | **Language:** Rust (edition 2024)

---

## Sprint Goal

**Registry (card 0022):** Ship an end-users-first registry of working ArcForm pipelines, organised across three pillars (Practical / Foundational / Investigative). v1 ships two canonical entries — `brewtrend` (Practical) and `gnaf` (Foundational) — fetched on demand via `arc registry list/show/fetch/run` from a hosted index. `fred` (Investigative) deferred to a follow-up spec gated on card 0021 (secrets management); the Investigative pillar exists in the index from v1 but is empty. Distribution: monorepo (`meridian-online/registry`) + git fetch with tarball fallback. Architecture supports two-tier ownership (canonical + contributor) from v1.

Delivery: rally coordinating two cards — card 0008 (assets rename) sequenced first, card 0022 (registry capability) second.

---

## Design Principles

1. **Asset-centric, not step-centric.** Nodes are data outputs; edges are data dependencies. The engine understands what data flows where, not just what commands run when. (decision 0001)
2. **Local-first.** Single binary, no cloud dependencies, no ops overhead. (decision 0006)
3. **Structurally transparent.** SQL steps are not black boxes — sqlparser-rs decomposes queries into load operations, CTE dependencies, and export targets. (decision 0007)
4. **Composable by design.** YAML manifests, discrete reusable steps, clear input/output contracts. (decision 0004)

---

## Architecture

### Source layout

```
src/
  main.rs         # Entry point
  cli.rs          # Clap CLI definition
  manifest.rs     # YAML manifest parsing (arcform.yaml)
  runner.rs       # Step execution engine (~1150 lines, largest module)
  engine.rs       # SQL engine invocation (DuckDB CLI delegation)
  asset.rs        # Assets, SQL auto-discovery, dependency validation
  introspect.rs   # SQL introspection via sqlparser-rs
  precondition.rs # Typed step preconditions (modified_after, command)
  state.rs        # Run state tracking (step hashes, staleness)
  error.rs        # Error types
```

### Key types

- **`Manifest`** — top-level project config loaded from `arcform.yaml`. Contains `name`, `engine`, `engine_version`, `db`, `steps`, `assets`.
- **`Step`** — a pipeline step. Either `sql` (path to .sql file) or `command` (shell string). Has `produces`, `depends_on`, `preconditions`.
- **`Precondition`** — typed freshness check. Variants: `modified_after` (file age), `command` (shell exit code). AND semantics — all must pass to skip.
- **`AssetOverride`** — manual asset dependency declaration (for command steps; SQL steps auto-discover).

### Execution model

1. Load `arcform.yaml` manifest
2. Build asset dependency graph (SQL introspection + manual overrides)
3. For each step in order:
   - Evaluate preconditions (if any) + SQL hash staleness
   - Skip if fresh; execute if stale
   - SQL steps: delegate to engine CLI (`duckdb -bail`)
   - Command steps: shell execution with real-time stdout streaming
4. Update run state after each step

### Dependencies

- **clap** — CLI argument parsing
- **serde / serde_yaml** — manifest (de)serialization
- **sqlparser** — SQL introspection (CTE/table extraction)
- **duckdb** — DuckDB Rust bindings (state backend)
- **semver** — engine version constraint checking
- **sha2** — SQL content hashing for staleness detection
- **humantime** — duration parsing for preconditions

---

## Decision Register

16 decisions in `orbit/decisions/` (MADR format):

| # | Decision |
|---|---|
| 0001 | Asset-centric pipeline engine (not step-centric) |
| 0002 | Implement in Rust |
| 0003 | Delegate SQL execution to engine CLIs |
| 0004 | YAML manifest with steps and separate asset declarations |
| 0005 | DuckDB as default engine |
| 0006 | Local-first, remote-compatible design |
| 0007 | SQL introspection via sqlparser-rs is a core feature |
| 0008 | CLI binary name is `arc` |
| 0009 | Hybrid engine invocation (defaults + command override) |
| 0010 | v0.1 scope: step execution foundation |
| 0011 | Pipeline catalogue takes "registry"; rename "asset registry" → "assets" |
| 0012 | Registry is end-users-first |
| 0013 | Registry uses a hosted index with on-demand fetch |
| 0014 | Registry distribution via monorepo + git fetch (tarball fallback) |
| 0015 | Registry organised around three pillars: Practical / Foundational / Investigative |
| 0016 | Registry supports two-tier ownership (canonical + contributor) |

**Vocabulary note (decision 0011):** forward usage refers to **assets** (not "asset registry") for within-pipeline data declarations, freeing **registry** for the user-facing pipeline catalogue. Historical artefacts (card 0008, spec `2026-04-15-asset-registry/`) keep their original names for traceability.

---

## Card Roadmap

22 cards in `orbit/cards/`. Shipped/active through 0017; planned from 0018 onward.

**Shipped:** 0001–0011, 0014–0017 (scaffolding, step execution, manifest validation, preflight, SQL passthrough, shell commands, progress feedback, assets, SQL introspection, run state, local-remote parity, step preconditions, execution resilience, parameterisation, lifecycle hooks)
**Sprint focus:** 0022 (registry)
**Planned:** 0012–0013, 0018–0021 (multi-engine dialects, lineage visualisation, parallel execution, typed executors, scheduling, secrets)

---

## Known Issues

- **Tests don't link on this machine** — `cargo test` fails with `cannot find -lduckdb`. The DuckDB shared library is not on the linker search path. Tests compile and run where `libduckdb.so` is available.
- **Dead code warning** — `MockStateBackend::set_step_state` is unused (test helper).

---

## Conventions

- Manifest filename: `arcform.yaml`
- SQL files live alongside the manifest, referenced by relative path
- Engine delegation: SQL runs via `duckdb -bail <db> < file.sql`
- State is persisted in the DuckDB database itself (run metadata tables)

---

## Workflow (orbit)

This project uses the orbit workflow: Card → Design → Spec → Implement → Review → Ship.

- `/orb:card` — capture a feature need with expected behaviours
- `/orb:distill` — extract capability cards from source material
- `/orb:discovery` — explore a vague idea through Socratic Q&A
- `/orb:design` — refine a feature card into technical decisions
- `/orb:spec` — crystallise interview into a structured specification
- `/orb:review-spec` — stress-test the spec before implementation
- `/orb:review-pr` — verify the PR against the spec's acceptance criteria

## Orbit vocabulary

- **Card** (`orbit/cards/*.yaml`) — a capability the product provides. User language. Never closed.
- **Memo** (`orbit/cards/memos/*.md`) — raw idea awaiting distillation.
- **Interview** (`orbit/specs/<slug>/interview.md`) — Q&A record from `/design` or `/discovery`.
- **Spec** (`orbit/specs/<slug>/spec.yaml`) — a discrete unit of work with numbered ACs.
- **Progress** (`orbit/specs/<slug>/progress.md`) — AC tracker during implementation.
- **Decision** (`orbit/decisions/*.md`) — MADR record of an architectural choice.

Cards describe *what*, specs describe *work*. Follow-up work is a new spec against an existing card — not a new card. New cards are for new capabilities.

## Current Sprint

goal: "Build the ArcForm registry — end-users-first catalogue of working pipelines across three pillars (Practical / Foundational / Investigative), distributed via hosted monorepo + on-demand fetch"

cards:
  - 0008: "Assets rename — 'asset registry' → 'asset' in forward usage (sequenced first)"
  - 0022: "Registry — CLI + monorepo + brewtrend (Practical) + gnaf (Foundational); fred deferred"
