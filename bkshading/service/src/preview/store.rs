//! Per-camera latest-JPEG store.
//!
//! Each camera's preview worker `put`s its newest encoded frame here; the HTTP handler
//! reads it for `GET /api/cameras/:id/preview.jpg`. Only the LATEST frame per camera is
//! kept (a preview is always "now", never a backlog). The mutex is held only for the
//! insert/clone, and a poisoned lock is recovered rather than panicking (a worker that
//! panicked mid-write must not take the whole HTTP surface down).
//!
//! Freshness (issue 808): nobody clears a frame when its NDI feed stops (the worker just
//! reconnects), so the readers that face the operator ask for a FRESH frame only
//! ([`PreviewStore::get_fresh`] / [`PreviewStore::is_live`], bounded by
//! [`crate::preview::PreviewConfig::max_frame_age_ms`]). A stopped feed then reads as "no
//! picture" instead of its last frame frozen as if live. Staleness is a READ decision: the
//! stored frame stays for diagnostics.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Wall-clock milliseconds since the Unix epoch: the clock `updated_ms` is stamped with and that
/// freshness is judged against. A clock read before the epoch reads 0. A backward step after a
/// put reads as age 0 (live), never as an underflow, and a forward step only shortens the frame's
/// remaining life, which the next frame (every ~333 ms at 3 fps) resets.
pub fn wall_clock_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One stored, JPEG-encoded preview frame.
#[derive(Clone)]
pub struct PreviewFrame {
    /// JPEG bytes (shared so the HTTP handler clones an `Arc`, not the buffer, under the lock).
    pub jpeg: Arc<Vec<u8>>,
    /// Monotonically increasing per camera — a cache-busting sequence for the web UI.
    pub seq: u64,
    /// Wall-clock ms when stored ([`wall_clock_ms`]); freshness is judged against it.
    pub updated_ms: u64,
}

impl PreviewFrame {
    /// Is this frame at most `max_age_ms` old at `now_ms`? A `now_ms` before `updated_ms` (a
    /// wall clock stepped back) is age 0, so fresh. Pure.
    pub fn is_fresh(&self, now_ms: u64, max_age_ms: u64) -> bool {
        now_ms.saturating_sub(self.updated_ms) <= max_age_ms
    }
}

/// Cheap-to-clone handle to the shared per-camera preview map.
///
/// shading#19: it also holds when a viewer was last seen (a `preview.jpg` fetch, or a visible
/// panel's `POST /api/preview/viewer`). A store made by [`PreviewStore::on_demand`] wants frames
/// only within its demand window after that; the workers drop their NDI receivers outside it.
/// One stamp for the whole service: a visible panel shows every camera of its venue.
#[derive(Clone, Default)]
pub struct PreviewStore {
    inner: Arc<Mutex<HashMap<String, PreviewFrame>>>,
    /// The demand window in ms; `None` = always wanted (tools and tests).
    demand_ms: Option<u64>,
    /// Wall-clock ms of the last viewer; 0 = none yet.
    viewer_ms: Arc<AtomicU64>,
}

impl PreviewStore {
    /// A store that always wants frames (no demand window).
    pub fn new() -> Self {
        Self::default()
    }

    /// A store that wants frames only within `window_ms` after the last viewer (shading#19).
    pub fn on_demand(window_ms: u64) -> Self {
        Self {
            demand_ms: Some(window_ms),
            ..Self::default()
        }
    }

    /// A viewer was seen at `now_ms` (a preview fetch, or the panel's viewer call).
    pub fn viewer_seen(&self, now_ms: u64) {
        // The newest call wins (a clock stepped back is followed, not ignored); at least 1, since 0
        // means "nobody yet".
        self.viewer_ms.store(now_ms.max(1), Ordering::Relaxed);
    }

    /// Does anyone watch at `now_ms`: a viewer seen at most the demand window ago? A store without
    /// a window always wants frames. A clock stepped back below the stamp reads as a viewer just
    /// seen (the stamp moves to the new time, so the window is never stretched by the step), never
    /// as an underflow.
    pub fn wanted(&self, now_ms: u64) -> bool {
        let Some(window) = self.demand_ms else {
            return true;
        };
        let seen = self.viewer_ms.load(Ordering::Relaxed);
        if seen == 0 {
            return false;
        }
        if now_ms < seen {
            // The wall clock stepped back: the window restarts at the new time instead of
            // holding the receivers for the size of the step (review, shading#19).
            let _ = self.viewer_ms.compare_exchange(
                seen,
                now_ms.max(1),
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
            return true;
        }
        now_ms - seen <= window
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

    /// The latest frame for `cam_id`, or `None` if none has been produced yet. Any age: a frame
    /// shown to the operator goes through [`Self::get_fresh`] instead.
    pub fn get(&self, cam_id: &str) -> Option<PreviewFrame> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.get(cam_id).cloned()
    }

    /// The latest frame for `cam_id` only while it is at most `max_age_ms` old at `now_ms`
    /// ([`PreviewFrame::is_fresh`]); `None` when there is no frame or it is stale (the feed
    /// stopped). What the preview endpoint serves.
    pub fn get_fresh(&self, cam_id: &str, now_ms: u64, max_age_ms: u64) -> Option<PreviewFrame> {
        self.get(cam_id).filter(|f| f.is_fresh(now_ms, max_age_ms))
    }

    /// Does `cam_id` have a fresh frame at `now_ms`? The `previewLive` flag of the camera view
    /// (issue 808).
    pub fn is_live(&self, cam_id: &str, now_ms: u64, max_age_ms: u64) -> bool {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .get(cam_id)
            .is_some_and(|f| f.is_fresh(now_ms, max_age_ms))
    }
}
