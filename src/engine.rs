use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::{Error, Result};

/// Output captured from a step execution.
/// Stdout is inherited (streams to terminal in real-time).
/// Stderr is captured for error reporting but also streamed for SQL steps.
#[derive(Debug)]
#[allow(dead_code)]
pub struct StepOutput {
    pub stderr: String,
}

/// Read stderr from a child process, streaming it to the terminal in real-time
/// while capturing the full content for error reporting.
fn stream_stderr(child: &mut std::process::Child) -> String {
    let Some(mut stderr) = child.stderr.take() else {
        return String::new();
    };

    let mut buf = [0u8; 4096];
    let mut captured = Vec::new();

    loop {
        match stderr.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let _ = std::io::Write::write_all(&mut std::io::stderr(), &buf[..n]);
                captured.extend_from_slice(&buf[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }

    String::from_utf8_lossy(&captured).to_string()
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
        let mut child = Command::new("duckdb")
            .arg(db_path)
            .arg("-f")
            .arg(sql_path)
            .stdout(Stdio::inherit())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::StepExecution {
                step: sql_path.display().to_string(),
                source: e,
            })?;

        let stderr = stream_stderr(&mut child);
        let status = child.wait().map_err(|e| Error::StepExecution {
            step: sql_path.display().to_string(),
            source: e,
        })?;

        if !status.success() {
            let code = status.code().unwrap_or(1);
            return Err(Error::StepFailed {
                step: String::new(),
                code,
                stderr,
            });
        }

        Ok(StepOutput { stderr })
    }

    fn execute_command(&self, command: &str) -> Result<StepOutput> {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| Error::StepExecution {
                step: command.to_string(),
                source: e,
            })?;

        let status = child.wait().map_err(|e| Error::StepExecution {
            step: command.to_string(),
            source: e,
        })?;

        if !status.success() {
            let code = status.code().unwrap_or(1);
            return Err(Error::StepFailed {
                step: String::new(),
                code,
                stderr: String::new(),
            });
        }

        Ok(StepOutput {
            stderr: String::new(),
        })
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
                stderr: String::new(),
            })
        }

        fn preflight(&self) -> Result<()> {
            self.calls.borrow_mut().push(MockCall::Preflight);
            Ok(())
        }
    }
}
