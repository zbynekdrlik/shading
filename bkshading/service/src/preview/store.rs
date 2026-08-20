//! Per-camera latest-JPEG store.
//!
//! Each camera's preview worker `put`s its newest encoded frame here; the HTTP handler
//! `get`s it for `GET /api/cameras/:id/preview.jpg`. Only the LATEST frame per camera is
//! kept (a preview is always "now", never a backlog). The mutex is held only for the
//! insert/clone, and a poisoned lock is recovered rather than panicking (a worker that
//! panicked mid-write must not take the whole HTTP surface down).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// One stored, JPEG-encoded preview frame.
#[derive(Clone)]
pub struct PreviewFrame {
    /// JPEG bytes (shared so the HTTP handler clones an `Arc`, not the buffer, under the lock).
    pub jpeg: Arc<Vec<u8>>,
    /// Monotonically increasing per camera — a cache-busting sequence for the web UI.
    pub seq: u64,
    /// Wall-clock ms when stored (diagnostics / staleness).
    pub updated_ms: u64,
}

/// Cheap-to-clone handle to the shared per-camera preview map.
#[derive(Clone, Default)]
pub struct PreviewStore {
    inner: Arc<Mutex<HashMap<String, PreviewFrame>>>,
}

impl PreviewStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace `cam_id`'s latest frame, bumping its sequence number.
    pub fn put(&self, cam_id: &str, jpeg: Vec<u8>, now_ms: u64) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let seq = guard
            .get(cam_id)
            .map(|f| f.seq.wrapping_add(1))
            .unwrap_or(0);
        guard.insert(
            cam_id.to_string(),
            PreviewFrame {
                jpeg: Arc::new(jpeg),
                seq,
                updated_ms: now_ms,
            },
        );
    }

    /// The latest frame for `cam_id`, or `None` if none has been produced yet.
    pub fn get(&self, cam_id: &str) -> Option<PreviewFrame> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.get(cam_id).cloned()
    }
}
