---
status: accepted
date-created: 2026-03-31
date-modified: 2026-03-31
---
# 0003. Delegate SQL Execution to Engine CLIs

## Context and Problem Statement

ArcForm needs to execute SQL steps. It could embed a DuckDB library or shell out to the engine's CLI. The choice affects portability and the "local = remote" guarantee.

## Considered Options

- **Embedded DuckDB library** — link DuckDB directly, execute SQL in-process. Fast, but ties ArcForm to one engine.
- **Shell out to engine CLI** — ArcForm builds the DAG, then invokes the engine's command-line tool. Engine-agnostic by design.

## Decision Outcome

Chosen option: "Shell out to engine CLI", because the core value proposition is "what runs locally also works remotely." By delegating to CLIs, ArcForm is engine-agnostic — the same manifest can target DuckDB locally and Postgres/BigQuery in production. ArcForm models and introspects the SQL; the engine executes it.

### Consequences

- Good, because engine-agnostic — supports any SQL engine with a CLI
- Good, because ArcForm stays thin — no engine library dependencies
- Good, because sqlparser-rs provides dialect awareness for introspection without execution
- Bad, because error messages come from the engine CLI, not ArcForm
- Bad, because per-step process spawning (negligible for analytical workloads)
