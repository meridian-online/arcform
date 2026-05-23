# Implementation Progress

**Spec:** specs/2026-04-16-sql-introspection/spec.yaml
**Started:** 2026-04-16

## Hard Constraints
- [x] SqlAssets grows from 2 to 4 fields: outputs, inputs, internal, destroys
- [x] CTE names must appear in `internal`, never in `inputs`
- [x] Existing test coverage preserved — tests updated for new shapes
- [x] AssetGraph::build must filter `internal` from cross-step dependency validation
- [x] StepAssets must expose internal and destroys for downstream lineage
- [x] Schema-qualified name handling explicitly deferred — no changes to object_name_to_string
- [x] Destructive ordering validation explicitly deferred — destroys captured but not checked in validate_order

## Acceptance Criteria
- [x] ac-01: SqlAssets has four fields: outputs, inputs, internal, destroys — struct updated with doc comments
- [x] ac-02: CTE names captured in `internal`, removed from `inputs` — test_ac02_cte_internal_not_inputs
- [x] ac-03: Nested CTEs both captured in `internal` — test_ac03_nested_ctes_in_internal
- [x] ac-04: CTE shadowing real table name handled correctly — test_ac04_cte_shadows_real_table
- [x] ac-05: DROP TABLE/VIEW populates `destroys` — test_ac05_drop_table_destroys, test_ac05_drop_view_destroys
- [x] ac-06: DROP + CREATE combined populates both destroys and outputs — test_ac06_drop_then_create
- [x] ac-07: ALTER TABLE populates `outputs` only — test_ac07_alter_table_outputs_only
- [x] ac-08: MERGE INTO handled — target in outputs, source in inputs — test_ac08_merge_into
- [x] ac-09: CREATE OR REPLACE handled as output — test_ac09_create_or_replace
- [x] ac-10: StepAssets gains internal and destroys, populated from SqlAssets — test_v03_ac10_step_assets_internal_from_cte, test_v03_ac10_step_assets_destroys_from_drop
- [x] ac-11: validate_order ignores internal assets — test_v03_ac11_cte_name_no_false_violation
- [x] ac-12: All existing introspect.rs tests pass with new shape — 30 introspect tests pass
- [x] ac-13: All existing asset.rs and runner.rs tests pass with new shape — 27 asset tests, 18 runner tests pass
- [x] ac-14: PIVOT/UNPIVOT source table extracted — test_ac14_pivot_source_table, test_ac14_unpivot_source_table (both parse and extract correctly)

## Results
- 75 tests total, all passing
- Bonus tests: recursive CTE, CTE with subquery, DROP multiple tables
- PIVOT/UNPIVOT both parse and extract correctly in sqlparser-rs 0.55
