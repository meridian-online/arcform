//! Placing a test's fixture files in its copy of the Protocol, and applying a `break:` to
//! one: DuckDB reads the file as a table named `fixture`, runs the break's SQL on it, and
//! writes it back in the format it came in.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use regex::Regex;

use crate::engine::{Engine, LoadedExtensions, ProtocolSql, check_extension_installs};
use crate::error::Error;

use super::plan::Placement;

/// Whether a placed fixture is rewritten through DuckDB, and with what.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoundTrip {
    /// Copied as it is.
    None,
    /// Read and written back with no statements, as the control run of a break test has it,
    /// so a failure the rewrite itself causes shows up there too.
    Control,
    /// Read, changed by the placement's `break:` statements, and written back.
    Break,
}

/// The folder in the copy that holds the break's SQL file and its scratch database.
const WORK_DIR: &str = ".arc-test";

/// Copy `placement`'s fixture from `protocol_dir` to `copy/<dest>`, creating its folders,
/// then rewrite it as `round_trip` says. `Err` is the refusal text, which makes the test fail.
pub(crate) fn place(
    copy: &Path,
    placement: &Placement,
    protocol_dir: &Path,
    engine: &dyn Engine,
    round_trip: RoundTrip,
) -> std::result::Result<(), String> {
    let source = protocol_dir.join(&placement.fixture);
    let dest = copy.join(&placement.dest);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "the fixture {} could not be placed at {}: {e}",
                placement.fixture, placement.dest
            )
        })?;
    }
    std::fs::copy(&source, &dest).map_err(|e| {
        format!(
            "the fixture {} could not be copied to {}: {e}",
            placement.fixture, placement.dest
        )
    })?;
    let statements = match round_trip {
        RoundTrip::None => return Ok(()),
        RoundTrip::Control => "",
        RoundTrip::Break => placement.statements.as_deref().unwrap_or(""),
    };
    rewrite(copy, &placement.fixture, &dest, statements, engine)
}

/// The file formats arc can read into a table and write back as they were.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Xlsx,
    Csv,
    Json,
}

/// The format of `fixture`, from its extension in any case, or `None` when arc cannot
/// round-trip it.
fn format_of(fixture: &str) -> Option<Format> {
    let extension = Path::new(fixture)
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    match extension.as_str() {
        "xlsx" => Some(Format::Xlsx),
        "csv" | "tsv" => Some(Format::Csv),
        "json" | "jsonl" | "ndjson" => Some(Format::Json),
        _ => None,
    }
}

