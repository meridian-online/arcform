//! The `finetype` command line, run with a version floor: the one place arc finds
//! the binary, checks its version and runs it, for the MCP tools that federate it
//! and for `arc table profile`.

use std::process::{Command, Stdio};

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
pub(crate) const MIN_VERSION: (u64, u64, u64) = (0, 6, 54);

/// The `finetype` binary to run: `$FINETYPE_BIN` if set, else `finetype` on PATH.
pub(crate) fn finetype_bin() -> String {
    std::env::var("FINETYPE_BIN").unwrap_or_else(|_| "finetype".to_string())
}

pub(crate) fn min_version() -> semver::Version {
    let (major, minor, patch) = MIN_VERSION;
    semver::Version::new(major, minor, patch)
}

/// Parse a `finetype --version` line — `finetype 0.6.53`, or a bare `0.6.53`, with an
/// optional leading `v` — into a semver.
pub(crate) fn parse_version(text: &str) -> Option<semver::Version> {
    let token = text.lines().next()?.split_whitespace().next_back()?;
    semver::Version::parse(token.trim_start_matches('v')).ok()
}

/// Run `<bin> --version` and parse the result.
pub(crate) fn detect_version(bin: &str) -> std::result::Result<semver::Version, String> {
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
pub(crate) fn ensure_min_version(bin: &str) -> std::result::Result<semver::Version, String> {
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
pub(crate) fn run_cli(
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
