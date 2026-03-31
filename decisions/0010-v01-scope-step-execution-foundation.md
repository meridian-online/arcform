---
status: accepted
date-created: 2026-03-31
date-modified: 2026-03-31
---
# 0010. v0.1 Scope: Step Execution Foundation

## Context and Problem Statement

ArcForm's full vision includes assets, SQL introspection, and selective re-materialisation. What ships first? The step execution layer is the foundation all other features build on.

## Considered Options

- **Steps + assets + sqlparser** — ship the full asset model from day one
- **Steps only** — parse manifest, execute steps sequentially against DuckDB. Foundation for everything else.
- **Steps + sqlparser** — add SQL parsing without the asset registry

## Decision Outcome

Chosen option: "Steps only", because shipping the loop before optimising it is a core principle. v0.1 delivers:

- `arc init` — scaffold project (`arcform.yaml`, `models/`, `sources/`)
- `arc run` — parse manifest, preflight engine, execute steps sequentially via DuckDB CLI
- Hybrid steps: `sql:` (engine-aware) and `command:` (raw shell)
- Halt-on-failure, clear error messages, engine preflight

This is the execution foundation. The asset registry (v0.2) and SQL introspection (v0.2/v0.3) layer on top.

### Roadmap

- **v0.1** — Step execution: manifest parsing, sequential execution, DuckDB CLI delegation
- **v0.2** — Asset registry + sqlparser: declare assets, auto-infer SQL dependencies
- **v0.3** — Selective re-materialisation: staleness tracking, partial re-runs
- **Future** — Typed step promotion, preflight version assertions, remote scheduling

### Consequences

- Good, because fast time-to-first-ship validates the execution architecture
- Good, because the step layer is usable standalone before assets land
- Good, because deferred features build on a proven, tested foundation
- Bad, because v0.1 is a step runner, not yet the asset engine the README describes
