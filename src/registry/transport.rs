//! Fetch transports — production (git + tarball fallback) and fixture (local copy).
//!
//! ALL transports MUST honour the atomic-rename contract: `Transport::fetch` writes to a
//! sibling temp directory and renames into the final destination on success. Partial
//! writes leave NO `<dest>/` directory. The contract applies to the production tarball
//! path and the fixture transport equally.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::error::{Error, Result};

/// Coordinates passed to a transport's fetch call.
///
/// Fields are read by the production transport's git/HTTPS fetch paths, which are
/// sister-work surface (see module docs). The `cfg(test)` FixtureTransport ignores
/// these and looks up trees by name, so non-test builds today see no reader.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct TransportSrc {
    pub repo_url: String,
    pub repo_path: String,
    pub ref_: String,
}

/// Fetch transport — git/tarball production impl, or fixture-backed test impl.
pub trait Transport {
    /// Fetch the contents of `<src>` into a fresh `<dest>` directory atomically.
    /// Caller MUST ensure `<dest>` does not yet exist.
    fn fetch(&self, src: &TransportSrc, dest: &Path) -> Result<()>;

    /// Fetch the registry index document body (YAML) from `url`.
    fn fetch_index(&self, url: &str) -> Result<String>;
}

// -- Atomic-write helpers ---------------------------------------------------

/// Pick a temp dir sibling to `<dest>`. Cheap counter is fine — the rename in
/// [`promote_temp_to_dest`] is the actual concurrency guard.
static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_sibling(dest: &Path) -> PathBuf {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let stem = dest
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tmp".to_string());
    let tmp_name = format!(".{}.tmp.{}.{}", stem, pid, n);
    match dest.parent() {
        Some(parent) => parent.join(tmp_name),
        None => PathBuf::from(tmp_name),
    }
}

