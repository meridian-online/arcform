//! The five FineType-federating MCP tools.
//!
//! Each shells the `finetype` CLI (on PATH, or the path in `$FINETYPE_BIN`) rather than
//! reimplementing type inference — the same posture the `datapackage_describe` operator
//! already takes. Every call is gated on a **minimum FineType version** ([`MIN_VERSION`])
//! so a stale binary — the documented cause of mislabelled columns — is refused with a
//! clear message instead of silently producing wrong types.
//!
//! The tool set mirrors the FineType MCP server it replaces: `infer`, `profile`,
//! `taxonomy`, `validate`, `generate`. Argument shapes map onto the CLI's flags.

use std::process::{Command, Stdio};

use serde_json::{Value, json};

use super::{ToolDef, ToolOutput, ToolResult};

/// The minimum `finetype` version these proxies target. The CLI surface they rely on
/// (JSON-Schema `profile`/`taxonomy` output, Parquet `validate`, batch `infer`) landed
/// earlier, but the floor tracks *label correctness*, not just surface: 0.6.54 is the
/// release that stopped eight-digit numbers typing as confident dates. Up to and
/// including 0.6.53 the year-first and day-first compact date leaves both validated on
/// `^\d{8}$`, so any eight-digit token — a financial figure, a surrogate key — came back
/// a high-confidence date *with a `strptime` transform attached*. That is worse than the
/// mislabelling 0.6.53 itself fixed: a consumer that follows the transform gets a
/// corrupted column rather than a wrong name for a correct one.
///
/// Kept in lockstep with `MIN_FINETYPE_VERSION` in the `datapackage_describe` operator
/// (`src/operator.rs`) — both gate the same binary on PATH.
const MIN_VERSION: (u64, u64, u64) = (0, 6, 54);

/// The `finetype` binary to run: `$FINETYPE_BIN` if set, else `finetype` on PATH.
fn finetype_bin() -> String {
    std::env::var("FINETYPE_BIN").unwrap_or_else(|_| "finetype".to_string())
}

fn min_version() -> semver::Version {
    let (major, minor, patch) = MIN_VERSION;
    semver::Version::new(major, minor, patch)
}

/// Parse a `finetype --version` line — `finetype 0.6.53`, or a bare `0.6.53`, with an
/// optional leading `v` — into a semver.
fn parse_version(text: &str) -> Option<semver::Version> {
    let token = text.lines().next()?.split_whitespace().next_back()?;
    semver::Version::parse(token.trim_start_matches('v')).ok()
}

/// Run `<bin> --version` and parse the result.
fn detect_version(bin: &str) -> std::result::Result<semver::Version, String> {
    let output = Command::new(bin)
        .arg("--version")
        .output()
        .map_err(|e| format!("could not run `{bin} --version`: {e}. Is finetype installed? Set FINETYPE_BIN to override the binary."))?;
    // clap prints the version to stdout; fall back to stderr just in case.
    let text = if output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stderr)
    } else {
        String::from_utf8_lossy(&output.stdout)
    };
    parse_version(&text)
        .ok_or_else(|| format!("could not parse a finetype version from `{}`", text.trim()))
}

/// The min-version gate: detect the installed version and refuse anything older than
/// [`MIN_VERSION`]. Returns the detected version on success.
fn ensure_min_version(bin: &str) -> std::result::Result<semver::Version, String> {
    let found = detect_version(bin)?;
    let min = min_version();
    if found < min {
        return Err(format!(
            "finetype {found} is older than the minimum {min} these tools require — upgrade finetype (an old binary can silently mistype columns)"
        ));
    }
    Ok(found)
}

