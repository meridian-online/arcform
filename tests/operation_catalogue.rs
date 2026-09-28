//! `arc operation list` and `arc operation describe`: the operations arc holds,
//! and what one of them takes, asked of the real `arc` binary.
//!
//! An agent or a second application that wants to write a step asks arc which
//! operations exist and what one takes, rather than reading another tool's code.
//! These tests are that question, asked through the command line:
//!
//!   1. **list** — each long name, with one line saying what it does, as text
//!      and as a JSON array;
//!   2. **describe** — the long name, what the operation is applied to, and a
//!      `parameters` JSON Schema that requires `where` and admits nothing else;
//!   3. **refuse** — an operation arc does not hold exits non-zero with nothing
//!      on stdout, and the message points at `arc operation list`;
//!   4. **help** — `arc --help` lists `operation`, and `arc operation --help`
//!      lists both verbs.
//!
//! Neither verb reads a protocol, so every run happens in an empty directory.

use std::process::{Command, Output};

use serde_json::{Value, json};

/// The long name the catalogue holds its one operation under.
const FILTER_ROWS: &str = "filter-rows";

/// Run the real `arc` binary with `args`, in a directory that holds no protocol.
fn arc(args: &[&str]) -> Output {
    let dir = tempfile::tempdir().expect("tempdir");
    Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(dir.path())
        .args(args)
        .output()
        .expect("spawn arc")
}

/// Run the binary, demand success, and return its stdout.
fn arc_ok(args: &[&str]) -> String {
    let out = arc(args);
    assert!(
        out.status.success(),
        "arc {args:?} failed (code {:?}):\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("stdout is UTF-8")
}

/// Run the binary and parse its stdout, all of it, as one JSON document.
fn arc_json(args: &[&str]) -> Value {
    let stdout = arc_ok(args);
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("arc {args:?} stdout is not JSON ({e}):\n{stdout}"))
}

/// The line of `text` that opens with `word` as its own word, or a failure that
/// shows the whole text.
fn line_opening_with<'a>(text: &'a str, word: &str) -> &'a str {
    let mut matching = text.lines().filter(|line| {
        line.trim_start()
            .strip_prefix(word)
            .is_some_and(|rest| rest.starts_with(char::is_whitespace))
    });
    let line = matching
        .next()
        .unwrap_or_else(|| panic!("no line opens with `{word}`:\n{text}"));
    assert!(
        matching.next().is_none(),
        "more than one line opens with `{word}`:\n{text}"
    );
    line
}

/// What follows `word` on `line`, trimmed: the line's description of it.
fn description_after<'a>(line: &'a str, word: &str) -> &'a str {
    line.trim_start()
        .strip_prefix(word)
        .expect("the line opens with the word")
        .trim()
}

#[test]
fn list_prints_the_long_name_with_one_line_saying_what_it_does() {
    let stdout = arc_ok(&["operation", "list"]);

    let line = line_opening_with(&stdout, FILTER_ROWS);
    assert!(
        !description_after(line, FILTER_ROWS).is_empty(),
        "`{FILTER_ROWS}` has no description on its line:\n{stdout}"
    );
    assert!(
        stdout.lines().all(|l| !l.trim().is_empty()),
        "the listing carries a blank line:\n{stdout:?}"
    );
}

#[test]
fn list_json_is_an_array_whose_entry_holds_the_long_name() {
    let listing = arc_json(&["operation", "list", "--json"]);

    let entries = listing.as_array().expect("the listing is a JSON array");
    let entry = entries
        .iter()
        .find(|e| e["long_name"] == FILTER_ROWS)
        .unwrap_or_else(|| panic!("no entry holds `{FILTER_ROWS}`:\n{listing}"));
    assert!(
        entry["summary"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty()),
        "the entry has no summary:\n{entry}"
    );
}

#[test]
fn the_text_listing_and_the_json_listing_say_the_same_thing() {
    let text = arc_ok(&["operation", "list"]);
    let listing = arc_json(&["operation", "list", "--json"]);

    let entry = listing
        .as_array()
        .expect("the listing is a JSON array")
        .iter()
        .find(|e| e["long_name"] == FILTER_ROWS)
        .expect("the listing holds the operation");
    let summary = entry["summary"].as_str().expect("summary is a string");
    assert_eq!(
        description_after(line_opening_with(&text, FILTER_ROWS), FILTER_ROWS),
        summary,
        "the two listings disagree about what `{FILTER_ROWS}` does"
    );
}

#[test]
fn describe_holds_the_long_name_what_it_is_applied_to_and_its_parameters() {
    let description = arc_json(&["operation", "describe", FILTER_ROWS]);

    assert_eq!(description["long_name"], FILTER_ROWS);
    assert!(
        description["summary"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty()),
        "no summary in:\n{description}"
    );
    assert_eq!(
        description["applied_to"],
        json!({ "kind": "table", "count": 1 }),
        "`{FILTER_ROWS}` is applied to one table"
    );
    assert!(
        description["parameters"].is_object(),
        "`parameters` is not a JSON Schema object:\n{description}"
    );
}

#[test]
fn the_parameters_schema_requires_where_as_a_string_and_admits_nothing_else() {
    let description = arc_json(&["operation", "describe", FILTER_ROWS]);
    let schema = &description["parameters"];

    assert_eq!(schema["type"], "object");
    assert_eq!(
        schema["required"],
        json!(["where"]),
        "`where` is the one required parameter"
    );
    assert_eq!(
        schema["additionalProperties"],
        Value::Bool(false),
        "the schema must be closed to any other key, as the boolean `false`"
    );
    let properties = schema["properties"]
        .as_object()
        .expect("`properties` is an object");
    assert_eq!(
        properties.keys().collect::<Vec<_>>(),
        ["where"],
        "`where` is the one parameter the schema declares"
    );
    assert_eq!(properties["where"]["type"], "string");
}

#[test]
fn describing_an_operation_arc_does_not_hold_is_refused_with_nothing_on_stdout() {
    let out = arc(&["operation", "describe", "no-such-operation"]);

    assert_eq!(
        out.status.code(),
        Some(1),
        "an operation arc does not hold exits 1, the code for `cannot run`"
    );
    assert!(
        out.stdout.is_empty(),
        "nothing goes to stdout on a refusal:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no-such-operation"),
        "the message does not name the operation asked for:\n{stderr}"
    );
    assert!(
        stderr.contains("`arc operation list` prints the operations arc holds"),
        "the message does not point at `arc operation list`:\n{stderr}"
    );
}

#[test]
fn a_long_name_is_matched_exactly_not_searched_for() {
    // A long name is an identity. A near miss is an operation arc does not hold,
    // whatever it is close to, and it is refused the way an unknown name is.
    for near_miss in [
        "Filter-Rows",
        "filter",
        "filter-row",
        "filter-rows-",
        "filter_rows",
    ] {
        let out = arc(&["operation", "describe", near_miss]);
        assert!(
            !out.status.success() && out.stdout.is_empty(),
            "`{near_miss}` was answered as if it were `{FILTER_ROWS}`:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

#[test]
fn help_lists_operation_and_operation_help_lists_its_two_verbs() {
    let top = arc_ok(&["--help"]);
    assert!(
        !description_after(line_opening_with(&top, "operation"), "operation").is_empty(),
        "`arc --help` lists `operation` with no line saying what it is:\n{top}"
    );

    let operation = arc_ok(&["operation", "--help"]);
    for verb in ["list", "describe"] {
        assert!(
            !description_after(line_opening_with(&operation, verb), verb).is_empty(),
            "`arc operation --help` lists `{verb}` with no line saying what it does:\n{operation}"
        );
    }
}