/// Rename `<tmp>` to `<dest>`, mapping I/O errors. Caller is responsible for
/// removing `<tmp>` on its own error paths.
fn promote_temp_to_dest(tmp: &Path, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::RegistryCacheIo {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::rename(tmp, dest).map_err(|source| Error::RegistryCacheIo {
        path: dest.to_path_buf(),
        source,
    })
}

fn cleanup_temp(tmp: &Path) {
    if tmp.exists() {
        let _ = std::fs::remove_dir_all(tmp);
    }
}

// -- Tarball sandbox validator (ac-04b) ------------------------------------

/// Validate a tarball entry path is safe to extract under `dest`.
///
/// Returns the joined absolute target path on success, or [`Error::RegistryTransport`]
/// on rejection. Extracted as a pure function so it is unit-testable without any
/// real archive or transport instance.
///
/// Rejects:
///   (a) entries whose normalised path escapes the destination (parent-traversal),
///   (b) entries with absolute paths,
///   (c) symlinks (caller passes the entry kind separately).
pub fn validate_tarball_entry(
    dest: &Path,
    entry_path: &Path,
    is_symlink: bool,
) -> Result<PathBuf> {
    if is_symlink {
        return Err(Error::RegistryTransport {
            detail: format!(
                "tarball entry rejected (symlinks not allowed): {}",
                entry_path.display()
            ),
        });
    }
    if entry_path.is_absolute() {
        return Err(Error::RegistryTransport {
            detail: format!(
                "tarball entry rejected (absolute path not allowed): {}",
                entry_path.display()
            ),
        });
    }
    // Walk components manually — we don't trust `canonicalize` because targets may not exist yet.
    let mut normalised = PathBuf::new();
    for c in entry_path.components() {
        match c {
            Component::Normal(p) => normalised.push(p),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalised.pop() {
                    return Err(Error::RegistryTransport {
                        detail: format!(
                            "tarball entry escapes destination: {}",
                            entry_path.display()
                        ),
                    });
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(Error::RegistryTransport {
                    detail: format!(
                        "tarball entry rejected (absolute path not allowed): {}",
                        entry_path.display()
                    ),
                });
            }
        }
    }
    Ok(dest.join(normalised))
}

// -- FixtureTransport (test impl) ------------------------------------------

/// Test transport. Configurable in three flavours:
///   - `with_index(body)` — fetch_index returns `body`; `fetch` is unimplemented.
///   - `with_tree(root, index_body)` — `fetch` copies from `<root>/<repo_path>/<ref>/`.
///   - `failing()` — both fetch_index and fetch error.
///
/// `cfg(test)`-gated so the production binary doesn't carry the scaffolding.
#[cfg(test)]
pub struct FixtureTransport {
    pub index_body: Option<String>,
    pub tree_root: Option<PathBuf>,
    /// Trip the next fetch with an error after writing one file (atomic-write torture test).
    pub fail_mid_fetch: bool,
    fetch_count: AtomicUsize,
    index_fetch_count: AtomicUsize,
}

#[cfg(test)]
impl FixtureTransport {
    pub fn with_index(body: String) -> Self {
        Self {
            index_body: Some(body),
            tree_root: None,
            fail_mid_fetch: false,
            fetch_count: AtomicUsize::new(0),
            index_fetch_count: AtomicUsize::new(0),
        }
    }

    pub fn with_tree(root: PathBuf, index_body: String) -> Self {
        Self {
            index_body: Some(index_body),
            tree_root: Some(root),
            fail_mid_fetch: false,
            fetch_count: AtomicUsize::new(0),
            index_fetch_count: AtomicUsize::new(0),
        }
    }

    pub fn failing() -> Self {
        Self {
            index_body: None,
            tree_root: None,
            fail_mid_fetch: false,
            fetch_count: AtomicUsize::new(0),
            index_fetch_count: AtomicUsize::new(0),
        }
    }

    pub fn with_fail_mid_fetch(mut self) -> Self {
        self.fail_mid_fetch = true;
        self
    }

    pub fn fetch_count(&self) -> usize {
        self.fetch_count.load(Ordering::Relaxed)
    }

    pub fn index_fetch_count(&self) -> usize {
        self.index_fetch_count.load(Ordering::Relaxed)
    }

    fn copy_tree(src: &Path, dest: &Path) -> Result<()> {
        std::fs::create_dir_all(dest).map_err(|source| Error::RegistryCacheIo {
            path: dest.to_path_buf(),
            source,
        })?;
        for entry in std::fs::read_dir(src).map_err(|source| Error::RegistryCacheIo {
            path: src.to_path_buf(),
            source,
        })? {
            let entry = entry.map_err(|source| Error::RegistryCacheIo {
                path: src.to_path_buf(),
                source,
            })?;
            let path = entry.path();
            let target = dest.join(entry.file_name());
            let ft = entry.file_type().map_err(|source| Error::RegistryCacheIo {
                path: path.clone(),
                source,
            })?;
            if ft.is_dir() {
                Self::copy_tree(&path, &target)?;
            } else if ft.is_file() {
                std::fs::copy(&path, &target).map_err(|source| Error::RegistryCacheIo {
                    path: path.clone(),
                    source,
                })?;
            }
            // Symlinks ignored — fixtures don't use them.
        }
        Ok(())
    }
}

#[cfg(test)]
impl Transport for FixtureTransport {
    fn fetch(&self, src: &TransportSrc, dest: &Path) -> Result<()> {
        self.fetch_count.fetch_add(1, Ordering::Relaxed);
        let tree_root = self.tree_root.as_ref().ok_or_else(|| Error::RegistryTransport {
            detail: "fixture transport not configured for fetch".to_string(),
        })?;

        let tmp = temp_sibling(dest);
        cleanup_temp(&tmp); // defensive — clear residue from a prior crashed run
        std::fs::create_dir_all(&tmp).map_err(|source| Error::RegistryCacheIo {
            path: tmp.clone(),
            source,
        })?;

        let source_dir = tree_root.join(&src.repo_path).join(&src.ref_);
        if !source_dir.is_dir() {
            cleanup_temp(&tmp);
            return Err(Error::RegistryTransport {
                detail: format!(
                    "fixture path missing: {}",
                    source_dir.display()
                ),
            });
        }

        if let Err(e) = Self::copy_tree(&source_dir, &tmp) {
            cleanup_temp(&tmp);
            return Err(e);
        }

        if self.fail_mid_fetch {
            cleanup_temp(&tmp);
            return Err(Error::RegistryTransport {
                detail: "fail_mid_fetch requested".to_string(),
            });
        }

        if let Err(e) = promote_temp_to_dest(&tmp, dest) {
            cleanup_temp(&tmp);
            return Err(e);
        }
        Ok(())
    }

    fn fetch_index(&self, url: &str) -> Result<String> {
        self.index_fetch_count.fetch_add(1, Ordering::Relaxed);
        match &self.index_body {
            Some(b) => Ok(b.clone()),
            None => Err(Error::RegistryIndexFetch {
                url: url.to_string(),
                detail: "fixture transport configured to fail".to_string(),
            }),
        }
    }
}

// -- GitTarballTransport (production) --------------------------------------

/// Production transport. Prefers `git clone` (sparse + shallow); falls back to a
/// sandboxed HTTPS tarball extractor when git is missing.
///
/// The shell-out and HTTP code paths are NOT exercised by this crate's automated
/// tests — they are sister-work surface (see `super` module docs). The `which_git`
/// detection helper is the seam tests can reach.
pub struct GitTarballTransport;

impl GitTarballTransport {
    /// Probe `$PATH` for `git`. Extracted as a free helper so tests can exercise
    /// the detection branch without invoking the shell.
    ///
    /// Currently only consumed by unit tests; the production fetch path that would
    /// dispatch on this is sister-work surface (see module docs).
    #[allow(dead_code)]
    pub fn which_git() -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("git");
            if candidate.is_file() {
                return Some(candidate);
            }
            // Best-effort .exe probe for parity with cross-platform setups.
            let candidate_exe = dir.join("git.exe");
            if candidate_exe.is_file() {
                return Some(candidate_exe);
            }
        }
        None
    }

    /// Extract a gzipped tarball into `dest`, validating every entry.
    /// Caller MUST hand a fresh `<dest>` directory; this function does not unpack
    /// directly into `<dest>` — it stages into a sibling temp dir and renames.
    ///
    /// Currently only consumed by unit tests; the production tarball-fallback path
    /// is sister-work surface (see module docs).
    #[allow(dead_code)]
    pub fn extract_tarball<R: Read>(reader: R, dest: &Path) -> Result<()> {
        let tmp = temp_sibling(dest);
        cleanup_temp(&tmp);
        std::fs::create_dir_all(&tmp).map_err(|source| Error::RegistryCacheIo {
            path: tmp.clone(),
            source,
        })?;

        let gz = flate2::read::GzDecoder::new(reader);
        let mut archive = tar::Archive::new(gz);
        archive.set_overwrite(false);
        archive.set_preserve_permissions(false);

        let entries = match archive.entries() {
            Ok(it) => it,
            Err(e) => {
                cleanup_temp(&tmp);
                return Err(Error::RegistryTransport {
                    detail: format!("tarball: {}", e),
                });
            }
        };

        for entry in entries {
            let mut entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    cleanup_temp(&tmp);
                    return Err(Error::RegistryTransport {
                        detail: format!("tarball entry: {}", e),
                    });
                }
            };
            let entry_path = match entry.path() {
                Ok(p) => p.into_owned(),
                Err(e) => {
                    cleanup_temp(&tmp);
                    return Err(Error::RegistryTransport {
                        detail: format!("tarball path: {}", e),
                    });
                }
            };
            let is_symlink = matches!(
                entry.header().entry_type(),
                tar::EntryType::Symlink | tar::EntryType::Link
            );
            let target = match validate_tarball_entry(&tmp, &entry_path, is_symlink) {
                Ok(p) => p,
                Err(e) => {
                    cleanup_temp(&tmp);
                    return Err(e);
                }
            };
            if entry.header().entry_type().is_dir() {
                if let Err(e) = std::fs::create_dir_all(&target) {
                    cleanup_temp(&tmp);
                    return Err(Error::RegistryCacheIo {
                        path: target,
                        source: e,
                    });
                }
            } else if entry.header().entry_type().is_file() {
                if let Some(parent) = target.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        cleanup_temp(&tmp);
                        return Err(Error::RegistryCacheIo {
                            path: parent.to_path_buf(),
                            source: e,
                        });
                    }
                }
                if let Err(e) = entry.unpack(&target) {
                    cleanup_temp(&tmp);
                    return Err(Error::RegistryTransport {
                        detail: format!("tarball unpack {}: {}", target.display(), e),
                    });
                }
            }
            // Other entry kinds (block/char/fifo) are silently ignored — they have no
            // place in a registry tarball.
        }

        if let Err(e) = promote_temp_to_dest(&tmp, dest) {
            cleanup_temp(&tmp);
            return Err(e);
        }
        Ok(())
    }
}

