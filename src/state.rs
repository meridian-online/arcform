//! Run state tracking for selective execution.
//!
//! The `StateBackend` trait provides a pluggable interface for persisting
//! step execution state across runs. The default implementation uses DuckDB
//! tables co-located with the project's data.

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Record of a step's last execution state.
#[derive(Debug, Clone)]
pub struct StepState {
    /// SHA-256 hex digest of the SQL file contents (or, for an `op:` step, its
    /// operator ref + `with:` config) at last run.
    pub sql_hash: String,
    /// SHA-256 hex digest over every FILE asset this step produced, plus every file it
    /// reads that nothing in the manifest produces, as those files stood on disk right
    /// after the step's last success. Empty string for a row written before this field
    /// existed, or for a step with no file-typed produced/external-read assets — see
    /// [`crate::runner`]'s `produced_artifact_hash`, the only writer.
    pub artifact_hash: String,
    /// Result of last execution.
    pub status: StepStatus,
}

/// Status of a step's last execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepStatus {
    Success,
    Failed,
}

impl StepStatus {
    pub fn as_str(&self) -> &str {
        match self {
            StepStatus::Success => "success",
            StepStatus::Failed => "failed",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "success" => StepStatus::Success,
            _ => StepStatus::Failed,
        }
    }
}

/// Why a step was skipped as fresh — the typed reason threaded out of staleness
/// computation and recorded per step in the run contract. It distinguishes the three
/// freshness mechanisms so history selectors (`state:modified+`, diff mode) can tell a
/// hash-clean skip from a precondition-driven one, rather than seeing a bare `[skip]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// SQL/op content hash unchanged and no preconditions gated the step.
    HashClean,
    /// The step's preconditions all evaluated fresh (`fresh`/`command` kinds).
    PreconditionFresh,
    /// A `modified_after` clock precondition judged the file still within its period.
    PreconditionModifiedAfter,
    /// A `tool` precondition found every external binary or artifact the step declares
    /// still identical to what it was when the step last ran. Distinct from
    /// `hash_clean`, which is silent about anything the step did not produce.
    PreconditionTool,
}

impl SkipReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            SkipReason::HashClean => "hash_clean",
            SkipReason::PreconditionFresh => "precondition_fresh",
            SkipReason::PreconditionModifiedAfter => "precondition_modified_after",
            SkipReason::PreconditionTool => "precondition_tool",
        }
    }
}

/// Compute SHA-256 hex digest of a byte slice.
pub fn content_hash(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    format!("{:x}", hasher.finalize())
}

/// Combined SHA-256 over a directory's contents, so a `Directory`-kind asset (a
/// `COPY … PARTITION_BY` target, or an `archive_extract` pattern-only `dest:`) is
/// answerable for drift the same way a `File` is answerable for its own bytes —
/// not skipped because it is a directory, and not satisfied by the directory node
/// merely existing while what is inside it changes underneath.
///
/// Walks every regular file in the tree (recursively, symlinks not followed),
/// hashes each one's bytes, then hashes the sorted `(relative_path, file_hash)`
/// pairs together. Sorted so the combined digest is independent of readdir order;
/// keyed on relative path so a file moving to a different name inside the tree
/// changes the digest even if no byte anywhere changed. `None` when any part of the
/// walk fails to read: the directory itself missing or permission-denied, a
/// subdirectory that cannot be listed, or a single child file whose bytes cannot be
/// read — each of those is an `.ok()?` in the walk below. That is the same
/// unconditional-staleness signal an
/// unreadable file already produces, so an absent or damaged tree forces a re-run
/// exactly as a deleted file does.
pub fn hash_directory_contents(dir: &Path) -> Option<String> {
    let mut entries: Vec<(String, String)> = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let read_dir = std::fs::read_dir(&current).ok()?;
        for entry in read_dir {
            let entry = entry.ok()?;
            let path = entry.path();
            let file_type = entry.file_type().ok()?;
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                let rel = path
                    .strip_prefix(dir)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/");
                let bytes = std::fs::read(&path).ok()?;
                entries.push((rel, content_hash(&bytes)));
            }
            // Symlinks and other file types: neither hashed nor descended into —
            // a directory tree with a dangling symlink still hashes deterministically
            // over its real files rather than erroring the whole asset.
        }
    }
    entries.sort();
    let mut hasher = Sha256::new();
    for (rel, hash) in &entries {
        hasher.update(rel.as_bytes());
        hasher.update(b"\0");
        hasher.update(hash.as_bytes());
        hasher.update(b"\n");
    }
    Some(format!("{:x}", hasher.finalize()))
}

