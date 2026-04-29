//! Subcommand handlers for `arc registry {list, show, fetch, run}`.
//!
//! Output is uv-aesthetic: quiet, signal-dense, terse errors. `--verbose` is
//! threaded through so transport detail appears on opt-in only.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::engine::DuckDbEngine;
use crate::error::{Error, Result};
use crate::registry::cache::{cache_path, ensure_cache_root, IndexCache};
use crate::registry::index::{Pillar, RegistryIndex};
use crate::registry::resolve::{resolve, ResolvedEntry, VersionSpec};
use crate::registry::transport::{Transport, TransportSrc};
use crate::state::DuckDbStateBackend;

/// Options bundle for the registry subcommands. Bundling keeps the call surface
/// stable as flags accumulate.
pub struct RunOptions<'a> {
    pub transport: &'a dyn Transport,
    pub cache_root: PathBuf,
    pub index_url: String,
    pub refresh: bool,
    // Carried for the global `--verbose` flag; the firehose-output paths land in
    // a follow-up spec. Field is part of the public surface contract today so
    // wiring is in place when those paths arrive.
    #[allow(dead_code)]
    pub verbose: bool,
}

impl<'a> RunOptions<'a> {
    fn index(&self) -> Result<RegistryIndex> {
        IndexCache::new(self.transport, self.cache_root.clone(), self.index_url.clone())
            .load(self.refresh)
    }
}

fn version_spec_from_flags(version: Option<String>, latest: bool) -> Option<VersionSpec> {
    match (version, latest) {
        (Some(v), false) => Some(VersionSpec::Pinned(v)),
        (None, true) => Some(VersionSpec::Latest),
        // (None, false) handled by caller; (Some, true) prevented by clap conflicts_with.
        _ => None,
    }
}

fn ensure_cached(
    transport: &dyn Transport,
    cache_root: &Path,
    resolved: &ResolvedEntry,
) -> Result<PathBuf> {
    let dest = cache_path(cache_root, resolved);
    if dest.is_dir() && dest.read_dir().map(|mut d| d.next().is_some()).unwrap_or(false) {
        return Ok(dest);
    }
    ensure_cache_root(&dest)?;
    let src = TransportSrc {
        repo_url: resolved.repo_url.clone(),
        repo_path: resolved.repo_path.clone(),
        ref_: resolved.ref_.clone(),
    };
    transport.fetch(&src, &dest)?;
    Ok(dest)
}

fn dir_size_bytes(path: &Path) -> u64 {
    fn walk(p: &Path, total: &mut u64) {
        if let Ok(rd) = std::fs::read_dir(p) {
            for entry in rd.flatten() {
                let path = entry.path();
                if let Ok(ft) = entry.file_type() {
                    if ft.is_dir() {
                        walk(&path, total);
                    } else if ft.is_file() {
                        if let Ok(meta) = entry.metadata() {
                            *total += meta.len();
                        }
                    }
                }
            }
        }
    }
    let mut total = 0u64;
    walk(path, &mut total);
    total
}

fn format_kb(bytes: u64) -> String {
    let kb = (bytes + 512) / 1024; // round
    format!("{} KB", kb)
}

// -- handle_list (ac-07) ---------------------------------------------------

/// `arc registry list` — uv-style grouped output.
pub fn handle_list<W: Write>(opts: &RunOptions<'_>, out: &mut W) -> Result<()> {
    let index = opts.index()?;
    render_list(&index, out)
}

