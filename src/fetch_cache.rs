//! The **shared fetch cache** — the same remote file is transferred once, however
//! many Protocols want it.
//!
//! An `http_fetch` writes its artifact inside the Protocol that asked for it, so
//! before this module a second Protocol naming the same URL downloaded the same
//! bytes again. The cache sits above that scope: it is a user-level store keyed by
//! **content hash** and located by **URL**, so the URL says where to look and the
//! hash says what was got.
//!
//! ```text
//! <root>/objects/<sha256 of the bytes>          the bytes
//! <root>/urls/<sha256 of the url>.arcmeta       the locator: which bytes that URL got
//! ```
//!
//! The locator is a [`FetchMeta`] — the same record `http_fetch` leaves beside the
//! artifact as `<out>.arcmeta`. One record type rather than two is deliberate: what
//! the cache holds for a URL and what a Protocol holds for its copy have to agree
//! field for field, because a run served from the cache must end with the sidecar an
//! uncached run would have written.
//!
//! # What the cache is for, and what it must not do
//!
//! Speed, and only speed: a second run — or a second Protocol — should not
//! re-download 127 MB. It is not a reproducibility device and not an offline mode.
//! The constraint that binds is that **a cached run and an uncached run produce the
//! same result**, so:
//!
//! * a hit is still revalidated against the origin (one conditional request, no
//!   payload) unless the manifest pinned the hash it wants, which names the bytes and
//!   leaves the origin nothing to add;
//! * an entry is verified on read — [`FetchCache::lookup`] hashes the object and
//!   compares it with the key before returning it — so a corrupt entry is evicted
//!   rather than served to a second consumer. That costs one local read of a file we
//!   were about to copy anyway, against a transfer it saves;
//! * a **credentialed** fetch is never shared. Bytes behind an `Authorization` or
//!   `Cookie` header belong to the credential, not to the URL, and a store keyed by
//!   URL cannot tell two credentials apart.
//!
//! Every write here is best-effort: a cache that cannot be written is a cache that
//! saves nothing, never a fetch that fails.
//!
//! # Where it lives
//!
//! `$ARCFORM_FETCH_CACHE` when set — `off` (any case) disables the cache, anything
//! else is the root directory to use — else `~/.arcform/fetch-cache`, beside the
//! local-history store and the registry cache. Nothing is written inside the Protocol
//! directory, so a cache hit changes nothing `git status` can see.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::ingress_meta::FetchMeta;

/// Overrides the cache root; `off` disables the cache. Without it the root is
/// `~/.arcform/fetch-cache`.
const CACHE_ENV: &str = "ARCFORM_FETCH_CACHE";

/// The `$ARCFORM_FETCH_CACHE` value that turns the cache off. Compared
/// case-insensitively.
const OFF: &str = "off";

/// Under the home directory, when the environment says nothing.
const DEFAULT_SUBDIR: &str = "fetch-cache";

/// A content-addressed store of fetched artifacts, shared by every Protocol run by
/// this user.
pub struct FetchCache {
    root: PathBuf,
}

impl FetchCache {
    /// The cache the environment asks for, or `None` when it is switched off or no
    /// home directory can be resolved to put it in.
    ///
    /// `None` is not an error: without a cache every fetch behaves exactly as it did
    /// before this module existed, which is the comparison
    /// `a_cached_run_and_an_uncached_run_agree` (in `src/operator.rs`) makes.
    pub fn from_env() -> Option<Self> {
        Self::resolve_root(std::env::var_os(CACHE_ENV), dirs::home_dir()).map(Self::at)
    }

