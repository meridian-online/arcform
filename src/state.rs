//! Run state tracking for selective execution.
//!
//! The `StateBackend` trait provides a pluggable interface for persisting
//! step execution state across runs. The default implementation uses DuckDB
//! tables co-located with the project's data.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Record of a step's last execution state.
#[derive(Debug, Clone)]
pub struct StepState {
    /// SHA-256 hex digest of the SQL file contents at last run.
    pub sql_hash: String,
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

/// Compute SHA-256 hex digest of a byte slice.
pub fn content_hash(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    format!("{:x}", hasher.finalize())
}

/// Trait for persisting step execution state across runs.
pub trait StateBackend {
    /// Initialise the backend (create tables, etc.). Idempotent.
    fn init(&self) -> Result<()>;

    /// Get the last recorded state for a step, or None if never run.
    fn get_step_state(&self, step_name: &str) -> Result<Option<StepState>>;

    /// Record a step's execution result.
    fn record_step(&self, step_name: &str, sql_hash: &str, status: StepStatus) -> Result<()>;

    /// Record the start of a pipeline run. Returns a run ID.
    fn start_run(&self) -> Result<String>;

    /// Record the completion of a pipeline run.
    fn finish_run(&self, run_id: &str, steps_executed: usize, outcome: &str) -> Result<()>;
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
            );",
        )
        .map_err(|e| Error::StateBackend(e.to_string()))?;
        Ok(())
    }

    fn get_step_state(&self, step_name: &str) -> Result<Option<StepState>> {
        let conn = self.open()?;
        let mut stmt = conn
            .prepare("SELECT sql_hash, status FROM _arcform_state WHERE step_name = ?1")
            .map_err(|e| Error::StateBackend(e.to_string()))?;

        let result = stmt
            .query_row([step_name], |row| {
                let hash: String = row.get(0)?;
                let status: String = row.get(1)?;
                Ok(StepState {
                    sql_hash: hash,
                    status: StepStatus::from_str(&status),
                })
            });

        match result {
            Ok(state) => Ok(Some(state)),
            Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(Error::StateBackend(e.to_string())),
        }
    }

    fn record_step(&self, step_name: &str, sql_hash: &str, status: StepStatus) -> Result<()> {
        let conn = self.open()?;
        conn.execute(
            "INSERT OR REPLACE INTO _arcform_state (step_name, sql_hash, last_run_at, status)
             VALUES (?1, ?2, current_timestamp, ?3)",
            duckdb::params![step_name, sql_hash, status.as_str()],
        )
        .map_err(|e| Error::StateBackend(e.to_string()))?;
        Ok(())
    }

    fn start_run(&self) -> Result<String> {
        let run_id = format!(
            "{}-{}",
            timestamp_id(),
            &uuid_simple()
        );
        let conn = self.open()?;
        conn.execute(
            "INSERT INTO _arcform_runs (run_id, started_at) VALUES (?1, current_timestamp)",
            [&run_id],
        )
        .map_err(|e| Error::StateBackend(e.to_string()))?;
        Ok(run_id)
    }

    fn finish_run(&self, run_id: &str, steps_executed: usize, outcome: &str) -> Result<()> {
        let conn = self.open()?;
        conn.execute(
            "UPDATE _arcform_runs SET finished_at = current_timestamp, steps_executed = ?1, outcome = ?2 WHERE run_id = ?3",
            duckdb::params![steps_executed as i64, outcome, run_id],
        )
        .map_err(|e| Error::StateBackend(e.to_string()))?;
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

    /// Mock state backend for testing.
    pub struct MockStateBackend {
        pub states: RefCell<HashMap<String, StepState>>,
        pub runs: RefCell<Vec<(String, Option<(usize, String)>)>>,
        pub init_called: RefCell<bool>,
    }

    impl MockStateBackend {
        pub fn new() -> Self {
            MockStateBackend {
                states: RefCell::new(HashMap::new()),
                runs: RefCell::new(Vec::new()),
                init_called: RefCell::new(false),
            }
        }

        /// Pre-populate a step's state (for testing "second run" scenarios).
        pub fn set_step_state(&self, step_name: &str, sql_hash: &str, status: StepStatus) {
            self.states.borrow_mut().insert(
                step_name.to_string(),
                StepState {
                    sql_hash: sql_hash.to_string(),
                    status,
                },
            );
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

        fn record_step(&self, step_name: &str, sql_hash: &str, status: StepStatus) -> Result<()> {
            self.states.borrow_mut().insert(
                step_name.to_string(),
                StepState {
                    sql_hash: sql_hash.to_string(),
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

        fn finish_run(&self, run_id: &str, steps_executed: usize, outcome: &str) -> Result<()> {
            if let Some(run) = self.runs.borrow_mut().iter_mut().find(|(id, _)| id == run_id) {
                run.1 = Some((steps_executed, outcome.to_string()));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-01: StateBackend trait compiles and MockStateBackend works.
    #[test]
    fn test_ac01_mock_state_backend() {
        let backend = mock::MockStateBackend::new();
        backend.init().unwrap();
        assert!(*backend.init_called.borrow());

        // No state initially.
        assert!(backend.get_step_state("foo").unwrap().is_none());

        // Record and retrieve.
        backend.record_step("foo", "abc123", StepStatus::Success).unwrap();
        let state = backend.get_step_state("foo").unwrap().unwrap();
        assert_eq!(state.sql_hash, "abc123");
        assert_eq!(state.status, StepStatus::Success);
    }

    // AC-01: Run tracking in mock.
    #[test]
    fn test_ac01_mock_run_tracking() {
        let backend = mock::MockStateBackend::new();
        let run_id = backend.start_run().unwrap();
        backend.finish_run(&run_id, 3, "success").unwrap();

        let runs = backend.runs.borrow();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].1.as_ref().unwrap().0, 3);
        assert_eq!(runs[0].1.as_ref().unwrap().1, "success");
    }

    // AC-02: DuckDbStateBackend creates tables on first use.
    #[test]
    fn test_ac02_duckdb_backend_init() {
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

    // AC-03: SQL content hash stored correctly.
    #[test]
    fn test_ac03_content_hash_stored() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.duckdb");
        let backend = DuckDbStateBackend::new(&db_path);
        backend.init().unwrap();

        let sql = "CREATE TABLE foo (id INT);";
        let hash = content_hash(sql.as_bytes());
        backend.record_step("load", &hash, StepStatus::Success).unwrap();

        let state = backend.get_step_state("load").unwrap().unwrap();
        assert_eq!(state.sql_hash, hash);
        assert_eq!(state.status, StepStatus::Success);
    }

    // AC-13: Run history recorded.
    #[test]
    fn test_ac13_run_history() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.duckdb");
        let backend = DuckDbStateBackend::new(&db_path);
        backend.init().unwrap();

        let run_id = backend.start_run().unwrap();
        backend.finish_run(&run_id, 5, "success").unwrap();

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
