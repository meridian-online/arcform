//! `arc protocol write-file` and the `arc mcp` tool `protocol_write_file`: a
//! Protocol's schema, panel and README files written through arc, each recorded
//! in the file's own history, asked of the real `arc` binary.
//!
//!   1. **write** — a schema, a panel and a README land where they belong and
//!      each is a version of its file;
//!   2. **refuse** — a path outside the Protocol, a place arc does not write,
//!      content that does not parse, `report.md` and a folder with no spec are
//!      each refused with the folder untouched;
//!   3. **mcp** — `protocol_write_file` writes the bytes the command writes.
//!
//! Each run points `ARCFORM_HISTORY_DIR` at a directory of its own.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

struct Protocol {
    _root: tempfile::TempDir,
    dir: PathBuf,
    history: PathBuf,
}

impl Protocol {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let protocol = Protocol {
            dir: root.path().join("rates"),
            history: root.path().join("history"),
            _root: root,
        };
        let out = protocol.run(protocol._root.path(), &["create-protocol", "rates"], None);
        assert!(out.status.success(), "create-protocol: {}", stderr(&out));
        protocol
    }

    fn run(&self, cwd: &Path, args: &[&str], stdin: Option<&str>) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_arc"))
            .current_dir(cwd)
            .env("ARCFORM_HISTORY_DIR", &self.history)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn arc");
        {
            let mut pipe = child.stdin.take().expect("stdin is piped");
            if let Some(text) = stdin {
                pipe.write_all(text.as_bytes()).unwrap();
            }
        }
        child.wait_with_output().expect("wait for arc")
    }

    fn write(&self, path: &str, content: &str) -> Output {
        self.run(&self.dir, &["protocol", "write-file", path], Some(content))
    }

    fn history_lines(&self, file: &str) -> usize {
        let out = self.run(&self.dir, &["history", "list", "--file", file], None);
        assert!(out.status.success(), "history list: {}", stderr(&out));
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .filter(|line| line.contains(" bytes  "))
            .count()
    }

    /// Every path under the Protocol's folder, sorted, so a refusal can be
    /// shown to have left the folder as it was.
    fn tree(&self) -> Vec<String> {
        fn walk(dir: &Path, base: &Path, into: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                into.push(path.strip_prefix(base).unwrap().display().to_string());
                if path.is_dir() && !path.is_symlink() {
                    walk(&path, base, into);
                }
            }
        }
        let mut paths = Vec::new();
        walk(&self.dir, &self.dir, &mut paths);
        paths.sort();
        paths
    }

    fn call(&self, arguments: Value) -> Value {
        let mut child = Command::new(env!("CARGO_BIN_EXE_arc"))
            .current_dir(&self.dir)
            .env("ARCFORM_HISTORY_DIR", &self.history)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn arc mcp");
        {
            let mut stdin = child.stdin.take().expect("stdin is piped");
            let request = json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "protocol_write_file", "arguments": arguments },
            });
            writeln!(stdin, "{request}").unwrap();
        }
        let out = child.wait_with_output().expect("wait for arc mcp");
        assert!(out.status.success(), "arc mcp failed: {}", stderr(&out));
        let stdout = String::from_utf8(out.stdout).unwrap();
        let response: Value = serde_json::from_str(stdout.lines().next().unwrap()).unwrap();
        response["result"].clone()
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

const SCHEMA: &str = r#"{"fields": [{"name": "rate", "type": "number"}]}"#;
const PANEL: &str = "plot:\n  mark: line\n";

#[test]
fn writes_schema_and_records_history() {
    let protocol = Protocol::new();
    let out = protocol.write("schemas/rates.json", SCHEMA);
    assert!(out.status.success(), "write failed: {}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(protocol.dir.join("schemas/rates.json")).unwrap(),
        SCHEMA
    );
    assert_eq!(protocol.history_lines("schemas/rates.json"), 1);
}

#[test]
fn writes_panel_under_panels() {
    let protocol = Protocol::new();
    let out = protocol.write("panels/trend.yaml", PANEL);
    assert!(out.status.success(), "write failed: {}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(protocol.dir.join("panels/trend.yaml")).unwrap(),
        PANEL
    );
    assert_eq!(protocol.history_lines("panels/trend.yaml"), 1);

    let readme = protocol.write("README.md", "# Rates\n");
    assert!(readme.status.success(), "readme: {}", stderr(&readme));
    assert_eq!(protocol.history_lines("README.md"), 1);
}

#[test]
fn refuses_path_outside_protocol() {
    let protocol = Protocol::new();
    let before = protocol.tree();
    let outside = protocol._root.path().join("x.json");
    let absolute = outside.display().to_string();
    for path in ["../x.json", absolute.as_str()] {
        let out = protocol.write(path, SCHEMA);
        assert!(!out.status.success(), "{path} was written");
        assert_eq!(protocol.tree(), before, "{path} changed the folder");
    }
    assert!(!outside.exists());
}

