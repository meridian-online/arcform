# Interview: ArcForm Foundational Decisions

**Date:** 2026-03-30
**Interviewer:** Nightingale

---

## Context

ArcForm is a new project under the Meridian umbrella. The README states: "Local-first data pipeline engine for analytical workflows." This interview clarifies what ArcForm IS, its boundaries, execution model, and v0.1 scope to establish a strong decision register before any code is written.

## Interview Q&A

### Q1: Core Identity
**Q:** What IS ArcForm at its core? Is it orchestrating multi-step transformations (like dbt/SQLMesh), or a data ingestion/loading tool, or both?
**A:** It's an orchestrator. While FineType and ArcForm are complementary, they can be used in isolation. Unix philosophy — "Write programs that do one thing and do it well" — applies here. "Multi-step DAG of SQL/script transforms over tables" is accurate.

### Q2: Execution Engine
**Q:** What execution engine runs the transforms? Is DuckDB the default, or is it more general?
**A:** DuckDB-native, but with sqlparser-rs we'll support a number of dialects. The goal is to represent the modelling of SQL statements, but let the statements be executed by the SQL engine/service directly using the command line.

### Q3: Config Format
**Q:** What does the pipeline definition look like?
**A:** YAML (maybe TOML, TBD) files for the DAG. SQL files as steps.

### Q4: Dependency Resolution
**Q:** Does ArcForm auto-detect dependencies from SQL or do users declare them explicitly?
**A:** Explicit, in YAML or SQL comment at top of file. DuckDB is the default engine.

### Q5: Implementation Language
**Q:** What's the implementation language?
**A:** Rust. Consistent with FineType ecosystem. Direct access to sqlparser-rs. Single binary distribution.

### Q6: Project Model
**Q:** What does the manifest declare? Environments? Variables?
**A:** Environments + variables make sense but need decisions. The goal is to make this easy for CLI-familiar analysts. "Environments" is focused on CLI tool versions (not venv). Templating is a minefield — Jinja is intense, SQLMesh steered away from it. `.env` files are simple. DuckDB supports variables natively. Deferred for later exploration. Reference: Dagu project.

### Q7: State Management
**Q:** How should ArcForm handle state?
**A:** Not stateless — needs to support incremental processing. But the binary doesn't need to solve this alone. Could track state with a DuckDB database file. Deferred for later exploration.

### Q8: Target User
**Q:** Who is the primary user?
**A:** Think "local-first but build for the cloud." Like Dagu or `nektos/act`. Defined workflows offer great dev experience. Always cases to build locally and schedule remotely. Key value prop: "what runs locally, also works remotely." Achieved by asserting versions in a preflight stage. Pain point from GCP CloudBuild: excellent platform, but could never test locally.

### Q9: v0.1 Scope
**Q:** What's the minimum feature set to ship?
**A:** DAG + execute. Parse YAML manifest, resolve DAG order, execute SQL files against DuckDB in sequence. No incremental, no environments, no variables.

### Q10: CLI Surface
**Q:** What CLI commands for v0.1?
**A:** `arcform init` + `arcform run`.

### Q11: Manifest Shape
**Q:** Flat step list, layered stages, or file-discovery with SQL comments?
**A:** Flat step list (like CloudBuild and Dagu). Sequential by default, `depends_on` added later for parallelism. Keep SQL files clean from orchestration markup.

### Q12: Directory Layout
**Q:** What does `arcform init` scaffold?
**A:** `arcform.yaml` + `models/` + `sources/`.

---

## Summary

### Goal
Build a local-first SQL transform orchestrator that models DAGs of SQL statements and delegates execution to engine CLIs. The core value proposition is: **what runs locally also works remotely**, achieved through version preflight assertions and engine-agnostic SQL dialect support.

### Constraints
- Rust implementation (ecosystem consistency, single binary, sqlparser-rs access)
- DuckDB as default execution engine
- YAML manifest + SQL files (orchestration in YAML, not in SQL)
- Flat step list, sequential by default
- Unix philosophy: one tool, one job. FineType is complementary, not a dependency
- v0.1 is minimal: DAG parse + sequential execute

### Success Criteria
- `arcform init` scaffolds a valid project (`arcform.yaml`, `models/`, `sources/`)
- `arcform run` parses the manifest, resolves step order, executes SQL files against DuckDB CLI
- SQL files remain clean of orchestration markup
- Single static binary, distributable via Homebrew

### Open Questions (Deferred)
- Templating strategy (Jinja vs `.env` vs DuckDB native variables)
- State management / incremental processing (DuckDB file as state store?)
- Environment model (CLI tool version assertions, not virtualenvs)
- YAML vs TOML for manifest format
- Licensing (MIT vs Apache-2.0)
- Preflight version assertion mechanism
- Remote execution / scheduling story
