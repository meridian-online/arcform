use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// Information about the detected engine, returned by preflight.
#[derive(Debug, Clone)]
pub struct EngineInfo {
    /// Parsed semantic version of the installed engine CLI,
    /// or None if version output was unparseable.
    pub version: Option<semver::Version>,
}

/// Output captured from a step execution.
/// Stdout is inherited (streams to terminal in real-time) unless output capture is active.
/// Stderr is captured for error reporting but also streamed for SQL steps.
#[derive(Debug)]
#[allow(dead_code)]
pub struct StepOutput {
    pub stderr: String,
    /// Captured stdout from a command step when output capture is active.
    /// None for SQL steps and command steps without output capture.
    pub stdout: Option<String>,
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
    /// `env` contains ARC_PARAM_ variables to inject into the child process environment.
    /// `timeout` is the maximum duration before the step is killed.
    fn execute_sql(&self, db_path: &Path, sql_path: &Path, env: &HashMap<String, String>, timeout: Option<Duration>) -> Result<StepOutput>;

    /// Execute a raw shell command.
    /// `env` contains ARC_PARAM_ variables to inject into the child process environment.
    /// If `capture_stdout` is true, stdout is piped and captured instead of inherited.
    /// `timeout` is the maximum duration before the step is killed.
    fn execute_command(&self, command: &str, env: &HashMap<String, String>, capture_stdout: bool, timeout: Option<Duration>) -> Result<StepOutput>;

    /// Check that the engine CLI is available and return information about it.
    /// Returns EngineInfo with the detected version (or None if unparseable).
    fn preflight(&self) -> Result<EngineInfo>;
}

/// Wait for a child process with an optional timeout.
/// Polls try_wait() at ~100ms intervals. On timeout, kills the child.
fn wait_with_timeout(child: &mut std::process::Child, timeout: Option<Duration>, step_name: &str) -> Result<std::process::ExitStatus> {
    let Some(deadline_duration) = timeout else {
        // No timeout — wait normally.
        return child.wait().map_err(|e| Error::StepExecution {
            step: step_name.to_string(),
            source: e,
        });
    };

    let deadline = Instant::now() + deadline_duration;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait(); // Reap the killed process.
                    return Err(Error::StepTimeout {
                        step: step_name.to_string(),
                    });
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                return Err(Error::StepExecution {
                    step: step_name.to_string(),
                    source: e,
                });
            }
        }
    }
}

/// DuckDB CLI engine implementation.
pub struct DuckDbEngine;

impl Engine for DuckDbEngine {
    fn execute_sql(&self, db_path: &Path, sql_path: &Path, env: &HashMap<String, String>, timeout: Option<Duration>) -> Result<StepOutput> {
        let step_name = sql_path.display().to_string();
        let mut child = Command::new("duckdb")
            .arg(db_path)
            .arg("-f")
            .arg(sql_path)
            .envs(env)
            .stdout(Stdio::inherit())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::StepExecution {
                step: step_name.clone(),
                source: e,
            })?;

        let stderr = stream_stderr(&mut child);
        let status = wait_with_timeout(&mut child, timeout, &step_name)?;

        if !status.success() {
            let code = status.code().unwrap_or(1);
            return Err(Error::StepFailed {
                step: String::new(),
                code,
                stderr,
            });
        }

