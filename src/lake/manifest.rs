//! The compatible `manifest.json`, kept current while its readers move to the
//! catalogue. Written after the catalogue, so an entry never names a snapshot the
//! catalogue lacks.

use std::time::Duration;

use serde_json::{Map, Value, json};

use super::meta::failed;
use super::store::{ObjectStore, PutCondition, StoreError};
use crate::error::Result;

/// One published table's entry under `datasets`.
#[derive(Debug, Clone, PartialEq)]
pub struct ManifestEntry {
    /// `namespace/name`.
    pub address: String,
    /// The readable copy's address.
    pub url: String,
    pub rows: u64,
    pub bytes: u64,
    pub sha256: String,
    /// The commit time, RFC 3339.
    pub published: String,
    /// `{"id", "record"}`: the run and its record.
    pub run: Value,
    /// Further keys, declared by the step.
    pub extra: Map<String, Value>,
}

impl ManifestEntry {
    fn fields(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("url".into(), json!(self.url));
        m.insert("rows".into(), json!(self.rows));
        m.insert("bytes".into(), json!(self.bytes));
        m.insert("sha256".into(), json!(self.sha256));
        m.insert("published".into(), json!(self.published));
        m.insert("run".into(), self.run.clone());
        // `extra` never overwrites a core key: those are the step's own facts.
        for (k, v) in &self.extra {
            m.entry(k.clone()).or_insert_with(|| v.clone());
        }
        m
    }
}

/// Apply the entries and pinned catalogue address `current` returns to the manifest
/// at `key`, and write it back with `IfMatch` only when it changed. Returns whether
/// it was written.
///
/// `current` is called on every attempt, after the manifest is read and before it is
/// written: it returns only the entries whose table no newer snapshot has changed,
/// and the pinned address only while the live catalogue is still the one this
/// publish wrote or verified. Re-asking each time keeps a refused retry from
/// re-applying stale entries over a newer publish's. A manifest that is not a JSON
/// object with a `datasets` object is an error: it is never rebuilt from empty.
pub fn update_manifest(
    store: &dyn ObjectStore,
    key: &str,
    current: &dyn Fn() -> Result<(Vec<ManifestEntry>, Option<String>)>,
    max_attempts: u32,
) -> Result<bool> {
    let max_attempts = max_attempts.max(1);
    let mut attempts = 0;
    let mut throttled = false;
    loop {
        attempts += 1;
        let found = store
            .get(key)
            .map_err(|e| failed(format!("read the manifest `{key}`: {e}")))?;
        let (mut doc, cond) = match &found {
            Some((bytes, etag)) => (parse(key, bytes)?, PutCondition::IfMatch(etag.clone())),
            None => (json!({"datasets": {}}), PutCondition::IfAbsent),
        };
        let (entries, pinned_catalog) = current()?;
        let before = doc.clone();
        apply(&mut doc, &entries, pinned_catalog.as_deref());
        if found.is_some() && doc == before {
            return Ok(false);
        }
        let mut bytes = serde_json::to_vec_pretty(&doc)
            .map_err(|e| failed(format!("serialise the manifest: {e}")))?;
        bytes.push(b'\n');
        match store.put(key, &bytes, cond, Some("public, max-age=60")) {
            Ok(_) => return Ok(true),
            Err(StoreError::PreconditionFailed) => {}
            Err(StoreError::Throttled) => throttled = true,
            Err(e) => return Err(failed(format!("write the manifest `{key}`: {e}"))),
        }
        if attempts >= max_attempts {
            let why = if throttled {
                "another writer won, or the store throttled"
            } else {
                "another writer won each time"
            };
            return Err(failed(format!(
                "could not write the manifest `{key}` after {attempts} attempts ({why})"
            )));
        }
        std::thread::sleep(Duration::from_millis(250 * u64::from(attempts)));
    }
}

fn parse(key: &str, bytes: &[u8]) -> Result<Value> {
    let doc: Value = serde_json::from_slice(bytes)
        .map_err(|e| failed(format!("the manifest `{key}` is not JSON: {e}")))?;
    if !doc.get("datasets").is_some_and(Value::is_object) {
        return Err(failed(format!(
            "the manifest `{key}` has no `datasets` object; not rewriting it"
        )));
    }
    Ok(doc)
}

