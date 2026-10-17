//! What an application that opens a Protocol written by someone else needs to read from
//! it, with arc's own rules: where its database is, which tables its newest run made and
//! what they held, which run its report names, and the versions its log records.
//!
//! Each function reads the filesystem and writes nothing.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::contract::{Contract, newest_contract, runs_dir};
use crate::error::{Error, Result};
use crate::history::{LOG_FILENAME, LogLine, parse_log_line};
use crate::manifest::Manifest;
use crate::report::{REPORT_FILENAME, RUN_LINE_PREFIX};

/// Where the Protocol in `dir` keeps its working database: the `db:` its manifest
/// names, else a file in arc's data folder. The file may not exist yet.
pub fn protocol_db_path(dir: &Path) -> Result<PathBuf> {
    Manifest::load(dir)?.db_path(dir)
}

/// The tables a run made or read, each with the content hash its contract records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunTables {
    /// The run the tables are of.
    pub run_id: String,
    /// Each table's name and its content hash, `None` when the run could not measure it.
    pub tables: BTreeMap<String, Option<String>>,
}

/// The tables of the run `run_id` names, or of the newest run when it is `None`, from
/// the run's contract. A run with no contract, and a Protocol that has not run, are
/// refused.
pub fn run_tables(dir: &Path, run_id: Option<&str>) -> Result<RunTables> {
    let runs = runs_dir(dir);
    let path = match run_id {
        Some(id) => {
            let named = Path::new(id).components().count() == 1
                && !id.contains(['/', '\\'])
                && id != "."
                && id != "..";
            let path = runs.join(format!("{id}.json"));
            if !named || !path.is_file() {
                return Err(crate::record::refused(format!(
                    "there is no run {id} in {}",
                    runs.display()
                )));
            }
            path
        }
        None => newest_contract(&runs)
            .ok_or_else(|| crate::record::refused(format!("{} has not run yet", dir.display())))?,
    };
    let json = fs::read_to_string(&path).map_err(|e| Error::FileRead {
        path: path.clone(),
        source: e,
    })?;
    let contract: Contract = serde_json::from_str(&json).map_err(|e| {
        crate::record::refused(format!(
            "the run contract {} is unreadable: {e}",
            path.display()
        ))
    })?;
    let tables = contract
        .assets
        .into_iter()
        .filter(|asset| asset.kind == "table")
        .map(|asset| (asset.name, asset.content_hash))
        .collect();
    Ok(RunTables {
        run_id: contract.run.run_id,
        tables,
    })
}

/// The run id the `report.md` in `dir` names in its Run section, or `None` when there
/// is no report or it names no run.
pub fn report_run(dir: &Path) -> Result<Option<String>> {
    let path = dir.join(REPORT_FILENAME);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::FileRead { path, source: e }),
    };
    // The Run section is arc's and comes last, so the last such line is its own even
    // when the prose quotes one.
    Ok(text.lines().rev().find_map(|line| {
        let id = line.strip_prefix(RUN_LINE_PREFIX)?.strip_prefix('`')?;
        Some(id.strip_suffix('`')?.to_string())
    }))
}

/// The lines of the log the Protocol in `dir` keeps, in file order. A line that is not
/// one of the log's is skipped, and a Protocol with no log has none.
pub fn read_log(dir: &Path) -> Result<Vec<LogLine>> {
    let path = dir.join(LOG_FILENAME);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::FileRead { path, source: e }),
    };
    Ok(text.lines().filter_map(parse_log_line).collect())
}
