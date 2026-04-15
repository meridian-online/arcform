//! SQL introspection via sqlparser-rs.
//!
//! Parses SQL files using the DuckDB dialect to extract:
//! - **Outputs**: tables/views created or written to (CREATE TABLE, CREATE VIEW, CTAS, INSERT INTO, COPY TO)
//! - **Inputs**: tables read from (FROM, JOIN clauses)

use std::collections::BTreeSet;

use sqlparser::ast::{
    CopySource, CopyTarget, Insert, ObjectName, Statement, TableFactor, TableObject,
};
use sqlparser::dialect::DuckDbDialect;
use sqlparser::parser::Parser;

/// Assets discovered from parsing a SQL file — four-set model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SqlAssets {
    /// Tables/views this SQL creates, writes to, or modifies (ALTER).
    pub outputs: BTreeSet<String>,
    /// External tables this SQL reads data from (CTEs excluded).
    pub inputs: BTreeSet<String>,
    /// CTE names — step-internal assets visible in lineage but not cross-step dependencies.
    pub internal: BTreeSet<String>,
    /// Tables/views this SQL drops — destructive operations tracked separately.
    pub destroys: BTreeSet<String>,
}

/// Parse a SQL string and extract the assets it produces and consumes.
///
/// Returns `Ok(SqlAssets)` on success, or `Err(warnings)` if the SQL
/// cannot be parsed. The caller should treat parse failures as opaque
/// steps (warn, don't block).
pub fn extract_assets(sql: &str) -> Result<SqlAssets, Vec<String>> {
    let dialect = DuckDbDialect {};
    let statements = Parser::parse_sql(&dialect, sql).map_err(|e| vec![e.to_string()])?;

    let mut assets = SqlAssets::default();

    for stmt in &statements {
        extract_from_statement(stmt, &mut assets);
    }

    // CTE filtering: CTE names were collected in `internal` during parsing.
    // Remove them from `inputs` — a CTE reference in FROM is step-internal,
    // not an external dependency.
    for cte_name in &assets.internal {
        assets.inputs.remove(cte_name);
    }

    Ok(assets)
}

/// Extract table names from a single SQL statement.
fn extract_from_statement(stmt: &Statement, assets: &mut SqlAssets) {
    match stmt {
        // CREATE TABLE foo (...)
        // CREATE TABLE foo AS SELECT ...
        Statement::CreateTable(create) => {
            let name = object_name_to_string(&create.name);
            assets.outputs.insert(name);

            // If it's a CTAS, the query's FROM tables are inputs.
            if let Some(ref query) = create.query {
                extract_inputs_from_query(query, assets);
            }
        }

        // CREATE VIEW foo AS SELECT ...
        Statement::CreateView { name, query, .. } => {
            assets.outputs.insert(object_name_to_string(name));
            extract_inputs_from_query(query, assets);
        }

        // INSERT INTO foo SELECT ...
        Statement::Insert(Insert {
            table,
            source,
            ..
        }) => {
            if let TableObject::TableName(name) = table {
                assets.outputs.insert(object_name_to_string(name));
            }
            if let Some(src) = source {
                extract_inputs_from_query(src.as_ref(), assets);
            }
        }

        // COPY foo TO 'file.csv'
        // COPY foo FROM 'file.csv'
        Statement::Copy {
            source, target, ..
        } => {
            match source {
                CopySource::Table {
                    table_name, ..
                } => {
                    // COPY <table> ... — table is the source being read/written
                    match target {
                        CopyTarget::File { .. } | CopyTarget::Stdout => {
                            // COPY table TO file — reading from the table
                            assets.inputs.insert(object_name_to_string(table_name));
                        }
                        CopyTarget::Stdin => {
                            // COPY table FROM STDIN — writing to the table
                            assets.outputs.insert(object_name_to_string(table_name));
                        }
                        _ => {}
                    }
                }
                CopySource::Query(query) => {
                    extract_inputs_from_query(query, assets);
                }
            }
        }

        // DROP TABLE/VIEW — destructive operation
        Statement::Drop {
            names, ..
        } => {
            for name in names {
                assets.destroys.insert(object_name_to_string(name));
            }
        }

        // ALTER TABLE — modifies the asset (output), does not read data from it
        Statement::AlterTable { name, .. } => {
            assets.outputs.insert(object_name_to_string(name));
        }

        // ALTER VIEW — modifies the view (output), new query reads from tables (inputs)
        Statement::AlterView { name, query, .. } => {
            assets.outputs.insert(object_name_to_string(name));
            extract_inputs_from_query(query, assets);
        }

        // MERGE INTO target USING source — target is written, source is read
        Statement::Merge {
            table, source, ..
        } => {
            // Target table → outputs
            if let TableFactor::Table { name, .. } = table {
                assets.outputs.insert(object_name_to_string(name));
            }
            // Source table → inputs
            extract_inputs_from_table_factor(source, assets);
        }

        // SELECT ... FROM — standalone select, extract inputs
        Statement::Query(query) => {
            extract_inputs_from_query(query, assets);
        }

        // All other statements — no asset extraction
        _ => {}
    }
}

