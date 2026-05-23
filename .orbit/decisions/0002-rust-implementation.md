---
status: accepted
date-created: 2026-03-31
date-modified: 2026-03-31
---
# 0002. Implement ArcForm in Rust

## Context and Problem Statement

ArcForm needs to parse SQL (via sqlparser-rs), manage a DAG of assets, and shell out to engine CLIs. What language should it be written in?

## Considered Options

- **Rust** — consistent with FineType, direct sqlparser-rs access, single binary
- **TypeScript (Deno)** — fast dev velocity, good YAML handling, Deno compile for single binary

## Decision Outcome

Chosen option: "Rust", because ecosystem consistency with FineType matters for maintainability, and direct access to sqlparser-rs (DataFusion's SQL parser) is essential for SQL introspection — the core differentiator. Single static binary distribution matches FineType's (Homebrew, GitHub releases).

### Consequences

- Good, because single binary distribution — same channel as FineType
- Good, because direct access to sqlparser-rs for SQL parsing and dependency inference
- Good, because ecosystem consistency — shared tooling, CI patterns, contributor familiarity
- Bad, because higher implementation cost for string-heavy orchestration logic
- Mitigated by strong crate ecosystem: `clap`, `serde`, `sqlparser`, `petgraph`, `thiserror`
