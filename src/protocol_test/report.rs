//! How `arc test` reports: the lines printed for each result, and the JUnit XML file.

use std::io::{self, Write};

use super::{Outcome, TestResult};

/// Show one result: `ok <name>`, or `FAIL <name>` with its risk line and what happened,
/// each line under it indented by two spaces.
pub(crate) fn print_result(out: &mut impl Write, result: &TestResult) -> io::Result<()> {
    match &result.outcome {
        Outcome::Passed => writeln!(out, "ok {}", result.name),
        Outcome::Failed { what } => {
            writeln!(out, "FAIL {}\n  risk: {}", result.name, result.risk)?;
            for line in what.lines() {
                writeln!(out, "  {line}")?;
            }
            Ok(())
        }
    }
}

/// The results as JUnit XML: one `testsuite` named for the Protocol, a `testcase` for each
/// test, and for a failed one a `failure` whose message is its risk line and whose body is
/// what happened.
pub(crate) fn junit(protocol: &str, results: &[TestResult]) -> String {
    let protocol = escape_attr(protocol);
    let failures = results
        .iter()
        .filter(|r| r.outcome != Outcome::Passed)
        .count();
    let total: f64 = results.iter().map(|r| r.seconds).sum();
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites>\n");
    xml.push_str(&format!(
        "  <testsuite name=\"{protocol}\" tests=\"{}\" failures=\"{failures}\" time=\"{total:.3}\">\n",
        results.len()
    ));
    for result in results {
        let head = format!(
            "    <testcase classname=\"{protocol}\" name=\"{}\" time=\"{:.3}\"",
            escape_attr(&result.name),
            result.seconds
        );
        match &result.outcome {
            Outcome::Passed => xml.push_str(&format!("{head}/>\n")),
            Outcome::Failed { what } => xml.push_str(&format!(
                "{head}>\n      <failure message=\"risk: {}\">{}</failure>\n    </testcase>\n",
                escape_attr(&result.risk),
                escape(what)
            )),
        }
    }
    xml.push_str("  </testsuite>\n</testsuites>\n");
    xml
}

/// `text` safe in XML element text: markup characters escaped, and each character XML 1.0
/// cannot hold (control characters other than tab, newline and carriage return, and the
/// noncharacters U+FFFE and U+FFFF) replaced with U+FFFD.
fn escape(text: &str) -> String {
    escape_with(text, false)
}

/// `text` safe in a double-quoted attribute: as [`escape`], and tab, newline and carriage
/// return written as character references so a parser does not normalise them to spaces.
fn escape_attr(text: &str) -> String {
    escape_with(text, true)
}

fn escape_with(text: &str, attribute: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\n' if attribute => out.push_str("&#10;"),
            '\r' if attribute => out.push_str("&#13;"),
            '\t' if attribute => out.push_str("&#9;"),
            '\n' | '\r' | '\t' => out.push(c),
            c if (c as u32) < 0x20 || matches!(c, '\u{FFFE}' | '\u{FFFF}') => {
                out.push('\u{FFFD}');
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passed(name: &str, seconds: f64) -> TestResult {
        TestResult {
            name: name.into(),
            risk: "a risk".into(),
            outcome: Outcome::Passed,
            seconds,
        }
    }

    fn failed(name: &str, risk: &str, what: &str) -> TestResult {
        TestResult {
            name: name.into(),
            risk: risk.into(),
            outcome: Outcome::Failed { what: what.into() },
            seconds: 0.5,
        }
    }

    #[test]
    fn junit_lists_every_test_with_its_time() {
        let xml = junit(
            "codes",
            &[passed("one", 1.25), failed("two", "r", "it broke")],
        );
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites>\n"));
        assert!(
            xml.contains("<testsuite name=\"codes\" tests=\"2\" failures=\"1\" time=\"1.750\">"),
            "{xml}"
        );
        assert!(
            xml.contains("<testcase classname=\"codes\" name=\"one\" time=\"1.250\"/>"),
            "{xml}"
        );
        assert!(
            xml.contains("<testcase classname=\"codes\" name=\"two\" time=\"0.500\">"),
            "{xml}"
        );
        assert!(xml.trim_end().ends_with("</testsuites>"), "{xml}");
    }

    #[test]
    fn a_failure_message_leads_with_the_risk() {
        let xml = junit(
            "p",
            &[failed("t", "labels go missing", "check 1 failed\n  x")],
        );
        assert!(
            xml.contains(
                "<failure message=\"risk: labels go missing\">check 1 failed\n  x</failure>"
            ),
            "{xml}"
        );
    }

    #[test]
    fn junit_escapes_markup() {
        let xml = junit(
            "a&b",
            &[failed(
                "say \"hi\" <now>",
                "r & <s>",
                "it's \"bad\" & <worse>",
            )],
        );
        assert!(xml.contains("name=\"a&amp;b\""), "{xml}");
        assert!(
            xml.contains("name=\"say &quot;hi&quot; &lt;now&gt;\""),
            "{xml}"
        );
        assert!(xml.contains("message=\"risk: r &amp; &lt;s&gt;\""), "{xml}");
        assert!(
            xml.contains(">it&apos;s &quot;bad&quot; &amp; &lt;worse&gt;</failure>"),
            "{xml}"
        );
    }

    #[test]
    fn print_result_shows_ok_or_the_fail_block() {
        let mut out = Vec::new();
        print_result(&mut out, &passed("one", 0.1)).unwrap();
        print_result(&mut out, &failed("two", "the risk", "line a\nline b")).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "ok one\nFAIL two\n  risk: the risk\n  line a\n  line b\n"
        );
    }

    #[test]
    fn characters_xml_cannot_hold_become_replacement_characters() {
        let xml = junit(
            "p",
            &[failed("a\0b\x07c\x1bd", "r\0\x07\x1b", "m\0\x07\x1b\ttab")],
        );
        assert!(
            xml.contains("name=\"a\u{FFFD}b\u{FFFD}c\u{FFFD}d\""),
            "{xml}"
        );
        assert!(
            xml.contains("message=\"risk: r\u{FFFD}\u{FFFD}\u{FFFD}\""),
            "{xml}"
        );
        assert!(
            xml.contains(">m\u{FFFD}\u{FFFD}\u{FFFD}\ttab</failure>"),
            "{xml}"
        );
        let nonchars = junit("p", &[failed("a\u{FFFE}b\u{FFFF}c", "r", "m")]);
        assert!(
            nonchars.contains("name=\"a\u{FFFD}b\u{FFFD}c\""),
            "{nonchars}"
        );
        assert!(
            !xml.chars().any(|c| c < ' ' && !"\n\t".contains(c)),
            "{xml:?}"
        );
    }

    #[test]
    fn line_breaks_in_an_attribute_are_character_references() {
        let xml = junit("p", &[failed("a\nb", "one\r\ntwo\tthree", "x\ny")]);
        assert!(xml.contains("name=\"a&#10;b\""), "{xml}");
        assert!(
            xml.contains("message=\"risk: one&#13;&#10;two&#9;three\""),
            "{xml}"
        );
        assert!(xml.contains(">x\ny</failure>"), "{xml}");
    }
}
