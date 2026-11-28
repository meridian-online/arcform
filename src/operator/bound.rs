//! A data bound, read in one place.
//!
//! A parameter's `x-at-most` says that its value may not exceed a number worked out on
//! the data a step reads: `rows_of` names the `with:` key whose Parquet is counted, and
//! `minus` what is taken off its row count. Two readers need that number. `arc run`
//! refuses a set value above it before the operator's script starts, and a description
//! of a step shows it beside the value. Both call [`work_out`], so the bound the run
//! enforces is the bound a description shows, and both read the rule from the
//! operator's own schema, so the schema cannot state one bound while the run enforces
//! another.

use std::path::Path;

use super::sql_lit;

/// The bound a rule gives on one step's `with:` block, as worked out from the files on
/// disk.
///
/// The run's refusal reads `at_most`, `path` and `rows`; `role` and `reason` are there
/// for a description of the step, which shows what was counted or why nothing was.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct WorkedBound {
    /// The largest value the parameter may take, or `None` when the input could not be
    /// counted.
    pub at_most: Option<u64>,
    /// The `with:` key whose file is counted (`rows_of`).
    pub role: String,
    /// The path that key names, as the manifest writes it. Empty when the key is unset.
    pub path: String,
    /// The rows the file holds, or `None` when it could not be counted.
    pub rows: Option<u64>,
    /// Why the input was not counted, when it was not. `None` whenever `at_most` is
    /// set.
    pub reason: Option<String>,
}

/// Work out the bound `rule`, a property's `x-at-most` value, gives on `with`, the
/// step's `with:` block, with paths resolved against `dir`.
///
/// `None` when `rule` is not an `x-at-most` object. Otherwise a bound, which says why
/// when it cannot be counted: the key is unset, the path is a URL or a pattern rather
/// than one local file, the file does not exist yet (an earlier step may write it), or
/// it cannot be read as Parquet.
pub(crate) fn work_out(
    rule: &serde_json::Value,
    with: &serde_json::Value,
    dir: &Path,
) -> Option<WorkedBound> {
    let role = rule.get("rows_of")?.as_str()?.to_string();
    let minus = rule.get("minus").and_then(|m| m.as_u64()).unwrap_or(0);
    let uncounted = |path: String, reason: String| WorkedBound {
        at_most: None,
        role: role.clone(),
        path,
        rows: None,
        reason: Some(reason),
    };

    let Some(path) = with.get(&role).and_then(|p| p.as_str()) else {
        return Some(uncounted(
            String::new(),
            format!("`{role}` is not set, so there are no rows to count"),
        ));
    };
    let path = path.to_string();
    if path.contains("://") || path.contains(['*', '?', '[']) {
        return Some(uncounted(
            path.clone(),
            format!("{path} is not a local file, so arc cannot count its rows before the run"),
        ));
    }
    let local = dir.join(&path);
    if !local.exists() {
        return Some(uncounted(
            path.clone(),
            format!(
                "{path} does not exist yet, so its rows cannot be counted until the step \
                 that writes it has run"
            ),
        ));
    }
    let Some(rows) = parquet_row_count(&local) else {
        return Some(uncounted(
            path.clone(),
            format!("{path} cannot be read as Parquet, so its rows cannot be counted"),
        ));
    };
    Some(WorkedBound {
        at_most: Some(rows.saturating_sub(minus)),
        role,
        path,
        rows: Some(rows),
        reason: None,
    })
}

/// The sentence `arc run` refuses with when `operator`'s `key` is set to `asked`, above
/// the counted bound `bound`.
///
/// The default it names, and whether the bound applies to it (`bounds_default`), come
/// from the operator's schema. `bound` must have been counted: its `at_most` and `rows`
/// set.
pub(crate) fn past_bound_sentence(
    operator: &str,
    key: &str,
    asked: u64,
    bound: &WorkedBound,
) -> String {
    let at_most = bound.at_most.unwrap_or(0);
    let rows = bound.rows.unwrap_or(0);
    let property = super::with_schema(operator)
        .map(|schema| schema["properties"][key].clone())
        .unwrap_or_default();
    let minus = rows.saturating_sub(at_most);
    let measure = match minus {
        0 => format!("the {rows} rows of {}", bound.path),
        1 => format!("one below the {rows} rows of {}", bound.path),
        n => format!("{n} below the {rows} rows of {}", bound.path),
    };
    let why = match (operator, key) {
        ("umap_project", "neighbors") => {
            " — UMAP cannot place a point against more rows than there are besides it"
        }
        _ => "",
    };
    let bounds_default = property["x-at-most"]["bounds_default"]
        .as_bool()
        .unwrap_or(false);
    let unset = match &property["default"] {
        serde_json::Value::Null => String::new(),
        default if bounds_default => {
            format!(", or leave it unset to take {default} or {at_most}, whichever is smaller")
        }
        default => format!(", or leave it unset to take {default}"),
    };
    format!(
        "{operator}: `{key}: {asked}` is above {at_most}, {measure}{why}. Set `{key}:` to \
         {at_most} or less{unset}"
    )
}

