//! Planning one test against the manifest: which steps its fixtures replace, which
//! steps never run, and whether anything left would reach the network.

use std::collections::BTreeSet;
use std::path::Path;

use indexmap::IndexMap;

use crate::asset::{AssetGraph, StepAssets};
use crate::asset_kind::{AssetKind, is_remote};
use crate::manifest::{Manifest, Step};
use crate::operator::Operator;

use super::ProtocolTest;

/// What a test changes in its copy of the Protocol before it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestPlan {
    /// Steps taken out of the copy's manifest, in manifest order: every fixtured step
    /// and every publishing step.
    pub removed_steps: Vec<String>,
    /// Fixture files to copy into place, in `fixtures:` order.
    pub placements: Vec<Placement>,
}

/// One fixture file and where it lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Placement {
    /// The checked-in file, relative to the Protocol's folder.
    pub fixture: String,
    /// The output it stands in for, relative to the Protocol's folder, spelt as the
    /// manifest declares it so the file lands where the step reading it looks.
    pub dest: String,
    /// The `break:` statements to run on it once placed, if any.
    pub statements: Option<String>,
}

/// Work out what `test` replaces and removes in the Protocol whose manifest and graph
/// are given, or the refusal that makes it a failing test.
///
/// A fixture key is a step name first, then an output path (matched whatever its
/// case). A step-name key needs a step that writes exactly one file; an output-path key
/// needs a step that writes that file, and every file that step writes must then be
/// keyed, since a fixtured step does not run. A fixture stands in for a file, so a step
/// that makes a table cannot have one, and a publishing step never runs in a test, so
/// it cannot have one either. What is left must not reach the network: a fetch step
/// with no fixture, or a SQL step reading or writing a URL, refuses the test.
///
/// `dir` is the Protocol's folder; every path in the plan is relative to it. Hooks are
/// not in the graph, so a `sql:` hook's file is read from it and refused the same way
/// when it reads or writes a URL; any other hook runs as written.
pub(crate) fn plan_test(
    manifest: &Manifest,
    graph: &AssetGraph,
    dir: &Path,
    test: &ProtocolTest,
) -> std::result::Result<TestPlan, String> {
    let no_assets = StepAssets::default();
    let assets = |step: &Step| graph.steps.get(&step.name).unwrap_or(&no_assets);
    let declared = |step: &Step, lowered: &str| {
        assets(step)
            .declared_case
            .get(lowered)
            .and_then(|spellings| spellings.iter().next())
            .cloned()
            .unwrap_or_else(|| lowered.to_string())
    };
    let file_outputs = |step: &Step| -> Vec<String> {
        let a = assets(step);
        a.produces
            .iter()
            .filter(|p| a.declared_kind.get(*p) == Some(&AssetKind::File))
            .cloned()
            .collect()
    };
    // A fixture cannot stand in for a publishing step, nor for one that makes a table.
    let refuse_unfixturable = |key: &str, step: &Step| -> std::result::Result<(), String> {
        if operator(step).is_some_and(|op| op.publishes()) {
            return Err(format!(
                "fixture '{key}': step '{}' publishes, and a publishing step never runs in a \
                 test; remove the fixture",
                step.name
            ));
        }
        let a = assets(step);
        if let Some(table) = a
            .produces
            .iter()
            .find(|p| a.declared_kind.get(*p) == Some(&AssetKind::Table))
        {
            return Err(format!(
                "fixture '{key}': step '{}' makes the table {}, and a fixture stands in for a \
                 file; give the fixture to the step that writes the file it reads",
                step.name,
                declared(step, table)
            ));
        }
        Ok(())
    };

    // Each fixtured step, with the lowercased outputs its fixtures stand in for.
    let mut keyed: IndexMap<&str, BTreeSet<String>> = IndexMap::new();
    let mut placements = Vec::new();
    for (key, fixture) in &test.fixtures {
        let (step, output) = if let Some(step) = manifest.steps.iter().find(|s| s.name == *key) {
            refuse_unfixturable(key, step)?;
            match file_outputs(step).as_slice() {
                [one] => (step, one.clone()),
                [] => {
                    return Err(format!(
                        "fixture '{key}': step '{key}' writes no file for a fixture to stand in for"
                    ));
                }
                many => {
                    let list: Vec<String> = many.iter().map(|p| declared(step, p)).collect();
                    return Err(format!(
                        "fixture '{key}': step '{key}' writes {} files ({}); key a fixture by \
                         each output path instead",
                        many.len(),
                        list.join(", ")
                    ));
                }
            }
        } else {
            let lowered = key.to_lowercase();
            let Some(step) = manifest
                .steps
                .iter()
                .find(|s| assets(s).produces.contains(&lowered))
            else {
                return Err(format!(
                    "fixture '{key}' is neither a step nor a file a step writes"
                ));
            };
            refuse_unfixturable(key, step)?;
            if assets(step).declared_kind.get(&lowered) != Some(&AssetKind::File) {
                return Err(format!(
                    "fixture '{key}': step '{}' does not write it as a single file, which is \
                     what a fixture stands in for",
                    step.name
                ));
            }
            (step, lowered)
        };
        let dest = declared(step, &output);
        if !keyed.entry(&step.name).or_default().insert(output) {
            return Err(format!(
                "fixture '{key}': {dest} of step '{}' already has a fixture",
                step.name
            ));
        }
        placements.push(Placement {
            fixture: fixture.clone(),
            dest,
            statements: test.breaks.get(key).cloned(),
        });
    }

    // A fixtured step does not run, so every file it writes has to come from a fixture.
    for step in &manifest.steps {
        let Some(outputs) = keyed.get(step.name.as_str()) else {
            continue;
        };
        let missing: Vec<String> = file_outputs(step)
            .iter()
            .filter(|p| !outputs.contains(*p))
            .map(|p| declared(step, p))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "step '{}' has a fixture for some of its outputs but not for {}; a fixtured \
                 step does not run, so give each a fixture",
                step.name,
                missing.join(", ")
            ));
        }
    }

    let mut removed_steps = Vec::new();
    for step in &manifest.steps {
        let op = operator(step);
        if keyed.contains_key(step.name.as_str()) || op.is_some_and(|op| op.publishes()) {
            removed_steps.push(step.name.clone());
            continue;
        }
        if let Some(op) = op.filter(|op| op.ingress()) {
            return Err(format!(
                "step '{}' fetches data ({}) and the test gives it no fixture; add one under \
                 fixtures:",
                step.name,
                op.name()
            ));
        }
        // A step DuckDB runs reads and writes what its SQL names; a URL there is the
        // network.
        if step.sql.is_some() || step.is_check() || step.is_measure() {
            let a = assets(step);
            refuse_remote(
                &format!("step '{}'", step.name),
                a.reads.iter().map(|r| declared(step, r)),
                a.produces.iter().map(|p| declared(step, p)),
            )?;
        }
    }

    // Hooks are not in the graph, so a `sql:` hook's file is read here the same way.
    let hooks = &manifest.hooks;
    for (slot, hook) in [
        ("on_init", &hooks.on_init),
        ("on_success", &hooks.on_success),
        ("on_failure", &hooks.on_failure),
        ("on_exit", &hooks.on_exit),
    ] {
        let Some(path) = hook.as_ref().and_then(|h| h.sql.as_deref()) else {
            continue;
        };
        // A file that cannot be read or parsed is the run's to report, not the plan's.
        let Ok(sql) = std::fs::read_to_string(dir.join(path)) else {
            continue;
        };
        let Ok(read) = crate::introspect::read_sql(&sql) else {
            continue;
        };
        let assets = read.merged();
        refuse_remote(
            &format!("hook '{slot}'"),
            assets.inputs.into_iter(),
            assets.outputs.into_iter(),
        )?;
    }

    Ok(TestPlan {
        removed_steps,
        placements,
    })
}

