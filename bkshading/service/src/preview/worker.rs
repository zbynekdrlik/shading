//! The per-camera preview loop — the runtime glue that drives a [`PreviewSource`] into the
//! [`PreviewStore`].
//!
//! Each configured camera with an NDI preview name gets ONE OS thread (not a tokio task:
//! the NDI capture is a blocking FFI call, so it stays off the async runtime). The loop is:
//! build the source → capture → decimate → encode → store, forever; on a source error it
//! logs, backs off, and rebuilds (a cambox feed that comes and goes self-heals). The pure
//! decision logic it composes (decimate, encode) is unit-tested separately.

use std::thread;
use std::time::{Duration, Instant};

use crate::preview::decimate::Decimator;
use crate::preview::encode::encode_jpeg;
use crate::preview::source::PreviewSource;
use crate::preview::store::{wall_clock_ms, PreviewStore};
use crate::preview::PreviewConfig;

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

/// Drive one connected source into the store until it fails. Public for the worker tests.
pub fn run_source(
    cam_id: &str,
    src: &mut dyn PreviewSource,
    cfg: &PreviewConfig,
    store: &PreviewStore,
    decim: &mut Decimator,
) {
    let timeout = Duration::from_millis(cfg.capture_timeout_ms);
    // Decimation runs on a MONOTONIC clock (immune to a wall-clock / NTP backward step, which
    // would otherwise pause emission until wall time caught up); the store's `updated_ms` stays
    // wall clock, the clock the readers judge freshness against (issue 808).
    let started = Instant::now();
    // This connection's clock starts at 0: a slot stamped on the previous connection's clock
    // would freeze the preview for as long as that one ran (shading#3).
    decim.reset();
    let mut last_encode_error: Option<String> = None;
    loop {
        // The source asks at frame arrival, so a frame the decimator drops is never built
        // (shading#3) and the kept cadence does not drift. The slot is spent by an encode attempt:
        // a kept frame the source could not build leaves it for the next one.
        let mut arrived_ms: Option<u64> = None;
        let mut keep = || {
            let now = started.elapsed().as_millis() as u64;
            arrived_ms = Some(now);
            decim.is_due(now)
        };
        let got = src.next_frame(timeout, &mut keep);
        match got {
            Ok(Some(frame)) => {
                match encode_jpeg(&frame, cfg.jpeg_quality) {
                    Ok(jpeg) => {
                        store.put(cam_id, jpeg, wall_clock_ms());
                        last_encode_error = None;
                    }
                    Err(e) => {
                        // Once per distinct error: a frame shape that fails every time must not
                        // log at the feed rate.
                        let text = format!("{e:#}");
                        if last_encode_error.as_deref() != Some(text.as_str()) {
                            tracing::warn!(cam = %cam_id, error = %text, "preview jpeg encode failed");
                            last_encode_error = Some(text);
                        }
                    }
                }
                // An encode attempt spends the slot whatever its result, so a frame that fails
                // every time is tried once per period, never at the feed rate.
                debug_assert!(
                    arrived_ms.is_some(),
                    "a source returned a frame without asking keep"
                );
                if let Some(at) = arrived_ms {
                    decim.mark_emitted(at);
                }
            }
            Ok(None) => { /* timeout, an event, or a frame thinned to the target fps */ }
            Err(e) => {
                tracing::warn!(cam = %cam_id, error = %e, "preview capture error; rebuilding source");
                return;
            }
        }
    }
}