/// Trait for persisting step execution state across runs.
pub trait StateBackend {
    /// Initialise the backend (create tables, etc.). Idempotent.
    fn init(&self) -> Result<()>;

    /// Get the last recorded state for a step, or None if never run.
    fn get_step_state(&self, step_name: &str) -> Result<Option<StepState>>;

    /// Record a step's execution result: its config hash, its combined produced/
    /// external-read artifact hash (see [`StepState::artifact_hash`]), and status.
    fn record_step(
        &self,
        step_name: &str,
        sql_hash: &str,
        artifact_hash: &str,
        status: StepStatus,
    ) -> Result<()>;

    /// Record the start of a pipeline run. Returns a run ID.
    fn start_run(&self) -> Result<String>;

    /// Record the completion of a pipeline run.
    fn finish_run(
        &self,
        run_id: &str,
        steps_executed: usize,
        outcome: &str,
        total_retries: usize,
    ) -> Result<()>;
}

/// DuckDB-backed state backend using the `duckdb` crate.
///
/// State tables are co-located in the project's database file.
/// Connection is opened/closed per operation to avoid file locking
/// conflicts with CLI-based step execution.
pub struct DuckDbStateBackend {
    db_path: std::path::PathBuf,
}

impl DuckDbStateBackend {
    pub fn new(db_path: &Path) -> Self {
        DuckDbStateBackend {
            db_path: db_path.to_path_buf(),
        }
    }

    fn open(&self) -> Result<duckdb::Connection> {
        // The Protocol declares the db path (e.g. `db: build/edgar.db`); its parent dir
        // may not exist yet on a fresh checkout (the retired opaque pipelines did
        // `mkdir -p build` in their first step). Create it so `arc run` is self-
        // sufficient — otherwise opening the state db fails before any step runs.
        if let Some(parent) = self.db_path.parent()
            && !parent.as_os_str().is_empty()
        {
            let _ = std::fs::create_dir_all(parent);
        }
        duckdb::Connection::open(&self.db_path).map_err(|e| Error::StateBackend(e.to_string()))
    }
}

impl StateBackend for DuckDbStateBackend {
    fn init(&self) -> Result<()> {
        let conn = self.open()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS _arcform_state (
                step_name TEXT PRIMARY KEY,
                sql_hash TEXT NOT NULL,
                last_run_at TIMESTAMP NOT NULL DEFAULT current_timestamp,
                status TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS _arcform_runs (
                run_id TEXT PRIMARY KEY,
                started_at TIMESTAMP NOT NULL DEFAULT current_timestamp,
                finished_at TIMESTAMP,
                steps_executed INTEGER,
                outcome TEXT
            );
            -- Migration: a state db created before artifact hashing existed has no
            -- such column. Idempotent, so every `init()` can run it unconditionally.
            -- The empty-string default never matches a freshly computed hash (see
            -- `produced_artifact_hash`), so the first post-upgrade run treats every
            -- step as artifact-stale exactly once and reseeds it — safe because it
            -- fails toward re-running, never toward skipping.
            ALTER TABLE _arcform_state ADD COLUMN IF NOT EXISTS artifact_hash TEXT DEFAULT '';",
        )
        .map_err(|e| Error::StateBackend(e.to_string()))?;
        Ok(())
    }

