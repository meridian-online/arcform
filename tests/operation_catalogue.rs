//! `arc operation list` and `arc operation describe`: the operations arc holds,
//! and what one of them takes, asked of the real `arc` binary.
//!
//! An agent or a second application that wants to write a step asks arc which
//! operations exist and what one takes, rather than reading another tool's code.
//! These tests are that question, asked through the command line:
//!
//!   1. **list** — each long name, with one line saying what it does, as text
//!      and as a JSON array;
//!   2. **describe** — the long name, one sentence saying what the operation
//!      does, the list of what it is applied to (a table, then a column of that
//!      table, each with a sentence), and a `parameters` JSON Schema that
//!      requires `where`, admits nothing else, and offers the comparisons a
//!      condition is written with without holding the condition to them;
//!   3. **refuse** — an operation arc does not hold exits non-zero with nothing
//!      on stdout, and the message points at `arc operation list`;
//!   4. **help** — `arc --help` lists `operation`, and `arc operation --help`
//!      lists both verbs.
//!
//! Neither verb reads a protocol, so every run happens in an empty directory.

use std::process::{Command, Output};

use serde_json::{Value, json};

/// The long names the catalogue holds its operations under.
const FILTER_ROWS: &str = "filter-rows";
const SORT_ROWS: &str = "sort-rows";

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

