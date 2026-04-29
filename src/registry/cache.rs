//! Cache root, cache path, and TTL-aware index cache.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};
use crate::registry::index::RegistryIndex;
use crate::registry::resolve::ResolvedEntry;
use crate::registry::transport::Transport;

const CACHE_ENV: &str = "ARCFORM_REGISTRY_CACHE";
const DEFAULT_CACHE_SUBDIR: &str = ".arcform/registry";

/// Index TTL — 1 hour, per spec constraints.
pub const DEFAULT_INDEX_TTL_SECS: u64 = 3600;

/// Pluggable home-directory provider — exists primarily so tests can simulate
/// the rare `home_dir() == None` branch deterministically without monkeypatching
/// process-wide environment.
pub trait HomeProvider {
    fn home_dir(&self) -> Option<PathBuf>;
}

/// Production [`HomeProvider`] — delegates to the `dirs` crate.
pub struct SystemHomeProvider;

impl HomeProvider for SystemHomeProvider {
    fn home_dir(&self) -> Option<PathBuf> {
        dirs::home_dir()
    }
}

/// Resolve the registry cache root using the system home provider.
///
/// Reads `$ARCFORM_REGISTRY_CACHE` if set, else `~/.arcform/registry/`. Errors with
/// [`Error::RegistryCacheRootMissing`] when neither is available.
pub fn cache_root() -> Result<PathBuf> {
    cache_root_with(&SystemHomeProvider)
}

/// Test-friendly variant of [`cache_root`] that takes a [`HomeProvider`] for the
/// fallback branch. The env var is honoured first regardless of provider.
pub fn cache_root_with(provider: &dyn HomeProvider) -> Result<PathBuf> {
    if let Some(p) = std::env::var_os(CACHE_ENV) {
        return Ok(PathBuf::from(p));
    }
    match provider.home_dir() {
        Some(home) => Ok(home.join(DEFAULT_CACHE_SUBDIR)),
        None => Err(Error::RegistryCacheRootMissing),
    }
}

/// Compute the on-disk cache path for a resolved entry.
///
/// Canonical:   `<root>/<name>/<ref>/`
/// Contributor: `<root>/<owner>/<name>/<ref>/`
pub fn cache_path(root: &Path, resolved: &ResolvedEntry) -> PathBuf {
    match &resolved.owner {
        Some(owner) => root.join(owner).join(&resolved.name).join(&resolved.ref_),
        None => root.join(&resolved.name).join(&resolved.ref_),
    }
}

