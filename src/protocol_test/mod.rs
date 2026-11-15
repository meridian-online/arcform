//! `arc test`: run a Protocol against checked-in fixture files in place of its fetches.

mod fixture;
mod plan;
mod report;
mod run;
mod spec;

use std::path::Path;

use crate::asset::AssetGraph;
use crate::engine::DuckDbEngine;
use crate::error::{Error, Result};
use crate::manifest::Manifest;

pub(crate) use report::{junit, print_result};
pub(crate) use run::{Outcome, TestResult};
pub(crate) use spec::{ProtocolTest, TESTS_FILENAME, load_tests};

/// Run the tests in the Protocol in `dir`, one after another in `tests.yaml` order, handing
/// each result to `report` as it comes: every test, or only the one called `only`. Returns
/// the Protocol's name with the results. A `tests.yaml` or manifest that cannot be read, or
/// a name no test has, stops before any test runs; a test that fails, or is refused, does
/// not stop the rest.
pub(crate) fn run_tests(
    dir: &Path,
    only: Option<&str>,
    mut report: impl FnMut(&TestResult),
) -> Result<(String, Vec<TestResult>)> {
    let mut tests = load_tests(dir)?;
    if let Some(name) = only {
        if !tests.iter().any(|t| t.name == name) {
            let names: Vec<&str> = tests.iter().map(|t| t.name.as_str()).collect();
            return Err(Error::TestFile {
                path: dir.join(TESTS_FILENAME),
                reason: format!(
                    "no test is named '{name}'; the tests are: {}",
                    names.join(", ")
                ),
            });
        }
        tests.retain(|t| t.name == name);
    }
    let manifest = Manifest::load(dir)?;
    let graph = AssetGraph::build(&manifest, dir);
    let mut results = Vec::with_capacity(tests.len());
    for test in &tests {
        let result = run::run_test(dir, &manifest, &graph, test, &DuckDbEngine);
        report(&result);
        results.push(result);
    }
    Ok((manifest.name.clone(), results))
}
