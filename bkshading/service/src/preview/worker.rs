//! The per-camera preview loop — the runtime glue that drives a [`PreviewSource`] into the
//! [`PreviewStore`].
//!
//! Each configured camera with an NDI preview name gets ONE OS thread (not a tokio task:
//! the NDI capture is a blocking FFI call, so it stays off the async runtime). The loop is:
//! build the source → capture → decimate → encode → store, forever; on a source error it
//! logs, backs off, and rebuilds (a cambox feed that comes and goes self-heals). The pure
//! decision logic it composes (decimate, encode) is unit-tested separately.
//!
//! shading#19: the receiver runs only while someone watches ([`PreviewStore::wanted`]). With no
//! viewer a cycle builds no source, and a running session ends `parked` the moment the viewer
//! leaves; the worker then polls the demand every [`PREVIEW_DEMAND_POLL_MS`]. Parking is logged at
//! info once per transition, never as a failure.

use std::thread;
use std::time::{Duration, Instant};

use crate::preview::decimate::Decimator;
use crate::preview::encode::encode_jpeg;
use crate::preview::source::PreviewSource;
use crate::preview::store::{wall_clock_ms, PreviewStore};
use crate::preview::{PreviewConfig, PREVIEW_DEMAND_POLL_MS};

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
    // One gate for every reason this camera's preview fails (build error, capture error), kept
    // across reconnects and cleared only when a frame gets through: a camera that stays off, or
    // a source that is found and then fails every time, warns once per streak, not every cycle,
    // forever (shading#3).
    let mut trouble = RepeatGate::default();
    let mut was_parked = false;
    loop {
        let log = cycle(
            cam_id,
            source_name,
            cfg,
            store,
            build,
            &mut decim,
            &mut trouble,
        );
        if log.parked != was_parked {
            if log.parked {
                tracing::info!(cam = %cam_id, source = %source_name, "preview parked: no viewer, NDI receiver disconnected");
            } else {
                tracing::info!(cam = %cam_id, source = %source_name, "preview resumed: a viewer is back");
            }
            was_parked = log.parked;
        }
        if log.parked {
            thread::sleep(Duration::from_millis(PREVIEW_DEMAND_POLL_MS));
        } else {
            thread::sleep(cfg.reconnect_backoff());
        }
    }
}

/// What one [`cycle`] logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycleLog {
    /// It warned (a failure new in this streak).
    pub warned: bool,
    /// The source stored frames after a warned streak.
    pub recovered: bool,
    /// Nobody watches: no source was built, or the session ended because the viewer left
    /// (shading#19). Not a failure.
    pub parked: bool,
}

/// One turn of the reconnect loop: build the source, run it until it fails, and log the
/// failure through the camera's `trouble` gate. With no viewer it builds nothing and returns
/// `parked` (shading#19). Public for the worker tests.
pub fn cycle(
    cam_id: &str,
    source_name: &str,
    cfg: &PreviewConfig,
    store: &PreviewStore,
    build: SourceBuilder,
    decim: &mut Decimator,
    trouble: &mut RepeatGate,
) -> CycleLog {
    if !store.wanted(wall_clock_ms()) {
        tracing::trace!(cam = %cam_id, "preview: no viewer, no receiver");
        return CycleLog {
            warned: false,
            recovered: false,
            parked: true,
        };
    }
    match build(source_name) {
        Ok(mut src) => {
            let was_failing = trouble.in_streak();
            if was_failing {
                tracing::info!(cam = %cam_id, source = %source_name, "preview source connected again");
            } else {
                tracing::debug!(cam = %cam_id, source = %source_name, "preview source connected");
            }
            let end = run_source(cam_id, src.as_mut(), cfg, store, decim, trouble);
            let recovered = end.stored && was_failing;
            if end.parked {
                // The viewer left: the source is dropped (disconnected) when `src` goes out of
                // scope; nothing failed.
                if end.stored {
                    trouble.clear();
                }
                tracing::debug!(cam = %cam_id, source = %source_name, "preview session parked: the viewer left");
                return CycleLog {
                    warned: false,
                    recovered,
                    parked: true,
                };
            }
            if end.stored {
                tracing::info!(cam = %cam_id, source = %source_name, "preview source delivered frames, then ended");
                trouble.clear();
            }
            let warned = trouble.first(&end.error);
            if warned {
                tracing::warn!(cam = %cam_id, error = %end.error, "preview capture error; rebuilding source (repeats log at debug)");
            } else {
                tracing::debug!(cam = %cam_id, error = %end.error, "preview capture error again; rebuilding source");
            }
            CycleLog {
                warned,
                recovered,
                parked: false,
            }
        }
        Err(e) => {
            let text = format!("{e:#}");
            let warned = trouble.first(&text);
            if warned {
                tracing::warn!(cam = %cam_id, source = %source_name, error = %text, "preview source build failed; retrying (repeats log at debug)");
            } else {
                tracing::debug!(cam = %cam_id, source = %source_name, error = %text, "preview source build failed again");
            }
            CycleLog {
                warned,
                recovered: false,
                parked: false,
            }
        }
    }
}

