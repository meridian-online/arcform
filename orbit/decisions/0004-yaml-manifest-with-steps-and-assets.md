---
status: accepted
date-created: 2026-03-31
date-modified: 2026-03-31
---
# 0004. YAML Manifest with Steps and Separate Asset Registry

## Context and Problem Statement

Users need to define their pipeline. In an asset-centric model, the manifest must capture both *what data is produced* (assets) and *how to produce it* (steps). Should these be merged or separate?

## Considered Options

- **Steps only** — flat step list like Dagu/CloudBuild. Assets are implicit side effects.
- **Assets only** — each asset declares its SQL/command inline. No separate step concept.
- **Steps + asset registry** — steps are execution units. A separate `assets:` section maps data outputs to steps and declares dependencies.

## Decision Outcome

Chosen option: "Steps + asset registry", because it separates concerns cleanly. Steps define HOW (the SQL file, the command). Assets define WHAT (the data produced, its dependencies). This allows:

1. One step to produce multiple assets
2. Assets to be tracked for staleness independently of steps
3. SQL introspection to auto-populate the asset registry for SQL steps
4. `command:` steps to declare assets explicitly (since they can't be introspected)

For v0.1, only steps are implemented. The asset registry is the next architectural layer.

### Consequences

- Good, because clean separation of execution (steps) and data (assets)
- Good, because assets can be auto-inferred from SQL via sqlparser
- Good, because the step layer works standalone as a foundation
- Bad, because two concepts to learn instead of one
