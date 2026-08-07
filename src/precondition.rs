use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::time::SystemTime;

use owo_colors::OwoColorize;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::ingress_meta::FetchMeta;

/// A typed precondition that determines whether a step is fresh.
///
/// Preconditions are evaluated during staleness computation. If all
/// preconditions for a step pass (return "fresh"), the step may be skipped.
/// The `preconditions` field uses Dagu-compatible naming.
/// Configuration for the `modified_after` precondition type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModifiedAfterConfig {
    /// Relative path to the file whose mtime is checked.
    pub path: String,
    /// Maximum period since last modification (e.g. "24h", "7d", "60m"). Parsed via humantime.
    pub period: String,
}

/// Configuration for the `fresh` precondition — content-aware ingress freshness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreshConfig {
    /// Relative path to a fetched artifact whose `<path>.arcmeta` sidecar records
    /// its remote identity and content hash (written by `http_fetch`). The
    /// precondition checks the file on disk against the recorded `sha256`, then
    /// HEAD-probes the recorded remote — the content-addressed replacement for
    /// `modified_after`'s clock check.
    pub path: String,
}

/// YAML format:
/// ```yaml
/// preconditions:
///   - modified_after:
///       path: data/cask.json
///       period: 24h
///   - command: "test $SKIP_FETCH"
/// ```
///
/// Uses `#[serde(untagged)]` — each variant is identified by its unique key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Precondition {
    /// Check that a file was modified after a given period ago.
    /// Path is relative to the manifest directory.
    /// If the file is missing or inaccessible, evaluates as stale (not an error).
    ModifiedAfter { modified_after: ModifiedAfterConfig },

    /// Content-aware ingress freshness, in two hops against a fetched artifact's
    /// `<path>.arcmeta` sidecar. First the bytes on disk are hashed and compared
    /// with the `sha256` the fetch recorded; a mismatch is stale and the remote is
    /// not contacted. Then the recorded remote is HEAD-probed and its `ETag` /
    /// `Last-Modified` compared: unchanged remote = fresh (skip the fetch and
    /// everything downstream), changed or unprobeable = stale (re-fetch). Replaces
    /// the clock-based `modified_after` for `http_fetch` outputs.
    Fresh { fresh: FreshConfig },

    /// Run a shell command. Exit 0 = fresh (skip), non-zero = stale (run).
    /// Execution errors (binary not found, crash) halt the pipeline.
    Command { command: String },
}