/// The number of rows in the Parquet at `path`, read in process, or `None` when it
/// cannot be read. It counts what `umap_project.py` counts: every row `read_parquet`
/// returns for the same path.
pub(crate) fn parquet_row_count(path: &Path) -> Option<u64> {
    let conn = duckdb::Connection::open_in_memory().ok()?;
    let rows: i64 = conn
        .query_row(
            &format!(
                "SELECT count(*) FROM read_parquet('{}')",
                sql_lit(&path.display().to_string())
            ),
            [],
            |row| row.get(0),
        )
        .ok()?;
    u64::try_from(rows).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn parquet_of_rows(dir: &Path, name: &str, rows: u64) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT range AS id, range * 0.5 AS x FROM range({rows})) TO '{}' \
             (FORMAT parquet);",
            sql_lit(&path.display().to_string())
        ))
        .unwrap();
        path
    }

    /// `umap_project`'s own `neighbors` rule, as its schema states it.
    fn neighbors_rule() -> serde_json::Value {
        super::super::with_schema("umap_project").unwrap()["properties"]["neighbors"]["x-at-most"]
            .clone()
    }

    #[test]
    fn forty_eight_rows_bound_neighbors_at_47() {
        let dir = tempfile::tempdir().unwrap();
        parquet_of_rows(dir.path(), "build/in.parquet", 48);
        let bound = work_out(
            &neighbors_rule(),
            &json!({ "input": "build/in.parquet", "columns": ["x"] }),
            dir.path(),
        )
        .expect("neighbors has an x-at-most rule");
        assert_eq!(
            bound,
            WorkedBound {
                at_most: Some(47),
                role: "input".to_string(),
                path: "build/in.parquet".to_string(),
                rows: Some(48),
                reason: None,
            }
        );
    }

    #[test]
    fn a_missing_input_has_no_bound_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let bound = work_out(
            &neighbors_rule(),
            &json!({ "input": "build/in.parquet" }),
            dir.path(),
        )
        .unwrap();
        assert_eq!((bound.at_most, bound.rows), (None, None));
        let reason = bound.reason.expect("an uncounted bound says why");
        assert!(reason.contains("does not exist yet"), "{reason}");
    }

    #[test]
    fn a_remote_input_is_not_counted() {
        let dir = tempfile::tempdir().unwrap();
        for path in ["https://example.org/a.parquet", "build/part-*.parquet"] {
            let bound = work_out(&neighbors_rule(), &json!({ "input": path }), dir.path()).unwrap();
            assert_eq!((bound.at_most, bound.rows), (None, None), "{path}");
            let reason = bound.reason.expect("an uncounted bound says why");
            assert!(reason.contains("not a local file"), "{reason}");
        }
    }

    #[test]
    fn an_unset_input_or_unreadable_file_is_not_counted() {
        let dir = tempfile::tempdir().unwrap();
        let unset = work_out(&neighbors_rule(), &json!({}), dir.path()).unwrap();
        assert_eq!(unset.at_most, None);
        assert!(unset.reason.unwrap().contains("not set"));

        std::fs::write(dir.path().join("not.parquet"), "a,b\n1,2\n").unwrap();
        let unreadable = work_out(
            &neighbors_rule(),
            &json!({ "input": "not.parquet" }),
            dir.path(),
        )
        .unwrap();
        assert_eq!(unreadable.at_most, None);
        assert!(
            unreadable
                .reason
                .unwrap()
                .contains("cannot be read as Parquet")
        );
    }

    #[test]
    fn a_value_that_is_not_a_rule_has_no_bound() {
        let dir = tempfile::tempdir().unwrap();
        let with = json!({ "input": "in.parquet" });
        assert_eq!(work_out(&serde_json::Value::Null, &with, dir.path()), None);
        assert_eq!(work_out(&json!({ "minus": 1 }), &with, dir.path()), None);
    }

    #[test]
    fn the_sentence_is_the_runs() {
        let bound = WorkedBound {
            at_most: Some(47),
            role: "input".to_string(),
            path: "build/in.parquet".to_string(),
            rows: Some(48),
            reason: None,
        };
        assert_eq!(
            past_bound_sentence("umap_project", "neighbors", 48, &bound),
            "umap_project: `neighbors: 48` is above 47, one below the 48 rows of \
             build/in.parquet — UMAP cannot place a point against more rows than there \
             are besides it. Set `neighbors:` to 47 or less, or leave it unset to take 15 \
             or 47, whichever is smaller"
        );
    }

    /// The sentence states how far below the row count the bound sits, and names the
    /// default only as the schema states it: with the bound applied when
    /// `bounds_default` says so, alone when it does not, and not at all when there is
    /// none.
    #[test]
    fn the_sentence_follows_the_rule_and_the_schema() {
        let counted = |at_most, rows| WorkedBound {
            at_most: Some(at_most),
            role: "input".to_string(),
            path: "in.parquet".to_string(),
            rows: Some(rows),
            reason: None,
        };
        assert_eq!(
            past_bound_sentence("umap_project", "min_dist", 9, &counted(8, 8)),
            "umap_project: `min_dist: 9` is above 8, the 8 rows of in.parquet. Set \
             `min_dist:` to 8 or less, or leave it unset to take 0.1"
        );
        assert_eq!(
            past_bound_sentence("umap_project", "absent", 9, &counted(6, 8)),
            "umap_project: `absent: 9` is above 6, 2 below the 8 rows of in.parquet. \
             Set `absent:` to 6 or less"
        );
    }

    #[test]
    fn parquet_row_count_counts_every_row_and_reads_none_from_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            parquet_row_count(&parquet_of_rows(dir.path(), "in.parquet", 48)),
            Some(48)
        );
        assert_eq!(
            parquet_row_count(&parquet_of_rows(dir.path(), "empty.parquet", 0)),
            Some(0)
        );
        assert_eq!(parquet_row_count(&dir.path().join("absent.parquet")), None);
    }
}
