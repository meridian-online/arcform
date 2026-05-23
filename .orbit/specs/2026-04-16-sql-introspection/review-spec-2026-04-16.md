# Spec Review

**Date:** 2026-04-16
**Reviewer:** Context-separated agent (fresh session)
**Spec:** specs/2026-04-16-sql-introspection/spec.yaml
**Verdict:** REQUEST_CHANGES

---

## Findings

### [HIGH] AC-05 assumes ALTER TABLE adds to both `outputs` and `inputs`, but interview says `reads` — field name mismatch
**Category:** Inconsistency between interview and spec
**Description:** The interview (Q5) says ALTER TABLE adds to `outputs` and `reads`. The spec AC-05 says ALTER TABLE populates `outputs` (modified asset) and `inputs` (depends on existing). The spec's verification test says: "outputs contains 'customers', inputs contains 'customers'". Meanwhile, the StepAssets struct uses `reads` (not `inputs`), and SqlAssets uses `inputs`. This is not a blocking issue on its own, but the spec conflates two different field names for the same concept across two structs (SqlAssets.inputs vs StepAssets.reads), and the interview used `reads` for the SqlAssets context. If the implementer follows the interview literally, they may wire it wrong.
**Evidence:** Interview Q5: "ALTER TABLE adds to both `outputs` (it modifies the asset) and `reads` (it depends on the asset existing)." Spec AC-05: "ALTER TABLE populates `outputs` (modified asset) and `inputs` (depends on existing)." StepAssets struct: field is named `reads`. SqlAssets struct: field is named `inputs`.
**Recommendation:** Standardize terminology. In the spec, always use `inputs` when referring to SqlAssets and `reads` when referring to StepAssets. The interview Q5 should be understood as referring to StepAssets.reads, not SqlAssets. Add a note to the spec clarifying the mapping: SqlAssets.inputs -> StepAssets.reads.

---

### [HIGH] AC-05 verification is semantically wrong — ALTER TABLE should NOT add the table to `inputs`
**Category:** Incorrect acceptance criterion
**Description:** AC-05 says ALTER TABLE should populate both `outputs` and `inputs` with the same table name. The interview rationale is "it depends on the asset existing." But the current `validate_order` logic treats inputs as cross-step dependencies. If ALTER TABLE adds `customers` to both `outputs` and `inputs`, the self-reference check in `validate_order` (line 141) will skip it — so it is functionally harmless. However, it is semantically misleading: ALTER TABLE does not *read data from* the table, it *modifies schema*. This blurs the meaning of `inputs` (which everywhere else means "reads data from"). If downstream tooling (card 0013 lineage) renders `inputs` as "data flows from X to this step", ALTER TABLE customers -> inputs(customers) will produce a false data-flow edge.
**Evidence:** `validate_order` line 141: `if step_assets.produces.contains(read_asset) { continue; }` — the self-reference is harmless today, but pollutes lineage semantics.
**Recommendation:** Reconsider whether ALTER TABLE should add to `inputs` at all. An alternative: ALTER TABLE adds only to `outputs` (the asset is modified), and lineage infers the "depends on existing" constraint from the ALTER semantics, not from an inputs entry. If the "depends on existing" constraint is important for ordering validation, consider a new relationship type rather than overloading `inputs`.

---

### [HIGH] CTE filtering has a name collision blindspot — no AC covers CTE names that shadow real table names
**Category:** Gap analysis — missing edge case
**Description:** AC-02 and AC-03 test CTE filtering with non-conflicting names: CTE `recent` reads from table `orders`. AC-09 tests that a CTE name matching *another step's output* doesn't trigger a false dependency. But no AC covers the case where a CTE name shadows a real table that the same SQL also reads. Example: `WITH orders AS (SELECT * FROM raw_orders) SELECT * FROM orders` — here `orders` is both a CTE name and a real table name in the database. The CTE shadows the real table. The spec says CTE names go into `internal` and are removed from `inputs`. If the implementer naively removes all CTE names from `inputs`, then the real `orders` table (which is read inside the CTE definition) would remain in `inputs` — which happens to be correct. But what about: `WITH orders AS (SELECT 1) SELECT * FROM orders JOIN customers ON ...`? Here the outer `FROM orders` refers to the CTE, not the real table. If the implementer strips `orders` from `inputs`, the real `orders` table (if it exists) is not being read — correct. This case actually works. But the subtle correctness depends on implementation order (strip CTE names *after* collecting all inputs), and the spec provides no test to anchor this.
**Evidence:** No AC or verification method covers CTE name shadowing a real table.
**Recommendation:** Add an explicit test case: `WITH customers AS (SELECT * FROM raw_customers) SELECT * FROM customers` verifying that `internal` contains `customers`, `inputs` contains `raw_customers` but NOT `customers`. This anchors the correct behavior.