/// Rewrite the placed file `dest` through DuckDB with `statements` run on it in between.
fn rewrite(
    copy: &Path,
    fixture: &str,
    dest: &Path,
    statements: &str,
    engine: &dyn Engine,
) -> std::result::Result<(), String> {
    let cannot = |reason: String| format!("the break of {fixture} cannot be applied: {reason}");
    let format = format_of(fixture).ok_or_else(|| {
        cannot(
            "arc rewrites only .xlsx, .csv, .tsv, .json, .jsonl and .ndjson files; check in a \
             second fixture with the change made instead"
                .to_string(),
        )
    })?;
    let dest = std::path::absolute(dest).map_err(|e| cannot(e.to_string()))?;
    let file_name = dest
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = dest.with_file_name(format!(".arc-test-{file_name}"));
    let sql = match format {
        Format::Xlsx => {
            let (sheet, end) = xlsx_layout(&dest).map_err(cannot)?;
            xlsx_sql(&dest, &temp, &sheet, &end, statements)
        }
        Format::Csv => csv_sql(&dest, &temp, statements),
        Format::Json => {
            let bytes = std::fs::read(&dest).map_err(|e| cannot(e.to_string()))?;
            let array = bytes
                .iter()
                .find(|byte| !byte.is_ascii_whitespace())
                .is_some_and(|byte| *byte == b'[');
            json_sql(&dest, &temp, array, statements)
        }
    };

    let work = std::path::absolute(copy.join(WORK_DIR)).map_err(|e| cannot(e.to_string()))?;
    std::fs::create_dir_all(&work).map_err(|e| cannot(e.to_string()))?;
    let sql_path = work.join("break.sql");
    std::fs::write(&sql_path, sql).map_err(|e| cannot(e.to_string()))?;

    // The break is Protocol SQL, and goes through the refusals a step's file does.
    let source = ProtocolSql {
        place: format!("the break of {fixture}"),
        step: None,
        file: format!("{WORK_DIR}/break.sql"),
        path: sql_path.clone(),
    };
    match check_extension_installs(std::slice::from_ref(&source), None, engine) {
        Ok(_) => {}
        Err(Error::ExtensionRefused { refusals }) => {
            return Err(format!(
                "the break of {fixture} was refused: it installs a DuckDB extension arc has \
                 not vetted:\n  {}",
                refusals.join("\n  ")
            ));
        }
        Err(other) => return Err(format!("the break of {fixture} was refused: {other}")),
    }

    // A fresh scratch database for each break, so one fixture's table is not another's.
    let db = work.join("break.duckdb");
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(work.join("break.duckdb.wal"));
    let ran = engine.execute_sql(
        &db,
        &sql_path,
        &HashMap::new(),
        None,
        &mut LoadedExtensions::default(),
    );
    if let Err(error) = ran {
        let _ = std::fs::remove_file(&temp);
        let message = match error {
            Error::StepFailed { stderr, .. } if !stderr.trim().is_empty() => {
                stderr.trim().to_string()
            }
            other => other.to_string(),
        };
        return Err(format!("the break of {fixture} did not run: {message}"));
    }
    std::fs::rename(&temp, &dest).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        cannot(format!(
            "the rewritten file could not replace the fixture: {e}"
        ))
    })
}

/// `text` as a SQL string literal.
fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// `path` as a SQL string literal.
fn path_literal(path: &Path) -> String {
    literal(&path.to_string_lossy())
}

/// `statements` with a line break after them, or nothing when there are none.
fn body(statements: &str) -> String {
    let statements = statements.trim();
    if statements.is_empty() {
        String::new()
    } else {
        format!("{statements}\n")
    }
}

/// The SQL that reads the only sheet of the workbook `dest`, cells `A1` to `end`, every cell
/// as text and every blank row kept, numbers its rows `row`, runs `statements`, and writes
/// the rows back in `row` order to `temp`, with no header and under the same sheet name.
fn xlsx_sql(dest: &Path, temp: &Path, sheet: &str, end: &str, statements: &str) -> String {
    let dest = path_literal(dest);
    let temp = path_literal(temp);
    let sheet = literal(sheet);
    let range = literal(&format!("A1:{end}"));
    format!(
        "INSTALL excel;\n\
         LOAD excel;\n\
         SET threads = 1;\n\
         CREATE TABLE fixture AS SELECT row_number() OVER () AS row, * FROM read_xlsx({dest}, \
         header = false, all_varchar = true, stop_at_empty = false, sheet = {sheet}, \
         range = {range});\n\
         {}\
         COPY (SELECT * EXCLUDE (row) FROM fixture ORDER BY row) TO {temp} \
         (FORMAT xlsx, HEADER false, SHEET {sheet});\n",
        body(statements)
    )
}

