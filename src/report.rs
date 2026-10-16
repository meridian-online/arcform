//! The report `arc report write` and the `report_write` tool put beside a Protocol.
//!
//! The agent supplies the prose: the answer, its caveats, what to do next. Arc
//! appends what it knows from the newest run contract (or the one a run id
//! names): how the answer was derived, the charts and the run. Row counts and
//! content hashes are the contract's own, so the database is never read for them.
//! The panel check does open the Protocol's current database, read-only, and the
//! Charts section says so.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::contract::{Contract, runs_dir};
use crate::error::{Error, Result};
use crate::history::LocalHistory;
use crate::manifest::{MANIFEST_FILENAME, Manifest};
use crate::panel_check::check_panels_from;
use crate::protocol_files::replace_file_with_history;

/// The report's place in the Protocol's folder.
pub(crate) const REPORT_FILENAME: &str = "report.md";

/// Write `report.md` in the Protocol folder `dir`: `prose`, then the three
/// sections arc generates from the run contract, and record the file in its
/// history. The newest run's contract is used unless `run_id` names one.
///
/// Refused, with nothing written: a folder with no `arcform.yaml`, empty prose,
/// a Protocol that has not run, and a run id with no contract. Returns the
/// report's path relative to `dir`.
pub(crate) fn write_report(
    dir: &Path,
    prose: &str,
    run_id: Option<&str>,
    history: &LocalHistory,
) -> Result<PathBuf> {
    let refuse = |message: String| Error::ManifestValidation(message);
    if !dir.join(MANIFEST_FILENAME).is_file() {
        return Err(refuse(format!(
            "{} holds no {MANIFEST_FILENAME}; make the Protocol first",
            dir.display()
        )));
    }
    if prose.trim().is_empty() {
        return Err(refuse(
            "the report has no prose; write the answer, its caveats and the next steps".to_string(),
        ));
    }
    let contract = load_contract(dir, run_id)?;
    let (descriptions, manifest_note) = match Manifest::load(dir) {
        Ok(m) => (
            m.steps
                .into_iter()
                .filter_map(|s| s.description.map(|d| (s.name, d)))
                .collect::<std::collections::BTreeMap<_, _>>(),
            None,
        ),
        Err(e) => (Default::default(), Some(e.to_string())),
    };
    let panels = check_panels_from(dir).map_err(|e| e.to_string());

    let mut text = String::from(prose.trim_end());
    text.push_str("\n\n");
    derivation(
        &mut text,
        &contract,
        &descriptions,
        manifest_note.as_deref(),
    );
    charts(&mut text, dir, &panels);
    run_section(&mut text, &contract);

    let path = dir.join(REPORT_FILENAME);
    if !crate::protocol_files::resolves_inside(dir, &path)? {
        return Err(refuse(format!(
            "{REPORT_FILENAME} is outside the Protocol folder {}",
            dir.display()
        )));
    }
    replace_file_with_history(&path, &text, history)?;
    Ok(PathBuf::from(REPORT_FILENAME))
}

/// The contract of the run `run_id` names, or of the newest run.
fn load_contract(dir: &Path, run_id: Option<&str>) -> Result<Contract> {
    let refuse = |message: String| Error::ManifestValidation(message);
    let runs = runs_dir(dir);
    let path = match run_id {
        Some(id) => {
            let named = Path::new(id).components().count() == 1
                && !id.contains(['/', '\\'])
                && id != "."
                && id != "..";
            let path = runs.join(format!("{id}.json"));
            if !named || !path.is_file() {
                return Err(refuse(format!(
                    "there is no run {id} in {}; leave the run out for the newest",
                    runs.display()
                )));
            }
            path
        }
        None => newest_contract(&runs).ok_or_else(|| {
            refuse(format!(
                "{} has not run yet: run the Protocol first (arc run, or protocol_run) \
                 so the report can say how the answer was derived",
                dir.display()
            ))
        })?,
    };
    let json = fs::read_to_string(&path).map_err(|e| Error::FileRead {
        path: path.clone(),
        source: e,
    })?;
    serde_json::from_str(&json).map_err(|e| {
        refuse(format!(
            "the run contract {} is unreadable: {e}",
            path.display()
        ))
    })
}

