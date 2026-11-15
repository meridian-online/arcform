//! `arc test`: run a Protocol against checked-in fixture files in place of its fetches.

mod fixture;
mod plan;
mod run;
mod spec;

use std::path::Path;

use crate::asset::AssetGraph;
use crate::engine::DuckDbEngine;
use crate::error::Result;
use crate::manifest::Manifest;

pub(crate) use run::{Outcome, TestResult};
pub(crate) use spec::{ProtocolTest, load_tests};

/// Run every test in the Protocol in `dir`, one after another in `tests.yaml` order,
/// handing each result to `report` as it comes. A `tests.yaml` or manifest that cannot be
/// read stops before any test runs; a test that fails, or is refused, does not stop the rest.
pub(crate) fn run_tests(
    dir: &Path,
    mut report: impl FnMut(&TestResult),
) -> Result<Vec<TestResult>> {
    let tests = load_tests(dir)?;
    let manifest = Manifest::load(dir)?;
    let graph = AssetGraph::build(&manifest, dir);
    let mut results = Vec::with_capacity(tests.len());
    for test in &tests {
        let result = run::run_test(dir, &manifest, &graph, test, &DuckDbEngine);
        report(&result);
        results.push(result);
    }
    Ok(results)
}

/// How `arc test` shows one result: `ok <name>`, or `FAIL <name>` with its risk line and
/// what happened, each line under it indented by two spaces.
pub(crate) fn render(result: &TestResult) -> String {
    match &result.outcome {
        Outcome::Passed => format!("ok {}\n", result.name),
        Outcome::Failed { what } => {
            let mut text = format!("FAIL {}\n  risk: {}\n", result.name, result.risk);
            for line in what.lines() {
                text.push_str("  ");
                text.push_str(line);
                text.push('\n');
            }
            text
        }
    }
}
