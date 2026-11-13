//! Publishing a Protocol's tables into a DuckLake catalogue held on S3-compatible
//! object storage (Cloudflare R2). Compiled under `http-fetch`, like `http_fetch`.

use std::sync::{Arc, Mutex};

pub(crate) mod store;

pub(crate) mod meta;

pub(crate) mod commit;

pub(crate) mod manifest;

pub(crate) mod publish;

#[cfg(test)]
mod test_server;

/// A run record a live publish asked to have uploaded once the run ends.
pub(crate) struct RunRecordUpload {
    pub store: Arc<dyn ObjectStore>,
    /// The record's key, `runs/<run_id>.json`.
    pub key: String,
    /// The snapshot the publish committed or found, named if the upload fails.
    pub snapshot_id: i64,
}

use store::ObjectStore;

/// The uploads queued during this process's run. A record is complete only when the
/// run ends, after the step that publishes, so the step queues it and the runner
/// uploads it after writing the record.
static RUN_RECORD_UPLOADS: Mutex<Vec<RunRecordUpload>> = Mutex::new(Vec::new());

/// Queue the finished run record for upload to `key` in `store`.
pub(crate) fn queue_run_record_upload(store: Arc<dyn ObjectStore>, key: String, snapshot_id: i64) {
    RUN_RECORD_UPLOADS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(RunRecordUpload {
            store,
            key,
            snapshot_id,
        });
}

/// Every queued upload, leaving the queue empty.
pub(crate) fn take_run_record_uploads() -> Vec<RunRecordUpload> {
    std::mem::take(
        &mut *RUN_RECORD_UPLOADS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
}