/// The SQL that reads the CSV file `dest` with the delimiter, quote and header DuckDB's
/// sniffer finds in it (`sniff_csv` gives `(empty)` for a quote or escape it found none of;
/// the quote is then `"` and the escape the quote, DuckDB's defaults), every column as text, runs `statements`, and writes it back to
/// `temp` with the same delimiter, quote and header.
fn csv_sql(dest: &Path, temp: &Path, statements: &str) -> String {
    let dest = path_literal(dest);
    let temp = path_literal(temp);
    format!(
        "SET VARIABLE arc_test_delim = (SELECT Delimiter FROM sniff_csv({dest}));\n\
         SET VARIABLE arc_test_quote = (SELECT coalesce(nullif(Quote, '(empty)'), '\"') FROM sniff_csv({dest}));\n\
         SET VARIABLE arc_test_escape = (SELECT coalesce(nullif(Escape, '(empty)'), nullif(Quote, '(empty)'), '\"') FROM sniff_csv({dest}));\n\
         SET VARIABLE arc_test_header = (SELECT HasHeader FROM sniff_csv({dest}));\n\
         CREATE TABLE fixture AS FROM read_csv({dest}, delim = getvariable('arc_test_delim'), \
         quote = getvariable('arc_test_quote'), escape = getvariable('arc_test_escape'), \
         header = getvariable('arc_test_header'), all_varchar = true);\n\
         {}\
         COPY fixture TO {temp} (FORMAT csv, DELIMITER getvariable('arc_test_delim'), \
         QUOTE getvariable('arc_test_quote'), ESCAPE getvariable('arc_test_escape'), \
         HEADER getvariable('arc_test_header'));\n",
        body(statements)
    )
}

/// The SQL that reads the JSON file `dest`, an array when `array` is true and one object
/// per line otherwise, runs `statements`, and writes it back to `temp` the same way.
fn json_sql(dest: &Path, temp: &Path, array: bool, statements: &str) -> String {
    let dest = path_literal(dest);
    let temp = path_literal(temp);
    let (format, array) = if array {
        ("array", "true")
    } else {
        ("newline_delimited", "false")
    };
    format!(
        "CREATE TABLE fixture AS FROM read_json({dest}, format = '{format}');\n\
         {}\
         COPY fixture TO {temp} (FORMAT json, ARRAY {array});\n",
        body(statements)
    )
}

