# Design: Asset Registry

**Date:** 2026-04-15
**Interviewer:** Nightingale
**Card:** cards/0008-asset-registry.yaml

---

## Context

Card: *Asset Registry* — 3 scenarios, goal: v0.2 — asset-centric model
Prior specs: 0 — first design session for the asset layer
Gap: v0.1 ships step execution but has no concept of what data is produced. The asset registry adds the semantic layer that makes ArcForm asset-centric rather than just a step runner.

Key author clarification at session start: **users should not have to manually declare assets for SQL steps.** The whole point of sqlparser-rs (decision 0007) is auto-discovery. The manifest `assets:` section is an override layer, not the primary declaration mechanism.

## Q&A

### Q1: Where should assets live in the manifest?

**Q:** Decision 0004 says a separate `assets:` section, but there's a spectrum from flat map to nested-under-steps. Which structure?

**A:** The question was reframed after the author clarified that assets should be auto-discovered from SQL, not manually declared. For SQL steps, sqlparser-rs parses the `.sql` file to extract reads/writes. The manifest `assets:` section exists only as an override layer (for corrections) and for command steps (which can't be introspected). This aligns with decision 0007.

### Q2: What's the v0.2 scope?

**Q:** Should the asset registry be purely discovery-based, or include the override layer too?

**A:** Discovery + override layer. sqlparser-rs parses SQL steps to auto-discover assets. Command steps require explicit produces/depends_on. A top-level `assets:` section allows users to correct or augment inferred assets. Full picture for v0.2.

### Q3: Does the asset graph change execution behaviour?

**Q:** Should v0.2 validate declared step order against the dependency graph, or actually reorder steps topologically?

**A:** Validate only. Steps still run in declared order (preserving v0.1 semantics). But arc checks the discovered graph against that order and hard-errors if a step reads an asset that hasn't been produced yet. Catches misorderings before they cause confusing SQL errors.

### Q4: What SQL patterns should sqlparser-rs extract?

**Q:** DuckDB supports CREATE TABLE, CREATE VIEW, INSERT INTO, COPY, CTAS. Where to draw the line?

**A:** DDL + DML outputs. Outputs: CREATE TABLE, CREATE VIEW, CREATE TABLE AS (CTAS), INSERT INTO, COPY TO. Inputs: table names in FROM/JOIN clauses. INSERT INTO a pre-existing table is acknowledged as ambiguous — the step "produces" that table in the sense that it writes to it.

### Q5: Override mechanism shape?

**Q:** When sqlparser infers wrong or incomplete assets, how does the user correct it?

**A:** Top-level `assets:` section in the manifest. Asset names are keys, with `produced_by` and `depends_on` fields. Overrides replace the auto-inferred graph for that asset.

### Q6: Lineage visualisation?

**Q:** Should v0.2 ship a CLI command for viewing the lineage graph?

**A:** The DAG visualisation is a separate capability warranting its own card. Mermaid rendering (via mermaid-rs-renderer) is one approach. For this card, the asset graph is built internally and used for validation. Presentation is out of scope.

### Q7: Validation mode for ordering issues?

**Q:** Hard error or warn when the graph shows a dependency ordering problem?

**A:** Hard error before execution. If the graph shows a step reading an asset before its producing step has run, arc refuses to run. Mirrors the preflight pattern — validate before execute.

### Q8: Handling unparseable SQL?

**Q:** How should arc handle SQL files that sqlparser-rs can't parse (DuckDB extensions, PIVOT, custom functions)?

**A:** Warn and treat the step as opaque. Skip that step's asset inference — it becomes like a command step. The user can add overrides in the `assets:` section if the step produces data. No hard error for parse failures.

---

## Summary

### Goal

Build the asset registry layer on top of v0.1's step execution foundation. Auto-discover assets from SQL via sqlparser-rs, validate dependency ordering, provide an override mechanism for corrections and command steps.

### Constraints

- sqlparser-rs for SQL parsing (not DuckDB's own parser)
- DDL + DML output patterns (CREATE TABLE/VIEW, CTAS, INSERT INTO, COPY TO)
- FROM/JOIN clauses as input signals
- Steps run in declared order (v0.1 semantics preserved)
- Dependency validation is pre-execution, not reordering
- Parse failures degrade gracefully (warn, treat as opaque)
- Visualisation is a separate card

### Success Criteria

- SQL steps auto-discover their produced and consumed assets
- Command steps declare assets explicitly via produces/depends_on
- Top-level `assets:` section overrides inferred assets
- `arc run` hard-errors if dependency order is violated
- Unparseable SQL warns and treats step as opaque
- All existing v0.1 tests continue to pass

### Decisions Surfaced

- **Auto-discovery over manual declaration**: sqlparser-rs parses SQL to infer assets. Users don't write asset declarations for SQL steps. (Refines decision 0004)
- **Validate-only execution**: Graph validates order but doesn't reorder steps. (New decision for v0.2)
- **Graceful parse degradation**: Unparseable SQL warns, doesn't block. (New decision for v0.2)
- **Visualisation is a separate card**: DAG rendering (mermaid-rs-renderer) is not in v0.2 asset registry scope. (Scope decision)

### Open Questions

- Exact fields for the `assets:` override schema (type field from decision 0004 — needed in v0.2 or defer?)
- How INSERT INTO ambiguity resolves in practice — is the inserting step the "producer" or an "appender"?
- sqlparser-rs DuckDB dialect support coverage — may need to test against real pipeline SQL