/// Pure renderer — extracted so tests don't need a transport.
pub fn render_list<W: Write>(index: &RegistryIndex, out: &mut W) -> Result<()> {
    let mut max_name = 0;
    let mut max_ver = 0;
    for e in &index.entries {
        max_name = max_name.max(e.display_name().len());
        max_ver = max_ver.max(e.current_version.len());
    }

    for (i, pillar) in Pillar::ALL_IN_ORDER.iter().enumerate() {
        if i > 0 {
            writeln!(out)?;
        }
        writeln!(out, "{}", pillar.header())?;
        let entries: Vec<_> = index.entries.iter().filter(|e| e.pillar == *pillar).collect();
        if entries.is_empty() {
            writeln!(out, "  (no entries yet)")?;
            continue;
        }
        for e in entries {
            let name = e.display_name();
            writeln!(
                out,
                "  {:<name_w$}  {:<ver_w$}  {}",
                name,
                e.current_version,
                e.summary,
                name_w = max_name,
                ver_w = max_ver,
            )?;
        }
    }
    Ok(())
}

// -- handle_show (ac-08) ---------------------------------------------------

/// `arc registry show <name>` — terse metadata block + README inline.
pub fn handle_show<W: Write>(opts: &RunOptions<'_>, query: &str, out: &mut W) -> Result<()> {
    let index = opts.index()?;
    let resolved = resolve(&index, query, None)?;
    let cache = ensure_cached(opts.transport, &opts.cache_root, &resolved)?;
    render_show(&index, &resolved, &cache, out)
}

fn render_show<W: Write>(
    index: &RegistryIndex,
    resolved: &ResolvedEntry,
    cache: &Path,
    out: &mut W,
) -> Result<()> {
    // Re-locate the entry so we can pull `summary`, `sources`, `schedule_guidance`.
    let entry = index
        .entries
        .iter()
        .find(|e| e.name == resolved.name && e.owner == resolved.owner)
        .expect("resolver guarantees entry presence");

    writeln!(out, "name:              {}", resolved.display_name())?;
    writeln!(out, "pillar:            {}", entry.pillar.header())?;
    writeln!(out, "summary:           {}", entry.summary)?;
    writeln!(out, "current_version:   {}", entry.current_version)?;
    writeln!(out, "repo_url:          {}", entry.repo_url)?;
    if let Some(sg) = &entry.schedule_guidance {
        writeln!(out, "schedule_guidance: {}", sg)?;
    }
    if !entry.sources.is_empty() {
        writeln!(out, "sources:")?;
        for s in &entry.sources {
            writeln!(out, "  - {}", s)?;
        }
    }
    writeln!(out)?;
    let readme = cache.join("README.md");
    if readme.is_file() {
        let body = std::fs::read_to_string(&readme).map_err(|source| Error::RegistryCacheIo {
            path: readme.clone(),
            source,
        })?;
        out.write_all(body.as_bytes())?;
        if !body.ends_with('\n') {
            writeln!(out)?;
        }
    } else {
        writeln!(out, "(no README)")?;
    }
    Ok(())
}

// -- handle_fetch (ac-09) --------------------------------------------------

/// `arc registry fetch <name>` — single completion line on success.
pub fn handle_fetch<W: Write, E: Write>(
    opts: &RunOptions<'_>,
    query: &str,
    version: Option<String>,
    latest: bool,
    out: &mut W,
    err: &mut E,
) -> Result<()> {
    let _ = err; // verbose stream — not used in the success path
    let index = opts.index()?;
    let spec = version_spec_from_flags(version, latest);
    let resolved = resolve(&index, query, spec)?;
    let dest = cache_path(&opts.cache_root, &resolved);
    let already = dest.is_dir()
        && dest.read_dir().map(|mut d| d.next().is_some()).unwrap_or(false);
    if already {
        writeln!(
            out,
            "✓ {} {} (cached)",
            resolved.display_name(),
            resolved.ref_
        )?;
        return Ok(());
    }
    let cache = ensure_cached(opts.transport, &opts.cache_root, &resolved)?;
    let size = dir_size_bytes(&cache);
    writeln!(
        out,
        "➜ {} {} ({})",
        resolved.display_name(),
        resolved.ref_,
        format_kb(size)
    )?;
    Ok(())
}