/// Refuse the first URL in what `who` (`step 'x'` or `hook 'on_init'`) reads, then in
/// what it writes.
fn refuse_remote(
    who: &str,
    mut reads: impl Iterator<Item = String>,
    mut writes: impl Iterator<Item = String>,
) -> std::result::Result<(), String> {
    if let Some(url) = reads.find(|r| is_remote(r)) {
        return Err(format!(
            "{who} reads {url}, which a test cannot reach; read it through a fetch step and \
             give that a fixture"
        ));
    }
    if let Some(url) = writes.find(|w| is_remote(w)) {
        return Err(format!("{who} writes {url}, which a test must not reach"));
    }
    Ok(())
}

/// The operator an `op:` step names, if it resolves.
fn operator(step: &Step) -> Option<&'static dyn Operator> {
    step.op
        .as_deref()
        .and_then(|op| crate::operator::resolve(op).ok())
}

#[cfg(all(test, feature = "http-fetch"))]
mod tests {
    use indexmap::IndexMap;

    use super::*;

    const FETCH: &str = "  - name: fetch\n    op: http_fetch@1\n    with:\n      url: https://example.com/a.csv\n      out: build/raw/a.csv\n";
    const PUBLISH: &str = "  - name: publish\n    op: ducklake_write@1\n    with:\n      public_base: https://lake.example/\n      catalog: catalog/test.ducklake\n      data_path: lake/\n      local_catalog: build/lake/test.ducklake\n      tables:\n        - table: demo.t\n          from: build/t.parquet\n          descriptor: t.datapackage.json\n      credential:\n        type: r2\n        bucket: test-bucket\n        env:\n          key_id: ARC_TEST_LAKE_KEY_ID\n          secret: ARC_TEST_LAKE_SECRET\n          account_id: ARC_TEST_LAKE_ACCOUNT_ID\n";
    const TWO_FILES: &str = "  - name: split\n    command: \"true\"\n    produces: [build/raw/a.csv, build/raw/b.csv]\n";
    const MAKES_TABLE: &str = "  - name: make\n    sql: make.sql\n";