impl Precondition {
    /// Evaluate this precondition. Returns Ok(true) if the step is fresh
    /// (should be skipped), Ok(false) if stale (should run).
    ///
    /// For `ModifiedWithin`: file issues return Ok(false) — stale, not error.
    /// For `Command`: execution errors return Err — pipeline halts.
    pub(crate) fn evaluate(
        &self,
        manifest_dir: &Path,
        step_name: &str,
        env: &HashMap<String, String>,
    ) -> Result<bool> {
        match self {
            Precondition::ModifiedAfter { modified_after } => {
                let period_duration =
                    humantime::parse_duration(&modified_after.period).map_err(|e| {
                        Error::ManifestValidation(format!(
                            "invalid duration '{}': {}",
                            modified_after.period, e
                        ))
                    })?;

                let file_path = manifest_dir.join(&modified_after.path);

                // Any failure to stat or get mtime → stale (not an error).
                let fresh = (|| -> Option<bool> {
                    let metadata = std::fs::metadata(&file_path).ok()?;
                    let modified = metadata.modified().ok()?;
                    let elapsed = SystemTime::now().duration_since(modified).ok()?;
                    Some(elapsed < period_duration)
                })();

                Ok(fresh.unwrap_or(false))
            }
            Precondition::Fresh { fresh } => {
                let file_path = manifest_dir.join(&fresh.path);
                // No artifact or no sidecar → stale: the fetch must run. (A probe
                // failure below is likewise stale — safe, and cheap: the operator's
                // own conditional GET then 304s rather than re-downloading.)
                if !file_path.exists() {
                    return Ok(false);
                }
                let Some(meta) = crate::ingress_meta::read(&file_path) else {
                    return Ok(false);
                };

                // The local copy is checked against the hash the fetch recorded before
                // the remote is asked anything. A file that is not the bytes its sidecar
                // describes is stale whatever the remote answers, and refusing here costs
                // no round trip.
                if let Some(refusal) = local_copy_refusal(&file_path, &meta) {
                    eprintln!("{} {}", "warning:".yellow(), refusal);
                    return Ok(false);
                }

                // The remote HEAD probe is the ureq path; it rides with the
                // `http_fetch` operator behind the `http-fetch` feature (see
                // src/operator.rs for the reachability note). Without that feature
                // there is no HTTP client to probe with, so fall back to "stale" —
                // identical to how an unreachable probe is treated below, and safe:
                // the fetch, if it then runs, 304s rather than re-downloading. This
                // fallback is dead in practice — preconditions are evaluated only by
                // the runner, which is CLI-only, and `cli` enables `http-fetch`.
                #[cfg(feature = "http-fetch")]
                {
                    let mut req =
                        ureq::head(&meta.url).set("User-Agent", crate::ingress_meta::DEFAULT_UA);
                    for (k, v) in &meta.request_headers {
                        req = req.set(k, v);
                    }
                    if let Some(ref etag) = meta.etag {
                        req = req.set("If-None-Match", etag);
                    }
                    if let Some(ref lm) = meta.last_modified {
                        req = req.set("If-Modified-Since", lm);
                    }

                    let fresh = match req.call() {
                        // Server confirms unchanged.
                        Ok(resp) if resp.status() == 304 => true,
                        Ok(resp) => {
                            // No 304 (e.g. server ignores conditionals) — compare identity
                            // ourselves: ETag first, then Last-Modified.
                            let etag_match = matches!(
                                (&meta.etag, resp.header("ETag")),
                                (Some(a), Some(b)) if a == b
                            );
                            let lm_match = matches!(
                                (&meta.last_modified, resp.header("Last-Modified")),
                                (Some(a), Some(b)) if a == b
                            );
                            etag_match || lm_match
                        }
                        Err(ureq::Error::Status(304, _)) => true,
                        // Unreachable/errored probe → stale.
                        Err(_) => false,
                    };
                    Ok(fresh)
                }
                #[cfg(not(feature = "http-fetch"))]
                {
                    Ok(false)
                }
            }
            Precondition::Command { command: cmd } => {
                let output = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(cmd)
                    .current_dir(manifest_dir)
                    .envs(env)
                    .output();

                match output {
                    Ok(o) => Ok(o.status.success()),
                    Err(e) => Err(Error::Precondition {
                        step: step_name.to_string(),
                        command: cmd.clone(),
                        detail: e.to_string(),
                    }),
                }
            }
        }
    }

    /// Validate this precondition at manifest load time (catch errors early).
    pub fn validate(&self) -> Result<()> {
        match self {
            Precondition::ModifiedAfter { modified_after } => {
                if modified_after.path.is_empty() {
                    return Err(Error::ManifestValidation(
                        "modified_after precondition: 'path' cannot be empty".to_string(),
                    ));
                }
                if modified_after.period.is_empty() {
                    return Err(Error::ManifestValidation(
                        "modified_after precondition: 'period' cannot be empty".to_string(),
                    ));
                }
                humantime::parse_duration(&modified_after.period).map_err(|e| {
                    Error::ManifestValidation(format!(
                        "modified_after precondition: invalid duration '{}': {}",
                        modified_after.period, e
                    ))
                })?;
                Ok(())
            }
            Precondition::Fresh { fresh } => {
                if fresh.path.is_empty() {
                    return Err(Error::ManifestValidation(
                        "fresh precondition: 'path' cannot be empty".to_string(),
                    ));
                }
                Ok(())
            }
            Precondition::Command { command: cmd } => {
                if cmd.is_empty() {
                    return Err(Error::ManifestValidation(
                        "command precondition: command cannot be empty".to_string(),
                    ));
                }
                Ok(())
            }
        }
    }
}