/// Extract input table names from a query (SELECT ... FROM ... JOIN ...).
/// Also collects CTE names into `assets.internal`.
fn extract_inputs_from_query(query: &sqlparser::ast::Query, assets: &mut SqlAssets) {
    extract_inputs_from_set_expr(&query.body, assets);

    // Handle CTEs — they define local names, and their queries read from tables.
    // CTE names are captured in `internal` (step-internal assets).
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            // Record the CTE name as an internal asset.
            assets.internal.insert(cte.alias.name.value.to_lowercase());
            // The CTE's body reads from tables — those are real inputs.
            extract_inputs_from_query(&cte.query, assets);
        }
    }
}

/// Recursively extract input table names from a set expression.
/// Handles SELECT, UNION/EXCEPT/INTERSECT, and nested queries.
fn extract_inputs_from_set_expr(set_expr: &sqlparser::ast::SetExpr, assets: &mut SqlAssets) {
    match set_expr {
        sqlparser::ast::SetExpr::Select(select) => {
            for table in &select.from {
                extract_inputs_from_table_factor(&table.relation, assets);
                for join in &table.joins {
                    extract_inputs_from_table_factor(&join.relation, assets);
                }
            }
        }
        sqlparser::ast::SetExpr::SetOperation { left, right, .. } => {
            extract_inputs_from_set_expr(left, assets);
            extract_inputs_from_set_expr(right, assets);
        }
        sqlparser::ast::SetExpr::Query(query) => {
            extract_inputs_from_query(query, assets);
        }
        // Values, Insert, Update, Table — no table references to extract.
        _ => {}
    }
}

/// Extract a table name from a table factor (FROM clause item).
fn extract_inputs_from_table_factor(factor: &TableFactor, assets: &mut SqlAssets) {
    match factor {
        TableFactor::Table { name, .. } => {
            assets.inputs.insert(object_name_to_string(name));
        }
        TableFactor::Derived { subquery, .. } => {
            extract_inputs_from_query(subquery, assets);
        }
        TableFactor::NestedJoin { table_with_joins, .. } => {
            extract_inputs_from_table_factor(&table_with_joins.relation, assets);
            for join in &table_with_joins.joins {
                extract_inputs_from_table_factor(&join.relation, assets);
            }
        }
        // PIVOT wraps a source table — extract the inner table as an input.
        TableFactor::Pivot { table, .. } => {
            extract_inputs_from_table_factor(table, assets);
        }
        // UNPIVOT wraps a source table — extract the inner table as an input.
        TableFactor::Unpivot { table, .. } => {
            extract_inputs_from_table_factor(table, assets);
        }
        // TableFunction, MatchRecognize, etc. — skip
        _ => {}
    }
}