    /// Plan `test` against a manifest of `steps`, with `files` written beside it.
    fn plan(
        steps: &str,
        files: &[(&str, &str)],
        test: &ProtocolTest,
    ) -> std::result::Result<TestPlan, String> {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
        }
        let manifest = Manifest::from_yaml_str(&format!("name: t\nsteps:\n{steps}")).unwrap();
        let graph = AssetGraph::build(&manifest, dir.path());
        plan_test(&manifest, &graph, dir.path(), test)
    }

    fn test_with(fixtures: &[(&str, &str)], breaks: &[(&str, &str)]) -> ProtocolTest {
        let map = |pairs: &[(&str, &str)]| -> IndexMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        ProtocolTest {
            name: "t".into(),
            risk: "r".into(),
            fixtures: map(fixtures),
            breaks: map(breaks),
            expect: None,
            checks: Vec::new(),
            params: IndexMap::new(),
            measure_history: None,
        }
    }

    fn placement(fixture: &str, dest: &str, statements: Option<&str>) -> Placement {
        Placement {
            fixture: fixture.into(),
            dest: dest.into(),
            statements: statements.map(str::to_string),
        }
    }

    #[test]
    fn the_four_fetch_operators_are_ingress_and_no_other_is() {
        let ingress: Vec<&str> = crate::operator::catalog_names()
            .into_iter()
            .filter(|name| crate::operator::resolve(name).unwrap().ingress())
            .collect();
        let expected = [
            "gleif_ra_fetch",
            "html_link_discover",
            "http_fetch",
            #[cfg(feature = "opendal")]
            "opendal_fetch",
        ];
        let mut ingress = ingress;
        ingress.sort_unstable();
        assert_eq!(ingress, expected);
    }

    #[test]
    fn a_fixture_by_step_name_replaces_the_fetch() {
        let test = test_with(&[("fetch", "testdata/a.csv")], &[]);
        let plan = plan(FETCH, &[], &test).unwrap();
        assert_eq!(plan.removed_steps, ["fetch"]);
        assert_eq!(
            plan.placements,
            [placement("testdata/a.csv", "build/raw/a.csv", None)]
        );
    }

    #[test]
    fn a_fixture_by_output_path_replaces_its_step() {
        // The key matches whatever its case; the file lands in the declared spelling.
        let steps = FETCH.replace("build/raw/a.csv", "build/raw/Data.csv");
        let test = test_with(&[("build/raw/data.csv", "testdata/a.csv")], &[]);
        let plan = plan(&steps, &[], &test).unwrap();
        assert_eq!(plan.removed_steps, ["fetch"]);
        assert_eq!(
            plan.placements,
            [placement("testdata/a.csv", "build/raw/Data.csv", None)]
        );
    }

    #[test]
    fn an_ingress_step_with_no_fixture_refuses_the_test() {
        let err = plan(FETCH, &[], &test_with(&[], &[])).unwrap_err();
        assert!(err.contains("step 'fetch'"), "{err}");
        assert!(err.contains("fetches data"), "{err}");
        assert!(err.contains("http_fetch"), "{err}");
    }

    #[test]
    fn a_sql_step_reading_a_url_refuses_the_test() {
        use crate::introspect::test_reading;
        // DuckDB v2.0.0's reading of this exact SQL is recorded, so no DuckDB starts.
        let sql = crate::duckdb_lineage::recorded_steps::URL_BARE;
        let err = test_reading::with(test_reading::recorded, || {
            plan(
                "  - name: load\n    sql: load.sql\n",
                &[("load.sql", sql)],
                &test_with(&[], &[]),
            )
        })
        .unwrap_err();
        assert!(err.contains("step 'load'"), "{err}");
        assert!(err.contains("https://x.org/a.csv"), "{err}");
        assert!(err.contains("a test cannot reach"), "{err}");
    }

    #[test]
    fn a_sql_step_writing_a_url_refuses_the_test() {
        use crate::introspect::test_reading;
        let sql = crate::duckdb_lineage::recorded_steps::URL_COPY;
        let err = test_reading::with(test_reading::recorded, || {
            plan(
                "  - name: export\n    sql: export.sql\n",
                &[("export.sql", sql)],
                &test_with(&[], &[]),
            )
        })
        .unwrap_err();
        assert_eq!(
            err,
            "step 'export' writes s3://b/out.parquet, which a test must not reach"
        );
    }

    #[test]
    fn a_sql_hook_reading_or_writing_a_url_refuses_the_test() {
        use crate::duckdb_lineage::recorded_steps::{URL_BARE, URL_COPY};
        use crate::introspect::test_reading;
        for (hook, sql, said) in [
            ("on_init", URL_BARE, "reads https://x.org/a.csv"),
            ("on_exit", URL_COPY, "writes s3://b/out.parquet"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("hook.sql"), sql).unwrap();
            let manifest = Manifest::from_yaml_str(&format!(
                "name: t\nhooks:\n  {hook}:\n    name: setup\n    sql: hook.sql\nsteps:\n  - name: s\n    command: \"true\"\n"
            ))
            .unwrap();
            let err = test_reading::with(test_reading::recorded, || {
                let graph = AssetGraph::build(&manifest, dir.path());
                plan_test(&manifest, &graph, dir.path(), &test_with(&[], &[]))
            })
            .unwrap_err();
            assert!(err.starts_with(&format!("hook '{hook}' {said}")), "{err}");
        }
    }

    #[test]
    fn publishing_steps_are_removed() {
        let test = test_with(&[("fetch", "testdata/a.csv")], &[]);
        let plan = plan(&format!("{FETCH}{PUBLISH}"), &[], &test).unwrap();
        assert_eq!(plan.removed_steps, ["fetch", "publish"]);
        assert_eq!(plan.placements.len(), 1);
    }

    #[test]
    fn a_step_with_two_file_outputs_needs_each_keyed_by_path() {
        let by_name = plan(TWO_FILES, &[], &test_with(&[("split", "x.csv")], &[])).unwrap_err();
        assert!(by_name.contains("step 'split'"), "{by_name}");
        assert!(by_name.contains("build/raw/a.csv"), "{by_name}");
        assert!(by_name.contains("build/raw/b.csv"), "{by_name}");

        let one = test_with(&[("build/raw/a.csv", "x.csv")], &[]);
        let err = plan(TWO_FILES, &[], &one).unwrap_err();
        assert!(err.contains("build/raw/b.csv"), "{err}");
        assert!(!err.contains("build/raw/a.csv"), "{err}");

        let both = test_with(
            &[("build/raw/a.csv", "x.csv"), ("build/raw/b.csv", "y.csv")],
            &[],
        );
        let plan = plan(TWO_FILES, &[], &both).unwrap();
        assert_eq!(plan.removed_steps, ["split"]);
        assert_eq!(
            plan.placements,
            [
                placement("x.csv", "build/raw/a.csv", None),
                placement("y.csv", "build/raw/b.csv", None)
            ]
        );
    }

    #[test]
    fn a_fixture_for_a_step_that_makes_a_table_is_refused() {
        let files = [("make.sql", "CREATE TABLE t AS SELECT 1 AS n;")];
        let err = plan(MAKES_TABLE, &files, &test_with(&[("make", "x.csv")], &[])).unwrap_err();
        assert!(err.contains("step 'make'"), "{err}");
        assert!(err.contains("table"), "{err}");
        let err = plan(MAKES_TABLE, &files, &test_with(&[("t", "x.csv")], &[])).unwrap_err();
        assert!(err.contains("table"), "{err}");
    }

    #[test]
    fn a_fixture_key_naming_nothing_is_refused() {
        let test = test_with(&[("fetch", "a.csv"), ("nowhere.csv", "b.csv")], &[]);
        let err = plan(FETCH, &[], &test).unwrap_err();
        assert!(err.contains("'nowhere.csv'"), "{err}");
        assert!(err.contains("neither a step nor"), "{err}");
    }

    #[test]
    fn a_fixtured_publish_step_is_refused() {
        let test = test_with(&[("fetch", "a.csv"), ("publish", "b.csv")], &[]);
        let err = plan(&format!("{FETCH}{PUBLISH}"), &[], &test).unwrap_err();
        assert!(err.contains("step 'publish'"), "{err}");
        assert!(err.contains("publishes"), "{err}");
    }

    #[test]
    fn the_break_travels_with_its_placement() {
        let test = test_with(
            &[("fetch", "testdata/a.csv")],
            &[("fetch", "DELETE FROM fixture WHERE row = 1")],
        );
        let plan = plan(FETCH, &[], &test).unwrap();
        assert_eq!(
            plan.placements,
            [placement(
                "testdata/a.csv",
                "build/raw/a.csv",
                Some("DELETE FROM fixture WHERE row = 1")
            )]
        );
    }
}