    /// A cache rooted at `root`. The directory is created lazily, on the first store.
    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }

    /// `$ARCFORM_FETCH_CACHE` first — `off` is `None`, any other non-empty value is
    /// the root — then `~/.arcform/fetch-cache`.
    fn resolve_root(env: Option<std::ffi::OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
        if let Some(dir) = env
            && !dir.is_empty()
        {
            if dir.to_string_lossy().eq_ignore_ascii_case(OFF) {
                return None;
            }
            return Some(PathBuf::from(dir));
        }
        home.map(|home| home.join(".arcform").join(DEFAULT_SUBDIR))
    }

    /// Where the bytes with digest `sha256` live.
    fn object_path(&self, sha256: &str) -> PathBuf {
        self.root.join("objects").join(sha256)
    }

    /// Where the locator for `url` lives. The file name is the digest of the URL, so
    /// any URL — however long, however punctuated — is a valid, fixed-width path that
    /// cannot escape the root. The URL itself is inside the record.
    fn locator_path(&self, url: &str) -> PathBuf {
        self.root.join("urls").join(format!(
            "{}.arcmeta",
            crate::state::content_hash(url.as_bytes())
        ))
    }

    /// Whether the cache already holds a locator for `url`, without verifying it.
    ///
    /// This is the cheap question — "is there anything to seed?" — asked by a fetch
    /// that is keeping its own artifact and only wants to know whether the shared
    /// store has heard of the URL.
    pub fn holds(&self, url: &str) -> bool {
        self.locator_path(url).exists()
    }

    /// What the cache holds for `url`, **verified**: the object named by the locator
    /// exists and hashes to the key it is filed under.
    ///
    /// A locator with no object, an object that fails its hash, and an unreadable
    /// record are all `None`, and all evict — an entry that cannot be trusted is not
    /// an entry, and leaving it in place would make every later run pay the same
    /// verification for the same answer.
    pub fn lookup(&self, url: &str) -> Option<FetchMeta> {
        let record = std::fs::read_to_string(self.locator_path(url)).ok()?;
        let meta: FetchMeta = serde_yaml::from_str(&record).ok()?;
        if meta.sha256.trim().is_empty() || !self.object_is_intact(&meta.sha256) {
            self.evict(url, Some(&meta.sha256));
            return None;
        }
        Some(meta)
    }

    /// The object with digest `sha256`, verified, or `None`. The lookup a **pinned**
    /// fetch makes: the manifest named the bytes, so the URL is only where they would
    /// have come from.
    pub fn pinned_object(&self, sha256: &str) -> Option<PathBuf> {
        self.object_is_intact(sha256)
            .then(|| self.object_path(sha256))
    }

    /// Copy a verified object into `out`, atomically: the bytes land in a sibling
    /// temp file and are renamed over `out`, so an interrupted materialisation never
    /// leaves a Protocol holding half an artifact.
    ///
    /// The copy re-hashes as it reads, which closes the window between
    /// [`lookup`](Self::lookup) verifying the object and this reading it. A mismatch
    /// evicts the entry and is an error: the caller has skipped a download on the
    /// strength of the entry, so there is nothing to fall back on but a re-run, and
    /// the engine's step-retry is what re-runs it — against a cache that no longer
    /// holds the bad object.
    pub fn materialise(&self, meta: &FetchMeta, out: &Path) -> std::io::Result<()> {
        let object = self.object_path(meta.sha256.trim());
        let tmp = out.with_extension("part");
        let copied = copy_hashing(&object, &tmp)?;
        if !copied.eq_ignore_ascii_case(meta.sha256.trim()) {
            let _ = std::fs::remove_file(&tmp);
            self.evict(&meta.url, Some(&meta.sha256));
            return Err(std::io::Error::other(format!(
                "cached copy of {} did not survive the read (filed under {}, read {copied}) \
                 — the entry is discarded",
                meta.url, meta.sha256
            )));
        }
        std::fs::rename(&tmp, out)
    }

    /// File `source`'s bytes under `meta.sha256` and point `meta.url` at them.
    ///
    /// The copy hashes as it reads and **refuses bytes that do not match the key**,
    /// which costs nothing over a copy that does not: the store has to read the file
    /// either way. Nothing enters this cache unverified and nothing leaves it
    /// unverified, so a caller with a corrupt artifact cannot make it a second
    /// consumer's problem.
    ///
    /// The object is written once — a digest already on disk is the same bytes by
    /// construction — and the locator is rewritten every time, because the validators
    /// a URL answers with move even when its content does not.
    ///
    /// Errors are returned for a test to assert on; every caller in the fetch path
    /// discards them. A cache that will not take a write costs a future transfer, not
    /// this one.
    pub fn store(&self, meta: &FetchMeta, source: &Path) -> std::io::Result<()> {
        let sha = meta.sha256.trim();
        if sha.is_empty() {
            return Err(std::io::Error::other(
                "refusing to file an artifact whose content hash is unknown",
            ));
        }
        let object = self.object_path(sha);
        if !object.exists() {
            if let Some(parent) = object.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp = object.with_extension(format!("part-{}", std::process::id()));
            let copied = copy_hashing(source, &tmp)?;
            if !copied.eq_ignore_ascii_case(sha) {
                let _ = std::fs::remove_file(&tmp);
                return Err(std::io::Error::other(format!(
                    "refusing to file {} under {sha}: its bytes hash to {copied}",
                    source.display()
                )));
            }
            std::fs::rename(&tmp, &object)?;
        }
        let locator = self.locator_path(&meta.url);
        if let Some(parent) = locator.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let record = serde_yaml::to_string(meta).map_err(std::io::Error::other)?;
        let tmp = locator.with_extension(format!("part-{}", std::process::id()));
        std::fs::write(&tmp, record)?;
        std::fs::rename(&tmp, &locator)
    }

    /// Drop a URL's locator and, when one is named, the object it points at.
    ///
    /// The object goes even though another URL's locator could name the same bytes.
    /// That costs the other URL one transfer and keeps the rule simple: an object
    /// this cache has found to be untrustworthy is not left on disk for a second
    /// consumer to find. The other locator's next lookup verifies, finds nothing, and
    /// re-fetches — which is the same path any cold cache takes.
    fn evict(&self, url: &str, sha256: Option<&str>) {
        let _ = std::fs::remove_file(self.locator_path(url));
        if let Some(sha) = sha256.map(str::trim).filter(|s| !s.is_empty()) {
            let _ = std::fs::remove_file(self.object_path(sha));
        }
    }

    /// Whether the object filed under `sha256` is present and hashes to it.
    fn object_is_intact(&self, sha256: &str) -> bool {
        let sha = sha256.trim();
        !sha.is_empty()
            && hash_file(&self.object_path(sha))
                .is_ok_and(|on_disk| on_disk.eq_ignore_ascii_case(sha))
    }
}

