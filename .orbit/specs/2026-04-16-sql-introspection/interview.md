# Design: SQL Introspection

**Date:** 2026-04-16
**Interviewer:** Nightingale
**Card:** cards/0009-sql-introspection.yaml

---

## Context

Card: *SQL introspection* — 4 scenarios, goal: "v0.2/v0.3 — zero-boilerplate SQL dependency tracking"
Prior specs: 1 — card 0008 (asset registry) shipped `introspect.rs` and `asset.rs` covering most scenarios
Gap: CTE names leak into the inputs set as false external dependencies; additional SQL patterns (DROP, ALTER, MERGE, DuckDB syntax) not handled

## Q&A

### Q1: Drive scope
**Q:** Card 0008 shipped introspect.rs and asset.rs — most of 0009's scenarios are already covered. What scope should this drive target?
**A:** CTE fix + introspection hardening. Fix the CTE leak, plus harden the parser against additional SQL patterns: DROP/ALTER awareness, MERGE statements, DuckDB-specific syntax (PIVOT, UNPIVOT), and better schema-qualified name handling.

### Q2: CTE filtering strategy
**Q:** For CTE filtering, where should we strip the CTE names from inputs?
**A:** CTEs are a rich part of the asset graph — they exist within the step but are still part of the data flow story. Reference: Dagster's Global Asset Lineage view shows every intermediate transformation as a first-class node. CTEs should be captured as step-internal assets, not discarded.

### Q3: CTE data model
**Q:** Should SqlAssets grow a third field (e.g. `internal: BTreeSet<String>`) to hold CTE names, separate from inputs and outputs?
**A:** Yes — three-set model. SqlAssets gets `outputs`, `inputs`, and `internal` (CTE names). Introspect captures the CTE names. Asset graph uses `internal` to filter them from cross-step validation. Lineage (card 0013) can render them as sub-nodes within a step.

### Q4: Additional SQL patterns
**Q:** Which additional SQL patterns should this drive add to the introspection engine?
**A:** All three: DROP and ALTER awareness, DuckDB-specific syntax, and MERGE/UPSERT.

### Q5: DROP/ALTER semantics
**Q:** How should DROP TABLE and ALTER TABLE affect the asset graph?
**A:** Track as a separate `destroys` set. SqlAssets gets a fourth field: `destroys: BTreeSet<String>`. DROP TABLE adds to `destroys`. ALTER TABLE adds to both `outputs` (it modifies the asset) and `reads` (it depends on the asset existing). Lineage can render destructive edges differently.

---

## Summary

### Goal
Harden the SQL introspection engine: fix CTE false dependencies, add DROP/ALTER/MERGE/DuckDB syntax support, and enrich SqlAssets with `internal` and `destroys` fields for downstream lineage.

### Constraints
- SqlAssets grows from 2 fields to 4: `outputs`, `inputs`, `internal`, `destroys`
- CTE names must be filtered from cross-step dependency validation in AssetGraph
- CTE names must be preserved for lineage visualisation (card 0013)
- DuckDB-specific syntax support is best-effort — depends on sqlparser-rs coverage
- Existing tests must continue to pass (backwards compatible)

### Success Criteria
- CTE names do not appear in `inputs` — they appear in `internal`
- DROP TABLE populates `destroys`
- ALTER TABLE populates `outputs` and `reads`
- MERGE INTO populates `outputs` (target) and `inputs` (source)
- DuckDB CREATE OR REPLACE handled
- All existing introspect.rs and asset.rs tests pass
- New tests cover each added pattern

### Decisions Surfaced
- **Three-set CTE model**: CTEs captured as `internal` assets, not discarded — enables Dagster-style intra-step lineage in card 0013
- **Four-field SqlAssets**: `outputs`, `inputs`, `internal`, `destroys` — richer model than the minimal two-field version
- **DROP as `destroys`**: Destructive operations tracked separately from productive ones — lineage can render them differently

### Open Questions
- Which DuckDB-specific syntax does sqlparser-rs 0.55 actually support? (spike during implementation)