/// `text` is one sentence: it opens with a capital, ends at its one full stop,
/// and holds no line break. A panel prints it on its own line.
fn is_one_sentence(text: &str) -> bool {
    let Some(body) = text.strip_suffix('.') else {
        return false;
    };
    text.chars().next().is_some_and(char::is_uppercase)
        && !text.contains('\n')
        && !body.contains(['.', '?', '!'])
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
fn list_prints_sort_rows_beside_filter_rows_each_with_a_sentence() {
    let text = arc_ok(&["operation", "list"]);
    for op in [FILTER_ROWS, SORT_ROWS] {
        let line = line_opening_with(&text, op);
        assert!(
            !description_after(line, op).is_empty(),
            "`{op}` has no description on its line:\n{text}"
        );
    }

    let listing = arc_json(&["operation", "list", "--json"]);
    let entries = listing.as_array().expect("the listing is a JSON array");
    for op in [FILTER_ROWS, SORT_ROWS] {
        let entry = entries
            .iter()
            .find(|e| e["long_name"] == op)
            .unwrap_or_else(|| panic!("no entry holds `{op}`:\n{listing}"));
        assert!(
            entry["summary"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty()),
            "the entry for `{op}` has no summary:\n{entry}"
        );
    }
}

#[test]
fn describe_sort_rows_returns_the_ordered_shape() {
    let description = arc_json(&["operation", "describe", SORT_ROWS]);

    assert_eq!(description["long_name"], SORT_ROWS);
    assert!(
        description["summary"].as_str().is_some_and(is_one_sentence),
        "the summary is not one sentence saying what the operation does:\n{description}"
    );

    // Applied to a table, then a column of that table, each with a sentence.
    let applied_to = description["applied_to"]
        .as_array()
        .unwrap_or_else(|| panic!("`applied_to` is not a list:\n{description}"));
    let entries: Vec<(&str, &str, Option<&str>)> = applied_to
        .iter()
        .map(|e| {
            (
                e["name"].as_str().unwrap_or("<no name>"),
                e["kind"].as_str().unwrap_or("<no kind>"),
                e.get("of")
                    .map(|of| of.as_str().unwrap_or("<not a string>")),
            )
        })
        .collect();
    assert_eq!(
        entries,
        [
            ("table", "table", None),
            ("column", "column", Some("table"))
        ],
        "`{SORT_ROWS}` is applied to a table, then a column of that table:\n{description}"
    );
    for entry in applied_to {
        assert!(
            entry["description"].as_str().is_some_and(is_one_sentence),
            "`{}` has no one-sentence description:\n{entry}",
            entry["name"]
        );
    }

    // Parameters: closed, one required string `order_by`, holding exactly the
    // four keys the shape names — no `title`, `enum` or `pattern`.
    let schema = &description["parameters"];
    assert_eq!(schema["type"], "object");
    assert_eq!(
        schema["required"],
        json!(["order_by"]),
        "`order_by` is the one required parameter"
    );
    assert_eq!(
        schema["additionalProperties"],
        Value::Bool(false),
        "the schema must be closed to any other key"
    );
    let properties = schema["properties"]
        .as_object()
        .expect("`properties` is an object");
    assert_eq!(
        properties.keys().collect::<Vec<_>>(),
        ["order_by"],
        "`order_by` is the one parameter the schema declares"
    );
    let order_by = &properties["order_by"];
    assert_eq!(order_by["type"], "string");
    assert_eq!(
        order_by["x-kind"], "order",
        "`order_by` does not say its value is an order:\n{order_by}"
    );
    assert_eq!(
        order_by["x-directions"],
        json!([
            { "word": "ascending", "sign": "asc" },
            { "word": "descending", "sign": "desc" },
        ]),
        "the directions offered, in order, as a word and a sign"
    );
    assert!(
        order_by["description"]
            .as_str()
            .is_some_and(|d| !d.trim().is_empty()),
        "`order_by` has no description:\n{order_by}"
    );
    let mut keys: Vec<&str> = order_by
        .as_object()
        .expect("`order_by` is an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["description", "type", "x-directions", "x-kind"],
        "`order_by` holds a key the shape does not name:\n{order_by}"
    );
}

#[test]
fn describe_holds_the_long_name_a_sentence_what_it_is_applied_to_and_its_parameters() {
    let description = arc_json(&["operation", "describe", FILTER_ROWS]);

    assert_eq!(description["long_name"], FILTER_ROWS);
    assert!(
        description["summary"].as_str().is_some_and(is_one_sentence),
        "the summary is not one sentence saying what the operation does:\n{description}"
    );
    assert!(
        description["parameters"].is_object(),
        "`parameters` is not a JSON Schema object:\n{description}"
    );
}

#[test]
fn describe_applies_the_filter_to_a_table_then_a_column_of_it_each_with_a_sentence() {
    let description = arc_json(&["operation", "describe", FILTER_ROWS]);
    let applied_to = description["applied_to"]
        .as_array()
        .unwrap_or_else(|| panic!("`applied_to` is not a list:\n{description}"));

    // Each entry as (name, kind, of), in order: the table, then its column.
    let entries: Vec<(&str, &str, Option<&str>)> = applied_to
        .iter()
        .map(|e| {
            (
                e["name"].as_str().unwrap_or("<no name>"),
                e["kind"].as_str().unwrap_or("<no kind>"),
                e.get("of")
                    .map(|of| of.as_str().unwrap_or("<not a string>")),
            )
        })
        .collect();
    assert_eq!(
        entries,
        [
            ("table", "table", None),
            ("column", "column", Some("table"))
        ],
        "`{FILTER_ROWS}` is applied to a table, then a column of that table:\n{description}"
    );
    for entry in applied_to {
        assert!(
            entry["description"].as_str().is_some_and(is_one_sentence),
            "`{}` has no one-sentence description:\n{entry}",
            entry["name"]
        );
        let mut keys: Vec<&str> = entry
            .as_object()
            .expect("an entry is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        let expected: &[&str] = if entry.get("of").is_some() {
            &["description", "kind", "name", "of"]
        } else {
            &["description", "kind", "name"]
        };
        assert_eq!(
            keys, expected,
            "an entry holds a key the shape does not name:\n{entry}"
        );
    }
}

#[test]
fn the_parameters_schema_requires_a_where_condition_and_admits_nothing_else() {
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
    assert!(
        !properties.contains_key("column"),
        "the column is what the operation is applied to, not an argument:\n{schema}"
    );
    assert_eq!(
        properties.keys().collect::<Vec<_>>(),
        ["where"],
        "`where` is the one parameter the schema declares"
    );
    let where_ = &properties["where"];
    assert_eq!(where_["type"], "string");
    assert_eq!(
        where_["x-kind"], "condition",
        "`where` does not say its value is a condition:\n{where_}"
    );
    assert!(
        where_["description"]
            .as_str()
            .is_some_and(|d| !d.trim().is_empty()),
        "`where` has no description:\n{where_}"
    );
}

#[test]
fn where_offers_six_comparisons_by_word_and_sign_and_is_not_held_to_them() {
    let description = arc_json(&["operation", "describe", FILTER_ROWS]);
    let where_ = &description["parameters"]["properties"]["where"];

    assert_eq!(
        where_["x-comparisons"],
        json!([
            { "word": "is", "sign": "=" },
            { "word": "is not", "sign": "!=" },
            { "word": "over", "sign": ">" },
            { "word": "under", "sign": "<" },
            { "word": "between", "sign": "between" },
            { "word": "is null", "sign": "is null" },
        ]),
        "the comparisons offered, in order, as a word and a sign"
    );
    // A condition is any SQL condition, recorded as written. The comparisons
    // are offered by word and are not a limit, so nothing in `where` holds the
    // value to them, or to the column: its keys are the four the shape names.
    for constraint in ["enum", "pattern"] {
        assert!(
            where_.get(constraint).is_none(),
            "`where` holds `{constraint}`, which would hold the condition to it:\n{where_}"
        );
    }
    let mut keys: Vec<&str> = where_
        .as_object()
        .expect("`where` is an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["description", "type", "x-comparisons", "x-kind"],
        "`where` holds a key that constrains or ties the condition:\n{where_}"
    );
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