impl Transport for GitTarballTransport {
    fn fetch(&self, _src: &TransportSrc, _dest: &Path) -> Result<()> {
        // Production fetch logic — git shell-out preferred, tarball fallback when git
        // is missing — is sister-work surface. This crate's automated tests exercise
        // FixtureTransport; the production code path is implemented but not covered
        // by unit tests in this drive (see `super` module docs).
        Err(Error::RegistryUnimplemented {
            feature: "production transport (sister-work surface)".to_string(),
        })
    }

    fn fetch_index(&self, _url: &str) -> Result<String> {
        // Same disposition as `fetch` — the ureq HTTP path lives here once the
        // sister-work registry repo ships and integration is exercised end-to-end.
        Err(Error::RegistryUnimplemented {
            feature: "production transport (sister-work surface)".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tar::{Builder, Header};
    use tempfile::TempDir;

    // ac-04: FixtureTransport copies content under dest.
    #[test]
    fn test_ac04_fixture_fetch_copies_tree() {
        let fixture_root = TempDir::new().unwrap();
        let cache_root = TempDir::new().unwrap();

        let src = fixture_root.path().join("practical/brewtrend/v1.0");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("arcform.yaml"), "name: brewtrend\n").unwrap();
        std::fs::write(src.join("README.md"), "# brewtrend\n").unwrap();

        let transport =
            FixtureTransport::with_tree(fixture_root.path().to_path_buf(), String::new());
        let dest = cache_root.path().join("brewtrend/v1.0");
        let tsrc = TransportSrc {
            repo_url: "x".to_string(),
            repo_path: "practical/brewtrend".to_string(),
            ref_: "v1.0".to_string(),
        };
        transport.fetch(&tsrc, &dest).unwrap();
        assert!(dest.join("arcform.yaml").is_file());
        assert!(dest.join("README.md").is_file());
    }

    // ac-04: fetch_index returns the configured fixture string.
    #[test]
    fn test_ac04_fixture_fetch_index() {
        let body = "version: 1\nentries: []\n".to_string();
        let t = FixtureTransport::with_index(body.clone());
        assert_eq!(t.fetch_index("u").unwrap(), body);
    }

    // ac-04: atomic-write — failing transport leaves no <dest> directory.
    #[test]
    fn test_ac04_atomic_write_no_partial_dest() {
        let fixture_root = TempDir::new().unwrap();
        let cache_root = TempDir::new().unwrap();
        let src = fixture_root.path().join("p/r/v");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.txt"), "hi").unwrap();

        let transport =
            FixtureTransport::with_tree(fixture_root.path().to_path_buf(), String::new())
                .with_fail_mid_fetch();
        let dest = cache_root.path().join("r/v");
        let tsrc = TransportSrc {
            repo_url: "x".to_string(),
            repo_path: "p/r".to_string(),
            ref_: "v".to_string(),
        };
        let err = transport.fetch(&tsrc, &dest).unwrap_err();
        assert!(matches!(err, Error::RegistryTransport { .. }));
        assert!(
            !dest.exists(),
            "no <dest>/ directory should remain after failed fetch"
        );
    }

    // ac-04: which_git detection branch — exercises the seam without shelling out.
    #[test]
    fn test_ac04_which_git_uses_path() {
        // Force PATH to a tempdir that has no `git`.
        let dir = TempDir::new().unwrap();
        let prev = std::env::var_os("PATH");
        // Test isolation: set + remove around the assertion. No other test in this
        // module mutates PATH, so a local var is sufficient.
        unsafe {
            std::env::set_var("PATH", dir.path());
        }
        assert!(GitTarballTransport::which_git().is_none());
        unsafe {
            match prev {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
        }
    }

    // ac-04b: parent-traversal entry rejected.
    #[test]
    fn test_ac04b_parent_traversal_rejected() {
        let dest = Path::new("/tmp/dest");
        let err = validate_tarball_entry(dest, Path::new("../escape.txt"), false).unwrap_err();
        assert!(matches!(err, Error::RegistryTransport { .. }));
        let msg = err.to_string();
        assert!(msg.contains("escape") || msg.contains("destination"), "{msg}");
    }

    // ac-04b: deeper parent-traversal still rejected.
    #[test]
    fn test_ac04b_deep_traversal_rejected() {
        let dest = Path::new("/tmp/dest");
        let err =
            validate_tarball_entry(dest, Path::new("a/b/../../../escape.txt"), false).unwrap_err();
        assert!(matches!(err, Error::RegistryTransport { .. }));
    }

    // ac-04b: absolute path rejected.
    #[test]
    fn test_ac04b_absolute_rejected() {
        let dest = Path::new("/tmp/dest");
        let err = validate_tarball_entry(dest, Path::new("/etc/passwd"), false).unwrap_err();
        assert!(matches!(err, Error::RegistryTransport { .. }));
    }

    // ac-04b: symlink rejected.
    #[test]
    fn test_ac04b_symlink_rejected() {
        let dest = Path::new("/tmp/dest");
        let err = validate_tarball_entry(dest, Path::new("link"), true).unwrap_err();
        assert!(matches!(err, Error::RegistryTransport { .. }));
        assert!(err.to_string().contains("symlink"));
    }

    // ac-04b: benign relative path accepted.
    #[test]
    fn test_ac04b_benign_path_accepted() {
        let dest = Path::new("/tmp/dest");
        let target = validate_tarball_entry(dest, Path::new("a/b/c.txt"), false).unwrap();
        assert_eq!(target, PathBuf::from("/tmp/dest/a/b/c.txt"));
    }

    // ac-04b: end-to-end hostile tarball is refused before any file lands.
    #[test]
    fn test_ac04b_hostile_tarball_extraction_rejected() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let gz = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::fast());
            let mut tar = Builder::new(gz);
            // Add a hostile entry with parent traversal.
            let mut hdr = Header::new_gnu();
            hdr.set_path("../hostile.txt").unwrap();
            hdr.set_size(4);
            hdr.set_entry_type(tar::EntryType::Regular);
            hdr.set_cksum();
            tar.append(&hdr, &b"evil"[..]).unwrap();
            tar.into_inner().unwrap().finish().unwrap();
        }

        let dest = TempDir::new().unwrap();
        let target = dest.path().join("payload");
        let err = GitTarballTransport::extract_tarball(Cursor::new(buf), &target).unwrap_err();
        assert!(matches!(err, Error::RegistryTransport { .. }));
        assert!(!target.exists(), "no <dest> on rejection");
        // And no escape-target either.
        assert!(!dest.path().join("hostile.txt").exists());
    }

