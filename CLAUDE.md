# Arcform

Local-first data pipeline engine for analytical workflows. Part of the [Meridian](https://meridian.online) project.

**Binary:** `arc` | **Version:** 0.1.0 | **Language:** Rust (edition 2024)

---

## Sprint Goal

**Reference pipelines (card 0022):** Ship realistic example pipelines (starting with brewtrend) that serve as both learning material and integration test fixtures. Each major arcform capability (SQL, commands, preconditions, dependencies) should have at least one reference pipeline demonstrating it.

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
  asset.rs        # Asset registry, SQL auto-discovery, dependency validation
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

10 decisions in `orbit/decisions/` (MADR format):

| # | Decision |
|---|---|
| 0001 | Asset-centric pipeline engine (not step-centric) |
| 0002 | Implement in Rust |
| 0003 | Delegate SQL execution to engine CLIs |
| 0004 | YAML manifest with steps and separate asset registry |
| 0005 | DuckDB as default engine |
| 0006 | Local-first, remote-compatible design |
| 0007 | SQL introspection via sqlparser-rs is a core feature |
| 0008 | CLI binary name is `arc` |
| 0009 | Hybrid engine invocation (defaults + command override) |
| 0010 | v0.1 scope: step execution foundation |

---

## Card Roadmap

22 cards in `orbit/cards/`. Shipped/active through 0014; planned from 0015 onward.

**Shipped:** 0001–0011, 0014 (scaffolding, step execution, manifest validation, preflight, SQL passthrough, shell commands, progress feedback, asset registry, SQL introspection, run state, local-remote parity, step preconditions)
**Sprint focus:** 0022 (reference pipelines)
**Planned:** 0012–0013, 0015–0021 (multi-engine dialects, lineage visualisation, execution resilience, parameterisation, lifecycle hooks, parallel execution, typed executors, scheduling, secrets, reference pipelines)

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

Artefacts live in `orbit/cards/`, `orbit/specs/`, and `orbit/decisions/`.

## Current Sprint

goal: "Build reference pipelines — ship realistic example pipelines that exercise all major arcform capabilities"

cards:
  - 0015: "Execution resilience — retry, backoff, timeouts"
  - 0016: "Pipeline parameterisation — params, dotenv, output capture"
  - 0017: "Lifecycle hooks — init, success, failure, exit handlers"
  - 0022: "Reference pipelines — brewtrend and others"
