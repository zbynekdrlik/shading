//! The per-camera preview loop — the runtime glue that drives a [`PreviewSource`] into the
//! [`PreviewStore`].
//!
//! Each configured camera with an NDI preview name gets ONE OS thread (not a tokio task:
//! the NDI capture is a blocking FFI call, so it stays off the async runtime). The loop is:
//! build the source → capture → decimate → encode → store, forever; on a source error it
//! logs, backs off, and rebuilds (a cambox feed that comes and goes self-heals). The pure
//! decision logic it composes (decimate, encode) is unit-tested separately.

use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::preview::decimate::Decimator;
use crate::preview::encode::encode_jpeg;
use crate::preview::source::PreviewSource;
use crate::preview::store::PreviewStore;
use crate::preview::PreviewConfig;

/// Wall-clock milliseconds (monotonic-enough for decimation and staleness).
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A source builder: NDI-source-name → a boxed [`PreviewSource`] (or an error to retry).
pub type SourceBuilder = fn(&str) -> anyhow::Result<Box<dyn PreviewSource>>;

/// Spawn the preview loop for one camera on its own OS thread. `build` selects the source
/// (stub by default, real NDI with `--features ndi`) and is retried on failure.
pub fn spawn_preview(
    cam_id: String,
    source_name: String,
    cfg: PreviewConfig,
    store: PreviewStore,
    build: SourceBuilder,
) {
    let log_name = source_name.clone();
    let spawned = thread::Builder::new()
        .name(format!("preview-{cam_id}"))
        .spawn(move || run_forever(&cam_id, &source_name, &cfg, &store, build));
    if let Err(e) = spawned {
        // A thread that cannot even start is logged, not panicked — one camera's preview
        // failing must never take the service down.
        tracing::error!(source = %log_name, error = %e, "failed to spawn preview thread");
    }
}

fn run_forever(
    cam_id: &str,
    source_name: &str,
    cfg: &PreviewConfig,
    store: &PreviewStore,
    build: SourceBuilder,
) {
    let mut decim = Decimator::new(cfg.fps);
    loop {
        match build(source_name) {
            Ok(mut src) => {
                tracing::info!(cam = %cam_id, source = %source_name, "preview source connected");
                run_source(cam_id, src.as_mut(), cfg, store, &mut decim);
                tracing::warn!(cam = %cam_id, "preview source ended; reconnecting");
            }
            Err(e) => {
                tracing::warn!(cam = %cam_id, source = %source_name, error = %e, "preview source build failed; retrying");
            }
        }
        thread::sleep(Duration::from_millis(cfg.reconnect_backoff_ms));
    }
}

fn run_source(
    cam_id: &str,
    src: &mut dyn PreviewSource,
    cfg: &PreviewConfig,
    store: &PreviewStore,
    decim: &mut Decimator,
) {
    let timeout = Duration::from_millis(cfg.capture_timeout_ms);
    // Decimation runs on a MONOTONIC clock (immune to a wall-clock / NTP backward step, which
    // would otherwise pause emission until wall time caught up); the store's `updated_ms` stays
    // wall-clock for diagnostics.
    let started = Instant::now();
    loop {
        match src.next_frame(timeout) {
            Ok(Some(frame)) => {
                if !decim.should_emit(started.elapsed().as_millis() as u64) {
                    continue; // thinned to the target fps
                }
                match encode_jpeg(&frame, cfg.jpeg_quality) {
                    Ok(jpeg) => store.put(cam_id, jpeg, now_ms()),
                    Err(e) => {
                        tracing::warn!(cam = %cam_id, error = %e, "preview jpeg encode failed")
                    }
                }
            }
            Ok(None) => { /* timeout — keep waiting for the next frame */ }
            Err(e) => {
                tracing::warn!(cam = %cam_id, error = %e, "preview capture error; rebuilding source");
                return;
            }
        }
    }
}
