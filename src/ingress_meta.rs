//! The ingress **freshness contract** — content identity persisted next to a
//! fetched artifact.
//!
//! An `http_fetch` writes a sidecar (`<out>.arcmeta`, YAML) recording the remote
//! identity of what it pulled: the `ETag`/`Last-Modified` the server returned and
//! the `sha256` of the bytes. That sidecar is what makes a fetch *content-aware*
//! rather than clock-based:
//!   - the operator replays the stored `ETag`/`Last-Modified` as an `If-None-Match`
//!     / `If-Modified-Since` conditional request, so an unchanged remote returns
//!     `304` and the bytes are **not** re-downloaded;
//!   - the [`crate::precondition`] `fresh` gate HEAD-probes the same identity, so a
//!     step (and everything downstream) re-runs **only when the remote changed** —
//!     the content-addressed replacement for the mtime `modified_after` gate.
//!
//! Written by `http_fetch`, read by the `fresh` precondition — hence its own module.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Default User-Agent for ingress requests. An unset UA is a `403` at the SEC and
/// several registries — shared by the operator's fetch and the `fresh` probe so the
/// conditional HEAD hits the same endpoint the fetch did.
pub const DEFAULT_UA: &str = "arcform-http_fetch/1 (+https://meridian.online)";

/// Remote identity of a fetched artifact, persisted as `<out>.arcmeta`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FetchMeta {
    /// The URL fetched — the `fresh` probe re-requests exactly this.
    pub url: String,
    /// Request headers the fetch sent (e.g. a UA) — replayed by the probe so a
    /// header-gated source is probed the same way it was fetched.
    #[serde(default)]
    pub request_headers: BTreeMap<String, String>,
    /// The server's `ETag`, if any — the primary conditional/freshness key.
    #[serde(default)]
    pub etag: Option<String>,
    /// The server's `Last-Modified`, if any — the fallback key.
    #[serde(default)]
    pub last_modified: Option<String>,
    /// SHA-256 of the fetched bytes — the content identity (audit + reproducibility).
    #[serde(default)]
    pub sha256: String,
    /// Best-effort fetch time (unix seconds) — audit only.
    #[serde(default)]
    pub fetched_unix: Option<u64>,
}

/// Sidecar path for a fetched artifact: `<out>.arcmeta` (appended, so
/// `build/edgar.parquet` → `build/edgar.parquet.arcmeta`).
pub fn meta_path(out: &Path) -> PathBuf {
    let mut s = out.as_os_str().to_owned();
    s.push(".arcmeta");
    PathBuf::from(s)
}

/// Read the sidecar for `out`, or `None` if it's absent or unparseable.
pub fn read(out: &Path) -> Option<FetchMeta> {
    let s = std::fs::read_to_string(meta_path(out)).ok()?;
    serde_yaml::from_str(&s).ok()
}

/// Write the sidecar for `out`.
pub fn write(out: &Path, meta: &FetchMeta) -> std::io::Result<()> {
    let s = serde_yaml::to_string(meta).map_err(std::io::Error::other)?;
    std::fs::write(meta_path(out), s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_path_appends_not_replaces() {
        // `build/edgar.parquet` → `build/edgar.parquet.arcmeta` (keeps the extension).
        assert_eq!(
            meta_path(Path::new("build/edgar.parquet")),
            PathBuf::from("build/edgar.parquet.arcmeta")
        );
    }

    #[test]
    fn sidecar_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("edgar.parquet");
        let mut headers = BTreeMap::new();
        headers.insert("User-Agent".to_string(), "meridian".to_string());
        let meta = FetchMeta {
            url: "https://openlake.meridian.online/edgar.parquet".to_string(),
            request_headers: headers,
            etag: Some("\"abc123\"".to_string()),
            last_modified: Some("Sat, 04 Jul 2026 06:31:19 GMT".to_string()),
            sha256: "deadbeef".to_string(),
            fetched_unix: Some(1_784_000_000),
        };
        write(&out, &meta).unwrap();
        let back = read(&out).expect("sidecar reads back");
        assert_eq!(back.url, meta.url);
        assert_eq!(back.etag, meta.etag);
        assert_eq!(back.last_modified, meta.last_modified);
        assert_eq!(back.sha256, meta.sha256);
        assert_eq!(
            back.request_headers.get("User-Agent").map(String::as_str),
            Some("meridian")
        );
    }

    #[test]
    fn read_absent_sidecar_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(&dir.path().join("nope.parquet")).is_none());
    }
}
