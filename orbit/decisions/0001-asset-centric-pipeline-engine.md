---
status: accepted
date-created: 2026-03-31
date-modified: 2026-03-31
---
# 0001. ArcForm is an Asset-Centric Pipeline Engine

## Context and Problem Statement

The Meridian ecosystem needs a data pipeline tool. The design space spans step-centric orchestrators (run tasks in order, like Dagu/CloudBuild/Make) to asset-centric engines (declare data outputs and their dependencies, like Dagster/dbt). FineType handles type inference. What role does ArcForm fill?

## Considered Options

- **Step-centric orchestrator** — nodes are commands, edges are execution order. The engine knows what runs when.
- **Asset-centric pipeline engine** — nodes are data outputs (tables, files), edges are data dependencies. The engine knows what data flows where.
- **Step-centric with asset layer** — steps execute, a separate asset registry tracks what they produce.

## Decision Outcome

Chosen option: "Asset-centric pipeline engine", because the core value of ArcForm is understanding the *data* flowing through a pipeline, not just the *commands* that run. This is what differentiates it from Dagu (general-purpose task runner) and positions it alongside Dagster/dbt as a data-aware tool.

The architecture separates concerns:
- **Steps** are the execution layer — how work gets done (SQL, shell commands)
- **Assets** are the data layer — what data is produced, what it depends on
- **SQL introspection** bridges the two — the engine parses SQL to auto-infer which assets a step reads and writes

### Consequences

- Good, because the engine understands data lineage, enabling selective re-materialisation
- Good, because SQL introspection reduces boilerplate (no manual depends_on for SQL steps)
- Good, because clear separation: FineType classifies data, ArcForm orchestrates data flow
- Bad, because higher architectural complexity than a simple step runner
- Bad, because SQL introspection is a non-trivial feature to implement correctly