    fn get_step_state(&self, step_name: &str) -> Result<Option<StepState>> {
        let conn = self.open()?;
        let mut stmt = conn
            .prepare(
                "SELECT sql_hash, artifact_hash, status FROM _arcform_state WHERE step_name = ?1",
            )
            .map_err(|e| Error::StateBackend(e.to_string()))?;

        let result = stmt.query_row([step_name], |row| {
            let hash: String = row.get(0)?;
            let artifact_hash: String = row.get(1)?;
            let status: String = row.get(2)?;
            Ok(StepState {
                sql_hash: hash,
                artifact_hash,
                status: StepStatus::from_str(&status),
            })
        });

        match result {
            Ok(state) => Ok(Some(state)),
            Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(Error::StateBackend(e.to_string())),
        }
    }

    fn record_step(
        &self,
        step_name: &str,
        sql_hash: &str,
        artifact_hash: &str,
        status: StepStatus,
    ) -> Result<()> {
        let conn = self.open()?;
        conn.execute(
            "INSERT OR REPLACE INTO _arcform_state (step_name, sql_hash, artifact_hash, last_run_at, status)
             VALUES (?1, ?2, ?3, current_timestamp, ?4)",
            duckdb::params![step_name, sql_hash, artifact_hash, status.as_str()],
        )
        .map_err(|e| Error::StateBackend(e.to_string()))?;
        Ok(())
    }

    fn start_run(&self) -> Result<String> {
        let run_id = format!("{}-{}", timestamp_id(), &uuid_simple());
        let conn = self.open()?;
        conn.execute(
            "INSERT INTO _arcform_runs (run_id, started_at) VALUES (?1, current_timestamp)",
            [&run_id],
        )
        .map_err(|e| Error::StateBackend(e.to_string()))?;
        Ok(run_id)
    }

    fn finish_run(
        &self,
        run_id: &str,
        steps_executed: usize,
        outcome: &str,
        total_retries: usize,
    ) -> Result<()> {
        let conn = self.open()?;
        conn.execute(
            "UPDATE _arcform_runs SET finished_at = current_timestamp, steps_executed = ?1, outcome = ?2 WHERE run_id = ?3",
            duckdb::params![steps_executed as i64, outcome, run_id],
        )
        .map_err(|e| Error::StateBackend(e.to_string()))?;
        // total_retries is the pipeline-wide roll-up; the durable per-step breakdown
        // (attempts + duration_sec per step) now lives in the run contract JSON, so this
        // aggregate is not persisted to the state table.
        let _ = total_retries;
        Ok(())
    }
}

/// Compact timestamp string for run IDs (YYYYMMDD-HHMMSS in UTC).
fn timestamp_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Convert epoch seconds to date-time components.
    // Simplified UTC conversion — no leap-second handling needed for IDs.
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Days since 1970-01-01 to Y-M-D.
    let (year, month, day) = days_to_date(days);
    format!("{year:04}{month:02}{day:02}-{hours:02}{minutes:02}{seconds:02}")
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_date(days: u64) -> (u64, u64, u64) {
    // Algorithm from Howard Hinnant's date library (public domain).
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Simple UUID-like string (no external dependency).
fn uuid_simple() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    format!("{:08x}", nanos)
}

#[cfg(test)]
pub mod mock {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// A recorded run: `(run_id, Option<(steps_executed, outcome)>)`.
    type MockRun = (String, Option<(usize, String)>);

    /// Mock state backend for testing.
    pub struct MockStateBackend {
        pub states: RefCell<HashMap<String, StepState>>,
        pub runs: RefCell<Vec<MockRun>>,
        pub init_called: RefCell<bool>,
        /// Total retries recorded by the last finish_run call.
        pub total_retries: std::cell::Cell<usize>,
    }

    impl MockStateBackend {
        pub fn new() -> Self {
            MockStateBackend {
                states: RefCell::new(HashMap::new()),
                runs: RefCell::new(Vec::new()),
                init_called: RefCell::new(false),
                total_retries: std::cell::Cell::new(0),
            }
        }
    }