/// Ensure the parent directory of a fetch destination exists. Surfaces I/O failures
/// as [`Error::RegistryCacheIo`] for clarity.
pub fn ensure_cache_root(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::RegistryCacheIo {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

/// Write the registry index document to disk atomically (temp + rename).
fn write_index_atomic(path: &Path, body: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::RegistryCacheIo {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, body).map_err(|source| Error::RegistryCacheIo {
        path: tmp.clone(),
        source,
    })?;
    std::fs::rename(&tmp, path).map_err(|source| Error::RegistryCacheIo {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn read_fetched_at(path: &Path) -> Option<u64> {
    let raw = std::fs::read_to_string(path).ok()?;
    raw.trim().parse::<u64>().ok()
}

fn write_fetched_at(path: &Path, secs: u64) -> Result<()> {
    let body = format!("{}\n", secs);
    std::fs::write(path, body).map_err(|source| Error::RegistryCacheIo {
        path: path.to_path_buf(),
        source,
    })
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// TTL-aware index cache wrapper around a [`Transport`].
pub struct IndexCache<'a> {
    pub transport: &'a dyn Transport,
    pub root: PathBuf,
    pub url: String,
    pub ttl_secs: u64,
}

impl<'a> IndexCache<'a> {
    pub fn new(transport: &'a dyn Transport, root: PathBuf, url: String) -> Self {
        Self {
            transport,
            root,
            url,
            ttl_secs: DEFAULT_INDEX_TTL_SECS,
        }
    }

    /// Override the default cache TTL. Used by tests; reserved for a future
    /// `--cache-ttl` flag.
    #[allow(dead_code)]
    pub fn with_ttl(mut self, ttl_secs: u64) -> Self {
        self.ttl_secs = ttl_secs;
        self
    }

    fn index_path(&self) -> PathBuf {
        self.root.join("index.yaml")
    }

    fn fetched_path(&self) -> PathBuf {
        self.root.join("index.yaml.fetched")
    }

    /// Load the index, fetching when stale or when `refresh` is `true`.
    ///
    /// - Within TTL with cache present: returns cached.
    /// - Stale or `refresh=true`: attempt transport fetch.
    /// - Fetch failure: when triggered by TTL with a stale-but-present cache, falls
    ///   back to the cached copy (offline grace). When triggered by `refresh=true`,
    ///   errors hard — the user explicitly asked for fresh.
    /// - No cache + fetch failure: errors.
    pub fn load(&self, refresh: bool) -> Result<RegistryIndex> {
        let idx_path = self.index_path();
        let fetched_path = self.fetched_path();

        let cached_present = idx_path.is_file();
        let fetched_at = read_fetched_at(&fetched_path);
        let now = now_unix_secs();
        let stale = match fetched_at {
            Some(at) => now.saturating_sub(at) > self.ttl_secs,
            None => true,
        };

        let must_fetch = refresh || stale || !cached_present;

        if must_fetch {
            match self.transport.fetch_index(&self.url) {
                Ok(body) => {
                    write_index_atomic(&idx_path, &body)?;
                    write_fetched_at(&fetched_path, now)?;
                    return RegistryIndex::parse(&body);
                }
                Err(e) => {
                    if cached_present && !refresh {
                        // Offline grace: TTL-triggered fetch failed but cache exists.
                        let body = std::fs::read_to_string(&idx_path).map_err(|source| {
                            Error::RegistryCacheIo {
                                path: idx_path.clone(),
                                source,
                            }
                        })?;
                        return RegistryIndex::parse(&body);
                    }
                    return Err(e);
                }
            }
        }

        let body = std::fs::read_to_string(&idx_path).map_err(|source| Error::RegistryCacheIo {
            path: idx_path.clone(),
            source,
        })?;
        RegistryIndex::parse(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::index::Pillar;
    use crate::registry::transport::FixtureTransport;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Serialise tests that mutate process env vars.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct NoneHomeProvider;
    impl HomeProvider for NoneHomeProvider {
        fn home_dir(&self) -> Option<PathBuf> {
            None
        }
    }

    fn resolved(name: &str, owner: Option<&str>, ref_: &str) -> ResolvedEntry {
        ResolvedEntry {
            name: name.to_string(),
            owner: owner.map(String::from),
            ref_: ref_.to_string(),
            repo_url: "x".to_string(),
            repo_path: "x".to_string(),
            pillar: Pillar::Practical,
        }
    }

    // ac-03: cache_path canonical layout.
    #[test]
    fn test_ac03_cache_path_canonical() {
        let root = PathBuf::from("/tmp/r");
        let r = resolved("brewtrend", None, "v1.0");
        assert_eq!(cache_path(&root, &r), PathBuf::from("/tmp/r/brewtrend/v1.0"));
    }

    // ac-03: cache_path contributor layout.
    #[test]
    fn test_ac03_cache_path_contributor() {
        let root = PathBuf::from("/tmp/r");
        let r = resolved("myproject", Some("someone"), "v0.3");
        assert_eq!(
            cache_path(&root, &r),
            PathBuf::from("/tmp/r/someone/myproject/v0.3")
        );
    }

    // ac-03: $ARCFORM_REGISTRY_CACHE override is honoured.
    #[test]
    fn test_ac03_env_override_honoured() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new().unwrap();
        // Safe: mutex above prevents racing with other env-mutating tests in this module.
        unsafe {
            std::env::set_var(CACHE_ENV, dir.path());
        }
        let root = cache_root().unwrap();
        unsafe {
            std::env::remove_var(CACHE_ENV);
        }
        assert_eq!(root, dir.path());
    }

    // ac-03: home_dir() == None branch surfaces RegistryCacheRootMissing.
    #[test]
    fn test_ac03_home_missing_errors() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var(CACHE_ENV);
        }
        let err = cache_root_with(&NoneHomeProvider).unwrap_err();
        assert!(matches!(err, Error::RegistryCacheRootMissing));
        let msg = err.to_string();
        assert!(
            msg.contains("ARCFORM_REGISTRY_CACHE"),
            "should suggest the env var: {msg}"
        );
    }

    // ac-05: first call fetches; second within TTL reuses cache.
    #[test]
    fn test_ac05_first_fetches_then_caches() {
        let dir = TempDir::new().unwrap();
        let body = r#"version: 1
entries: []
"#;
        let transport = FixtureTransport::with_index(body.to_string());
        let cache = IndexCache::new(&transport, dir.path().to_path_buf(), "u".to_string());

        let _ = cache.load(false).unwrap();
        assert_eq!(transport.index_fetch_count(), 1);
        let _ = cache.load(false).unwrap();
        assert_eq!(
            transport.index_fetch_count(),
            1,
            "second load within TTL should not refetch"
        );
    }

    // ac-05: --refresh forces fetch even when fresh.
    #[test]
    fn test_ac05_refresh_forces_fetch() {
        let dir = TempDir::new().unwrap();
        let body = "version: 1\nentries: []\n".to_string();
        let transport = FixtureTransport::with_index(body);
        let cache = IndexCache::new(&transport, dir.path().to_path_buf(), "u".to_string());

        cache.load(false).unwrap();
        cache.load(true).unwrap();
        assert_eq!(transport.index_fetch_count(), 2);
    }

    // ac-05: stale + transport failure with cache present returns cached (offline grace).
    #[test]
    fn test_ac05_offline_grace_on_ttl_refresh() {
        let dir = TempDir::new().unwrap();
        let good = FixtureTransport::with_index("version: 1\nentries: []\n".to_string());
        let cache = IndexCache::new(&good, dir.path().to_path_buf(), "u".to_string()).with_ttl(0);
        cache.load(false).unwrap();

        // Force the cache to look stale by rewriting fetched timestamp to long ago.
        std::fs::write(dir.path().join("index.yaml.fetched"), "0\n").unwrap();

        let bad = FixtureTransport::failing();
        let cache = IndexCache::new(&bad, dir.path().to_path_buf(), "u".to_string()).with_ttl(0);
        // refresh=false → falls back to cached copy.
        cache.load(false).expect("should serve cached copy on ttl-refresh failure");
    }

    // ac-05: --refresh + transport failure errors hard (no offline grace under explicit refresh).
    #[test]
    fn test_ac05_refresh_failure_errors_hard() {
        let dir = TempDir::new().unwrap();
        let good = FixtureTransport::with_index("version: 1\nentries: []\n".to_string());
        IndexCache::new(&good, dir.path().to_path_buf(), "u".to_string())
            .load(false)
            .unwrap();

        let bad = FixtureTransport::failing();
        let cache = IndexCache::new(&bad, dir.path().to_path_buf(), "u".to_string());
        let err = cache.load(true).unwrap_err();
        assert!(matches!(err, Error::RegistryIndexFetch { .. }));
    }

    // ac-05: no cache + transport failure errors with a clear message.
    #[test]
    fn test_ac05_no_cache_failure_errors() {
        let dir = TempDir::new().unwrap();
        let bad = FixtureTransport::failing();
        let cache = IndexCache::new(&bad, dir.path().to_path_buf(), "u".to_string());
        let err = cache.load(false).unwrap_err();
        assert!(matches!(err, Error::RegistryIndexFetch { .. }));
    }

    // ac-05: .fetched file contains a Unix epoch integer.
    #[test]
    fn test_ac05_fetched_file_format_is_epoch_integer() {
        let dir = TempDir::new().unwrap();
        let body = "version: 1\nentries: []\n".to_string();
        let transport = FixtureTransport::with_index(body);
        let cache = IndexCache::new(&transport, dir.path().to_path_buf(), "u".to_string());
        cache.load(false).unwrap();
        let raw = std::fs::read_to_string(dir.path().join("index.yaml.fetched")).unwrap();
        let parsed: u64 = raw.trim().parse().expect("fetched file should parse as u64");
        assert!(parsed > 0, "epoch should be positive, got {parsed}");
    }
}