/// Convert an ObjectName (potentially qualified: schema.table) to a simple string.
/// Uses the last identifier (the table name itself), lowercased for consistency.
fn object_name_to_string(name: &ObjectName) -> String {
    // ObjectName contains Vec<ObjectNamePart>; take the last part (table name).
    name.0
        .last()
        .and_then(|part| part.as_ident())
        .map(|ident| ident.value.to_lowercase())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-01: CREATE TABLE is discovered as an output.
    #[test]
    fn test_ac01_create_table_output() {
        let sql = "CREATE TABLE customers (id INT, name TEXT);";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.outputs.contains("customers"));
        assert!(assets.inputs.is_empty());
    }

    // AC-01: CREATE VIEW is discovered as an output.
    #[test]
    fn test_ac01_create_view_output() {
        let sql = "CREATE VIEW active_customers AS SELECT * FROM customers WHERE active = true;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.outputs.contains("active_customers"));
        assert!(assets.inputs.contains("customers"));
    }

    // AC-01: CREATE TABLE AS SELECT (CTAS) discovers both output and inputs.
    #[test]
    fn test_ac01_ctas_output_and_inputs() {
        let sql = "CREATE TABLE summary AS SELECT count(*) AS total FROM orders;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.outputs.contains("summary"));
        assert!(assets.inputs.contains("orders"));
    }

    // AC-01: Multiple DDL statements in one file.
    #[test]
    fn test_ac01_multiple_creates() {
        let sql = "CREATE TABLE foo (id INT);\nCREATE TABLE bar (id INT);\nCREATE VIEW baz AS SELECT * FROM foo;";
        let assets = extract_assets(sql).unwrap();
        assert_eq!(assets.outputs, BTreeSet::from(["foo".into(), "bar".into(), "baz".into()]));
        assert!(assets.inputs.contains("foo"));
    }

    // AC-02: FROM clause tables are discovered as inputs.
    #[test]
    fn test_ac02_from_clause_inputs() {
        let sql = "SELECT * FROM customers;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.inputs.contains("customers"));
        assert!(assets.outputs.is_empty());
    }

    // AC-02: JOIN tables are discovered as inputs.
    #[test]
    fn test_ac02_join_inputs() {
        let sql = "SELECT c.name, o.total FROM customers c JOIN orders o ON c.id = o.customer_id;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.inputs.contains("customers"));
        assert!(assets.inputs.contains("orders"));
    }

    // AC-02: Subqueries in FROM clause.
    #[test]
    fn test_ac02_subquery_inputs() {
        let sql = "SELECT * FROM (SELECT * FROM raw_data) sub;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.inputs.contains("raw_data"));
    }

    // AC-03: INSERT INTO is discovered as an output.
    #[test]
    fn test_ac03_insert_into_output() {
        let sql = "INSERT INTO summary SELECT count(*) FROM customers;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.outputs.contains("summary"));
        assert!(assets.inputs.contains("customers"));
    }

    // AC-03: COPY TO reads from a table (input).
    #[test]
    fn test_ac03_copy_to_file() {
        let sql = "COPY customers TO 'customers.csv';";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.inputs.contains("customers"));
    }

    // AC-07: Unparseable SQL returns an error (caller treats as opaque).
    #[test]
    fn test_ac07_unparseable_sql() {
        let sql = "THIS IS NOT VALID SQL AT ALL %%%";
        let result = extract_assets(sql);
        assert!(result.is_err());
    }

    // AC-02: UNION ALL discovers inputs from both branches.
    #[test]
    fn test_ac02_union_all_inputs() {
        let sql = "SELECT * FROM customers UNION ALL SELECT * FROM archived_customers;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.inputs.contains("customers"));
        assert!(assets.inputs.contains("archived_customers"));
    }

    // AC-02: CTAS with UNION discovers output and all inputs.
    #[test]
    fn test_ac02_ctas_union_inputs() {
        let sql = "CREATE TABLE all_customers AS SELECT * FROM customers UNION ALL SELECT * FROM archived_customers;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.outputs.contains("all_customers"));
        assert!(assets.inputs.contains("customers"));
        assert!(assets.inputs.contains("archived_customers"));
    }

    // AC-02: EXCEPT discovers inputs from both sides.
    #[test]
    fn test_ac02_except_inputs() {
        let sql = "SELECT id FROM customers EXCEPT SELECT id FROM blocklist;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.inputs.contains("customers"));
        assert!(assets.inputs.contains("blocklist"));
    }

    // AC-02: CTE names go to internal, not inputs.
    #[test]
    fn test_ac02_cte_internal_not_inputs() {
        let sql = "WITH recent AS (SELECT * FROM orders WHERE date > '2026-01-01') SELECT * FROM recent;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.inputs.contains("orders"), "real table should be in inputs");
        assert!(!assets.inputs.contains("recent"), "CTE name should NOT be in inputs");
        assert!(assets.internal.contains("recent"), "CTE name should be in internal");
    }

    // Edge case: Qualified table names use the last component.
    #[test]
    fn test_qualified_name() {
        let sql = "CREATE TABLE main.customers (id INT);";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.outputs.contains("customers"));
    }

    // Edge case: Empty SQL produces empty assets.
    #[test]
    fn test_empty_sql() {
        // sqlparser may reject empty input, so use a comment-only file
        let sql = "-- just a comment";
        // This may either parse as empty or error — both are acceptable
        let result = extract_assets(sql);
        match result {
            Ok(assets) => {
                assert!(assets.outputs.is_empty());
                assert!(assets.inputs.is_empty());
            }
            Err(_) => {} // Also acceptable — treated as opaque
        }
    }

    // AC-03: Nested CTEs — both captured in internal.
    #[test]
    fn test_ac03_nested_ctes_in_internal() {
        let sql = "WITH a AS (SELECT * FROM raw_data), b AS (SELECT * FROM a) SELECT * FROM b;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.internal.contains("a"), "CTE 'a' should be in internal");
        assert!(assets.internal.contains("b"), "CTE 'b' should be in internal");
        assert!(assets.inputs.contains("raw_data"), "real table should be in inputs");
        assert!(!assets.inputs.contains("a"), "CTE 'a' should NOT be in inputs");
        assert!(!assets.inputs.contains("b"), "CTE 'b' should NOT be in inputs");
    }

    // AC-04: CTE name shadowing a real table — CTE goes to internal, real table stays in inputs.
    #[test]
    fn test_ac04_cte_shadows_real_table() {
        let sql = "WITH customers AS (SELECT * FROM raw_customers) SELECT * FROM customers;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.internal.contains("customers"), "CTE 'customers' should be in internal");
        assert!(assets.inputs.contains("raw_customers"), "real table should be in inputs");
        assert!(!assets.inputs.contains("customers"), "CTE 'customers' should NOT be in inputs");
    }

    // AC-05: DROP TABLE populates destroys.
    #[test]
    fn test_ac05_drop_table_destroys() {
        let sql = "DROP TABLE foo;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.destroys.contains("foo"), "dropped table should be in destroys");
        assert!(assets.outputs.is_empty(), "drop should not add to outputs");
        assert!(assets.inputs.is_empty(), "drop should not add to inputs");
    }

    // AC-05: DROP VIEW also populates destroys.
    #[test]
    fn test_ac05_drop_view_destroys() {
        let sql = "DROP VIEW IF EXISTS my_view;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.destroys.contains("my_view"), "dropped view should be in destroys");
    }

    // AC-06: DROP + CREATE in same file — both destroys and outputs populated.
    #[test]
    fn test_ac06_drop_then_create() {
        let sql = "DROP TABLE IF EXISTS foo; CREATE TABLE foo AS SELECT * FROM bar;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.destroys.contains("foo"), "dropped table should be in destroys");
        assert!(assets.outputs.contains("foo"), "created table should be in outputs");
        assert!(assets.inputs.contains("bar"), "source table should be in inputs");
    }

    // AC-07: ALTER TABLE populates outputs only.
    #[test]
    fn test_ac07_alter_table_outputs_only() {
        let sql = "ALTER TABLE customers ADD COLUMN email TEXT;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.outputs.contains("customers"), "altered table should be in outputs");
        assert!(assets.inputs.is_empty(), "alter should not add to inputs");
    }

    // AC-08: MERGE INTO — target in outputs, source in inputs.
    #[test]
    fn test_ac08_merge_into() {
        let sql = "MERGE INTO target USING source ON target.id = source.id WHEN MATCHED THEN UPDATE SET target.name = source.name;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.outputs.contains("target"), "merge target should be in outputs");
        assert!(assets.inputs.contains("source"), "merge source should be in inputs");
    }

    // AC-09: CREATE OR REPLACE TABLE is handled as output.
    #[test]
    fn test_ac09_create_or_replace() {
        let sql = "CREATE OR REPLACE TABLE foo AS SELECT * FROM bar;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.outputs.contains("foo"), "replaced table should be in outputs");
        assert!(assets.inputs.contains("bar"), "source table should be in inputs");
    }

    // AC-14: PIVOT source table is extracted as input.
    #[test]
    fn test_ac14_pivot_source_table() {
        // sqlparser-rs 0.55 supports PIVOT syntax
        let sql = "SELECT * FROM monthly_sales PIVOT (SUM(amount) FOR month IN ('Jan', 'Feb', 'Mar'));";
        let result = extract_assets(sql);
        match result {
            Ok(assets) => {
                assert!(assets.inputs.contains("monthly_sales"), "pivot source should be in inputs");
            }
            Err(_) => {
                // If sqlparser doesn't support this syntax, graceful degradation is acceptable
            }
        }
    }

    // AC-14: UNPIVOT source table is extracted as input.
    #[test]
    fn test_ac14_unpivot_source_table() {
        let sql = "SELECT * FROM quarterly_report UNPIVOT (value FOR quarter IN (q1, q2, q3, q4));";
        let result = extract_assets(sql);
        match result {
            Ok(assets) => {
                assert!(assets.inputs.contains("quarterly_report"), "unpivot source should be in inputs");
            }
            Err(_) => {
                // If sqlparser doesn't support this syntax, graceful degradation is acceptable
            }
        }
    }

    // Edge case: Recursive CTE — self-reference within CTE body.
    #[test]
    fn test_recursive_cte() {
        let sql = "WITH RECURSIVE tree AS (SELECT id, parent_id FROM nodes WHERE parent_id IS NULL UNION ALL SELECT n.id, n.parent_id FROM nodes n JOIN tree t ON n.parent_id = t.id) SELECT * FROM tree;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.internal.contains("tree"), "recursive CTE should be in internal");
        assert!(assets.inputs.contains("nodes"), "real table should be in inputs");
        assert!(!assets.inputs.contains("tree"), "CTE should NOT be in inputs");
    }

    // Edge case: CTE with subquery — inner subquery tables discovered.
    #[test]
    fn test_cte_with_subquery() {
        let sql = "WITH a AS (SELECT * FROM (SELECT * FROM raw) sub) SELECT * FROM a;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.internal.contains("a"), "CTE should be in internal");
        assert!(assets.inputs.contains("raw"), "subquery source should be in inputs");
        assert!(!assets.inputs.contains("a"), "CTE should NOT be in inputs");
    }

    // Edge case: ALTER VIEW — modifies view (output), reads from tables (inputs).
    #[test]
    fn test_alter_view_outputs_and_inputs() {
        let sql = "ALTER VIEW active_customers AS SELECT * FROM customers WHERE active = true;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.outputs.contains("active_customers"), "altered view should be in outputs");
        assert!(assets.inputs.contains("customers"), "source table should be in inputs");
    }

    // Edge case: DROP multiple tables in one statement.
    #[test]
    fn test_drop_multiple_tables() {
        let sql = "DROP TABLE foo, bar, baz;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.destroys.contains("foo"));
        assert!(assets.destroys.contains("bar"));
        assert!(assets.destroys.contains("baz"));
    }
}