/// Copy `from` to `to`, returning the SHA-256 hex digest of what was copied. One
/// read serves both, so verifying a copy costs nothing a copy did not already cost.
fn copy_hashing(from: &Path, to: &Path) -> std::io::Result<String> {
    let mut reader = std::fs::File::open(from)?;
    let mut file = std::fs::File::create(to)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
    }
    let _ = file.sync_all();
    Ok(format!("{:x}", hasher.finalize()))
}

/// SHA-256 hex digest of a file's bytes, read in fixed-size chunks so peak memory does
/// not scale with the artifact. Same digest and same hex representation as
/// [`crate::state::content_hash`] over the whole byte slice — pinned by
/// `test_fresh_hash_file_matches_content_hash`.
pub fn hash_file(path: &Path) -> std::io::Result<String> {
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

/// A 64-character hex digest, lowercased, or `None` for any other shape — the gate on
/// a manifest's pinned `sha256`, so a typo is a manifest error rather than a pin that
/// can never match.
pub fn parse_digest(value: &str) -> Option<String> {
    let v = value.trim();
    (v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit())).then(|| v.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(url: &str, bytes: &[u8]) -> FetchMeta {
        FetchMeta {
            url: url.to_string(),
            etag: Some("\"v1\"".to_string()),
            sha256: crate::state::content_hash(bytes),
            ..Default::default()
        }
    }

    // Store a fetched artifact and read it back, then materialise it somewhere else —
    // the two Protocols of the card, minus the network.
    #[test]
    fn a_stored_artifact_is_found_by_url_and_materialises_byte_identically() {
        let home = tempfile::tempdir().unwrap();
        let cache = FetchCache::at(home.path().join("cache"));
        let first = tempfile::tempdir().unwrap();
        let bytes = b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT394\n";
        let source = first.path().join("edgar.parquet");
        std::fs::write(&source, bytes).unwrap();
        let m = meta("https://example.invalid/edgar.parquet", bytes);

        cache.store(&m, &source).unwrap();
        assert!(cache.holds(&m.url));

        let found = cache.lookup(&m.url).expect("the URL locates the entry");
        assert_eq!(found.sha256, m.sha256, "the entry is keyed by content hash");
        assert_eq!(
            found.etag, m.etag,
            "and carries the validators to revalidate"
        );

        let second = tempfile::tempdir().unwrap();
        let out = second.path().join("reference.parquet");
        cache.materialise(&found, &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), bytes);
        assert!(
            !out.with_extension("part").exists(),
            "the temp file is renamed, not left behind"
        );
    }

    // A URL the cache has never seen is a miss, and a miss is silent.
    #[test]
    fn an_unknown_url_is_a_miss() {
        let home = tempfile::tempdir().unwrap();
        let cache = FetchCache::at(home.path().join("cache"));
        assert!(!cache.holds("https://example.invalid/never-fetched"));
        assert!(
            cache
                .lookup("https://example.invalid/never-fetched")
                .is_none()
        );
    }

    // AC4, at the store's own boundary: an object whose bytes no longer hash to the
    // key they are filed under is not returned to the next consumer, and does not
    // survive to be offered again.
    #[test]
    fn a_corrupt_object_is_refused_and_evicted() {
        let home = tempfile::tempdir().unwrap();
        let cache = FetchCache::at(home.path().join("cache"));
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT394\n";
        let source = dir.path().join("edgar.parquet");
        std::fs::write(&source, bytes).unwrap();
        let m = meta("https://example.invalid/edgar.parquet", bytes);
        cache.store(&m, &source).unwrap();

        // One byte flipped, length unchanged — the mutation a size check would miss.
        let object = cache.object_path(&m.sha256);
        std::fs::write(&object, b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT395\n").unwrap();

        assert!(
            cache.lookup(&m.url).is_none(),
            "a corrupt entry must not be served"
        );
        assert!(
            !object.exists(),
            "and must not be left for the next consumer"
        );
        assert!(!cache.holds(&m.url), "its locator goes with it");
    }

    // The pinned path looks the object up by hash, and gets nothing when the hash is
    // not what the store holds.
    #[test]
    fn a_pinned_digest_finds_only_the_object_it_names() {
        let home = tempfile::tempdir().unwrap();
        let cache = FetchCache::at(home.path().join("cache"));
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT394\n";
        let source = dir.path().join("edgar.parquet");
        std::fs::write(&source, bytes).unwrap();
        let m = meta("https://example.invalid/edgar.parquet", bytes);
        cache.store(&m, &source).unwrap();

        assert_eq!(
            cache.pinned_object(&m.sha256),
            Some(cache.object_path(&m.sha256))
        );
        assert!(
            cache
                .pinned_object(&crate::state::content_hash(b"other bytes"))
                .is_none()
        );
    }

    // An artifact whose hash the caller does not know cannot be filed, because the
    // key IS the hash — there is nothing to file it under.
    #[test]
    fn an_unhashed_artifact_is_not_stored() {
        let home = tempfile::tempdir().unwrap();
        let cache = FetchCache::at(home.path().join("cache"));
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("edgar.parquet");
        std::fs::write(&source, b"bytes").unwrap();

        let mut m = meta("https://example.invalid/edgar.parquet", b"bytes");
        m.sha256 = String::new();
        assert!(cache.store(&m, &source).is_err());
        assert!(!cache.holds(&m.url));
    }

    // Two URLs, same bytes: one object, two locators. The object is written once.
    #[test]
    fn two_urls_with_the_same_bytes_share_one_object() {
        let home = tempfile::tempdir().unwrap();
        let cache = FetchCache::at(home.path().join("cache"));
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT394\n";
        let source = dir.path().join("edgar.parquet");
        std::fs::write(&source, bytes).unwrap();

        cache
            .store(&meta("https://a.invalid/edgar.parquet", bytes), &source)
            .unwrap();
        cache
            .store(&meta("https://b.invalid/edgar.parquet", bytes), &source)
            .unwrap();

        let objects: Vec<_> = std::fs::read_dir(cache.root.join("objects"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(objects.len(), 1, "the bytes are filed under their digest");
        assert!(cache.lookup("https://a.invalid/edgar.parquet").is_some());
        assert!(cache.lookup("https://b.invalid/edgar.parquet").is_some());
    }

    // The locator is named by a digest, so a URL carrying slashes, query strings and
    // percent-escapes cannot reach outside the cache root.
    #[test]
    fn a_locator_path_is_a_digest_not_a_url() {
        let cache = FetchCache::at(PathBuf::from("/tmp/arcform-cache-root"));
        let hostile = "https://example.invalid/../../etc/passwd?a=b#c";
        let path = cache.locator_path(hostile);
        assert!(path.starts_with("/tmp/arcform-cache-root/urls"));
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            format!("{}.arcmeta", crate::state::content_hash(hostile.as_bytes()))
        );
    }

    #[test]
    fn the_environment_chooses_the_root_and_can_switch_the_cache_off() {
        use std::ffi::OsString;
        let home = PathBuf::from("/home/analyst");

        assert_eq!(
            FetchCache::resolve_root(None, Some(home.clone())),
            Some(home.join(".arcform").join("fetch-cache")),
            "no environment: beside the history store"
        );
        assert_eq!(
            FetchCache::resolve_root(Some(OsString::from("")), Some(home.clone())),
            Some(home.join(".arcform").join("fetch-cache")),
            "an empty value is not a path"
        );
        assert_eq!(
            FetchCache::resolve_root(
                Some(OsString::from("/mnt/scratch/cache")),
                Some(home.clone())
            ),
            Some(PathBuf::from("/mnt/scratch/cache"))
        );
        assert_eq!(
            FetchCache::resolve_root(Some(OsString::from("OFF")), Some(home)),
            None,
            "`off` in any case disables the cache"
        );
        assert_eq!(
            FetchCache::resolve_root(None, None),
            None,
            "no home directory, no cache — and no error"
        );
    }

    // The chunked file hash and the whole-slice hash agree, so an entry verified here
    // compares like with like against a sidecar written by the fetch.
    #[test]
    fn test_fresh_hash_file_matches_content_hash() {
        let dir = tempfile::tempdir().unwrap();
        // Longer than the 64 KiB read buffer, so more than one chunk is fed to the hasher.
        let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let out = dir.path().join("big.bin");
        std::fs::write(&out, &bytes).unwrap();
        assert_eq!(
            hash_file(&out).unwrap(),
            crate::state::content_hash(&bytes),
            "chunked and whole-slice digests agree"
        );
    }

    #[test]
    fn a_pinned_digest_is_64_hex_characters_or_nothing() {
        let digest = crate::state::content_hash(b"bytes");
        assert_eq!(parse_digest(&digest), Some(digest.clone()));
        assert_eq!(
            parse_digest(&format!("  {}  ", digest.to_uppercase())),
            Some(digest),
            "trimmed and lowercased, so a pasted digest matches"
        );
        assert!(parse_digest("deadbeef").is_none(), "too short");
        assert!(parse_digest(&"z".repeat(64)).is_none(), "not hex");
        assert!(parse_digest("").is_none());
    }
}
