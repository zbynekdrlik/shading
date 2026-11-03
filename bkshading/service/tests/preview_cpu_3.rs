//! shading#3: the preview worker must not pay for frames it then throws away.
//!
//! An NDI feed arrives at 60 fps and the preview keeps 3 fps. v0.1.0 copied and converted every
//! frame to RGB before the decimator dropped it, which burned about one CPU core on strih-snv
//! with three live cameras. A source now asks the worker's `keep` callback when a frame arrives
//! (the decision is made at arrival, so the 3 fps cadence does not drift), and skips the copy and
//! convert for a frame the worker will not keep.

use std::time::Duration;

use bkshading::preview::decimate::Decimator;
use bkshading::preview::frame::RawFrame;
use bkshading::preview::pattern::test_pattern_rgb;
use bkshading::preview::source::PreviewSource;
use bkshading::preview::store::PreviewStore;
use bkshading::preview::worker::run_source;
use bkshading::preview::PreviewConfig;

/// A feed that delivers a frame on every call, as fast as the worker asks, and ends after
/// `limit` calls. It counts the frames the worker asked it to build.
struct FastFeed {
    calls: u32,
    limit: u32,
    built: u32,
}

impl PreviewSource for FastFeed {
    fn name(&self) -> &str {
        "fast"
    }

    fn next_frame(
        &mut self,
        _timeout: Duration,
        keep: &mut dyn FnMut() -> bool,
    ) -> anyhow::Result<Option<RawFrame>> {
        self.calls += 1;
        // A worker that ignores the error and keeps reading must fail the test, not hang it.
        assert!(
            self.calls <= self.limit + 1,
            "worker kept reading after the feed failed"
        );
        if self.calls > self.limit {
            anyhow::bail!("feed ended");
        }
        if !keep() {
            return Ok(None);
        }
        self.built += 1;
        Ok(Some(RawFrame::new(32, 18, test_pattern_rgb(32, 18, 0))))
    }
}

/// A feed whose first kept frame cannot be built (an unsupported FourCC, null data): it
/// returns `Ok(None)` after `keep()` said yes. It records every `keep()` answer.
struct BadFirstFrame {
    answers: Vec<bool>,
    built: u32,
}

impl PreviewSource for BadFirstFrame {
    fn name(&self) -> &str {
        "bad-first"
    }

    fn next_frame(
        &mut self,
        _timeout: Duration,
        keep: &mut dyn FnMut() -> bool,
    ) -> anyhow::Result<Option<RawFrame>> {
        if self.answers.len() >= 50 {
            anyhow::bail!("feed ended");
        }
        let kept = keep();
        self.answers.push(kept);
        if !kept || self.answers.len() == 1 {
            return Ok(None); // the first kept frame turns out unbuildable
        }
        self.built += 1;
        Ok(Some(RawFrame::new(32, 18, test_pattern_rgb(32, 18, 0))))
    }
}

#[test]
fn an_unbuildable_kept_frame_does_not_spend_the_slot_3() {
    let cfg = PreviewConfig {
        fps: 0.1, // 10 s period: every answer below falls in one slot
        ..PreviewConfig::default()
    };
    let store = PreviewStore::new();
    let mut decim = Decimator::new(cfg.fps);
    let mut feed = BadFirstFrame {
        answers: Vec::new(),
        built: 0,
    };

    run_source("cam1", &mut feed, &cfg, &store, &mut decim);

    assert_eq!(
        &feed.answers[..3],
        &[true, true, false],
        "the next good frame is kept"
    );
    assert_eq!(feed.built, 1);
    assert!(
        store.get("cam1").is_some(),
        "the good frame reached the store"
    );
}

#[test]
fn worker_builds_only_the_frames_it_keeps_3() {
    let cfg = PreviewConfig {
        // A 10 s period: the 600 instant frames below can never straddle two slots, even on a
        // slow, instrumented CI runner.
        fps: 0.1,
        ..PreviewConfig::default()
    };
    let store = PreviewStore::new();
    let mut decim = Decimator::new(cfg.fps);
    let mut feed = FastFeed {
        calls: 0,
        limit: 600,
        built: 0,
    };

    run_source("cam1", &mut feed, &cfg, &store, &mut decim);

    assert_eq!(
        feed.calls, 601,
        "the worker kept reading until the feed ended"
    );
    // 600 frames arrive well inside one 10 s period, so only the first one is kept.
    assert_eq!(
        feed.built, 1,
        "the worker asked for {} frames, keeps 1",
        feed.built
    );
    assert!(
        store.get("cam1").is_some(),
        "the kept frame reached the store"
    );
}

