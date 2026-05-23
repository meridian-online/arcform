---
status: accepted
date-created: 2026-03-31
date-modified: 2026-03-31
---
# 0007. SQL Introspection via sqlparser-rs is a Core Feature

## Context and Problem Statement

ArcForm's asset model requires knowing what data each SQL step reads and writes. This can come from explicit declarations (user writes `depends_on`) or automatic inference (parse the SQL). Which approach?

## Considered Options

- **Explicit only** — users declare all dependencies manually. Simple, but verbose.
- **Auto-inference only** — parse SQL to extract table references. No boilerplate, but fragile with dynamic SQL.
- **Auto-inference with override** — parse SQL by default, allow explicit depends_on to override or supplement.

## Decision Outcome

Chosen option: "Auto-inference with override", because SQL introspection is ArcForm's core differentiator — it's what makes it more than another Dagu. The engine parses SQL files via sqlparser-rs to extract:

- Tables read (FROM, JOIN clauses)
- Tables written (CREATE TABLE, INSERT INTO, COPY TO)
- CTE definitions

This auto-populates the asset dependency graph for SQL steps. Users can override with explicit `depends_on` for edge cases (dynamic SQL, indirect dependencies). `command:` steps always require explicit asset declarations since they can't be introspected.

### Consequences

- Good, because SQL steps require zero dependency boilerplate
- Good, because the engine understands data lineage structurally
- Good, because explicit override handles edge cases gracefully
- Bad, because sqlparser-rs must handle the target dialect correctly (DuckDB extensions, etc.)
- Bad, because dynamic SQL or complex string interpolation can't be parsed
- Note: sqlparser integration is deferred from v0.1 but is architecturally central, not optional