/// Check a fetched artifact's bytes against the `sha256` its `<path>.arcmeta` sidecar
/// recorded at fetch time.
///
/// `None` means the file on disk hashes to the value the fetch wrote. `Some` is the
/// refusal text, which names the file and attributes the failure to the local copy —
/// the remote has not been contacted when this runs.
///
/// A sidecar carrying no `sha256` is refused rather than waved through: the field is
/// `#[serde(default)]`, so an empty value is what a hand-edited or pre-field sidecar
/// deserialises to, and an unverifiable artifact is not a verified one. The price is one
/// conditional GET, which the operator answers with a `304`, and the re-fetch writes a
/// sidecar that does carry the hash.
fn local_copy_refusal(file_path: &Path, meta: &FetchMeta) -> Option<String> {
    let recorded = meta.sha256.trim();
    if recorded.is_empty() {
        return Some(format!(
            "fresh: local copy {} cannot be verified: its .arcmeta records no sha256 \
             — re-fetching; the remote was not consulted",
            file_path.display()
        ));
    }
    let on_disk = match hash_file(file_path) {
        Ok(hex) => hex,
        Err(e) => {
            return Some(format!(
                "fresh: local copy {} could not be read to verify it ({e}) \
                 — re-fetching; the remote was not consulted",
                file_path.display()
            ));
        }
    };
    if on_disk.eq_ignore_ascii_case(recorded) {
        return None;
    }
    Some(format!(
        "fresh: local copy {} does not match the sha256 its fetch recorded \
         (recorded {recorded}, on disk {on_disk}) — re-fetching; the remote was not consulted",
        file_path.display()
    ))
}