---

### [MEDIUM] AC-09 verification is an integration test but the spec has no integration test infrastructure
**Category:** Test adequacy
**Description:** AC-09 says: "Integration test: step with CTE referencing name that matches another step's output does not trigger violation." This is well-specified conceptually, but the existing test infrastructure in `asset.rs` already supports this pattern (see `build_graph` helper with SQL files). The word "integration test" implies something more than a unit test. If the implementer interprets this as requiring a runner-level test (which reads manifests, runs preflight, etc.), the scope expands. The verification should be explicit about which test level is required.
**Evidence:** AC-09 verification uses the phrase "Integration test" without defining what integration means in this context.
**Recommendation:** Change verification to: "Unit test in asset.rs: build a graph where step-A creates table `recent`, step-B's SQL uses `WITH recent AS (...) SELECT * FROM recent`, and validate_order succeeds (no false violation on `recent`)."

---

### [MEDIUM] AC-12 is too vague to be verifiable — "investigate" and "where possible" are not acceptance criteria
**Category:** Constraint check — AC is not falsifiable
**Description:** AC-12 says: "investigate sqlparser-rs 0.55 support for PIVOT, UNPIVOT, SUMMARIZE and handle where possible." The verification says: "At minimum: test that these patterns either parse correctly or degrade gracefully." I verified that sqlparser-rs 0.55 *does* support `TableFactor::Pivot` and `TableFactor::Unpivot`. SUMMARIZE is not a standard SQL statement and may or may not parse with DuckDB dialect. The AC's "investigate" framing means it cannot fail — any outcome satisfies it. This is not an acceptance criterion; it is a spike task.
**Evidence:** sqlparser 0.55 has `TableFactor::Pivot` and `TableFactor::Unpivot` variants. `Statement` enum has no `Summarize` variant.
**Recommendation:** Split AC-12 into concrete ACs: (a) "Pivot/Unpivot in FROM clause: extract the source table as an input" with a specific test, (b) "SUMMARIZE: verify parse behavior and document — if it parses, extract tables; if not, degrade gracefully with warning." Remove "investigate" — the investigation is done (answer: Pivot/Unpivot are supported, SUMMARIZE is not a Statement variant).

---

### [MEDIUM] `object_name_to_string` drops schema qualifiers — spec does not address this for multi-schema pipelines
**Category:** Assumption audit — schema-qualified names
**Description:** The current `object_name_to_string` takes only the last component of a qualified name (`schema.table` -> `table`). The spec mentions "better schema-qualified name handling" in the interview (Q1) but no AC addresses it. If two tables in different schemas share the same name (e.g., `staging.customers` and `production.customers`), they will collide in the asset graph as both become `customers`. The spec's four-field model (all `BTreeSet<String>`) inherits this limitation.
**Evidence:** Interview Q1 mentions "better schema-qualified name handling." No AC addresses schema qualification. `object_name_to_string` at introspect.rs line 180-187 discards all but the last identifier.
**Recommendation:** Either (a) add an AC for schema-qualified name handling (preserve full qualified name when schema is present, e.g., `staging.customers` stays as `staging.customers`), or (b) explicitly declare this as out-of-scope with a note that it is deferred. Do not leave it as an untracked gap from the interview.

---

### [MEDIUM] No AC covers multi-statement SQL files with mixed statement types
**Category:** Gap analysis — missing edge case
**Description:** Real SQL files often contain multiple statements: `DROP TABLE IF EXISTS foo; CREATE TABLE foo AS SELECT * FROM bar;` (a common drop-and-recreate pattern). The spec has individual ACs for DROP (AC-04) and CREATE (AC-07), but no AC verifies that a single SQL file with both DROP and CREATE correctly populates both `destroys` and `outputs`. The current `extract_assets` iterates all statements, so this should work mechanically, but without a test the behavior is unverified.
**Evidence:** No AC combines DROP + CREATE in a single SQL file.
**Recommendation:** Add a test: `DROP TABLE IF EXISTS foo; CREATE TABLE foo AS SELECT * FROM bar;` -> `destroys` contains `foo`, `outputs` contains `foo`, `inputs` contains `bar`.

---

### [MEDIUM] `destroys` has no interaction with `validate_order` — destructive ordering is unvalidated
**Category:** Gap analysis — missing validation logic
**Description:** The spec adds a `destroys` field but does not specify how `validate_order` should handle it. If step-A creates table `foo` and step-C reads from `foo`, but step-B drops `foo`, the current `validate_order` will not catch this — it only checks that reads are produced by a prior step, not that the asset hasn't been destroyed between production and consumption. The spec says `destroys` is "tracked separately" and lineage can render it differently, but there is no AC requiring ordering validation against destructive operations.
**Evidence:** AC-04 only verifies `destroys` is populated. No AC checks that `validate_order` considers `destroys`. The `validate_order` code has no concept of asset destruction.
**Recommendation:** This is likely intentional deferral (the spec focuses on introspection, not graph validation of destroys). But add an explicit note: "Destructive ordering validation is deferred — `destroys` is captured for lineage but does not participate in `validate_order` in this drive." Otherwise an implementer may wonder whether they need to add destroy-aware validation.