/// Spawn `<bin> <args…>`, optionally feeding `stdin_data`, and return captured stdout.
/// A spawn failure or non-zero exit is an `Err` carrying the child's stderr.
fn run_cli(
    bin: &str,
    args: &[String],
    stdin_data: Option<&str>,
) -> std::result::Result<String, String> {
    let mut command = Command::new(bin);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin_data.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });

    let mut child = command
        .spawn()
        .map_err(|e| format!("could not spawn `{bin}`: {e}"))?;

    if let Some(data) = stdin_data {
        use std::io::Write;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "failed to open finetype stdin".to_string())?;
        stdin
            .write_all(data.as_bytes())
            .map_err(|e| format!("writing to finetype stdin: {e}"))?;
        // Drop closes stdin so the child sees EOF.
        drop(stdin);
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("waiting on finetype: {e}"))?;
    if !output.status.success() {
        let subcommand = args.first().map(String::as_str).unwrap_or("");
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "`finetype {subcommand}` failed ({}): {}",
            output.status,
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

// ─────────────────────────────────────────────────────────────────────────────
// Small argument readers
// ─────────────────────────────────────────────────────────────────────────────

fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn opt_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

fn opt_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tools — each gates on the min version, then federates one `finetype` subcommand.
// The `*_with` forms take the binary explicitly so tests can drive a fixture without
// touching the process environment.
// ─────────────────────────────────────────────────────────────────────────────

fn infer(args: &Value) -> ToolResult {
    infer_with(&finetype_bin(), args)
}

fn infer_with(bin: &str, args: &Value) -> ToolResult {
    ensure_min_version(bin)?;
    let values: Vec<String> = if let Some(array) = args.get("values").and_then(Value::as_array) {
        array
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    } else if let Some(single) = opt_str(args, "value") {
        vec![single.to_string()]
    } else {
        return Err(
            "infer requires `values` (array of strings) or `value` (a single string)".to_string(),
        );
    };
    if values.is_empty() {
        return Err("infer: `values` must be a non-empty array of strings".to_string());
    }
    let header = opt_str(args, "header");
    // Column-mode batch inference reads a JSON object from stdin and writes one JSON
    // line per input — no temp file needed.
    let payload = json!({ "header": header, "values": values }).to_string();
    let stdin = format!("{payload}\n");
    let cli = vec![
        "infer".to_string(),
        "--mode".to_string(),
        "column".to_string(),
        "--batch".to_string(),
    ];
    let stdout = run_cli(bin, &cli, Some(stdin.as_str()))?;
    Ok(ToolOutput::from_cli(stdout))
}

fn profile(args: &Value) -> ToolResult {
    profile_with(&finetype_bin(), args)
}

fn profile_with(bin: &str, args: &Value) -> ToolResult {
    ensure_min_version(bin)?;
    let file = opt_str(args, "file")
        .or_else(|| opt_str(args, "path"))
        .ok_or_else(|| "profile requires `file` (path to a CSV/JSON file)".to_string())?;
    let format = opt_str(args, "format").unwrap_or("json");
    let mut cli = vec![
        "profile".to_string(),
        "--file".to_string(),
        file.to_string(),
        "--output".to_string(),
        format.to_string(),
    ];
    if let Some(sample) = opt_u64(args, "sample_size") {
        cli.push("--sample-size".to_string());
        cli.push(sample.to_string());
    }
    if let Some(delimiter) = opt_str(args, "delimiter") {
        cli.push("--delimiter".to_string());
        cli.push(delimiter.to_string());
    }
    if opt_bool(args, "stats").unwrap_or(false) {
        cli.push("--stats".to_string());
    }
    let stdout = run_cli(bin, &cli, None)?;
    Ok(ToolOutput::from_cli(stdout))
}

fn taxonomy(args: &Value) -> ToolResult {
    taxonomy_with(&finetype_bin(), args)
}

fn taxonomy_with(bin: &str, args: &Value) -> ToolResult {
    ensure_min_version(bin)?;
    let format = opt_str(args, "format").unwrap_or("json");
    let mut cli = vec!["taxonomy".to_string()];
    if let Some(key) = opt_str(args, "type_key").or_else(|| opt_str(args, "key")) {
        cli.push(key.to_string());
    }
    cli.push("--output".to_string());
    cli.push(format.to_string());
    if let Some(domain) = opt_str(args, "domain") {
        cli.push("--domain".to_string());
        cli.push(domain.to_string());
    }
    if let Some(category) = opt_str(args, "category") {
        cli.push("--category".to_string());
        cli.push(category.to_string());
    }
    if let Some(priority) = opt_u64(args, "priority") {
        cli.push("--priority".to_string());
        cli.push(priority.to_string());
    }
    if opt_bool(args, "full").unwrap_or(false) {
        cli.push("--full".to_string());
    }
    if let Some(labels) = opt_str(args, "taxonomy_file") {
        cli.push("--file".to_string());
        cli.push(labels.to_string());
    }
    let stdout = run_cli(bin, &cli, None)?;
    Ok(ToolOutput::from_cli(stdout))
}

fn validate(args: &Value) -> ToolResult {
    validate_with(&finetype_bin(), args)
}

fn validate_with(bin: &str, args: &Value) -> ToolResult {
    ensure_min_version(bin)?;
    let file = opt_str(args, "file")
        .or_else(|| opt_str(args, "path"))
        .ok_or_else(|| "validate requires `file` (path to a CSV/Parquet file)".to_string())?;
    let schema = opt_str(args, "schema")
        .ok_or_else(|| "validate requires `schema` (path to a JSON Schema file)".to_string())?;
    let format = opt_str(args, "format").unwrap_or("json");
    let mut cli = vec![
        "validate".to_string(),
        file.to_string(),
        schema.to_string(),
        "--output".to_string(),
        format.to_string(),
    ];
    // Default to lenient so a report with rejected rows still returns exit 0 with the
    // quality report as content; `strict: true` lets rejects fail the call instead.
    if !opt_bool(args, "strict").unwrap_or(false) {
        cli.push("--lenient".to_string());
    }
    let stdout = run_cli(bin, &cli, None)?;
    Ok(ToolOutput::from_cli(stdout))
}

fn generate(args: &Value) -> ToolResult {
    generate_with(&finetype_bin(), args)
}

fn generate_with(bin: &str, args: &Value) -> ToolResult {
    ensure_min_version(bin)?;
    let samples = opt_u64(args, "samples").unwrap_or(10);
    // The CLI `generate` writes to a file; capture it via a per-process temp path.
    let out_path =
        std::env::temp_dir().join(format!("arc-mcp-generate-{}.ndjson", std::process::id()));
    let mut cli = vec![
        "generate".to_string(),
        "--samples".to_string(),
        samples.to_string(),
        "--output".to_string(),
        out_path.display().to_string(),
    ];
    if let Some(priority) = opt_u64(args, "priority") {
        cli.push("--priority".to_string());
        cli.push(priority.to_string());
    }
    if let Some(seed) = opt_u64(args, "seed") {
        cli.push("--seed".to_string());
        cli.push(seed.to_string());
    }
    if opt_bool(args, "localized").unwrap_or(false) {
        cli.push("--localized".to_string());
    }
    if let Some(labels) = opt_str(args, "taxonomy_file") {
        cli.push("--taxonomy".to_string());
        cli.push(labels.to_string());
    }
    run_cli(bin, &cli, None)?;
    let content = std::fs::read_to_string(&out_path)
        .map_err(|e| format!("reading generated output {}: {e}", out_path.display()))?;
    let _ = std::fs::remove_file(&out_path);
    Ok(ToolOutput::text(content))
}

// ─────────────────────────────────────────────────────────────────────────────
// Input schemas + registration
// ─────────────────────────────────────────────────────────────────────────────

fn infer_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "values": { "type": "array", "items": { "type": "string" }, "description": "String values to classify (a column's sample; pass several for column-mode)." },
            "value": { "type": "string", "description": "A single value to classify (alternative to `values`)." },
            "header": { "type": "string", "description": "Optional column header for context-aware classification." }
        }
    })
}

