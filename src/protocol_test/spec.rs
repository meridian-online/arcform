//! Reading a Protocol's `tests.yaml`: the list of tests, checked before any runs.

use std::collections::HashSet;
use std::path::Path;

use indexmap::IndexMap;
use serde::Deserialize;

use crate::error::{Error, Result};

/// The file beside `arcform.yaml` that holds a Protocol's tests.
pub(crate) const TESTS_FILENAME: &str = "tests.yaml";

/// One test: fixtures standing in for fetches, optional breaks, and what to assert.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProtocolTest {
    pub name: String,
    /// What would go wrong in a published dataset if this test were missing.
    pub risk: String,
    /// Step name (or output path) to the checked-in file that stands in for it.
    pub fixtures: IndexMap<String, String>,
    /// Fixture key to the SQL statements that break it, joined by newlines.
    pub breaks: IndexMap<String, String>,
    pub expect: Option<Expect>,
    /// Inline failing-rows queries, run after the Protocol's own steps.
    pub checks: Vec<String>,
    /// Parameter overrides, each as text.
    pub params: IndexMap<String, String>,
    /// A CSV standing in for earlier runs' measures.
    pub measure_history: Option<String>,
}

/// The run must fail with a message containing `fails_with`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Expect {
    pub fails_with: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTest {
    name: String,
    risk: Option<String>,
    #[serde(default)]
    fixtures: IndexMap<String, String>,
    #[serde(default, rename = "break")]
    breaks: IndexMap<String, RawBreak>,
    expect: Option<RawExpect>,
    #[serde(default)]
    checks: Vec<String>,
    #[serde(default)]
    params: IndexMap<String, serde_yaml::Value>,
    measure_history: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawBreak {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExpect {
    fails_with: String,
}

fn refuse(dir: &Path, reason: String) -> Error {
    Error::TestFile {
        path: dir.join(TESTS_FILENAME),
        reason,
    }
}

/// Read and validate `tests.yaml` in `dir`. The first refusal wins, in file order.
pub(crate) fn load_tests(dir: &Path) -> Result<Vec<ProtocolTest>> {
    let path = dir.join(TESTS_FILENAME);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        refuse(
            dir,
            format!("{TESTS_FILENAME}: cannot read {}: {e}", path.display()),
        )
    })?;
    let raw: Vec<RawTest> =
        serde_yaml::from_str(&text).map_err(|e| refuse(dir, format!("{TESTS_FILENAME}: {e}")))?;

    let mut seen = HashSet::new();
    let mut tests = Vec::with_capacity(raw.len());
    for r in raw {
        let name = r.name;
        if name.trim().is_empty() {
            return Err(refuse(
                dir,
                format!("{TESTS_FILENAME}: a test has an empty name:"),
            ));
        }
        let bad = |what: String| refuse(dir, format!("{TESTS_FILENAME}: test '{name}': {what}"));
        if !seen.insert(name.clone()) {
            return Err(refuse(
                dir,
                format!("{TESTS_FILENAME}: two tests are named '{name}'"),
            ));
        }
        let risk = match r.risk.map(|s| s.trim().to_string()) {
            Some(s) if !s.is_empty() => s,
            _ => {
                return Err(bad(
                    "risk: is required, one line saying what would go wrong in a published dataset without this test".into(),
                ));
            }
        };
        for (key, file) in &r.fixtures {
            if !dir.join(file).is_file() {
                return Err(bad(format!("fixture '{key}': file {file} does not exist")));
            }
        }
        if let Some(h) = &r.measure_history
            && !dir.join(h).is_file()
        {
            return Err(bad(format!("measure_history: file {h} does not exist")));
        }
        let mut breaks = IndexMap::new();
        for (key, b) in r.breaks {
            if !r.fixtures.contains_key(&key) {
                return Err(bad(format!("break: '{key}' has no fixture of that name")));
            }
            let statements: Vec<String> = match b {
                RawBreak::One(s) => vec![s],
                RawBreak::Many(v) => v,
            };
            // A trailing `-- comment` would swallow a `;` on the same line, so the
            // terminator goes on a line of its own.
            let normalise = |s: &str| {
                let s = s.trim();
                if s.ends_with(';') {
                    s.to_string()
                } else {
                    format!("{s}\n;")
                }
            };
            if statements.iter().all(|s| s.trim().is_empty()) {
                return Err(bad(format!("break: '{key}' is empty")));
            }
            let sql = statements
                .iter()
                .filter(|s| !s.trim().is_empty())
                .map(|s| normalise(s))
                .collect::<Vec<_>>()
                .join("\n");
            breaks.insert(key, sql);
        }
        if !r.checks.is_empty() && r.expect.is_some() {
            return Err(bad(
                "checks: cannot be combined with expect:; a test either expects a failure or asserts its checks pass".into(),
            ));
        }
        let expect = match r.expect {
            Some(e) if e.fails_with.trim().is_empty() => {
                return Err(bad("expect: fails_with: must not be empty".into()));
            }
            Some(e) => Some(Expect {
                fails_with: e.fails_with,
            }),
            None => None,
        };
        let mut params = IndexMap::new();
        for (k, v) in r.params {
            let text = match v {
                serde_yaml::Value::String(s) => s,
                serde_yaml::Value::Number(n) => n.to_string(),
                serde_yaml::Value::Bool(b) => b.to_string(),
                _ => {
                    return Err(bad(format!(
                        "params: '{k}' must be a string, number or boolean"
                    )));
                }
            };
            params.insert(k, text);
        }
        tests.push(ProtocolTest {
            name,
            risk,
            fixtures: r.fixtures,
            breaks,
            expect,
            checks: r.checks,
            params,
            measure_history: r.measure_history,
        });
    }
    Ok(tests)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(yaml: &str) -> Result<Vec<ProtocolTest>> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("testdata")).unwrap();
        std::fs::write(dir.path().join("testdata/soc_definitions.xlsx"), b"x").unwrap();
        std::fs::write(dir.path().join(TESTS_FILENAME), yaml).unwrap();
        load_tests(dir.path())
    }

    fn msg(yaml: &str) -> String {
        load(yaml).unwrap_err().to_string()
    }

    #[test]
    fn a_test_without_risk_is_refused() {
        let m = msg("- name: x\n  fixtures:\n    f: testdata/soc_definitions.xlsx\n");
        assert!(m.contains("test 'x'") && m.contains("risk:"), "{m}");
    }

    #[test]
    fn two_tests_with_one_name_are_refused() {
        let m = msg("- name: x\n  risk: r\n- name: x\n  risk: r\n");
        assert!(m.contains("two tests are named 'x'"), "{m}");
    }

    #[test]
    fn a_break_on_a_key_with_no_fixture_is_refused() {
        let m = msg("- name: x\n  risk: r\n  break:\n    nokey: SELECT 1\n");
        assert!(m.contains("break:") && m.contains("nokey"), "{m}");
    }

    #[test]
    fn checks_with_expect_are_refused() {
        let m =
            msg("- name: x\n  risk: r\n  checks: [SELECT 1]\n  expect:\n    fails_with: boom\n");
        assert!(m.contains("checks:") && m.contains("expect:"), "{m}");
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let m = msg("- name: x\n  risk: r\n  brake:\n    f: SELECT 1\n");
        assert!(m.contains("brake"), "{m}");
    }

    #[test]
    fn a_missing_fixture_file_is_refused() {
        let m = msg("- name: x\n  risk: r\n  fixtures:\n    f: testdata/gone.xlsx\n");
        assert!(m.contains("testdata/gone.xlsx"), "{m}");
    }

    #[test]
    fn an_empty_fails_with_is_refused() {
        let m = msg("- name: x\n  risk: r\n  expect:\n    fails_with: '  '\n");
        assert!(m.contains("fails_with"), "{m}");
    }

    #[test]
    fn params_take_scalars_as_text() {
        let t =
            load("- name: x\n  risk: r\n  params:\n    release: 2018\n    on: true\n    s: abc\n")
                .unwrap();
        assert_eq!(t[0].params["release"], "2018");
        assert_eq!(t[0].params["on"], "true");
        assert_eq!(t[0].params["s"], "abc");
        let m = msg("- name: x\n  risk: r\n  params:\n    p: [1, 2]\n");
        assert!(m.contains("params:") && m.contains("'p'"), "{m}");
    }

    #[test]
    fn a_break_is_one_string_or_a_list_joined_with_semicolons() {
        let t = load(
            "- name: x\n  risk: r\n  fixtures:\n    f: testdata/soc_definitions.xlsx\n    g: testdata/soc_definitions.xlsx\n  break:\n    f:\n      - UPDATE fixture SET a = 1\n      - DELETE FROM fixture;\n    g: ' DELETE FROM fixture '\n",
        )
        .unwrap();
        assert_eq!(
            t[0].breaks["f"],
            "UPDATE fixture SET a = 1\n;\nDELETE FROM fixture;"
        );
        assert_eq!(t[0].breaks["g"], "DELETE FROM fixture\n;");
    }

    #[test]
    fn a_single_string_and_a_one_item_list_store_the_same_text() {
        let head =
            "- name: x\n  risk: r\n  fixtures:\n    f: testdata/soc_definitions.xlsx\n  break:\n";
        let one = load(&format!("{head}    f: DELETE FROM fixture\n")).unwrap();
        let list = load(&format!("{head}    f: [DELETE FROM fixture]\n")).unwrap();
        assert_eq!(one[0].breaks["f"], list[0].breaks["f"]);
    }

    #[test]
    fn a_statement_ending_in_a_comment_keeps_its_terminator_on_its_own_line() {
        let t = load(
            "- name: x\n  risk: r\n  fixtures:\n    f: testdata/soc_definitions.xlsx\n  break:\n    f: DELETE FROM fixture -- all rows\n",
        )
        .unwrap();
        assert_eq!(t[0].breaks["f"], "DELETE FROM fixture -- all rows\n;");
    }

    #[test]
    fn a_blank_name_or_an_empty_break_is_refused() {
        let m = msg("- name: ' '\n  risk: r\n");
        assert!(m.contains("name:"), "{m}");
        let head =
            "- name: x\n  risk: r\n  fixtures:\n    f: testdata/soc_definitions.xlsx\n  break:\n";
        for b in ["    f: ''\n", "    f: []\n"] {
            let m = msg(&format!("{head}{b}"));
            assert!(
                m.contains("break:") && m.contains("'f'") && m.contains("empty"),
                "{m}"
            );
        }
    }

    #[test]
    fn no_tests_file_is_an_error_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let m = load_tests(dir.path()).unwrap_err().to_string();
        assert!(m.contains("tests.yaml"), "{m}");
    }

    #[test]
    fn the_spec_example_loads() {
        let t = load(
            "- name: a moved definitions header is refused
  risk: BLS moves the header row and titles load into the wrong columns
  fixtures:
    fetch_definitions: testdata/soc_definitions.xlsx
  break:
    fetch_definitions: UPDATE fixture SET B = 'Code' WHERE row = 8
  expect:
    fails_with: header row has moved

- name: every parent code exists
  risk: an orphaned code publishes a broken hierarchy
  fixtures:
    fetch_definitions: testdata/soc_definitions.xlsx
  checks:
    - SELECT code FROM soc_2018 WHERE parent NOT IN (SELECT code FROM soc_2018)
  params:
    release: \"2018\"
",
        )
        .unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].name, "a moved definitions header is refused");
        assert_eq!(
            t[0].breaks["fetch_definitions"],
            "UPDATE fixture SET B = 'Code' WHERE row = 8\n;"
        );
        assert_eq!(
            t[0].expect.as_ref().unwrap().fails_with,
            "header row has moved"
        );
        assert_eq!(t[1].checks.len(), 1);
        assert_eq!(t[1].params["release"], "2018");
        assert!(t[1].expect.is_none() && t[1].measure_history.is_none());
    }
}
