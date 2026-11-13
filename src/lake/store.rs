//! The object-store seam: a small blocking interface over "get an object and its
//! ETag" and "put an object, optionally only if it is unchanged (or absent)".
//!
//! `DirStore` is the local implementation (tests, local runs); `S3Store` signs
//! requests for an S3-compatible endpoint with `rusty-s3` and sends them with `ureq`;
//! `PublicHttpStore` reads over plain https with no credentials.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusty_s3::actions::{GetObject, PutObject, S3Action};
use rusty_s3::{Bucket, Credentials, Method, UrlStyle};
use sha2::{Digest, Sha256};

/// An object's version tag, opaque and compared exactly as the store gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ETag(pub String);

/// The condition a write must meet to take effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PutCondition {
    Any,
    IfMatch(ETag),
    IfAbsent,
}

/// Why a store operation failed. `Display` never includes credentials or signed URLs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    PreconditionFailed,
    Throttled,
    NotFound,
    Other(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::PreconditionFailed => write!(f, "the object changed (precondition failed)"),
            StoreError::Throttled => write!(f, "the store is throttling requests"),
            StoreError::NotFound => write!(f, "object not found"),
            StoreError::Other(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for StoreError {}

pub trait ObjectStore: Send + Sync {
    /// The object's bytes and ETag, or `None` when the key does not exist.
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, ETag)>, StoreError>;

    fn put(
        &self,
        key: &str,
        bytes: &[u8],
        cond: PutCondition,
        cache_control: Option<&str>,
    ) -> Result<ETag, StoreError>;

    fn put_file(
        &self,
        key: &str,
        path: &Path,
        cond: PutCondition,
        cache_control: Option<&str>,
    ) -> Result<ETag, StoreError>;
}

fn other(e: impl std::fmt::Display) -> StoreError {
    StoreError::Other(e.to_string())
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ---------------------------------------------------------------- DirStore

/// A store backed by a directory: keys are paths under `root`.
pub struct DirStore {
    root: PathBuf,
}

/// Holds a key's lock file; removing it on drop releases the key.
struct LockGuard(PathBuf);

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

impl DirStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        DirStore { root: root.into() }
    }

    fn path_of(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }

    fn lock(&self, path: &Path) -> Result<LockGuard, StoreError> {
        let mut name = path.as_os_str().to_owned();
        name.push(".lock");
        let lock_path = PathBuf::from(name);
        let start = Instant::now();
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(_) => return Ok(LockGuard(lock_path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if start.elapsed() > Duration::from_secs(30) {
                        return Err(other("timed out waiting for the object's lock"));
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(e) => return Err(other(e)),
            }
        }
    }

    fn etag_of_existing(path: &Path) -> Result<Option<ETag>, StoreError> {
        match fs::read(path) {
            Ok(b) => Ok(Some(ETag(sha256_hex(&b)))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(other(e)),
        }
    }
}

impl ObjectStore for DirStore {
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, ETag)>, StoreError> {
        match fs::read(self.path_of(key)) {
            Ok(b) => {
                let etag = ETag(sha256_hex(&b));
                Ok(Some((b, etag)))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(other(e)),
        }
    }

    fn put(
        &self,
        key: &str,
        bytes: &[u8],
        cond: PutCondition,
        _cache_control: Option<&str>,
    ) -> Result<ETag, StoreError> {
        let path = self.path_of(key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(other)?;
        }
        let _guard = self.lock(&path)?;
        let current = Self::etag_of_existing(&path)?;
        match (&cond, &current) {
            (PutCondition::IfAbsent, Some(_)) => return Err(StoreError::PreconditionFailed),
            (PutCondition::IfMatch(want), Some(have)) if want != have => {
                return Err(StoreError::PreconditionFailed);
            }
            (PutCondition::IfMatch(_), None) => return Err(StoreError::PreconditionFailed),
            _ => {}
        }
        let mut tmp_name = path.as_os_str().to_owned();
        tmp_name.push(".tmp");
        let tmp = PathBuf::from(tmp_name);
        let mut f = fs::File::create(&tmp).map_err(other)?;
        f.write_all(bytes).map_err(other)?;
        f.sync_all().map_err(other)?;
        drop(f);
        fs::rename(&tmp, &path).map_err(other)?;
        Ok(ETag(sha256_hex(bytes)))
    }

    fn put_file(
        &self,
        key: &str,
        path: &Path,
        cond: PutCondition,
        cache_control: Option<&str>,
    ) -> Result<ETag, StoreError> {
        let bytes = fs::read(path).map_err(other)?;
        self.put(key, &bytes, cond, cache_control)
    }
}

// ---------------------------------------------------------------- S3Store

/// A request ready to send: the presigned URL and the headers that must accompany it.
#[derive(Debug, Clone)]
pub(crate) struct PreparedRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

/// Map an HTTP status to the store error it stands for, if it is one.
fn map_status(status: u16) -> Option<StoreError> {
    match status {
        412 => Some(StoreError::PreconditionFailed),
        429 => Some(StoreError::Throttled),
        _ => None,
    }
}

/// An S3-compatible bucket (Cloudflare R2 among them), path-style addressed.
pub struct S3Store {
    bucket: Bucket,
    credentials: Credentials,
}

const SIGN_FOR: Duration = Duration::from_secs(3600);

impl S3Store {
    /// Cloudflare R2: `https://<account_id>.r2.cloudflarestorage.com`, region `auto`.
    pub fn r2(
        account_id: &str,
        bucket: &str,
        key_id: &str,
        secret: &str,
    ) -> Result<Self, StoreError> {
        Self::new(
            &format!("https://{account_id}.r2.cloudflarestorage.com"),
            bucket,
            "auto",
            key_id,
            secret,
        )
    }

    pub fn new(
        endpoint_url: &str,
        bucket: &str,
        region: &str,
        key_id: &str,
        secret: &str,
    ) -> Result<Self, StoreError> {
        let endpoint = url::Url::parse(endpoint_url)
            .map_err(|_| other("the object-store endpoint is not a valid URL"))?;
        let bucket = Bucket::new(
            endpoint,
            UrlStyle::Path,
            bucket.to_string(),
            region.to_string(),
        )
        .map_err(|e| other(format!("invalid object-store bucket: {e}")))?;
        Ok(S3Store {
            bucket,
            credentials: Credentials::new(key_id, secret),
        })
    }

    /// Build the signed request for `method` on `key`. The condition's header is
    /// signed, so the store cannot be made to ignore it.
    pub(crate) fn request(
        &self,
        method: Method,
        key: &str,
        cond: &PutCondition,
    ) -> PreparedRequest {
        let mut headers: Vec<(String, String)> = Vec::new();
        match cond {
            PutCondition::Any => {}
            PutCondition::IfMatch(ETag(e)) => headers.push(("if-match".into(), e.clone())),
            PutCondition::IfAbsent => headers.push(("if-none-match".into(), "*".into())),
        }
        let url = match method {
            Method::Put => {
                let mut a = PutObject::new(&self.bucket, Some(&self.credentials), key);
                for (k, v) in &headers {
                    a.headers_mut().insert(k.clone(), v.clone());
                }
                a.sign(SIGN_FOR)
            }
            _ => {
                let mut a = GetObject::new(&self.bucket, Some(&self.credentials), key);
                for (k, v) in &headers {
                    a.headers_mut().insert(k.clone(), v.clone());
                }
                a.sign(SIGN_FOR)
            }
        };
        PreparedRequest {
            url: url.to_string(),
            headers,
        }
    }

    fn send_put(
        &self,
        key: &str,
        len: u64,
        body: impl std::io::Read,
        cond: PutCondition,
        cache_control: Option<&str>,
    ) -> Result<ETag, StoreError> {
        let prepared = self.request(Method::Put, key, &cond);
        let mut req = ureq::put(&prepared.url).set("Content-Length", &len.to_string());
        for (k, v) in &prepared.headers {
            req = req.set(k, v);
        }
        if let Some(cc) = cache_control {
            req = req.set("Cache-Control", cc);
        }
        match req.send(body) {
            Ok(resp) => Ok(ETag(resp.header("etag").unwrap_or_default().to_string())),
            Err(e) => Err(map_ureq_error(e, key)),
        }
    }
}

/// Turn a ureq failure into a `StoreError` naming only the object key and the HTTP
/// status. A presigned URL holds the key id and signature, so no URL (and no
/// transport message, which may quote one) is ever carried into the error.
fn map_ureq_error(e: ureq::Error, key: &str) -> StoreError {
    match e {
        ureq::Error::Status(code, _) => map_status(code).unwrap_or_else(|| {
            if code == 404 {
                StoreError::NotFound
            } else {
                StoreError::Other(format!("the object store answered HTTP {code} for `{key}`"))
            }
        }),
        ureq::Error::Transport(t) => StoreError::Other(format!(
            "the object store could not be reached for `{key}` ({:?})",
            t.kind()
        )),
    }
}

fn read_get_response(
    result: Result<ureq::Response, ureq::Error>,
    key: &str,
) -> Result<Option<(Vec<u8>, ETag)>, StoreError> {
    match result {
        Ok(resp) => {
            let etag = ETag(resp.header("etag").unwrap_or_default().to_string());
            let mut bytes = Vec::new();
            resp.into_reader()
                .read_to_end(&mut bytes)
                .map_err(|_| other("reading the object-store response failed"))?;
            Ok(Some((bytes, etag)))
        }
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(e) => Err(map_ureq_error(e, key)),
    }
}

use std::io::Read as _;

impl ObjectStore for S3Store {
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, ETag)>, StoreError> {
        let prepared = self.request(Method::Get, key, &PutCondition::Any);
        read_get_response(ureq::get(&prepared.url).call(), key)
    }

    fn put(
        &self,
        key: &str,
        bytes: &[u8],
        cond: PutCondition,
        cache_control: Option<&str>,
    ) -> Result<ETag, StoreError> {
        self.send_put(key, bytes.len() as u64, bytes, cond, cache_control)
    }

    fn put_file(
        &self,
        key: &str,
        path: &Path,
        cond: PutCondition,
        cache_control: Option<&str>,
    ) -> Result<ETag, StoreError> {
        let file = fs::File::open(path).map_err(other)?;
        let len = file.metadata().map_err(other)?.len();
        self.send_put(key, len, file, cond, cache_control)
    }
}

// --------------------------------------------------------- PublicHttpStore

/// Read-only access to a publicly readable bucket over plain https, no credentials.
pub struct PublicHttpStore {
    base_url: String,
}

impl PublicHttpStore {
    pub fn new(base_url: impl Into<String>) -> Self {
        PublicHttpStore {
            base_url: base_url.into(),
        }
    }
}

impl ObjectStore for PublicHttpStore {
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, ETag)>, StoreError> {
        let url = format!("{}/{}", self.base_url.trim_end_matches('/'), key);
        read_get_response(ureq::get(&url).call(), key)
    }

    fn put(
        &self,
        _key: &str,
        _bytes: &[u8],
        _cond: PutCondition,
        _cache_control: Option<&str>,
    ) -> Result<ETag, StoreError> {
        Err(StoreError::Other("read-only".into()))
    }

    fn put_file(
        &self,
        _key: &str,
        _path: &Path,
        _cond: PutCondition,
        _cache_control: Option<&str>,
    ) -> Result<ETag, StoreError> {
        Err(StoreError::Other("read-only".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn store() -> (tempfile::TempDir, DirStore) {
        let dir = tempfile::tempdir().unwrap();
        let s = DirStore::new(dir.path());
        (dir, s)
    }

    #[test]
    fn dir_store_round_trips_and_reports_etag() {
        let (_d, s) = store();
        let put_tag = s.put("a/b.bin", b"hello", PutCondition::Any, None).unwrap();
        let (bytes, tag) = s.get("a/b.bin").unwrap().unwrap();
        assert_eq!(bytes, b"hello");
        assert_eq!(tag, put_tag);
        assert!(s.get("missing").unwrap().is_none());
    }

    #[test]
    fn if_match_refuses_a_stale_etag() {
        let (_d, s) = store();
        let a = s.put("k", b"A", PutCondition::Any, None).unwrap();
        s.put("k", b"B", PutCondition::Any, None).unwrap();
        let err = s
            .put("k", b"C", PutCondition::IfMatch(a), None)
            .unwrap_err();
        assert_eq!(err, StoreError::PreconditionFailed);
        assert_eq!(s.get("k").unwrap().unwrap().0, b"B");
    }

    #[test]
    fn if_absent_refuses_an_existing_key() {
        let (_d, s) = store();
        s.put("k", b"A", PutCondition::IfAbsent, None).unwrap();
        let err = s.put("k", b"B", PutCondition::IfAbsent, None).unwrap_err();
        assert_eq!(err, StoreError::PreconditionFailed);
        assert_eq!(s.get("k").unwrap().unwrap().0, b"A");
    }

    #[test]
    fn racing_conditional_writes_have_one_winner() {
        let (_d, s) = store();
        s.put("k", b"seed", PutCondition::Any, None).unwrap();
        let s = Arc::new(s);
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let (s, barrier) = (s.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let (_, tag) = s.get("k").unwrap().unwrap();
                    barrier.wait();
                    s.put(
                        "k",
                        format!("w{i}").as_bytes(),
                        PutCondition::IfMatch(tag),
                        None,
                    )
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert!(
            results
                .iter()
                .filter_map(|r| r.as_ref().err())
                .all(|e| *e == StoreError::PreconditionFailed)
        );
    }

    #[test]
    fn s3_store_signs_conditional_headers() {
        let s = S3Store::r2("acct", "bucket", "k", "s3cr3t-value").unwrap();
        let req = s.request(
            Method::Put,
            "cat/live.ducklake",
            &PutCondition::IfMatch(ETag("\"abc\"".into())),
        );
        let url = url::Url::parse(&req.url).unwrap();
        assert_eq!(url.host_str(), Some("acct.r2.cloudflarestorage.com"));
        assert!(url.path().starts_with("/bucket/"));
        let signed = url
            .query_pairs()
            .find(|(k, _)| k == "X-Amz-SignedHeaders")
            .unwrap()
            .1
            .into_owned();
        assert!(signed.split(';').any(|h| h == "if-match"), "{signed}");
        assert!(
            req.headers
                .iter()
                .any(|(k, v)| k == "if-match" && v == "\"abc\"")
        );
        let all = format!("{} {:?}", req.url, req.headers);
        assert!(!all.contains("s3cr3t-value"));

        let absent = s.request(Method::Put, "k", &PutCondition::IfAbsent);
        assert!(
            absent
                .headers
                .contains(&("if-none-match".into(), "*".into()))
        );
    }

    #[test]
    fn s3_status_codes_map_to_store_errors() {
        assert_eq!(map_status(412), Some(StoreError::PreconditionFailed));
        assert_eq!(map_status(429), Some(StoreError::Throttled));
        assert_eq!(map_status(200), None);
    }

    #[test]
    fn a_transport_error_names_no_secret() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let s = S3Store::new(
            &format!("http://127.0.0.1:{port}"),
            "bucket",
            "auto",
            "KEYID-123",
            "s3cr3t-value",
        )
        .unwrap();
        let err = s.get("cat/live.ducklake").unwrap_err();
        assert!(matches!(err, StoreError::Other(_)));
        let shown = err.to_string();
        assert!(shown.contains("cat/live.ducklake"), "{shown}");
        for bad in [
            "KEYID-123",
            "s3cr3t-value",
            "X-Amz-Signature",
            "X-Amz-Credential",
        ] {
            assert!(!shown.contains(bad), "{shown}");
        }
    }

    #[test]
    fn public_http_store_is_read_only() {
        let s = PublicHttpStore::new("https://example.invalid");
        assert_eq!(
            s.put("k", b"x", PutCondition::Any, None),
            Err(StoreError::Other("read-only".into()))
        );
    }
}
