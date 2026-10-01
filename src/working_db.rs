//! Where a Protocol's working database lives when its `arcform.yaml` names no `db:`.
//!
//! A Protocol directory is what a second person is handed — by a copy, a zip, a shared
//! drive or a clone — and it runs where it lands. The database a run builds is not part
//! of that: the receiver rebuilds it anyway, and a database file inside a folder a sync
//! client is watching gets synced under a running `arc run`. So a Protocol that names no
//! `db:` keeps its database, and the write-ahead log DuckDB writes beside it, in arc's
//! own data folder, as local history ([`crate::history`]) and the fetch cache
//! ([`crate::fetch_cache`]) keep theirs. An explicit `db:` stays where its author wrote
//! it, relative to the manifest directory.
//!
//! # Where the data folder is
//!
//! `$ARCFORM_DB_DIR` when set and non-empty, else `~/.arcform/db`; refused with
//! [`Error::DbRootMissing`], whose message names the variable, when there is neither.
//!
//! Inside it, each Protocol directory gets a directory keyed by a hash of its canonical
//! path, holding `<name>.duckdb` and a `protocol-path` file naming the directory in the
//! clear for a person inspecting the folder. Two Protocols with one `name:` in two
//! directories therefore never share a database, and a directory copied to a new path
//! gets a fresh one, so every SQL step runs again where it lands.
//!
//! `arc run` names the database's path once per run when the manifest names no `db:`,
//! and every `command:` step and hook reads the path from `ARC_DB_PATH`, so a step that
//! runs the DuckDB CLI by hand opens the database the SQL steps ran on.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// The variable that puts the data folder somewhere other than `~/.arcform/db`.
pub(crate) const DB_DIR_ENV: &str = "ARCFORM_DB_DIR";

/// The variable a `command:` step and a hook read the run's database path from.
pub(crate) const DB_PATH_ENV: &str = "ARC_DB_PATH";

/// The file beside a database in the data folder that names its Protocol directory.
const PROTOCOL_PATH_FILE: &str = "protocol-path";

/// A database in the data folder, and the Protocol directory it is keyed to.
#[derive(Debug)]
pub(crate) struct Located {
    /// `<root>/<key>/<name>.duckdb`.
    pub(crate) path: PathBuf,
    /// The canonical Protocol directory the key is made from, which
    /// [`mark`] names beside the database.
    pub(crate) protocol_dir: PathBuf,
}

/// The database of the Protocol named `name` in `dir`, in the data folder.
pub(crate) fn locate(name: &str, dir: &Path) -> Result<Located> {
    let root = resolve_root(std::env::var_os(DB_DIR_ENV), dirs::home_dir())?;
    path_under(&root, name, dir)
}

/// The database of the Protocol named `name` in `dir`, under the data folder `root`:
/// `<root>/<key>/<name>.duckdb`, the key made from `dir`'s canonical path.
/// Refused when `name` is not a plain file name — one holding a `/`, or `.` or `..` —
/// because the database would then leave the directory keyed to `dir`, where two
/// Protocols could share it.
fn path_under(root: &Path, name: &str, dir: &Path) -> Result<Located> {
    if Path::new(name).file_name() != Some(std::ffi::OsStr::new(name)) {
        return Err(Error::DbNameNotAFileName {
            name: name.to_string(),
        });
    }
    let canonical = canonical_dir(dir)?;
    Ok(Located {
        path: root
            .join(path_key(&canonical))
            .join(format!("{name}.duckdb")),
        protocol_dir: canonical,
    })
}

/// `dir` with every link and `..` resolved, so one directory reached by two spellings
/// has one key.
fn canonical_dir(dir: &Path) -> Result<PathBuf> {
    dir.canonicalize().map_err(|e| Error::FileRead {
        path: dir.to_path_buf(),
        source: e,
    })
}