fn apply(doc: &mut Value, entries: &[ManifestEntry], pinned_catalog: Option<&str>) {
    for entry in entries {
        let datasets = doc["datasets"].as_object_mut().expect("checked by parse");
        let slot = datasets
            .entry(entry.address.clone())
            .or_insert_with(|| json!({}));
        if !slot.is_object() {
            *slot = json!({});
        }
        let target = slot.as_object_mut().expect("just made an object");
        for (k, v) in entry.fields() {
            target.insert(k, v);
        }
    }
    if let Some(url) = pinned_catalog {
        if !doc.get("catalog").is_some_and(Value::is_object) {
            doc["catalog"] = json!({});
        }
        doc["catalog"]["url"] = json!(url);
    }
}

/// A store that runs a hook once, just before the first manifest write, and then
/// refuses that write as a lost race would.
#[cfg(test)]
pub(crate) struct BeforeFirstPut<'a> {
    inner: &'a dyn ObjectStore,
    hook: std::sync::Mutex<Option<Box<dyn FnOnce() + Send + 'a>>>,
}

#[cfg(test)]
impl<'a> BeforeFirstPut<'a> {
    pub(crate) fn new(inner: &'a dyn ObjectStore, hook: impl FnOnce() + Send + 'a) -> Self {
        Self {
            inner,
            hook: std::sync::Mutex::new(Some(Box::new(hook))),
        }
    }
}