fn profile_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "file": { "type": "string", "description": "Path to a CSV/JSON file to profile." },
            "format": { "type": "string", "enum": ["json", "json-schema"], "default": "json", "description": "Output shape." },
            "sample_size": { "type": "integer", "minimum": 1, "description": "Max values sampled per column." },
            "delimiter": { "type": "string", "description": "CSV delimiter (auto-detected when omitted)." },
            "stats": { "type": "boolean", "description": "Attach observed-data constraints to json-schema output." }
        },
        "required": ["file"]
    })
}

fn taxonomy_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "type_key": { "type": "string", "description": "Type key (identity.person.email) or glob (identity.person.*)." },
            "domain": { "type": "string", "description": "Filter by domain." },
            "category": { "type": "string", "description": "Filter by category." },
            "priority": { "type": "integer", "description": "Minimum release priority." },
            "format": { "type": "string", "enum": ["json", "json-schema", "plain", "csv"], "default": "json", "description": "Output shape." },
            "full": { "type": "boolean", "description": "Export all fields (description, validation, samples)." },
            "taxonomy_file": { "type": "string", "description": "Taxonomy file/dir (defaults to finetype's bundled labels)." }
        }
    })
}

fn validate_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "file": { "type": "string", "description": "Path to a CSV/Parquet file to validate." },
            "schema": { "type": "string", "description": "Path to a JSON Schema file to validate against." },
            "format": { "type": "string", "enum": ["json", "plain"], "default": "json", "description": "Report format." },
            "strict": { "type": "boolean", "description": "Fail the call when rows are rejected (default: report only)." }
        },
        "required": ["file", "schema"]
    })
}