/// The name of the only sheet in the workbook at `path`, and the last cell of its used
/// range (`"D88"`): the worksheet's `<dimension ref>`, or the furthest column and row of
/// any cell when it has none. A workbook with more than one sheet is refused, since DuckDB
/// writes one sheet back and dropping the others would test a different file.
fn xlsx_layout(path: &Path) -> std::result::Result<(String, String), String> {
    let shown = path.display();
    let file =
        std::fs::File::open(path).map_err(|e| format!("{shown} could not be opened: {e}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("{shown} is not an xlsx workbook: {e}"))?;
    let mut member = |name: &str| -> std::result::Result<String, String> {
        let mut entry = archive
            .by_name(name)
            .map_err(|e| format!("{shown} has no {name}: {e}"))?;
        let mut text = String::new();
        entry
            .read_to_string(&mut text)
            .map_err(|e| format!("{shown}: {name} could not be read: {e}"))?;
        Ok(text)
    };

    let workbook = member("xl/workbook.xml")?;
    let sheet_tag = Regex::new(r"<(?:\w+:)?sheet\s[^>]*>").expect("a valid pattern");
    let sheets: Vec<&str> = sheet_tag
        .find_iter(&workbook)
        .map(|found| found.as_str())
        .collect();
    let names: Vec<String> = sheets
        .iter()
        .filter_map(|tag| attribute(tag, "name"))
        .collect();
    let tag = match sheets.as_slice() {
        [] => return Err(format!("{shown} has no sheet")),
        [only] => *only,
        _ => {
            return Err(format!(
                "{shown} has more than one sheet ({}), and DuckDB writes back only one; check \
                 in a second fixture with the change made instead",
                names.join(", ")
            ));
        }
    };
    let name = attribute(tag, "name").ok_or_else(|| format!("{shown}: its sheet has no name"))?;
    let id = attribute(tag, "r:id").ok_or_else(|| format!("{shown}: its sheet has no r:id"))?;

    let rels = member("xl/_rels/workbook.xml.rels")?;
    let relationship = Regex::new(r"<(?:\w+:)?Relationship\s[^>]*>").expect("a valid pattern");
    let target = relationship
        .find_iter(&rels)
        .map(|found| found.as_str())
        .find(|tag| attribute(tag, "Id").as_deref() == Some(id.as_str()))
        .and_then(|tag| attribute(tag, "Target"))
        .ok_or_else(|| format!("{shown}: no worksheet is named for sheet '{name}'"))?;
    let sheet_path = match target.strip_prefix('/') {
        Some(absolute) => absolute.to_string(),
        None => format!("xl/{target}"),
    };
    let worksheet = member(&sheet_path)?;

    let dimension =
        Regex::new(r#"<(?:\w+:)?dimension\s[^>]*\bref="([^"]+)""#).expect("a valid pattern");
    if let Some(found) = dimension.captures(&worksheet) {
        let range = &found[1];
        let end = range.rsplit(':').next().unwrap_or(range);
        return Ok((name, end.to_ascii_uppercase()));
    }
    let cell =
        Regex::new(r#"<(?:\w+:)?c\s[^>]*\br="([A-Za-z]+)([0-9]+)""#).expect("a valid pattern");
    let mut furthest: Option<(u64, u64)> = None;
    for found in cell.captures_iter(&worksheet) {
        let column = column_number(&found[1]);
        let row: u64 = found[2].parse().unwrap_or(0);
        let (max_column, max_row) = furthest.unwrap_or((0, 0));
        furthest = Some((max_column.max(column), max_row.max(row)));
    }
    let (column, row) = furthest.ok_or_else(|| format!("{shown}: sheet '{name}' has no cells"))?;
    Ok((name, format!("{}{row}", column_letters(column))))
}

/// The value of `name` in the XML start tag `tag`, with XML's escapes undone.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let pattern = Regex::new(&format!(r#"\s{}="([^"]*)""#, regex::escape(name))).ok()?;
    let raw = pattern.captures(tag)?.get(1)?.as_str();
    Some(
        raw.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

/// `A` is 1, `Z` 26, `AA` 27.
fn column_number(letters: &str) -> u64 {
    letters.bytes().fold(0, |n, b| {
        n * 26 + u64::from(b.to_ascii_uppercase() - b'A' + 1)
    })
}

/// 1 is `A`, 27 `AA`.
fn column_letters(mut n: u64) -> String {
    let mut letters = Vec::new();
    while n > 0 {
        let rem = (n - 1) % 26;
        letters.push(b'A' + rem as u8);
        n = (n - 1) / 26;
    }
    letters.reverse();
    String::from_utf8(letters).expect("ASCII letters")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{DuckDbEngine, duckdb_program};
    use std::io::Write;
    use std::path::PathBuf;

    const TITLE_BLOCK: &str = "tests/fixtures/arc_test/title_block.xlsx";

    fn repo() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    /// What the DuckDB command line prints for `sql`, as CSV.
    fn query(sql: &str) -> String {
        let output = duckdb_program()
            .unwrap()
            .command()
            .args(["-csv", "-c", sql])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    /// Every cell of `path` from A1 to `end`, one CSV line per row.
    fn cells(path: &Path, end: &str) -> Vec<String> {
        query(&format!(
            "LOAD excel; SELECT * FROM read_xlsx({}, header = false, all_varchar = true, \
             stop_at_empty = false, range = 'A1:{end}');",
            path_literal(path)
        ))
        .lines()
        .skip(1)
        .map(str::to_string)
        .collect()
    }

    /// A Protocol folder holding `fixture` under `testdata/`, written with `bytes`, and an
    /// empty copy beside it.
    fn protocol_with(fixture: &str, bytes: &[u8]) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let protocol = root.path().join("protocol");
        let copy = root.path().join("copy");
        std::fs::create_dir_all(protocol.join("testdata")).unwrap();
        std::fs::create_dir_all(&copy).unwrap();
        std::fs::write(protocol.join("testdata").join(fixture), bytes).unwrap();
        (root, protocol, copy)
    }

    fn placement(fixture: &str, dest: &str, statements: Option<&str>) -> Placement {
        Placement {
            fixture: format!("testdata/{fixture}"),
            dest: dest.to_string(),
            statements: statements.map(str::to_string),
        }
    }

    fn title_block() -> (tempfile::TempDir, PathBuf, PathBuf) {
        protocol_with(
            "title_block.xlsx",
            &std::fs::read(repo().join(TITLE_BLOCK)).unwrap(),
        )
    }

    /// A zip holding `entries`, as a workbook arc reads only the layout of.
    fn zip_of(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, body) in entries {
                zw.start_file(*name, opts).unwrap();
                zw.write_all(body.as_bytes()).unwrap();
            }
            zw.finish().unwrap();
        }
        buf
    }

    const RELS: &str = r#"<Relationships><Relationship Id="rId1" Type="worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="worksheet" Target="worksheets/sheet2.xml"/></Relationships>"#;

    #[test]
    fn the_checked_in_workbook_gives_its_sheet_and_dimension() {
        let layout = xlsx_layout(&repo().join(TITLE_BLOCK)).unwrap();
        assert_eq!(layout, ("Sheet1".to_string(), "D10".to_string()));
    }

    #[test]
    fn a_sheet_with_no_dimension_ends_at_its_furthest_cell() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.xlsx");
        let bytes = zip_of(&[
            (
                "xl/workbook.xml",
                r#"<workbook><sheets><sheet name="Codes &amp; names" sheetId="1" r:id="rId2"/></sheets></workbook>"#,
            ),
            ("xl/_rels/workbook.xml.rels", RELS),
            (
                "xl/worksheets/sheet2.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"/></row><row r="3"><c r="AB3"/></row><row r="12"><c t="s" r="C12"/></row></sheetData></worksheet>"#,
            ),
        ]);
        std::fs::write(&path, bytes).unwrap();
        let layout = xlsx_layout(&path).unwrap();
        assert_eq!(layout, ("Codes & names".to_string(), "AB12".to_string()));
    }

    #[test]
    fn a_workbook_with_two_sheets_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.xlsx");
        let bytes = zip_of(&[
            (
                "xl/workbook.xml",
                r#"<workbook><sheets><sheet name="One" sheetId="1" r:id="rId1"/><sheet name="Two" sheetId="2" r:id="rId2"/></sheets></workbook>"#,
            ),
            ("xl/_rels/workbook.xml.rels", RELS),
        ]);
        std::fs::write(&path, bytes).unwrap();
        let refusal = xlsx_layout(&path).unwrap_err();
        assert!(refusal.contains("more than one sheet"), "{refusal}");
        assert!(refusal.contains("One, Two"), "{refusal}");
    }

    #[test]
    fn column_letters_and_numbers_agree() {
        for (letters, n) in [("A", 1), ("Z", 26), ("AA", 27), ("AZ", 52), ("ZZ", 702)] {
            assert_eq!(column_number(letters), n);
            assert_eq!(column_letters(n), letters);
        }
    }

    #[test]
    fn a_plain_placement_copies_the_file_into_new_folders() {
        let (_root, protocol, copy) = protocol_with("a.txt", b"as it is\n");
        let placed = placement("a.txt", "build/raw/deep/a.txt", None);
        place(&copy, &placed, &protocol, &DuckDbEngine, RoundTrip::None).unwrap();
        assert_eq!(
            std::fs::read(copy.join("build/raw/deep/a.txt")).unwrap(),
            b"as it is\n"
        );
    }

    #[test]
    fn an_xls_break_is_refused() {
        let (_root, protocol, copy) = protocol_with("old.XLS", b"not read");
        let placed = placement("old.XLS", "build/raw/old.xls", Some("DELETE FROM fixture;"));
        let refusal =
            place(&copy, &placed, &protocol, &DuckDbEngine, RoundTrip::Break).unwrap_err();
        assert!(refusal.contains("testdata/old.XLS"), "{refusal}");
        assert!(refusal.contains("check in a second fixture"), "{refusal}");
    }

    #[test]
    #[ignore = "needs a DuckDB 2 CLI with the excel extension"]
    fn a_workbook_duckdb_writes_gives_its_sheet_and_last_cell() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.xlsx");
        query(&format!(
            "LOAD excel; COPY (SELECT * FROM (VALUES ('a', 'b', 'c'), ('d', NULL, 'f')) t(x, y, z)) \
             TO {} (FORMAT xlsx, HEADER true, SHEET 'Data');",
            path_literal(&path)
        ));
        let layout = xlsx_layout(&path).unwrap();
        assert_eq!(layout, ("Data".to_string(), "C3".to_string()));
    }

    #[test]
    #[ignore = "needs a DuckDB 2 CLI with the excel extension"]
    fn an_xlsx_control_round_trip_keeps_every_cell() {
        let (_root, protocol, copy) = title_block();
        let placed = placement("title_block.xlsx", "build/raw/t.xlsx", None);
        place(&copy, &placed, &protocol, &DuckDbEngine, RoundTrip::Control).unwrap();
        let before = cells(&protocol.join("testdata/title_block.xlsx"), "D10");
        let after = cells(&copy.join("build/raw/t.xlsx"), "D10");
        assert_eq!(before.len(), 10, "{before:?}");
        assert_eq!(before[0], "Survey of things 2018,NULL,NULL,NULL");
        assert_eq!(before[7], "NULL,NULL,NULL,NULL");
        assert_eq!(after, before);
        assert_eq!(
            xlsx_layout(&copy.join("build/raw/t.xlsx")).unwrap(),
            ("Sheet1".to_string(), "D10".to_string())
        );
    }

    #[test]
    #[ignore = "needs a DuckDB 2 CLI with the excel extension"]
    fn an_xlsx_break_updates_a_cell() {
        let (_root, protocol, copy) = title_block();
        let placed = placement(
            "title_block.xlsx",
            "build/raw/t.xlsx",
            Some("UPDATE fixture SET B = 'Code' WHERE row = 4;"),
        );
        place(&copy, &placed, &protocol, &DuckDbEngine, RoundTrip::Break).unwrap();
        let mut expected = cells(&protocol.join("testdata/title_block.xlsx"), "D10");
        assert_eq!(expected[3], "Name,Ref,Count,Note");
        expected[3] = "Name,Code,Count,Note".to_string();
        assert_eq!(cells(&copy.join("build/raw/t.xlsx"), "D10"), expected);
    }

    #[test]
    #[ignore = "needs a DuckDB 2 CLI with the excel extension"]
    fn an_xlsx_break_inserts_a_row() {
        let (_root, protocol, copy) = title_block();
        let placed = placement(
            "title_block.xlsx",
            "build/raw/t.xlsx",
            Some(
                "UPDATE fixture SET row = row + 1 WHERE row >= 4;\n\
                 INSERT INTO fixture (row, A) VALUES (4, 'Inserted above the headers');",
            ),
        );
        place(&copy, &placed, &protocol, &DuckDbEngine, RoundTrip::Break).unwrap();
        let mut expected = cells(&protocol.join("testdata/title_block.xlsx"), "D10");
        expected.insert(3, "Inserted above the headers,NULL,NULL,NULL".to_string());
        let written = copy.join("build/raw/t.xlsx");
        assert_eq!(xlsx_layout(&written).unwrap().1, "D11");
        let after = cells(&written, "D11");
        assert_eq!(after[4], "Name,Ref,Count,Note");
        assert_eq!(after, expected);
    }

    #[test]
    #[ignore = "needs a DuckDB 2 CLI with the excel extension"]
    fn a_csv_round_trip_keeps_delimiter_and_header() {
        let text = "code;name\n007;Alpha\n010;\"Be;ta\"\n";
        let (_root, protocol, copy) = protocol_with("codes.csv", text.as_bytes());
        let placed = placement("codes.csv", "build/raw/codes.csv", None);
        place(&copy, &placed, &protocol, &DuckDbEngine, RoundTrip::Control).unwrap();
        assert_eq!(
            std::fs::read_to_string(copy.join("build/raw/codes.csv")).unwrap(),
            text
        );
    }

    #[test]
    #[ignore = "needs a DuckDB 2 CLI with the excel extension"]
    fn a_csv_break_changes_a_value_and_keeps_the_rest() {
        let text = "code;name\n007;Alpha\n010;Beta\n";
        let (_root, protocol, copy) = protocol_with("codes.csv", text.as_bytes());
        let placed = placement(
            "codes.csv",
            "build/raw/codes.csv",
            Some("UPDATE fixture SET code = '7' WHERE name = 'Alpha';"),
        );
        place(&copy, &placed, &protocol, &DuckDbEngine, RoundTrip::Break).unwrap();
        assert_eq!(
            std::fs::read_to_string(copy.join("build/raw/codes.csv")).unwrap(),
            "code;name\n7;Alpha\n010;Beta\n"
        );
    }

    #[test]
    #[ignore = "needs a DuckDB 2 CLI with the excel extension"]
    fn a_json_array_stays_an_array() {
        let text = "  [{\"code\": \"007\", \"n\": 1}, {\"code\": \"010\", \"n\": 2}]\n";
        let (_root, protocol, copy) = protocol_with("codes.json", text.as_bytes());
        let placed = placement(
            "codes.json",
            "build/raw/codes.json",
            Some("DELETE FROM fixture WHERE n = 2;"),
        );
        place(&copy, &placed, &protocol, &DuckDbEngine, RoundTrip::Break).unwrap();
        let written = std::fs::read_to_string(copy.join("build/raw/codes.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(value, serde_json::json!([{"code": "007", "n": 1}]));
    }

    #[test]
    #[ignore = "needs a DuckDB 2 CLI with the excel extension"]
    fn ndjson_stays_ndjson() {
        let text = "{\"code\": \"007\", \"n\": 1}\n{\"code\": \"010\", \"n\": 2}\n";
        let (_root, protocol, copy) = protocol_with("codes.ndjson", text.as_bytes());
        let placed = placement("codes.ndjson", "build/raw/codes.ndjson", None);
        place(&copy, &placed, &protocol, &DuckDbEngine, RoundTrip::Control).unwrap();
        let written = std::fs::read_to_string(copy.join("build/raw/codes.ndjson")).unwrap();
        let lines: Vec<serde_json::Value> = written
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            lines,
            vec![
                serde_json::json!({"code": "007", "n": 1}),
                serde_json::json!({"code": "010", "n": 2})
            ]
        );
    }

    #[test]
    #[ignore = "needs a DuckDB 2 CLI with the excel extension"]
    fn a_break_that_duckdb_rejects_names_the_fixture_and_leaves_no_temporary_file() {
        let (_root, protocol, copy) = protocol_with("codes.csv", b"code\n007\n");
        let placed = placement(
            "codes.csv",
            "build/raw/codes.csv",
            Some("UPDATE fixture SET nonesuch = 1;"),
        );
        let refusal =
            place(&copy, &placed, &protocol, &DuckDbEngine, RoundTrip::Break).unwrap_err();
        assert!(
            refusal.starts_with("the break of testdata/codes.csv did not run: "),
            "{refusal}"
        );
        assert!(refusal.contains("nonesuch"), "{refusal}");
        let left: Vec<_> = std::fs::read_dir(copy.join("build/raw"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, vec![std::ffi::OsString::from("codes.csv")]);
    }

    #[test]
    #[ignore = "needs a DuckDB 2 CLI with the excel extension"]
    fn a_break_installing_an_unvetted_extension_is_refused() {
        let (_root, protocol, copy) = protocol_with("codes.csv", b"code\n007\n");
        let placed = placement(
            "codes.csv",
            "build/raw/codes.csv",
            Some("INSTALL nonesuch_ext FROM community;"),
        );
        let refusal =
            place(&copy, &placed, &protocol, &DuckDbEngine, RoundTrip::Break).unwrap_err();
        assert!(
            refusal.starts_with("the break of testdata/codes.csv was refused"),
            "{refusal}"
        );
        assert!(refusal.contains("nonesuch_ext"), "{refusal}");
        assert_eq!(
            std::fs::read(copy.join("build/raw/codes.csv")).unwrap(),
            b"code\n007\n"
        );
    }
}