#[test]
fn decimator_says_due_without_using_up_the_slot_3() {
    let mut d = Decimator::new(4.0); // 250 ms
    assert!(d.is_due(0));
    assert!(d.is_due(0), "asking twice does not spend the slot");
    assert!(d.should_emit(0));
    assert!(!d.is_due(100));
    assert!(!d.is_due(249));
    assert!(d.is_due(250));
    assert!(d.should_emit(250));
    assert!(!d.is_due(260));
}

#[cfg(feature = "ndi")]
#[test]
fn ndi_capture_step_builds_only_a_kept_usable_video_frame_3() {
    use bkshading::preview::ndi_source::{capture_step, CaptureStep};
    let mut asked = 0;
    let mut yes = || {
        asked += 1;
        true
    };
    assert_eq!(capture_step(1, true, &mut yes), CaptureStep::Build);
    let mut no = || false;
    // An unwanted video frame is freed untouched: no copy, no convert.
    assert_eq!(capture_step(1, true, &mut no), CaptureStep::Free);
    // An unusable video frame (unsupported FourCC, null data) is freed without asking keep,
    // so it never spends a decimation slot.
    assert_eq!(capture_step(1, false, &mut yes), CaptureStep::Free);
    // No frame: keep is never asked, so a timeout or an event cannot spend a slot.
    for kind in [0, 2, 3, 100] {
        assert_eq!(
            capture_step(kind, true, &mut yes),
            CaptureStep::Nothing,
            "type {kind}"
        );
    }
    assert_eq!(capture_step(4, true, &mut yes), CaptureStep::Lost);
    assert_eq!(asked, 1, "keep is asked only for a usable video frame");
}

#[cfg(feature = "ndi")]
#[test]
fn ndi_finish_capture_frees_every_video_frame_exactly_once_3() {
    use bkshading::preview::ndi_source::{finish_capture, CaptureStep};
    for (step, built, freed) in [
        (CaptureStep::Build, 1, 1),
        (CaptureStep::Free, 0, 1),
        (CaptureStep::Nothing, 0, 0),
        (CaptureStep::Lost, 0, 0),
    ] {
        let order = std::cell::RefCell::new(Vec::new());
        let out = finish_capture(
            step,
            || {
                order.borrow_mut().push("build");
                7
            },
            || order.borrow_mut().push("free"),
        );
        let order = order.into_inner();
        let b = order.iter().filter(|e| **e == "build").count();
        let f = order.iter().filter(|e| **e == "free").count();
        assert_eq!((b, f), (built, freed), "{step:?}");
        assert_eq!(out.is_some(), built == 1, "{step:?}");
        if step == CaptureStep::Build {
            assert_eq!(
                order,
                ["build", "free"],
                "copy the pixels before the SDK frees them"
            );
        }
    }
}

#[cfg(feature = "ndi")]
#[test]
fn ndi_preview_bpp_knows_the_supported_fourccs_3() {
    use bkshading::preview::ndi_source::preview_bpp;
    let cc = |s: &[u8; 4]| u32::from_le_bytes(*s);
    assert_eq!(preview_bpp(cc(b"UYVY")), Some(2));
    for f in [b"BGRA", b"BGRX", b"RGBA", b"RGBX"] {
        assert_eq!(preview_bpp(cc(f)), Some(4));
    }
    assert_eq!(preview_bpp(cc(b"NV12")), None);
}

#[cfg(feature = "ndi")]
#[test]
fn ndi_lost_link_waits_before_it_rebuilds_3() {
    use bkshading::preview::ndi_source::{lost_rebuilds, LOST_REBUILD_AFTER};
    const { assert!(LOST_REBUILD_AFTER >= 2) };
    for n in 1..LOST_REBUILD_AFTER {
        assert!(
            !lost_rebuilds(n),
            "{n} errors in a row: short wait, same receiver"
        );
    }
    assert!(lost_rebuilds(LOST_REBUILD_AFTER));
}

#[cfg(feature = "ndi")]
#[test]
fn ndi_capture_outcome_maps_every_frame_type_3() {
    use bkshading::preview::ndi_source::{capture_outcome, CaptureOutcome};
    assert_eq!(capture_outcome(0), CaptureOutcome::Timeout);
    assert_eq!(capture_outcome(1), CaptureOutcome::Video);
    // Audio is never requested (null audio pointer), metadata and status changes are events.
    assert_eq!(capture_outcome(2), CaptureOutcome::Other(2));
    assert_eq!(capture_outcome(3), CaptureOutcome::Other(3));
    assert_eq!(capture_outcome(100), CaptureOutcome::Other(100));
    // Frame type error = the connection was lost: the worker must rebuild, not spin.
    assert_eq!(capture_outcome(4), CaptureOutcome::Lost);
}