fn generate_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "samples": { "type": "integer", "minimum": 1, "default": 10, "description": "Samples generated per label." },
            "priority": { "type": "integer", "description": "Minimum release priority." },
            "seed": { "type": "integer", "description": "Random seed for reproducibility." },
            "localized": { "type": "boolean", "description": "Emit 4-level labels with locale suffixes." },
            "taxonomy_file": { "type": "string", "description": "Taxonomy file/dir (defaults to finetype's bundled labels)." }
        }
    })
}

/// The five FineType-federating tools.
pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "infer",
            description: "Infer the semantic type of string values (federates `finetype infer`). Pass `values` (a column's sample) with an optional `header`.",
            input_schema: infer_schema,
            handler: infer,
        },
        ToolDef {
            name: "profile",
            description: "Profile every column of a CSV/JSON file — semantic types, or a table-level JSON Schema (federates `finetype profile`).",
            input_schema: profile_schema,
            handler: profile,
        },
        ToolDef {
            name: "taxonomy",
            description: "Browse the FineType type taxonomy, or export per-type JSON Schema (federates `finetype taxonomy`).",
            input_schema: taxonomy_schema,
            handler: taxonomy,
        },
        ToolDef {
            name: "validate",
            description: "Validate CSV/Parquet data against a JSON Schema and return a quality report (federates `finetype validate`).",
            input_schema: validate_schema,
            handler: validate,
        },
        ToolDef {
            name: "generate",
            description: "Generate synthetic sample data from the taxonomy (federates `finetype generate`).",
            input_schema: generate_schema,
            handler: generate,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_handles_finetype_and_bare_forms() {
        assert_eq!(
            parse_version("finetype 0.6.53\n"),
            Some(semver::Version::new(0, 6, 53))
        );
        assert_eq!(
            parse_version("0.6.52"),
            Some(semver::Version::new(0, 6, 52))
        );
        assert_eq!(parse_version("v1.2.3"), Some(semver::Version::new(1, 2, 3)));
        assert_eq!(parse_version("not a version"), None);
        assert_eq!(parse_version(""), None);
    }

    /// Every release the floor exists to keep out, with the defect that put it there.
    /// Facts about those binaries, not about the constant — so they stay rejected
    /// however the constant is edited.
    const SUPERSEDED_RELEASES: &[(semver::Version, &str)] = &[
        (
            semver::Version::new(0, 6, 52),
            "wrong ticker / industry-level / legal-name labels",
        ),
        (
            semver::Version::new(0, 6, 53),
            "eight-digit figures typed as confident dates, with a transform",
        ),
    ];

    #[test]
    fn min_version_constant_is_the_documented_floor() {
        assert_eq!(min_version(), semver::Version::new(0, 6, 54));
        // The floor must sit ABOVE every superseded release — pin that relation, not
        // just the literal, so a careless edit to the constant cannot quietly re-admit
        // one of those binaries.
        for (release, defect) in SUPERSEDED_RELEASES {
            assert!(
                min_version() > *release,
                "the floor must sit above {release} — {defect}"
            );
        }
    }

    #[test]
    fn all_five_proxies_are_registered() {
        let names: Vec<&str> = tools().iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            ["infer", "profile", "taxonomy", "validate", "generate"]
        );
    }

    /// Point `dir/finetype` at the checked-in `finetype` stand-in
    /// (`tests/fixtures/fake_finetype_dispatcher.sh`) so the gate can be exercised
    /// through the same subprocess call production uses, with no real binary.
    ///
    /// REMEDY TAKEN (of the available ones) and why, so nobody rediscovers the
    /// race this replaced: the previous version wrote a fresh `#!/bin/sh` script
    /// straight into `dir` with `std::fs::write` (open, write, implicit close)
    /// and then exec'd that same path. `std::fs::write` closes its own handle
    /// before returning, so nothing about THIS thread was unsound — but `cargo
    /// test` runs hundreds of tests across parallel threads, and `fork()`
    /// duplicates a process's *entire* file-descriptor table, from every thread,
    /// at the instant it is called. If any OTHER thread called `fork()` (e.g. via
    /// `Command::spawn` for a wholly unrelated subprocess — this crate has half a
    /// dozen call sites) while this thread's write-handle on the fixture was
    /// still open, the forked child inherited a duplicate of that writable
    /// descriptor and held it open until ITS OWN exec completed. If this thread's
    /// exec of the same fixture path landed inside that window, the kernel's
    /// execve() refused with ETXTBSY ("Text file busy") — CI run 31676889338,
    /// `src/mcp/finetype.rs:516`. The failing thread was never the one at fault,
    /// which is why the failure moved around and why a bare re-run cleared it.
    ///
    /// A retry loop around the exec would have "fixed" the symptom without
    /// touching the mechanism — it removes the signal (a real, rare failure
    /// mode) rather than the flake, so it was rejected. The alternative taken
    /// here removes the mechanism instead: `dir/finetype` is now a *symlink* to a fixture file
    /// this process never writes at all. `symlink()` is a directory-entry
    /// operation — it never opens its target for writing, so creating it can
    /// never race anyone's fork. And the checked-in dispatcher it points at
    /// exists on disk from the git checkout, before this test binary — let alone
    /// its thread pool — is even loaded, so there is no window, ever, in which
    /// any thread could hold it open for writing. The one thing still written
    /// per call, the sidecar `version` file, is only ever *read* (by the
    /// dispatcher, via `dirname "$0"`) — never exec'd — and ETXTBSY guards only
    /// exec targets, so a concurrent fork racing that write is harmless.
    #[cfg(unix)]
    fn fake_finetype(dir: &std::path::Path, version: &str) -> std::path::PathBuf {
        let dispatcher = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/fake_finetype_dispatcher.sh"
        ));
        std::fs::write(dir.join("version"), version).unwrap();
        let path = dir.join("finetype");
        std::os::unix::fs::symlink(dispatcher, &path).unwrap();
        path
    }

    /// The exact binaries the floor exists to keep out. Nothing downstream masks
    /// their output any more — 0.6.52 emits superseded labels, and 0.6.53 attaches a
    /// `strptime` transform to eight-digit figures — so the gate is the last line of
    /// defence for both.
    #[cfg(unix)]
    #[test]
    fn every_superseded_release_is_refused() {
        for (release, defect) in SUPERSEDED_RELEASES {
            let version = release.to_string();
            let dir = tempfile::tempdir().unwrap();
            let bin = fake_finetype(dir.path(), &version);

            let err = ensure_min_version(bin.to_str().unwrap())
                .expect_err(&format!("the gate must refuse {version} — {defect}"));
            assert!(
                err.contains("older than the minimum"),
                "gate message for {version}: {err}"
            );

            // …and the refusal must happen before any subcommand runs, on the tool path.
            let err = taxonomy_with(bin.to_str().unwrap(), &json!({ "format": "json" }))
                .expect_err("the tool path must refuse it too");
            assert!(
                err.contains(&version),
                "gate message names the version: {err}"
            );
        }

        // The floor itself passes — the gate refuses what is BELOW it, not everything.
        let ok_dir = tempfile::tempdir().unwrap();
        let ok_bin = fake_finetype(ok_dir.path(), "0.6.54");
        ensure_min_version(ok_bin.to_str().unwrap()).expect("the floor itself must pass");
    }

    /// The subprocess federation + min-version gate, exercised against a fixture
    /// `finetype` so it runs without the real binary. The fixture echoes its argv, and
    /// reports whatever version we bake into it.
    #[cfg(unix)]
    #[test]
    fn proxy_over_subprocess_gates_on_version_and_forwards_args() {
        let new_dir = tempfile::tempdir().unwrap();
        let old_dir = tempfile::tempdir().unwrap();

        // A new-enough fixture: the gate passes and the subcommand + flags are forwarded.
        let new_bin = fake_finetype(new_dir.path(), "0.6.99");
        let out = taxonomy_with(new_bin.to_str().unwrap(), &json!({ "format": "json" }))
            .expect("gate passes for a new-enough finetype");
        assert!(
            out.text.contains("ARGS:"),
            "fixture stdout returned: {}",
            out.text
        );
        assert!(
            out.text.contains("taxonomy"),
            "subcommand forwarded: {}",
            out.text
        );
        assert!(
            out.text.contains("--output"),
            "flags forwarded: {}",
            out.text
        );

        // An old fixture: the min-version gate trips before any subcommand runs.
        let old_bin = fake_finetype(old_dir.path(), "0.6.10");
        let err = taxonomy_with(old_bin.to_str().unwrap(), &json!({ "format": "json" }))
            .expect_err("gate refuses an old finetype");
        assert!(
            err.contains("older than the minimum"),
            "gate message: {err}"
        );
    }

    #[test]
    fn missing_binary_is_a_clear_error() {
        let err = ensure_min_version("/nonexistent/finetype-xyzzy").unwrap_err();
        assert!(err.contains("could not run"), "unexpected error: {err}");
    }
}