/// Lets each distinct error text through once per failure streak, so a failure that repeats
/// at a fixed rate, or two that alternate, warn once and the repeats log at debug (shading#3).
#[derive(Debug, Default)]
pub struct RepeatGate {
    seen: std::collections::HashSet<String>,
}

impl RepeatGate {
    /// True the first time `text` appears in this streak: worth a warning.
    pub fn first(&mut self, text: &str) -> bool {
        self.seen.insert(text.to_string())
    }

    /// Has anything failed since the last [`Self::clear`]?
    pub fn in_streak(&self) -> bool {
        !self.seen.is_empty()
    }

    /// Something worked: the next error starts a new streak.
    pub fn clear(&mut self) {
        self.seen.clear();
    }
}

/// Why one connection's [`run_source`] ended.
#[derive(Debug)]
pub struct SessionEnd {
    /// The source error that ended it.
    pub error: String,
    /// Whether this connection stored at least one frame.
    pub stored: bool,
    /// It ended because nobody watches any more (shading#19), not on a source error.
    pub parked: bool,
}

/// Drive one connected source into the store until it fails, or until nobody watches
/// ([`PreviewStore::wanted`], checked before every capture: `parked`). `trouble` is the camera's
/// warning gate, shared across reconnects: a stored frame clears it. Public for the worker tests.
pub fn run_source(
    cam_id: &str,
    src: &mut dyn PreviewSource,
    cfg: &PreviewConfig,
    store: &PreviewStore,
    decim: &mut Decimator,
    trouble: &mut RepeatGate,
) -> SessionEnd {
    let timeout = cfg.capture_timeout();
    // Decimation runs on a MONOTONIC clock (immune to a wall-clock / NTP backward step, which
    // would otherwise pause emission until wall time caught up); the store's `updated_ms` stays
    // wall clock, the clock the readers judge freshness against (issue 808).
    let started = Instant::now();
    // This connection's clock starts at 0: a slot stamped on the previous connection's clock
    // would freeze the preview for as long as that one ran (shading#3).
    decim.reset();
    let mut stored = false;
    loop {
        if !store.wanted(wall_clock_ms()) {
            return SessionEnd {
                error: "no viewer".into(),
                stored,
                parked: true,
            };
        }
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
                        stored = true;
                        trouble.clear();
                    }
                    Err(e) => {
                        // Once per distinct error: a frame shape that fails every time must not
                        // log at the feed rate.
                        let text = format!("{e:#}");
                        if trouble.first(&text) {
                            tracing::warn!(cam = %cam_id, error = %text, "preview jpeg encode failed");
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
                // run_forever logs it, once per streak.
                return SessionEnd {
                    error: format!("{e:#}"),
                    stored,
                    parked: false,
                };
            }
        }
    }
}