/// Failure presentation for `fetch` — extracted so the binary entry point can
/// keep error formatting consistent with the success-path prefixes.
#[allow(dead_code)]
pub fn render_fetch_failure<W: Write>(
    err_stream: &mut W,
    display_name: &str,
    ref_: &str,
    err: &Error,
) -> std::io::Result<()> {
    writeln!(err_stream, "✗ {} {}: {}", display_name, ref_, err)
}

// -- handle_run (ac-10) ----------------------------------------------------

/// `arc registry run <name>` — resolves, ensures cache, hands off to runner.
///
/// Does NOT mutate process cwd. `--param` raw strings are parsed via the existing
/// [`crate::cli::parse_params`] helper before any cache or transport work.
pub fn handle_run(
    opts: &RunOptions<'_>,
    query: &str,
    version: Option<String>,
    latest: bool,
    force: bool,
    raw_params: &[String],
) -> Result<()> {
    // Validate --param FIRST so a bad value does not even consult the transport.
    let cli_params = crate::cli::parse_params(raw_params)?;

    let index = opts.index()?;
    let spec = version_spec_from_flags(version, latest);
    let resolved = resolve(&index, query, spec)?;
    let cache = ensure_cached(opts.transport, &opts.cache_root, &resolved)?;

    let engine = DuckDbEngine;
    let manifest = crate::manifest::Manifest::load(&cache)?;
    let db_path = manifest.db_path(&cache);
    let state = DuckDbStateBackend::new(&db_path);
    crate::runner::run_with_params(&cache, &engine, &state, force, &cli_params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::transport::FixtureTransport;
    use tempfile::TempDir;

    fn fixture_index_yaml() -> String {
        r#"
version: 1
entries:
  - name: brewtrend
    pillar: practical
    summary: Homebrew analytics
    repo_url: https://example.com/r
    repo_path: practical/brewtrend
    current_version: v1.0
    sources: []
  - name: gnaf
    pillar: foundational
    summary: AU addresses
    repo_url: https://example.com/r
    repo_path: foundational/gnaf
    current_version: v0.4
    sources: []
  - name: myproject
    owner: someone
    pillar: practical
    summary: Personal example
    repo_url: https://example.com/u
    repo_path: ""
    current_version: v0.3
    sources: []
"#
        .to_string()
    }

    // ac-07: list output — golden assertion across all three pillars + empty Investigative.
    #[test]
    fn test_ac07_list_groups_by_pillar_with_empty_placeholder() {
        let idx = RegistryIndex::parse(&fixture_index_yaml()).unwrap();
        let mut buf = Vec::new();
        render_list(&idx, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();

        // Pillar order.
        let practical = s.find("PRACTICAL").unwrap();
        let foundational = s.find("FOUNDATIONAL").unwrap();
        let investigative = s.find("INVESTIGATIVE").unwrap();
        assert!(practical < foundational && foundational < investigative);

        // Practical has both canonical brewtrend and contributor someone/myproject.
        assert!(s.contains("brewtrend"));
        assert!(s.contains("someone/myproject"));
        // Foundational has gnaf.
        assert!(s.contains("gnaf"));
        // Investigative is empty → placeholder rendered.
        let inv_block = &s[investigative..];
        assert!(
            inv_block.contains("(no entries yet)"),
            "empty pillar should render placeholder; got:\n{}",
            inv_block
        );
    }

    // ac-08: show prints metadata block + README contents inline.
    #[test]
    fn test_ac08_show_renders_metadata_and_readme() {
        let fixture_root = TempDir::new().unwrap();
        let cache_root = TempDir::new().unwrap();

        let src = fixture_root.path().join("practical/brewtrend/v1.0");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("README.md"), "# brewtrend\n\nDaily homebrew signal.\n").unwrap();

        let transport = FixtureTransport::with_tree(
            fixture_root.path().to_path_buf(),
            fixture_index_yaml(),
        );
        let opts = RunOptions {
            transport: &transport,
            cache_root: cache_root.path().to_path_buf(),
            index_url: "u".to_string(),
            refresh: false,
            verbose: false,
        };

        let mut out = Vec::new();
        handle_show(&opts, "brewtrend", &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("name:              brewtrend"));
        assert!(s.contains("pillar:            PRACTICAL"));
        assert!(s.contains("current_version:   v1.0"));
        assert!(s.contains("# brewtrend"));
        assert!(s.contains("Daily homebrew signal"));
    }

    // ac-08: missing README path renders the (no README) line.
    #[test]
    fn test_ac08_show_no_readme_renders_placeholder() {
        let fixture_root = TempDir::new().unwrap();
        let cache_root = TempDir::new().unwrap();

        let src = fixture_root.path().join("practical/brewtrend/v1.0");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("arcform.yaml"), "name: brewtrend\n").unwrap();

        let transport = FixtureTransport::with_tree(
            fixture_root.path().to_path_buf(),
            fixture_index_yaml(),
        );
        let opts = RunOptions {
            transport: &transport,
            cache_root: cache_root.path().to_path_buf(),
            index_url: "u".to_string(),
            refresh: false,
            verbose: false,
        };

        let mut out = Vec::new();
        handle_show(&opts, "brewtrend", &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("(no README)"));
    }

    // ac-09: cold fetch prints ➜ line.
    #[test]
    fn test_ac09_cold_fetch_prints_arrow_line() {
        let fixture_root = TempDir::new().unwrap();
        let cache_root = TempDir::new().unwrap();
        let src = fixture_root.path().join("practical/brewtrend/v1.0");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.txt"), "hi").unwrap();

        let transport = FixtureTransport::with_tree(
            fixture_root.path().to_path_buf(),
            fixture_index_yaml(),
        );
        let opts = RunOptions {
            transport: &transport,
            cache_root: cache_root.path().to_path_buf(),
            index_url: "u".to_string(),
            refresh: false,
            verbose: false,
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        handle_fetch(&opts, "brewtrend", None, false, &mut out, &mut err).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("➜ brewtrend v1.0"), "{s}");
        assert!(s.contains("KB"));
    }

    // ac-09: already-cached prints ✓ line.
    #[test]
    fn test_ac09_already_cached_prints_check_line() {
        let fixture_root = TempDir::new().unwrap();
        let cache_root = TempDir::new().unwrap();
        let src = fixture_root.path().join("practical/brewtrend/v1.0");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.txt"), "hi").unwrap();

        let transport = FixtureTransport::with_tree(
            fixture_root.path().to_path_buf(),
            fixture_index_yaml(),
        );
        let opts = RunOptions {
            transport: &transport,
            cache_root: cache_root.path().to_path_buf(),
            index_url: "u".to_string(),
            refresh: false,
            verbose: false,
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        handle_fetch(&opts, "brewtrend", None, false, &mut out, &mut err).unwrap();
        out.clear();
        handle_fetch(&opts, "brewtrend", None, false, &mut out, &mut err).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("✓ brewtrend v1.0 (cached)"), "{s}");
    }

    // ac-09: failed fetch leaves no <ref>/ directory and a subsequent fetch is cold, not cached.
    #[test]
    fn test_ac09_partial_fetch_does_not_appear_cached() {
        let fixture_root = TempDir::new().unwrap();
        let cache_root = TempDir::new().unwrap();
        let src = fixture_root.path().join("practical/brewtrend/v1.0");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.txt"), "hi").unwrap();

        let bad_transport = FixtureTransport::with_tree(
            fixture_root.path().to_path_buf(),
            fixture_index_yaml(),
        )
        .with_fail_mid_fetch();
        let opts_bad = RunOptions {
            transport: &bad_transport,
            cache_root: cache_root.path().to_path_buf(),
            index_url: "u".to_string(),
            refresh: false,
            verbose: false,
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let _ = handle_fetch(&opts_bad, "brewtrend", None, false, &mut out, &mut err).unwrap_err();
        let dest = cache_root.path().join("brewtrend/v1.0");
        assert!(!dest.exists(), "no <ref> dir should remain after failed fetch");

        // Subsequent fetch with a good transport is cold (➜) not cached (✓).
        let good_transport = FixtureTransport::with_tree(
            fixture_root.path().to_path_buf(),
            fixture_index_yaml(),
        );
        let opts_good = RunOptions {
            transport: &good_transport,
            cache_root: cache_root.path().to_path_buf(),
            index_url: "u".to_string(),
            refresh: false,
            verbose: false,
        };
        out.clear();
        handle_fetch(&opts_good, "brewtrend", None, false, &mut out, &mut err).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("➜ brewtrend v1.0"), "expected cold-fetch arrow: {s}");
    }

    // ac-09 + ac-02: --latest errors with RegistryUnimplemented.
    #[test]
    fn test_ac09_latest_errors_unimplemented() {
        let fixture_root = TempDir::new().unwrap();
        let cache_root = TempDir::new().unwrap();
        let transport = FixtureTransport::with_tree(
            fixture_root.path().to_path_buf(),
            fixture_index_yaml(),
        );
        let opts = RunOptions {
            transport: &transport,
            cache_root: cache_root.path().to_path_buf(),
            index_url: "u".to_string(),
            refresh: false,
            verbose: false,
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let r = handle_fetch(&opts, "brewtrend", None, true, &mut out, &mut err).unwrap_err();
        assert!(matches!(r, Error::RegistryUnimplemented { .. }));
    }

    // ac-10: bad --param surfaces ManifestValidation BEFORE transport invocation.
    #[test]
    fn test_ac10_bad_param_errors_before_transport_use() {
        // A transport that panics on use ensures we assert "no fetch happened".
        struct PanicTransport;
        impl Transport for PanicTransport {
            fn fetch(&self, _: &TransportSrc, _: &Path) -> Result<()> {
                panic!("transport.fetch must not be called when --param is invalid");
            }
            fn fetch_index(&self, _: &str) -> Result<String> {
                panic!("transport.fetch_index must not be called when --param is invalid");
            }
        }
        let cache_root = TempDir::new().unwrap();
        let opts = RunOptions {
            transport: &PanicTransport,
            cache_root: cache_root.path().to_path_buf(),
            index_url: "u".to_string(),
            refresh: false,
            verbose: false,
        };
        let bad_params = vec!["NOEQUALSIGN".to_string()];
        let err = handle_run(&opts, "brewtrend", None, false, false, &bad_params).unwrap_err();
        assert!(matches!(err, Error::ManifestValidation(_)));
    }

    // ac-10: cwd is unchanged after a registry run.
    #[test]
    fn test_ac10_run_does_not_mutate_cwd() {
        let fixture_root = TempDir::new().unwrap();
        let cache_root = TempDir::new().unwrap();
        let src = fixture_root.path().join("practical/brewtrend/v1.0");
        std::fs::create_dir_all(&src).unwrap();
        // Minimal arcform.yaml + a single command step that prints something.
        std::fs::write(
            src.join("arcform.yaml"),
            r#"name: brewtrend
engine: duckdb
steps:
  - id: hello
    command: echo registry-run-ok
"#,
        )
        .unwrap();

        let transport = FixtureTransport::with_tree(
            fixture_root.path().to_path_buf(),
            fixture_index_yaml(),
        );
        let opts = RunOptions {
            transport: &transport,
            cache_root: cache_root.path().to_path_buf(),
            index_url: "u".to_string(),
            refresh: false,
            verbose: false,
        };

        let cwd_before = std::env::current_dir().unwrap();
        // The run may itself fail (state backend etc) — that's fine; we only need to
        // assert the function doesn't `chdir` the process before whatever happens.
        let _ = handle_run(&opts, "brewtrend", None, false, false, &[]);
        let cwd_after = std::env::current_dir().unwrap();
        assert_eq!(cwd_before, cwd_after, "registry run must not mutate cwd");
    }
}