---

### [LOW] Existing test `test_cte_inputs` in introspect.rs asserts CTE name IS in inputs — must be updated
**Category:** Backwards compatibility risk
**Description:** The spec constraint says "existing tests must continue to pass." But the existing test at introspect.rs line 309-317 (`test_cte_inputs`) explicitly asserts that `recent` IS in `inputs`: `assert!(assets.inputs.contains("recent"))`. The spec's AC-02 requires the opposite: CTE names must NOT be in `inputs`. This test will necessarily break and must be updated. The spec acknowledges this in AC-10 ("tests updated for four-field struct"), but the constraint "existing tests must continue to pass" is misleading — what it really means is "existing tests must be updated to reflect new semantics and then pass."
**Evidence:** introspect.rs line 316: `assert!(assets.inputs.contains("recent"));` directly contradicts AC-02.
**Recommendation:** Reword the constraint: "Existing test *coverage* is preserved — tests are updated for the new four-field SqlAssets shape, and all updated tests pass." The current wording suggests no test modifications are needed, which is false.

---

### [LOW] AC-08 verification is vague — "propagates" is not testable
**Category:** Test adequacy
**Description:** AC-08 says StepAssets gains `internal` and `destroys` fields, and verification says: "AssetGraph::build propagates internal and destroys from introspect results to StepAssets." This describes the mechanism, not the assertion. A proper verification would be: "Test: build graph with SQL containing CTE -> StepAssets.internal contains CTE name. Test: build graph with SQL containing DROP -> StepAssets.destroys contains dropped table."
**Evidence:** AC-08 verification is a description of behavior, not a test specification.
**Recommendation:** Rewrite verification as specific assertions: "Test in asset.rs: SQL step with CTE -> step's internal set is non-empty and contains the CTE name. SQL step with DROP TABLE -> step's destroys set is non-empty and contains the table name."

---

### [LOW] Subqueries in CTE definitions that reference other CTEs are not explicitly tested
**Category:** Gap analysis — nested CTE edge case
**Description:** AC-03 covers `WITH a AS (...), b AS (SELECT * FROM a)`. But it does not cover CTEs with subqueries: `WITH a AS (SELECT * FROM (SELECT * FROM raw) sub) SELECT * FROM a`. The subquery inside the CTE should contribute to `inputs`. The current code handles this through `extract_inputs_from_query` -> `extract_inputs_from_set_expr` -> `extract_inputs_from_table_factor` -> Derived, so it should work. But the spec lacks a verification test for this pattern.
**Evidence:** No AC covers CTEs containing derived tables / subqueries.
**Recommendation:** Low priority but worth adding a test: CTE containing a subquery correctly discovers the subquery's source table in `inputs`.

---

### [LOW] Recursive CTEs not addressed
**Category:** Gap analysis
**Description:** DuckDB supports recursive CTEs (`WITH RECURSIVE`). The spec does not mention them. A recursive CTE references itself in its definition. The current sqlparser-rs `with.recursive` flag exists but the spec has no AC for it. The CTE name would appear in `internal`, but the self-reference within the CTE body (recursive part referencing the CTE name) should not be treated as an external input.
**Evidence:** No mention of `WITH RECURSIVE` in spec or interview.
**Recommendation:** Either add a test for recursive CTEs or note it as explicitly deferred. The risk is low since the CTE filtering logic (remove CTE names from inputs) would naturally handle this.

---

## Honest Assessment

This is a well-structured spec with clear ACs for the core functionality. The four-field SqlAssets model is sound, and the CTE filtering approach (capture in `internal`, exclude from `inputs`) is the right design. However, the spec has three problems that should be addressed before implementation. First, the ALTER TABLE semantics (AC-05) are questionable — adding the same table to both `outputs` and `inputs` overloads the meaning of `inputs` in a way that will produce misleading lineage edges. Second, AC-12 is not an acceptance criterion — it is a spike, and the spike is already answerable (Pivot/Unpivot are supported in sqlparser 0.55, SUMMARIZE is not). It should be converted to concrete, testable ACs. Third, the "existing tests must pass unchanged" constraint is literally false — the existing `test_cte_inputs` test asserts the exact behavior being changed, and this needs to be acknowledged explicitly. The remaining findings are lower severity but worth addressing for completeness, particularly the CTE-name-shadows-real-table edge case and the missing DROP+CREATE combined test.