#[cfg(unix)]
#[test]
fn refuses_a_link_that_leaves_the_protocol() {
    let protocol = Protocol::new();
    let elsewhere = protocol._root.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, protocol.dir.join("panels")).unwrap();
    let before = protocol.tree();
    let out = protocol.write("panels/trend.yaml", PANEL);
    assert!(!out.status.success());
    assert_eq!(protocol.tree(), before);
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn refuses_a_file_that_is_a_link_to_outside_and_replaces_a_dangling_one() {
    let protocol = Protocol::new();
    let panels = protocol.dir.join("panels");
    std::fs::create_dir(&panels).unwrap();
    let target = protocol._root.path().join("outside.yaml");
    std::fs::write(&target, "plot: {}\n").unwrap();
    std::os::unix::fs::symlink(&target, panels.join("trend.yaml")).unwrap();
    let before = protocol.tree();
    let out = protocol.write("panels/trend.yaml", PANEL);
    assert!(!out.status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "plot: {}\n");
    assert_eq!(protocol.tree(), before);

    // A dangling link inside the folder is replaced, and nothing lands outside.
    std::fs::remove_file(panels.join("trend.yaml")).unwrap();
    let gone = protocol._root.path().join("gone.yaml");
    std::os::unix::fs::symlink(&gone, panels.join("old.yaml")).unwrap();
    let out = protocol.write("panels/old.yaml", PANEL);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(panels.join("old.yaml")).unwrap(),
        PANEL
    );
    assert!(!gone.exists());
}

#[test]
fn keeps_what_it_replaces_and_both_quick_writes() {
    let protocol = Protocol::new();
    std::fs::write(protocol.dir.join("README.md"), "by hand\n").unwrap();
    assert!(protocol.write("README.md", "first\n").status.success());
    assert!(protocol.write("README.md", "second\n").status.success());
    assert_eq!(protocol.history_lines("README.md"), 3);

    let list = protocol.run(
        &protocol.dir,
        &["history", "list", "--file", "README.md", "--json"],
        None,
    );
    let entries: Value = serde_json::from_slice(&list.stdout).unwrap();
    let mut texts = Vec::new();
    for entry in entries.as_array().unwrap() {
        let id = entry["id"].as_str().unwrap();
        let shown = protocol.run(
            &protocol.dir,
            &["history", "show", id, "--file", "README.md"],
            None,
        );
        texts.push(String::from_utf8(shown.stdout).unwrap());
    }
    for kept in ["by hand\n", "first\n", "second\n"] {
        assert!(
            texts.iter().any(|t| t == kept),
            "{kept:?} missing: {texts:?}"
        );
    }
}

#[test]
fn refuses_unlisted_path() {
    let protocol = Protocol::new();
    let before = protocol.tree();
    let out = protocol.write("models/x.sql", "select 1");
    assert!(!out.status.success());
    let message = stderr(&out);
    for place in [
        "schemas/<name>.json",
        "panels/<name>.yaml",
        "README.md",
        "report.md",
    ] {
        assert!(message.contains(place), "{place} missing from: {message}");
    }
    assert_eq!(protocol.tree(), before);
}

#[test]
fn refuses_invalid_schema_json() {
    let protocol = Protocol::new();
    let before = protocol.tree();
    let out = protocol.write("schemas/rates.json", "{not json");
    assert!(!out.status.success());
    assert!(stderr(&out).contains("JSON"), "{}", stderr(&out));
    assert_eq!(protocol.tree(), before);

    let out = protocol.write("panels/trend.yaml", "title: no plot\n");
    assert!(!out.status.success());
    assert!(stderr(&out).contains("plot"), "{}", stderr(&out));
    assert_eq!(protocol.tree(), before);
}

#[test]
fn refuses_report_and_names_report_write() {
    let protocol = Protocol::new();
    let before = protocol.tree();
    let out = protocol.write("report.md", "# Findings\n");
    assert!(!out.status.success());
    assert!(stderr(&out).contains("report_write"), "{}", stderr(&out));
    assert_eq!(protocol.tree(), before);
}

#[test]
fn refuses_without_arcform_yaml() {
    let root = tempfile::tempdir().unwrap();
    let history = root.path().join("history");
    let out = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(root.path())
        .env("ARCFORM_HISTORY_DIR", &history)
        .args(["protocol", "write-file", "README.md"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(stderr(&out).contains("arcform.yaml"), "{}", stderr(&out));
    assert!(!root.path().join("README.md").exists());
}

#[test]
fn mcp_writes_the_bytes_the_command_writes() {
    let protocol = Protocol::new();
    let result = protocol.call(json!({ "path": "schemas/rates.json", "content": SCHEMA }));
    assert_ne!(result["isError"], true, "{result}");
    let body: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["kind"], "schema");
    assert_eq!(body["warnings"], json!([]));
    assert_eq!(
        std::fs::read_to_string(protocol.dir.join("schemas/rates.json")).unwrap(),
        SCHEMA
    );
    assert_eq!(protocol.history_lines("schemas/rates.json"), 1);

    let before = protocol.tree();
    let result = protocol.call(json!({ "path": "report.md", "content": "x" }));
    assert_eq!(result["isError"], true, "{result}");
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("report_write"),
        "{result}"
    );
    assert_eq!(protocol.tree(), before);
}

#[cfg(unix)]
#[test]
fn a_failed_write_leaves_no_checkpoint_line_in_the_log() {
    use std::os::unix::fs::PermissionsExt;
    let protocol = Protocol::new();
    std::fs::write(protocol.dir.join("README.md"), "by hand\n").unwrap();
    let log = || std::fs::read(protocol.dir.join("arcform-log.txt")).unwrap_or_default();
    let before = log();
    // A folder that cannot take a new file makes the atomic write fail.
    let locked = std::fs::Permissions::from_mode(0o555);
    std::fs::set_permissions(&protocol.dir, locked).unwrap();
    let out = protocol.write("README.md", "new\n");
    std::fs::set_permissions(&protocol.dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    if out.status.success() {
        return; // running with rights that ignore the mode: nothing to show
    }
    assert_eq!(log(), before);
    assert_eq!(
        std::fs::read_to_string(protocol.dir.join("README.md")).unwrap(),
        "by hand\n"
    );
}
