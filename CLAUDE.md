# Arcform

Local-first data pipeline engine for analytical workflows. Part of the [Meridian](https://meridian.online) project.

**Binary:** `arc` | **Version:** 0.1.0 | **Language:** Rust (edition 2024)

## Sprint goal

**Content before catalogue.** Re-anchored 2026-06-21. The registry CLI/index/transport is scaffolded, but a catalogue is the *distribution* layer and must follow *content*. Near-term focus:

- **M0:** adopt Frictionless Data Package as meridian's data-*description* standard (Table Schema, `foreignKeys`, provenance). A Data Package describes data; it does **not** execute — the pipeline stays arcform yaml+sql. Aligns with the dovetail modelling layer.
- **M1:** ship `brewtrend` as one real, runnable `examples/` pipeline (fetch → load → transform → analyse → export), shipping a `datapackage.json` that dogfoods the Data Package standard and doubling as an integration fixture.

Catalogue path beyond M1: M2 (3–5 pipelines + settled entry convention) → M3 (fetch+run distribution, finishing the registry transport) → M4 (read-only web catalogue, reuse finetype's types-registry pattern) → M5 (contribution flywheel). A registry entry is a *pipeline*, not a Data Package.

## Design principles

1. **Asset-centric, not step-centric.** Nodes are data outputs; edges are data dependencies. The engine understands what data flows where, not just what commands run when.
2. **Local-first.** Single binary, no cloud dependencies, no ops overhead.
3. **Structurally transparent.** SQL steps are not black boxes — sqlparser-rs decomposes queries into load operations, CTE dependencies, and export targets.
4. **Composable by design.** YAML manifests, discrete reusable steps, clear input/output contracts.

## Decisions

Design decisions and their rationale live in the project's private planning repo. Two vocabulary/standard notes worth stating inline: *assets* (not "asset registry") names within-pipeline data declarations, freeing *registry* for the user-facing pipeline catalogue; and meridian describes data via Frictionless Data Package — a description format, not an execution format.

## Roadmap

The build track is maintained in the project's private planning repo. In plain terms:

- **Shipped:** scaffolding, step execution, manifest validation, engine preflight, SQL passthrough, shell commands, progress feedback, assets, SQL introspection, run state, local-remote parity, step preconditions, execution resilience, parameterisation, lifecycle hooks.
- **Sprint focus:** the `brewtrend` reference pipeline (M1) on the Data Package standard (M0).
- **Resequenced:** the registry — scaffolding merged; the distribution phase (M3+) is deferred behind content.
- **Planned:** multi-engine dialects, lineage visualisation, parallel execution, typed executors, scheduling, secrets.

## Known issues

- **Tests need libduckdb to link** — where the DuckDB shared library is not on the linker search path, `cargo test` fails with `cannot find -lduckdb`. Point the linker at it (for example `DUCKDB_LIB_DIR`) and the suite compiles and runs.

## Tier-2 references — load on demand

**Before modifying the runner, manifest parser, SQL introspection, or asset dependency graph:** Read `docs/ARCHITECTURE.md`.

## Prose style — how to talk to the author

British English. Direct, warm, never chatty. Lead with the answer or the recommendation — don't restate the question, apologise, or bury the lede behind context. Recommendations are imperative and singular ("Run X on Y"), not a menu; if two paths genuinely matter, lay them out *and* pick one. Keep it short: name any load-bearing assumption inline rather than sanding the call into mush, and keep detail available on request instead of dumping it. Use plain words a peer outside the project would understand; define a term of art the first time you reach for it. No corporate hedging, no stacked qualifiers, no apologetic preambles.