        Ok(StepOutput { stderr, stdout: None })
    }

    fn execute_command(&self, command: &str, env: &HashMap<String, String>, capture_stdout: bool, timeout: Option<Duration>) -> Result<StepOutput> {
        let stdout_cfg = if capture_stdout { Stdio::piped() } else { Stdio::inherit() };
        // Stderr: inherited for command steps (streams to terminal).
        // When capturing stdout, stderr remains inherited so errors are visible.
        let stderr_cfg = Stdio::inherit();

        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .envs(env)
            .stdout(stdout_cfg)
            .stderr(stderr_cfg)
            .spawn()
            .map_err(|e| Error::StepExecution {
                step: command.to_string(),
                source: e,
            })?;

        // If capturing, read stdout before wait() to avoid deadlocks.
        // Timeout applies to the wait after stdout drain (constraint #10).
        let captured_stdout = if capture_stdout {
            let mut stdout_buf = String::new();
            if let Some(mut stdout) = child.stdout.take() {
                let _ = stdout.read_to_string(&mut stdout_buf);
            }
            let trimmed = stdout_buf.trim_end_matches('\n').to_string();
            Some(trimmed)
        } else {
            None
        };

        let status = wait_with_timeout(&mut child, timeout, command)?;

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
            stdout: captured_stdout,
        })
    }

    fn preflight(&self) -> Result<EngineInfo> {
        let output = Command::new("duckdb").arg("--version").output();

        match output {
            Ok(o) if o.status.success() => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let version = parse_version_output(&stdout);
                Ok(EngineInfo { version })
            }
            _ => Err(Error::EngineNotFound {
                engine: "duckdb".to_string(),
            }),
        }
    }
}