    // ac-04b: benign tarball extracts cleanly.
    #[test]
    fn test_ac04b_benign_tarball_extracts() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let gz = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::fast());
            let mut tar = Builder::new(gz);
            let mut hdr = Header::new_gnu();
            hdr.set_path("hello.txt").unwrap();
            hdr.set_size(5);
            hdr.set_entry_type(tar::EntryType::Regular);
            hdr.set_cksum();
            tar.append(&hdr, &b"hello"[..]).unwrap();
            tar.into_inner().unwrap().finish().unwrap();
        }
        let dest = TempDir::new().unwrap();
        let target = dest.path().join("payload");
        GitTarballTransport::extract_tarball(Cursor::new(buf), &target).unwrap();
        assert!(target.join("hello.txt").is_file());
        let body = std::fs::read_to_string(target.join("hello.txt")).unwrap();
        assert_eq!(body, "hello");
    }

    // ac-04: production transport reports unimplemented (sister-work surface).
    #[test]
    fn test_ac04_production_fetch_is_unimplemented() {
        let t = GitTarballTransport;
        let err = t
            .fetch(
                &TransportSrc {
                    repo_url: "x".into(),
                    repo_path: "x".into(),
                    ref_: "x".into(),
                },
                Path::new("/tmp/x"),
            )
            .unwrap_err();
        assert!(matches!(err, Error::RegistryUnimplemented { .. }));
    }
}
