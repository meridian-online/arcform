//! Publishing a Protocol's tables into a DuckLake catalogue held on S3-compatible
//! object storage (Cloudflare R2). Compiled under `http-fetch`, like `http_fetch`.

use std::cell::RefCell;
use std::sync::Arc;

pub(crate) mod store;

pub(crate) mod meta;

pub(crate) mod commit;

pub(crate) mod manifest;

pub(crate) mod publish;

pub(crate) mod rebase;

#[cfg(test)]
mod test_server;

/// A run record a live publish asked to have uploaded once the run ends.
pub(crate) struct RunRecordUpload {
    pub store: Arc<dyn ObjectStore>,
    /// The record's key, `<runs><run_id>.json`: `runs` is the configured prefix
    /// (default `runs/`), and a manifest entry's `run.record` is `public_base` plus this.
    pub key: String,
    /// The snapshot the publish committed or found, named if the upload fails.
    pub snapshot_id: i64,
}

use store::ObjectStore;

thread_local! {
    /// The uploads queued during the run on this thread. A record is complete only
    /// when the run ends, after the step that publishes, so the step queues it and
    /// the runner uploads it after writing the record. Per thread, because a run
    /// calls its steps on its own thread: two runs in one process (or two tests)
    /// never take each other's uploads.
    static RUN_RECORD_UPLOADS: RefCell<Vec<RunRecordUpload>> = const { RefCell::new(Vec::new()) };
}

/// Queue the finished run record for upload to `key` in `store`.
pub(crate) fn queue_run_record_upload(store: Arc<dyn ObjectStore>, key: String, snapshot_id: i64) {
    RUN_RECORD_UPLOADS.with_borrow_mut(|queue| {
        queue.push(RunRecordUpload {
            store,
            key,
            snapshot_id,
        })
    });
}

/// Every upload queued on this thread, leaving the queue empty.
pub(crate) fn take_run_record_uploads() -> Vec<RunRecordUpload> {
    RUN_RECORD_UPLOADS.with_borrow_mut(std::mem::take)
}