/// Parse a version string from engine CLI output.
///
/// Handles formats like:
/// - "v1.5.2 (Variegata) 8a5851971f"
/// - "v0.10.0 1234abc"
/// - "1.5.2"
///
/// Returns None if the version cannot be parsed.
pub fn parse_version_output(output: &str) -> Option<semver::Version> {
    // Find the first token that looks like a version (with or without leading 'v').
    for token in output.split_whitespace() {
        let stripped = token.strip_prefix('v').unwrap_or(token);
        if let Ok(ver) = semver::Version::parse(stripped) {
            return Some(ver);
        }
    }
    None
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
        /// If true, preflight returns EngineNotFound.
        pub preflight_should_fail: RefCell<bool>,
        /// Version to report from preflight. Defaults to 2.0.0.
        pub version: RefCell<Option<semver::Version>>,
        /// Simulated stdout for command steps with capture_stdout=true.
        pub simulated_stdout: RefCell<Option<String>>,
        /// If true, return StepTimeout when timeout is Some(_).
        pub timeout_should_fire: RefCell<bool>,
    }

    #[derive(Debug, Clone)]
    pub enum MockCall {
        Sql {
            db_path: String,
            sql_content: String,
            env: HashMap<String, String>,
        },
        Command {
            command: String,
            env: HashMap<String, String>,
            capture_stdout: bool,
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
                preflight_should_fail: RefCell::new(false),
                version: RefCell::new(Some(semver::Version::new(2, 0, 0))),
                simulated_stdout: RefCell::new(None),
                timeout_should_fire: RefCell::new(false),
            }
        }

        /// Set simulated stdout for command steps with capture_stdout=true.
        pub fn set_simulated_stdout(&self, stdout: &str) {
            *self.simulated_stdout.borrow_mut() = Some(stdout.to_string());
        }

        /// Make the mock return StepTimeout when a timeout is provided.
        pub fn set_timeout_fire(&self) {
            *self.timeout_should_fire.borrow_mut() = true;
        }

        /// Set the version that preflight will report.
        pub fn set_version(&self, version: Option<semver::Version>) {
            *self.version.borrow_mut() = version;
        }

        /// Fail on every execution call.
        pub fn set_failure(&self, code: i32, stderr: &str) {
            *self.should_fail.borrow_mut() = Some((code, stderr.to_string()));
        }

        /// Make preflight return EngineNotFound.
        pub fn set_preflight_failure(&self) {
            *self.preflight_should_fail.borrow_mut() = true;
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
        fn execute_sql(&self, db_path: &Path, sql_path: &Path, env: &HashMap<String, String>, timeout: Option<Duration>) -> Result<StepOutput> {
            let sql_content = fs::read_to_string(sql_path).map_err(|e| Error::FileRead {
                path: sql_path.to_path_buf(),
                source: e,
            })?;

            self.calls.borrow_mut().push(MockCall::Sql {
                db_path: db_path.display().to_string(),
                sql_content,
                env: env.clone(),
            });

            // Simulate timeout if configured and a timeout was provided.
            if timeout.is_some() && *self.timeout_should_fire.borrow() {
                return Err(Error::StepTimeout {
                    step: sql_path.display().to_string(),
                });
            }

            if let Some((code, stderr)) = self.should_fail_now() {
                return Err(Error::StepFailed {
                    step: String::new(),
                    code,
                    stderr,
                });
            }

            Ok(StepOutput {
                stderr: String::new(),
                stdout: None,
            })
        }

        fn execute_command(&self, command: &str, env: &HashMap<String, String>, capture_stdout: bool, timeout: Option<Duration>) -> Result<StepOutput> {
            self.calls.borrow_mut().push(MockCall::Command {
                command: command.to_string(),
                env: env.clone(),
                capture_stdout,
            });

            // Simulate timeout if configured and a timeout was provided.
            if timeout.is_some() && *self.timeout_should_fire.borrow() {
                return Err(Error::StepTimeout {
                    step: command.to_string(),
                });
            }

            if let Some((code, stderr)) = self.should_fail_now() {
                return Err(Error::StepFailed {
                    step: String::new(),
                    code,
                    stderr,
                });
            }

            let stdout = if capture_stdout {
                Some(self.simulated_stdout.borrow().clone().unwrap_or_default())
            } else {
                None
            };

            Ok(StepOutput {
                stderr: String::new(),
                stdout,
            })
        }

        fn preflight(&self) -> Result<EngineInfo> {
            self.calls.borrow_mut().push(MockCall::Preflight);
            if *self.preflight_should_fail.borrow() {
                return Err(Error::EngineNotFound {
                    engine: "duckdb".to_string(),
                });
            }
            Ok(EngineInfo {
                version: self.version.borrow().clone(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // lrp ac-03: Parse version with codename (standard DuckDB format).
    #[test]
    fn test_lrp_ac03_parse_version_with_codename() {
        let version = parse_version_output("v1.5.2 (Variegata) 8a5851971f");
        assert_eq!(version.unwrap(), semver::Version::new(1, 5, 2));
    }

    // lrp ac-03: Parse version without codename.
    #[test]
    fn test_lrp_ac03_parse_version_without_codename() {
        let version = parse_version_output("v0.10.0 1234abc");
        assert_eq!(version.unwrap(), semver::Version::new(0, 10, 0));
    }

    // lrp ac-03: Parse bare version (no leading 'v').
    #[test]
    fn test_lrp_ac03_parse_bare_version() {
        let version = parse_version_output("1.5.2");
        assert_eq!(version.unwrap(), semver::Version::new(1, 5, 2));
    }

    // lrp ac-12: Unparseable output returns None.
    #[test]
    fn test_lrp_ac12_unparseable_version_returns_none() {
        assert!(parse_version_output("not a version").is_none());
        assert!(parse_version_output("").is_none());
        assert!(parse_version_output("duckdb").is_none());
    }

    // lrp ac-10: MockEngine returns configurable version.
    #[test]
    fn test_lrp_ac10_mock_engine_configurable_version() {
        let engine = mock::MockEngine::new();

        // Default is 2.0.0.
        let info = engine.preflight().unwrap();
        assert_eq!(info.version.unwrap(), semver::Version::new(2, 0, 0));

        // Set custom version.
        engine.set_version(Some(semver::Version::new(1, 3, 0)));
        let info = engine.preflight().unwrap();
        assert_eq!(info.version.unwrap(), semver::Version::new(1, 3, 0));

        // Set None (unparseable).
        engine.set_version(None);
        let info = engine.preflight().unwrap();
        assert!(info.version.is_none());
    }
}
