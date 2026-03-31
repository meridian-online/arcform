use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result};

/// Output captured from a step execution.
#[derive(Debug)]
pub struct StepOutput {
    pub stdout: String,
    pub stderr: String,
}

/// Trait for executing pipeline steps.
pub trait Engine {
    /// Execute a SQL file against a database.
    fn execute_sql(&self, db_path: &Path, sql_path: &Path) -> Result<StepOutput>;

    /// Execute a raw shell command.
    fn execute_command(&self, command: &str) -> Result<StepOutput>;

    /// Check that the engine CLI is available and executable.
    fn preflight(&self) -> Result<()>;
}

/// DuckDB CLI engine implementation.
pub struct DuckDbEngine;

impl Engine for DuckDbEngine {
    fn execute_sql(&self, db_path: &Path, sql_path: &Path) -> Result<StepOutput> {
        let output = Command::new("duckdb")
            .arg(db_path)
            .arg("-f")
            .arg(sql_path)
            .output()
            .map_err(|e| Error::StepExecution {
                step: sql_path.display().to_string(),
                source: e,
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            let code = output.status.code().unwrap_or(1);
            return Err(Error::StepFailed {
                step: String::new(), // Caller will provide the step name.
                code,
                stderr,
            });
        }

        Ok(StepOutput { stdout, stderr })
    }

    fn execute_command(&self, command: &str) -> Result<StepOutput> {
        let output = Command::new("sh")
            .arg("-c")
            .arg(command)
            .output()
            .map_err(|e| Error::StepExecution {
                step: command.to_string(),
                source: e,
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            let code = output.status.code().unwrap_or(1);
            return Err(Error::StepFailed {
                step: String::new(),
                code,
                stderr,
            });
        }

        Ok(StepOutput { stdout, stderr })
    }

    fn preflight(&self) -> Result<()> {
        let output = Command::new("duckdb").arg("--version").output();

        match output {
            Ok(o) if o.status.success() => Ok(()),
            _ => Err(Error::EngineNotFound {
                engine: "duckdb".to_string(),
            }),
        }
    }
}

#[cfg(test)]
#[allow(dead_code)]
pub mod mock {
    use super::*;
    use std::cell::RefCell;
    use std::fs;

    /// Records all calls for test assertions.
    pub struct MockEngine {
        pub calls: RefCell<Vec<MockCall>>,
        /// If set, fail on every call.
        pub should_fail: RefCell<Option<(i32, String)>>,
        /// If set, fail only on the Nth execution call (0-indexed, excludes preflight).
        pub fail_on_call: RefCell<Option<usize>>,
        /// Tracks the current execution call index (excludes preflight).
        exec_count: RefCell<usize>,
    }

    #[derive(Debug, Clone)]
    pub enum MockCall {
        Sql {
            db_path: String,
            sql_content: String,
        },
        Command {
            command: String,
        },
        Preflight,
    }

    impl MockEngine {
        pub fn new() -> Self {
            MockEngine {
                calls: RefCell::new(Vec::new()),
                should_fail: RefCell::new(None),
                fail_on_call: RefCell::new(None),
                exec_count: RefCell::new(0),
            }
        }

        /// Fail on every execution call.
        pub fn set_failure(&self, code: i32, stderr: &str) {
            *self.should_fail.borrow_mut() = Some((code, stderr.to_string()));
        }

        /// Fail only on the Nth execution call (0-indexed, excludes preflight).
        pub fn set_fail_on_call(&self, n: usize, code: i32, stderr: &str) {
            *self.fail_on_call.borrow_mut() = Some(n);
            *self.should_fail.borrow_mut() = Some((code, stderr.to_string()));
        }

        /// Check if this execution call should fail.
        fn should_fail_now(&self) -> Option<(i32, String)> {
            let current = *self.exec_count.borrow();
            *self.exec_count.borrow_mut() += 1;

            if let Some(fail_at) = *self.fail_on_call.borrow() {
                if current == fail_at {
                    return self.should_fail.borrow().clone();
                }
                return None;
            }

            // No fail_on_call set — use global should_fail.
            self.should_fail.borrow().clone()
        }
    }

    impl Engine for MockEngine {
        fn execute_sql(&self, db_path: &Path, sql_path: &Path) -> Result<StepOutput> {
            let sql_content = fs::read_to_string(sql_path).map_err(|e| Error::FileRead {
                path: sql_path.to_path_buf(),
                source: e,
            })?;

            self.calls.borrow_mut().push(MockCall::Sql {
                db_path: db_path.display().to_string(),
                sql_content,
            });

            if let Some((code, stderr)) = self.should_fail_now() {
                return Err(Error::StepFailed {
                    step: String::new(),
                    code,
                    stderr,
                });
            }

            Ok(StepOutput {
                stdout: String::new(),
                stderr: String::new(),
            })
        }

        fn execute_command(&self, command: &str) -> Result<StepOutput> {
            self.calls.borrow_mut().push(MockCall::Command {
                command: command.to_string(),
            });

            if let Some((code, stderr)) = self.should_fail_now() {
                return Err(Error::StepFailed {
                    step: String::new(),
                    code,
                    stderr,
                });
            }

            Ok(StepOutput {
                stdout: String::new(),
                stderr: String::new(),
            })
        }

        fn preflight(&self) -> Result<()> {
            self.calls.borrow_mut().push(MockCall::Preflight);
            Ok(())
        }
    }
}
