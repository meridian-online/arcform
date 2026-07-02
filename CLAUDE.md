# Arcform

Local-first data pipeline engine for analytical workflows. Part of the [Meridian](https://meridian.online) project.

**Binary:** `arc` | **Version:** 0.1.0 | **Language:** Rust (edition 2024)

## Sprint goal

**Content before catalogue (cards 0017, 0023):** Re-anchored 2026-06-21. The registry CLI/index/transport (card 0022) is scaffolded, but a catalogue is the *distribution* layer and must follow *content*. Near-term focus:

- **M0 — decision 0017:** adopt Frictionless Data Package as meridian's data-*description* standard (Table Schema, `foreignKeys`, provenance). A Data Package describes data; it does **not** execute — the pipeline stays arcform yaml+sql. Aligns with dovetail choice 0002.
- **M1 — card 0023:** ship `brewtrend` as one real, runnable `examples/` pipeline (fetch → load → transform → analyse → export), shipping a `datapackage.json` that dogfoods decision 0017 and doubling as an integration fixture.

Catalogue path beyond M1: M2 (3–5 pipelines + settled entry convention) → M3 (fetch+run distribution, finishing card 0022's transport) → M4 (read-only web catalogue, reuse finetype's types-registry pattern) → M5 (contribution flywheel, decision 0016). A registry entry is a *pipeline*, not a Data Package.

## Design principles

1. **Asset-centric, not step-centric.** Nodes are data outputs; edges are data dependencies. The engine understands what data flows where, not just what commands run when. (decision 0001)
2. **Local-first.** Single binary, no cloud dependencies, no ops overhead. (decision 0006)
3. **Structurally transparent.** SQL steps are not black boxes — sqlparser-rs decomposes queries into load operations, CTE dependencies, and export targets. (decision 0007)
4. **Composable by design.** YAML manifests, discrete reusable steps, clear input/output contracts. (decision 0004)

## Decision register

Decisions live in a private planning repo: full MADR decision records plus one-line summaries. Vocabulary note (decision 0011): forward usage refers to **assets** (not "asset registry") for within-pipeline data declarations, freeing **registry** for the user-facing pipeline catalogue. Data standard (decision 0017): meridian describes data via Frictionless Data Package — a description format, not an execution format.

## Card roadmap

The engine roadmap lives in a private planning repo: arcform's own build track plus a Meridian-wide coordinating view. Shipped/active through 0017; planned from 0018 onward.

- **Shipped:** 0001–0011, 0014–0017 (scaffolding, step execution, manifest validation, preflight, SQL passthrough, shell commands, progress feedback, assets, SQL introspection, run state, local-remote parity, step preconditions, execution resilience, parameterisation, lifecycle hooks)
- **Sprint focus:** 0023 (reference pipeline — M1) on decision 0017 (M0)
- **Resequenced:** 0022 (registry) — scaffolding merged, distribution phase (M3+) deferred behind content
- **Planned:** 0012–0013, 0018–0021 (multi-engine dialects, lineage visualisation, parallel execution, typed executors, scheduling, secrets)

## Known issues

- **Tests don't link on this machine** — `cargo test` fails with `cannot find -lduckdb`. The DuckDB shared library is not on the linker search path. Tests compile and run where `libduckdb.so` is available.
- **Dead code warning** — `MockStateBackend::set_step_state` is unused (test helper).

## Tier-2 references — load on demand

**Before modifying the runner, manifest parser, SQL introspection, or asset dependency graph:** Read `docs/ARCHITECTURE.md`.

## Prose style — how to talk to the author

British English. Direct, warm, never chatty. Lead with the answer or the recommendation — don't restate the question, apologise, or bury the lede behind context. Recommendations are imperative and singular ("Run X on Y"), not a menu; if two paths genuinely matter, lay them out *and* pick one. Keep it short: name any load-bearing assumption inline rather than sanding the call into mush, and keep detail available on request instead of dumping it. Use plain words a peer outside the project would understand; define a term of art the first time you reach for it. No corporate hedging, no stacked qualifiers, no apologetic preambles.