/// The contract with the greatest run id. Ids begin with a sortable UTC timestamp,
/// so this is the newest run, wherever the files were copied or restored from.
fn newest_contract(runs: &Path) -> Option<PathBuf> {
    fs::read_dir(runs)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "json"))
        .max()
}

/// `text` on one line, so it cannot break a list item.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn rows(n: i64) -> String {
    format!("{n} row{}", if n == 1 { "" } else { "s" })
}

fn derivation(
    text: &mut String,
    contract: &Contract,
    descriptions: &std::collections::BTreeMap<String, String>,
    manifest_note: Option<&str>,
) {
    text.push_str("## How this was derived\n\n");
    if let Some(note) = manifest_note {
        let _ = writeln!(
            text,
            "Step descriptions from the Protocol were not read: {}\n",
            one_line(note)
        );
    }
    if contract.steps.is_empty() {
        text.push_str("The run had no steps.\n\n");
    }
    for (i, step) in contract.steps.iter().enumerate() {
        let described = step
            .narrative
            .doc
            .as_ref()
            .or(step.narrative.label.as_ref())
            .or(descriptions.get(&step.name));
        let _ = write!(text, "{}. `{}`", i + 1, step.name);
        if let Some(description) = described {
            let _ = write!(text, ": {}", one_line(description));
        }
        let _ = write!(text, " ({})", step.status.state);
        let made: Vec<String> = contract
            .assets
            .iter()
            .filter(|a| a.produced_by.as_deref() == Some(step.name.as_str()))
            .map(|a| match (&a.path, a.row_count) {
                (Some(path), Some(n)) => format!("`{path}` ({})", rows(n)),
                (Some(path), None) => format!("`{path}`"),
                (None, Some(n)) => format!("`{}` ({})", a.name, rows(n)),
                (None, None) => format!("`{}`", a.name),
            })
            .collect();
        if !made.is_empty() {
            let _ = write!(text, ", made {}", made.join(", "));
        }
        text.push('\n');
    }
    text.push('\n');
}

fn charts(
    text: &mut String,
    dir: &Path,
    panels: &std::result::Result<
        (bool, Vec<(PathBuf, Vec<crate::panel_check::PanelFinding>)>),
        String,
    >,
) {
    text.push_str("## Charts\n\n");
    let (from_database, panels) = match panels {
        Ok((from_database, panels)) => (*from_database, panels),
        Err(reason) => {
            let _ = writeln!(text, "Panels not checked: {}\n", one_line(reason));
            return;
        }
    };
    if panels.is_empty() {
        text.push_str("The Protocol has no panels.\n\n");
        return;
    }
    text.push_str(
        "Findings are checked against the Protocol's current database, not the run's.\n\n",
    );
    for (path, findings) in panels {
        let title = fs::read_to_string(dir.join(path))
            .ok()
            .and_then(|s| serde_yaml::from_str::<serde_yaml::Value>(&s).ok())
            .and_then(|v| v.get("title").and_then(|t| t.as_str()).map(one_line));
        let _ = write!(text, "- `{}`", path.display());
        if let Some(title) = title {
            let _ = write!(text, ": {title}");
        }
        text.push('\n');
        for finding in findings {
            let _ = writeln!(text, "  - Check: {}", one_line(&finding.to_string()));
        }
    }
    text.push('\n');
}

fn run_section(text: &mut String, contract: &Contract) {
    let run = &contract.run;
    text.push_str("## Run\n\n");
    let _ = writeln!(text, "- Run: `{}`", run.run_id);
    let _ = writeln!(
        text,
        "- Finished: {}",
        run.finished_at.as_deref().unwrap_or("not recorded")
    );
    let _ = writeln!(text, "- Outcome: {}", run.outcome);
    if !contract.assets.is_empty() {
        text.push_str("- Assets, by content hash:\n");
        for asset in &contract.assets {
            let _ = writeln!(
                text,
                "  - `{}`: {}",
                asset.id,
                asset.content_hash.as_deref().unwrap_or("not measured")
            );
        }
    }
}
