//! Live camera preview (issue 808, M2).
//!
//! Owner architecture (2026-08-20): the cambox publishes ONE NDI stream (consumed by strih
//! OBS AND this service); for the operator preview the service subscribes to the NDI
//! LOW-quality variant, thins it to a few fps, JPEG-encodes it, and serves the latest frame
//! over the existing HTTP surface into the web UI's 4+4 preview blocks. NDI→web reuses the
//! minimal receive pattern the presenter project and the appliance's own `src/ndi.rs` use.
//!
//! Layered so the pixel/decision logic is pure and CI-testable without libndi:
//! - pure core: [`frame`], [`pattern`], [`decimate`], [`encode`], [`convert`], [`store`],
//!   [`ndi_paths`] (cross-platform runtime discovery — the ordered candidate paths),
//!   [`shared_runtime`] (process-shared load-once runtime keeper — reconnect-safe lifecycle)
//! - runtime glue: [`source`] (trait + stub), [`worker`] (one OS thread per camera)
//! - `#[cfg(feature = "ndi")]` [`ndi_source`]: the real libndi receiver at bandwidth LOWEST
//!   (mirrors `src/ndi.rs`), OFF by default and UNVERIFIED against a live source in this lane.

pub mod convert;
pub mod decimate;
pub mod encode;
pub mod frame;
pub mod ndi_paths;
pub mod pattern;
pub mod shared_runtime;
pub mod source;
pub mod store;
pub mod worker;

#[cfg(feature = "ndi")]
pub mod ndi_source;

use serde::Deserialize;

use crate::config::ServiceConfig;
use crate::preview::store::PreviewStore;

fn default_fps() -> f64 {
    3.0
}
fn default_jpeg_quality() -> u8 {
    55
}
fn default_capture_timeout_ms() -> u64 {
    1000
}
fn default_reconnect_backoff_ms() -> u64 {
    2000
}

/// Preview tuning (the `[preview]` config table). All fields have sensible defaults, so the
/// table is optional.
#[derive(Debug, Clone, Deserialize)]
pub struct PreviewConfig {
    /// Target preview frame rate (frames/sec). ~2–5 is plenty for shading (colour, not motion).
    #[serde(default = "default_fps")]
    pub fps: f64,
    /// JPEG quality 0–100 (lower = smaller frames; a preview needs no archival quality).
    #[serde(default = "default_jpeg_quality")]
    pub jpeg_quality: u8,
    /// How long one capture call blocks waiting for a frame before looping.
    #[serde(default = "default_capture_timeout_ms")]
    pub capture_timeout_ms: u64,
    /// Backoff before rebuilding a source that failed / ended.
    #[serde(default = "default_reconnect_backoff_ms")]
    pub reconnect_backoff_ms: u64,
}

impl Default for PreviewConfig {
    fn default() -> Self {
        Self {
            fps: default_fps(),
            jpeg_quality: default_jpeg_quality(),
            capture_timeout_ms: default_capture_timeout_ms(),
            reconnect_backoff_ms: default_reconnect_backoff_ms(),
        }
    }
}

/// Spawn a preview worker for every camera that has an `ndi_preview` source name, returning
/// the shared store the HTTP layer reads. A camera without a preview name (a handheld with no
/// feed) gets no worker — its block stays params-only.
pub fn start_all(config: &ServiceConfig) -> PreviewStore {
    let store = PreviewStore::new();
    for cam in &config.cameras {
        if let Some(name) = &cam.ndi_preview {
            worker::spawn_preview(
                cam.id.clone(),
                name.clone(),
                config.preview.clone(),
                store.clone(),
                source::build_default_source,
            );
        }
    }
    store
}
