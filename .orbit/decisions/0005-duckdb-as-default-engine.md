---
status: accepted
date-created: 2026-03-31
date-modified: 2026-03-31
---
# 0005. DuckDB as Default Engine

## Context and Problem Statement

ArcForm delegates SQL execution to engine CLIs and uses sqlparser-rs for multi-dialect support. Which engine should be the default target?

## Considered Options

- **DuckDB** — local-first analytical database, single-file, CLI available
- **PostgreSQL** — industry standard, requires a running server
- **SQLite** — ubiquitous, but limited analytical SQL support

## Decision Outcome

Chosen option: "DuckDB", because it aligns with the "local-first" positioning. DuckDB runs anywhere without a server, handles analytical workloads well, reads CSV/Parquet natively, and is the engine FineType already targets.

### Consequences

- Good, because DuckDB is zero-config — perfect for local-first
- Good, because DuckDB reads CSV/Parquet/JSON natively
- Good, because ecosystem alignment with FineType's DuckDB extension
- Bad, because DuckDB's SQL dialect differs from Postgres/BigQuery
- Mitigated by sqlparser-rs dialect support