/// The first sixteen hex digits of the SHA-256 of `path`'s bytes, as local history
/// keys a spec by its canonical path.
fn path_key(path: &Path) -> String {
    let digest = Sha256::digest(path.as_os_str().as_encoded_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Name the Protocol directory `dir` in the clear beside the database at `db_path`,
/// creating the database's directory. A file that already names it is left alone.
pub(crate) fn mark(db_path: &Path, dir: &Path) -> Result<()> {
    let Some(key_dir) = db_path.parent() else {
        return Ok(());
    };
    let refused = |path: &Path| {
        let path = path.to_path_buf();
        move |source| Error::DbFolderWrite { path, source }
    };
    std::fs::create_dir_all(key_dir).map_err(refused(key_dir))?;
    let marker = key_dir.join(PROTOCOL_PATH_FILE);
    let text = format!("{}\n", dir.display());
    if std::fs::read_to_string(&marker).is_ok_and(|old| old == text) {
        return Ok(());
    }
    std::fs::write(&marker, text).map_err(refused(&marker))
}

/// `$ARCFORM_DB_DIR` when set and non-empty, else `~/.arcform/db`.
fn resolve_root(env: Option<std::ffi::OsString>, home: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(dir) = env
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir));
    }
    home.map(|home| home.join(".arcform").join("db"))
        .ok_or(Error::DbRootMissing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_variable_names_the_root_when_it_is_set() {
        let root = resolve_root(Some("/data/arc-db".into()), Some("/home/a".into())).unwrap();
        assert_eq!(root, PathBuf::from("/data/arc-db"));
    }

    #[test]
    fn an_unset_or_empty_variable_falls_back_to_the_home_directory() {
        for env in [None, Some(std::ffi::OsString::new())] {
            let root = resolve_root(env, Some("/home/a".into())).unwrap();
            assert_eq!(root, PathBuf::from("/home/a/.arcform/db"));
        }
    }

    #[test]
    fn neither_a_variable_nor_a_home_directory_is_refused_naming_the_variable() {
        let err = resolve_root(None, None).unwrap_err();
        assert!(matches!(err, Error::DbRootMissing), "{err:?}");
        assert!(err.to_string().contains(DB_DIR_ENV), "{err}");
    }

    #[test]
    fn the_database_is_the_protocols_name_under_a_key_of_its_canonical_directory() {
        let root = tempfile::tempdir().unwrap();
        let protocol = tempfile::tempdir().unwrap();
        let located = path_under(root.path(), "tides", protocol.path()).unwrap();
        let path = located.path;

        let canonical = protocol.path().canonicalize().unwrap();
        assert_eq!(located.protocol_dir, canonical);
        assert_eq!(
            path,
            root.path().join(path_key(&canonical)).join("tides.duckdb")
        );
        assert!(
            !path.starts_with(protocol.path()) && !path.starts_with(&canonical),
            "the database is not inside the Protocol directory: {}",
            path.display()
        );
    }

    #[test]
    fn one_directory_reached_two_ways_has_one_key_and_two_directories_have_two() {
        let root = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        let a = base.path().join("a");
        let b = base.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();

        let direct = path_under(root.path(), "p", &a).unwrap().path;
        let roundabout = path_under(root.path(), "p", &b.join("..").join("a"))
            .unwrap()
            .path;
        assert_eq!(direct, roundabout);

        let other = path_under(root.path(), "p", &b).unwrap().path;
        assert_ne!(direct, other, "one name in two directories, two databases");
    }

    #[test]
    fn a_name_that_is_not_a_plain_file_name_is_refused_naming_it() {
        let root = tempfile::tempdir().unwrap();
        let protocol = tempfile::tempdir().unwrap();
        for name in ["../shared", "a/b", "..", ".", "a/"] {
            let err = path_under(root.path(), name, protocol.path()).unwrap_err();
            assert!(
                matches!(&err, Error::DbNameNotAFileName { name: n } if n == name),
                "{name}: {err:?}"
            );
        }
        assert!(path_under(root.path(), "tides.v2", protocol.path()).is_ok());
    }

    #[test]
    fn a_marker_that_cannot_be_written_is_refused_naming_it() {
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("k").join(PROTOCOL_PATH_FILE);
        std::fs::create_dir_all(&marker).unwrap();
        let err = mark(
            &root.path().join("k").join("p.duckdb"),
            Path::new("/work/p"),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::DbFolderWrite { path, .. } if *path == marker),
            "{err:?}"
        );
    }

    #[test]
    fn a_directory_that_does_not_exist_is_refused_naming_it() {
        let root = tempfile::tempdir().unwrap();
        let gone = root.path().join("gone");
        let err = path_under(root.path(), "p", &gone).unwrap_err();
        assert!(err.to_string().contains("gone"), "{err}");
    }

    #[test]
    fn the_key_is_sixteen_hex_digits_of_the_sha256_of_the_path() {
        // `printf /a | sha256sum` = 6a50dc8584134c7de537c0052ff6d236bf874355e050c90523e0c5ff2a543a28
        assert_eq!(path_key(Path::new("/a")), "6a50dc8584134c7d");
    }

    #[test]
    fn the_marker_names_the_directory_in_the_clear_and_is_rewritten_when_it_differs() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("k").join("p.duckdb");
        mark(&db, Path::new("/work/p")).unwrap();
        let marker = root.path().join("k").join(PROTOCOL_PATH_FILE);
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "/work/p\n");

        std::fs::write(&marker, "something else\n").unwrap();
        mark(&db, Path::new("/work/p")).unwrap();
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "/work/p\n");
    }
}