#[cfg(test)]
impl ObjectStore for BeforeFirstPut<'_> {
    fn get(
        &self,
        key: &str,
    ) -> std::result::Result<Option<(Vec<u8>, super::store::ETag)>, StoreError> {
        self.inner.get(key)
    }
    fn head(&self, key: &str) -> std::result::Result<Option<super::store::ObjectHead>, StoreError> {
        self.inner.head(key)
    }
    fn put(
        &self,
        key: &str,
        bytes: &[u8],
        cond: PutCondition,
        cache_control: Option<&str>,
    ) -> std::result::Result<super::store::ETag, StoreError> {
        // The hook's own writes go to `inner`, never back through here.
        if let Some(hook) = self.hook.lock().unwrap().take() {
            hook();
        }
        self.inner.put(key, bytes, cond, cache_control)
    }
    fn put_file(
        &self,
        key: &str,
        path: &std::path::Path,
        cond: PutCondition,
        cache_control: Option<&str>,
    ) -> std::result::Result<super::store::ETag, StoreError> {
        self.inner.put_file(key, path, cond, cache_control)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lake::store::DirStore;
    use tempfile::TempDir;

    const KEY: &str = "manifest.json";

    fn entry(address: &str, sha: &str) -> ManifestEntry {
        let mut extra = Map::new();
        extra.insert("searchRows".into(), json!(7));
        ManifestEntry {
            address: address.to_string(),
            url: format!("https://example.test/{address}.parquet"),
            rows: 3,
            bytes: 100,
            sha256: sha.to_string(),
            published: "2026-11-13T00:00:00Z".to_string(),
            run: json!({"id": "r1", "record": "runs/r1.json"}),
            extra,
        }
    }

    fn read(store: &DirStore) -> Value {
        serde_json::from_slice(&store.get(KEY).unwrap().unwrap().0).unwrap()
    }

    #[test]
    fn an_entry_is_written_and_other_entries_are_kept() {
        let dir = TempDir::new().unwrap();
        let store = DirStore::new(dir.path());
        store
            .put(
                KEY,
                br#"{"datasets":{"a/b":{"url":"old"}},"other":1}"#,
                PutCondition::Any,
                None,
            )
            .unwrap();
        let written = update_manifest(
            &store,
            KEY,
            &|| {
                Ok((
                    vec![entry("c/d", "abc")],
                    Some("catalog/open-1.ducklake".to_string()),
                ))
            },
            3,
        )
        .unwrap();
        assert!(written);
        let doc = read(&store);
        assert_eq!(doc["datasets"]["a/b"]["url"], "old");
        assert_eq!(doc["datasets"]["c/d"]["sha256"], "abc");
        assert_eq!(doc["datasets"]["c/d"]["searchRows"], 7);
        assert_eq!(doc["datasets"]["c/d"]["run"]["id"], "r1");
        assert_eq!(doc["catalog"]["url"], "catalog/open-1.ducklake");
        assert_eq!(doc["other"], 1);
    }

    #[test]
    fn an_absent_manifest_is_created() {
        let dir = TempDir::new().unwrap();
        let store = DirStore::new(dir.path());
        assert!(
            update_manifest(&store, KEY, &|| Ok((vec![entry("c/d", "abc")], None)), 3).unwrap()
        );
        assert_eq!(read(&store)["datasets"]["c/d"]["rows"], 3);
    }

    #[test]
    fn a_manifest_that_is_not_json_is_refused() {
        let dir = TempDir::new().unwrap();
        let store = DirStore::new(dir.path());
        store
            .put(KEY, b"not json", PutCondition::Any, None)
            .unwrap();
        let err = update_manifest(&store, KEY, &|| Ok((vec![entry("c/d", "abc")], None)), 3)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not JSON"), "{err}");
        assert_eq!(store.get(KEY).unwrap().unwrap().0, b"not json");

        store.put(KEY, b"{}", PutCondition::Any, None).unwrap();
        let err = update_manifest(&store, KEY, &|| Ok((vec![entry("c/d", "abc")], None)), 3)
            .unwrap_err()
            .to_string();
        assert!(err.contains("datasets"), "{err}");
    }

    #[test]
    fn an_identical_manifest_is_not_rewritten() {
        let dir = TempDir::new().unwrap();
        let store = DirStore::new(dir.path());
        let cur = || Ok((vec![entry("c/d", "abc")], Some("p".to_string())));
        assert!(update_manifest(&store, KEY, &cur, 3).unwrap());
        let etag = store.etag(KEY).unwrap();
        assert!(!update_manifest(&store, KEY, &cur, 3).unwrap());
        assert_eq!(store.etag(KEY).unwrap(), etag);
    }

    #[test]
    fn a_key_the_step_does_not_set_survives_inside_the_entry() {
        let dir = TempDir::new().unwrap();
        let store = DirStore::new(dir.path());
        store
            .put(
                KEY,
                br#"{"datasets":{"c/d":{"url":"old","keep":"me"}}}"#,
                PutCondition::Any,
                None,
            )
            .unwrap();
        update_manifest(&store, KEY, &|| Ok((vec![entry("c/d", "abc")], None)), 3).unwrap();
        let doc = read(&store);
        assert_eq!(doc["datasets"]["c/d"]["keep"], "me");
        assert_eq!(doc["datasets"]["c/d"]["sha256"], "abc");
    }

    #[test]
    fn extra_cannot_overwrite_the_core_keys() {
        let dir = TempDir::new().unwrap();
        let store = DirStore::new(dir.path());
        let mut e = entry("c/d", "abc");
        e.extra.insert("sha256".into(), json!("evil"));
        e.extra.insert("url".into(), json!("evil"));
        update_manifest(&store, KEY, &|| Ok((vec![e.clone()], None)), 3).unwrap();
        let doc = read(&store);
        assert_eq!(doc["datasets"]["c/d"]["sha256"], "abc");
        assert_eq!(
            doc["datasets"]["c/d"]["url"],
            "https://example.test/c/d.parquet"
        );
        assert_eq!(doc["datasets"]["c/d"]["searchRows"], 7);
    }

    #[test]
    fn a_refused_first_write_retries_and_keeps_what_the_winner_wrote() {
        let dir = TempDir::new().unwrap();
        let inner = DirStore::new(dir.path());
        // Absent at the read; another writer creates it before ours lands, so the
        // first write (IfAbsent) is refused and the retry writes IfMatch.
        let store = BeforeFirstPut::new(&inner, || {
            inner
                .put(
                    KEY,
                    br#"{"datasets":{"a/b":{"url":"theirs"}}}"#,
                    PutCondition::Any,
                    None,
                )
                .unwrap();
        });
        let calls = std::cell::Cell::new(0);
        let cur = || {
            calls.set(calls.get() + 1);
            Ok((vec![entry("c/d", "abc")], None))
        };
        assert!(update_manifest(&store, KEY, &cur, 3).unwrap());
        assert_eq!(calls.get(), 2, "entries are recomputed on every attempt");
        let doc = read(&inner);
        assert_eq!(doc["datasets"]["a/b"]["url"], "theirs");
        assert_eq!(doc["datasets"]["c/d"]["sha256"], "abc");
    }
}