/// SHA-256 hex digest of a file's bytes, read in fixed-size chunks so peak memory does
/// not scale with the artifact. Same digest and same hex representation as
/// [`crate::state::content_hash`] over the whole byte slice — pinned by
/// `test_fresh_hash_file_matches_content_hash`.
fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Evaluate all preconditions for a step. Returns Ok(true) if ALL pass (step is fresh).
/// Returns Ok(false) if any precondition says stale.
/// Returns Err if a command precondition fails to execute.
pub(crate) fn evaluate_all(
    preconditions: &[Precondition],
    manifest_dir: &Path,
    step_name: &str,
    env: &HashMap<String, String>,
) -> Result<bool> {
    for p in preconditions {
        if !p.evaluate(manifest_dir, step_name, env)? {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime};

    // Precondition enum exists with both variants.
    #[test]
    fn test_pre_enum_variants() {
        let ma = Precondition::ModifiedAfter {
            modified_after: ModifiedAfterConfig {
                path: "data/file.json".to_string(),
                period: "24h".to_string(),
            },
        };
        let cmd = Precondition::Command {
            command: "true".to_string(),
        };
        // Both variants compile and construct.
        assert!(matches!(ma, Precondition::ModifiedAfter { .. }));
        assert!(matches!(cmd, Precondition::Command { .. }));
    }

    // Serde round-trip for both variants.
    #[test]
    fn test_pre_serde_roundtrip() {
        let preconditions = vec![
            Precondition::ModifiedAfter {
                modified_after: ModifiedAfterConfig {
                    path: "data/cask.json".to_string(),
                    period: "24h".to_string(),
                },
            },
            Precondition::Command {
                command: "test $SKIP".to_string(),
            },
        ];
        let yaml = serde_yaml::to_string(&preconditions).unwrap();
        let parsed: Vec<Precondition> = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.len(), 2);
        assert!(matches!(parsed[0], Precondition::ModifiedAfter { .. }));
        assert!(matches!(parsed[1], Precondition::Command { .. }));
    }

    fn ma(path: &str, period: &str) -> Precondition {
        Precondition::ModifiedAfter {
            modified_after: ModifiedAfterConfig {
                path: path.to_string(),
                period: period.to_string(),
            },
        }
    }

    fn cmd(command: &str) -> Precondition {
        Precondition::Command {
            command: command.to_string(),
        }
    }

    fn empty_env() -> HashMap<String, String> {
        HashMap::new()
    }

    // modified_after evaluates file mtime — fresh file.
    #[test]
    fn test_pre_modified_after_fresh() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("data.json"), "{}").unwrap();
        assert!(
            ma("data.json", "1h")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
    }

    // modified_after evaluates file mtime — stale file.
    #[test]
    fn test_pre_modified_after_stale() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("data.json");
        fs::write(&file_path, "{}").unwrap();

        // Set mtime to 48 hours ago.
        let old_time = SystemTime::now() - Duration::from_secs(48 * 3600);
        let ft = filetime::FileTime::from_system_time(old_time);
        filetime::set_file_mtime(&file_path, ft).unwrap();

        assert!(
            !ma("data.json", "24h")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
    }

    // modified_after with missing file = stale (not error).
    #[test]
    fn test_pre_missing_file_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            !ma("nonexistent.json", "24h")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
    }

    // command precondition — exit 0 = fresh.
    #[test]
    fn test_pre_command_exit_zero_fresh() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            cmd("true")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
    }

    // command precondition — non-zero = stale.
    #[test]
    fn test_pre_command_nonzero_stale() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            !cmd("false")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
    }

    // Command precondition receives resolved params as env vars.
    #[test]
    fn test_command_precondition_receives_env() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = HashMap::new();
        env.insert("ARC_PARAM_SEARCH_TERM".to_string(), "hello".to_string());

        // Command checks if the env var is set — should return fresh (exit 0).
        assert!(
            cmd("test -n \"$ARC_PARAM_SEARCH_TERM\"")
                .evaluate(dir.path(), "test-step", &env)
                .unwrap()
        );

        // Without the env var, the same command returns stale (exit non-zero).
        assert!(
            !cmd("test -n \"$ARC_PARAM_SEARCH_TERM\"")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
    }

    // AND semantics — all must pass.
    #[test]
    fn test_pre_and_semantics_all_fresh() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.json"), "{}").unwrap();
        fs::write(dir.path().join("b.json"), "{}").unwrap();

        let preconditions = vec![ma("a.json", "1h"), ma("b.json", "1h")];
        assert!(evaluate_all(&preconditions, dir.path(), "test-step", &empty_env()).unwrap());
    }

    // AND semantics — one stale makes all stale.
    #[test]
    fn test_pre_and_semantics_one_stale() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("fresh.json"), "{}").unwrap();

        let preconditions = vec![ma("fresh.json", "1h"), ma("stale.json", "1h")];
        assert!(!evaluate_all(&preconditions, dir.path(), "test-step", &empty_env()).unwrap());
    }

    // command non-zero exit = stale (not error).
    #[test]
    fn test_pre_command_nonzero_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            !cmd("exit 1")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
    }

    // duration parsing supports various formats.
    #[test]
    fn test_pre_duration_parsing() {
        for duration_str in &["24h", "60m", "7d", "1h30m", "30s", "2d12h"] {
            assert!(
                humantime::parse_duration(duration_str).is_ok(),
                "Failed: {}",
                duration_str
            );
        }
    }

    // validation rejects empty path.
    #[test]
    fn test_pre_validate_empty_path() {
        assert!(ma("", "24h").validate().is_err());
    }

    // validation rejects empty period.
    #[test]
    fn test_pre_validate_empty_period() {
        assert!(ma("file.json", "").validate().is_err());
    }

    // validation rejects invalid duration.
    #[test]
    fn test_pre_validate_invalid_duration() {
        assert!(ma("file.json", "banana").validate().is_err());
    }

    // validation rejects empty command.
    #[test]
    fn test_pre_validate_empty_command() {
        assert!(cmd("").validate().is_err());
    }

    // validation passes for valid preconditions.
    #[test]
    fn test_pre_validate_valid() {
        assert!(ma("data.json", "24h").validate().is_ok());
        assert!(cmd("test -f output.csv").validate().is_ok());
    }

    fn fresh(path: &str) -> Precondition {
        Precondition::Fresh {
            fresh: FreshConfig {
                path: path.to_string(),
            },
        }
    }

    // fresh: no artifact on disk → stale (must fetch), never a network probe.
    #[test]
    fn test_fresh_missing_artifact_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            !fresh("build/edgar.parquet")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
    }

    // fresh: artifact present but no `.arcmeta` sidecar → stale (nothing to probe).
    #[test]
    fn test_fresh_missing_sidecar_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("data.parquet"), b"bytes").unwrap();
        assert!(
            !fresh("data.parquet")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
    }

    // A 127.0.0.1 listener that answers every request `304 Not Modified` and counts the
    // connections it accepted. The `fresh` probe reads a 304 as fresh, so a gate that
    // reaches this server declares the artifact fresh; the counter is how a test asserts
    // the gate did not reach it.
    #[cfg(feature = "http-fetch")]
    fn probe_server_304() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n");
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}/artifact"), hits)
    }

    #[cfg(feature = "http-fetch")]
    fn probe_hits(hits: &std::sync::atomic::AtomicUsize) -> usize {
        hits.load(std::sync::atomic::Ordering::SeqCst)
    }

    // Write `fetched` to `<dir>/data.parquet` and the sidecar `http_fetch` would have
    // written for it, then leave `on_disk` in the file's place.
    fn fetched_then_replaced(dir: &Path, url: &str, fetched: &[u8], on_disk: &[u8]) {
        let out = dir.join("data.parquet");
        fs::write(&out, fetched).unwrap();
        crate::ingress_meta::write(
            &out,
            &crate::ingress_meta::FetchMeta {
                url: url.to_string(),
                etag: Some("\"v1\"".to_string()),
                sha256: crate::state::content_hash(fetched),
                ..Default::default()
            },
        )
        .unwrap();
        fs::write(&out, on_disk).unwrap();
    }

    // fresh: the artifact the fetch left in place is fresh, and the verdict comes from
    // the remote probe — the control for the two corruption tests below, which point at
    // the same 304 server and must reach a different verdict.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn test_fresh_intact_artifact_is_fresh_via_the_probe() {
        let dir = tempfile::tempdir().unwrap();
        let (url, hits) = probe_server_304();
        let bytes = b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT394\n";
        fetched_then_replaced(dir.path(), &url, bytes, bytes);

        assert!(
            fresh("data.parquet")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
        assert_eq!(probe_hits(&hits), 1, "the remote was probed");
    }

    // fresh: one byte flipped, length unchanged — stale, and the remote is never asked.
    // A size check would not see this mutation; the sidecar's sha256 does.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn test_fresh_corrupted_artifact_is_stale_without_probing() {
        let dir = tempfile::tempdir().unwrap();
        let (url, hits) = probe_server_304();
        fetched_then_replaced(
            dir.path(),
            &url,
            b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT394\n",
            b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT395\n",
        );

        assert!(
            !fresh("data.parquet")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap(),
            "a corrupted artifact must not pass the gate"
        );
        assert_eq!(probe_hits(&hits), 0, "the remote must not be consulted");
    }

    // fresh: the artifact truncated mid-record — stale, and the remote is never asked.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn test_fresh_truncated_artifact_is_stale_without_probing() {
        let dir = tempfile::tempdir().unwrap();
        let (url, hits) = probe_server_304();
        fetched_then_replaced(
            dir.path(),
            &url,
            b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT394\n",
            b"cik,lei\n0000320193,HWU",
        );

        assert!(
            !fresh("data.parquet")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap(),
            "a truncated artifact must not pass the gate"
        );
        assert_eq!(probe_hits(&hits), 0, "the remote must not be consulted");
    }

    // fresh: a sidecar with no recorded sha256 leaves the artifact unverifiable → stale.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn test_fresh_sidecar_without_sha256_is_stale_without_probing() {
        let dir = tempfile::tempdir().unwrap();
        let (url, hits) = probe_server_304();
        let out = dir.path().join("data.parquet");
        fs::write(&out, b"bytes").unwrap();
        crate::ingress_meta::write(
            &out,
            &crate::ingress_meta::FetchMeta {
                url,
                etag: Some("\"v1\"".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        assert!(
            !fresh("data.parquet")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
        assert_eq!(probe_hits(&hits), 0, "the remote must not be consulted");
    }

    // The refusal reaches stderr. `evaluate` prints it with `eprintln!`, which libtest
    // swallows in-process, so the test re-executes its own binary with `--nocapture` and
    // reads the child's stderr: the assertions below are on bytes the operator would see.
    // The child arm is this same test, told by the env var to build the fixture and
    // evaluate rather than to spawn.
    #[test]
    fn test_fresh_refusal_is_printed_to_stderr() {
        const CHILD: &str = "ARC_TEST_FRESH_REFUSAL_CHILD";

        if std::env::var_os(CHILD).is_some() {
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("edgar.parquet");
            fs::write(&out, b"cik,lei\n0000320193,HWU").unwrap();
            crate::ingress_meta::write(
                &out,
                &crate::ingress_meta::FetchMeta {
                    // Port 1 on the loopback: were the probe ever reached, it would
                    // refuse the connection rather than answer.
                    url: "http://127.0.0.1:1/never-probed".to_string(),
                    sha256: crate::state::content_hash(
                        b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT394\n",
                    ),
                    ..Default::default()
                },
            )
            .unwrap();
            assert!(
                !fresh("edgar.parquet")
                    .evaluate(dir.path(), "test-step", &empty_env())
                    .unwrap()
            );
            return;
        }

        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "precondition::tests::test_fresh_refusal_is_printed_to_stderr",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .expect("re-execute the test binary");
        assert!(output.status.success(), "child run: {:?}", output.status);

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("warning:"), "warns: {stderr}");
        assert!(stderr.contains("edgar.parquet"), "names the file: {stderr}");
        assert!(
            stderr.contains("local copy"),
            "names the local copy: {stderr}"
        );
        assert!(
            stderr.contains("the remote was not consulted"),
            "clears the remote: {stderr}"
        );
    }

    // The refusal names the file that failed and attributes the failure to the local
    // copy. This string is what `evaluate` prints to stderr before returning stale.
    #[test]
    fn test_fresh_refusal_names_the_file_and_the_local_copy() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("edgar.parquet");
        fs::write(&out, b"cik,lei\n0000320193,HWU").unwrap();
        let meta = crate::ingress_meta::FetchMeta {
            sha256: crate::state::content_hash(b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT394\n"),
            ..Default::default()
        };

        let msg = local_copy_refusal(&out, &meta).expect("a mismatch is refused");
        assert!(msg.contains("edgar.parquet"), "names the file: {msg}");
        assert!(msg.contains("local copy"), "names the local copy: {msg}");
        assert!(
            msg.contains("the remote was not consulted"),
            "clears the remote: {msg}"
        );
        assert!(!msg.contains('\n'), "single line: {msg}");
    }

    // The refusal for a sidecar with no hash carries the same attribution.
    #[test]
    fn test_fresh_refusal_for_missing_hash_names_the_local_copy() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("edgar.parquet");
        fs::write(&out, b"bytes").unwrap();

        let msg = local_copy_refusal(&out, &crate::ingress_meta::FetchMeta::default())
            .expect("an unverifiable artifact is refused");
        assert!(msg.contains("edgar.parquet"), "names the file: {msg}");
        assert!(msg.contains("local copy"), "names the local copy: {msg}");
        assert!(
            msg.contains("the remote was not consulted"),
            "clears the remote: {msg}"
        );
    }

    // The chunked file hash and the whole-slice hash agree, so the gate compares like
    // with like against a sidecar written by the fetch.
    #[test]
    fn test_fresh_hash_file_matches_content_hash() {
        let dir = tempfile::tempdir().unwrap();
        // Longer than the 64 KiB read buffer, so more than one chunk is fed to the hasher.
        let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let out = dir.path().join("big.bin");
        fs::write(&out, &bytes).unwrap();
        assert_eq!(
            hash_file(&out).unwrap(),
            crate::state::content_hash(&bytes),
            "chunked and whole-slice digests agree"
        );
    }

    // fresh: validation + serde round-trip.
    #[test]
    fn test_fresh_validate_and_roundtrip() {
        assert!(fresh("build/x.parquet").validate().is_ok());
        assert!(fresh("").validate().is_err());
        let yaml = serde_yaml::to_string(&vec![fresh("build/x.parquet")]).unwrap();
        let parsed: Vec<Precondition> = serde_yaml::from_str(&yaml).unwrap();
        assert!(matches!(parsed[0], Precondition::Fresh { .. }));
    }
}