    impl StateBackend for MockStateBackend {
        fn init(&self) -> Result<()> {
            *self.init_called.borrow_mut() = true;
            Ok(())
        }

        fn get_step_state(&self, step_name: &str) -> Result<Option<StepState>> {
            Ok(self.states.borrow().get(step_name).cloned())
        }

        fn record_step(
            &self,
            step_name: &str,
            sql_hash: &str,
            artifact_hash: &str,
            status: StepStatus,
        ) -> Result<()> {
            self.states.borrow_mut().insert(
                step_name.to_string(),
                StepState {
                    sql_hash: sql_hash.to_string(),
                    artifact_hash: artifact_hash.to_string(),
                    status,
                },
            );
            Ok(())
        }

        fn start_run(&self) -> Result<String> {
            let id = format!("run-{}", self.runs.borrow().len() + 1);
            self.runs.borrow_mut().push((id.clone(), None));
            Ok(id)
        }

        fn finish_run(
            &self,
            run_id: &str,
            steps_executed: usize,
            outcome: &str,
            total_retries: usize,
        ) -> Result<()> {
            if let Some(run) = self
                .runs
                .borrow_mut()
                .iter_mut()
                .find(|(id, _)| id == run_id)
            {
                run.1 = Some((steps_executed, outcome.to_string()));
            }
            self.total_retries.set(total_retries);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A populated directory hashes deterministically, independent of readdir order —
    // two identical trees (built via different insertion order) hash equal.
    #[test]
    fn hash_directory_contents_is_order_independent() {
        let dir_a = std::env::temp_dir().join(format!("arc-hdc-a-{}", std::process::id()));
        let dir_b = std::env::temp_dir().join(format!("arc-hdc-b-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
        std::fs::create_dir_all(dir_a.join("sub")).unwrap();
        std::fs::create_dir_all(dir_b.join("sub")).unwrap();

        std::fs::write(dir_a.join("a.txt"), b"one").unwrap();
        std::fs::write(dir_a.join("sub/b.txt"), b"two").unwrap();
        // Same content, opposite write order.
        std::fs::write(dir_b.join("sub/b.txt"), b"two").unwrap();
        std::fs::write(dir_b.join("a.txt"), b"one").unwrap();

        let hash_a = hash_directory_contents(&dir_a);
        let hash_b = hash_directory_contents(&dir_b);
        assert!(hash_a.is_some());
        assert_eq!(
            hash_a, hash_b,
            "directory hash must not depend on readdir order"
        );

        std::fs::remove_dir_all(&dir_a).unwrap();
        std::fs::remove_dir_all(&dir_b).unwrap();
    }

    // Emptying a directory (files removed, directory itself kept) must change the
    // hash — this is round 7's regression: `fs::metadata().is_dir()` alone treats a
    // present-but-emptied directory identically to a populated one, so corruption
    // stands forever at exit 0. A content-manifest hash cannot make that mistake.
    #[test]
    fn hash_directory_contents_changes_when_emptied() {
        let dir = std::env::temp_dir().join(format!("arc-hdc-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("part-0.parquet"), b"partition bytes").unwrap();

        let populated = hash_directory_contents(&dir);
        std::fs::remove_file(dir.join("part-0.parquet")).unwrap();
        let emptied = hash_directory_contents(&dir);

        assert!(populated.is_some());
        assert!(emptied.is_some());
        assert_ne!(
            populated, emptied,
            "emptying a directory while keeping it present must change the hash"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    // An absent directory hashes to None — the same unconditional-staleness signal
    // an unreadable file already gives.
    #[test]
    fn hash_directory_contents_none_when_absent() {
        let dir = std::env::temp_dir().join(format!("arc-hdc-absent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(hash_directory_contents(&dir), None);
    }

    // StateBackend trait compiles and MockStateBackend works.
    #[test]
    fn test_mock_state_backend() {
        let backend = mock::MockStateBackend::new();
        backend.init().unwrap();
        assert!(*backend.init_called.borrow());

        // No state initially.
        assert!(backend.get_step_state("foo").unwrap().is_none());

        // Record and retrieve.
        backend
            .record_step("foo", "abc123", "", StepStatus::Success)
            .unwrap();
        let state = backend.get_step_state("foo").unwrap().unwrap();
        assert_eq!(state.sql_hash, "abc123");
        assert_eq!(state.status, StepStatus::Success);
    }

    // Run tracking in mock.
    #[test]
    fn test_mock_run_tracking() {
        let backend = mock::MockStateBackend::new();
        let run_id = backend.start_run().unwrap();
        backend.finish_run(&run_id, 3, "success", 0).unwrap();

        let runs = backend.runs.borrow();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].1.as_ref().unwrap().0, 3);
        assert_eq!(runs[0].1.as_ref().unwrap().1, "success");
    }

    // DuckDbStateBackend creates tables on first use.
    #[test]
    fn test_duckdb_backend_init() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.duckdb");
        let backend = DuckDbStateBackend::new(&db_path);

        backend.init().unwrap();

        // Verify tables exist by querying them.
        let conn = duckdb::Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM _arcform_state", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let count: i64 = conn
            .query_row("SELECT count(*) FROM _arcform_runs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        // Idempotent: calling init again should not fail.
        backend.init().unwrap();
    }

    // The state db's parent dir is created on open — a fresh all-config Protocol
    // (`db: build/x.db`) has no `build/` yet (the retired opaque pipelines did
    // `mkdir -p build`), and opening the state db must not fail before any step runs.
    #[test]
    fn init_creates_missing_db_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        // `build/` does NOT exist yet — mirrors a fresh checkout.
        let db_path = dir.path().join("build").join("x.db");
        assert!(!db_path.parent().unwrap().exists());

        let backend = DuckDbStateBackend::new(&db_path);
        backend.init().unwrap(); // must not error on the missing parent

        assert!(
            db_path.exists(),
            "state db (and its parent) should be created"
        );
        // Tables are usable.
        backend
            .record_step("s", "h", "", StepStatus::Success)
            .unwrap();
        assert_eq!(backend.get_step_state("s").unwrap().unwrap().sql_hash, "h");
    }

    // SQL content hash stored correctly.
    #[test]
    fn test_content_hash_stored() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.duckdb");
        let backend = DuckDbStateBackend::new(&db_path);
        backend.init().unwrap();

        let sql = "CREATE TABLE foo (id INT);";
        let hash = content_hash(sql.as_bytes());
        backend
            .record_step("load", &hash, "", StepStatus::Success)
            .unwrap();

        let state = backend.get_step_state("load").unwrap().unwrap();
        assert_eq!(state.sql_hash, hash);
        assert_eq!(state.status, StepStatus::Success);
    }

    // Run history recorded.
    #[test]
    fn test_run_history() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.duckdb");
        let backend = DuckDbStateBackend::new(&db_path);
        backend.init().unwrap();

        let run_id = backend.start_run().unwrap();
        backend.finish_run(&run_id, 5, "success", 0).unwrap();

        let conn = duckdb::Connection::open(&db_path).unwrap();
        let (steps, outcome): (i64, String) = conn
            .query_row(
                "SELECT steps_executed, outcome FROM _arcform_runs WHERE run_id = ?1",
                [&run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(steps, 5);
        assert_eq!(outcome, "success");
    }

    // Content hash is deterministic.
    #[test]
    fn test_content_hash_deterministic() {
        let hash1 = content_hash(b"SELECT 1;");
        let hash2 = content_hash(b"SELECT 1;");
        assert_eq!(hash1, hash2);

        let hash3 = content_hash(b"SELECT 2;");
        assert_ne!(hash1, hash3);
    }
}
