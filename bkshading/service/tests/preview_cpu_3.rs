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

/// A feed whose frames all fail to encode (invalid RGB length), as an odd frame shape would.
struct BrokenFrames {
    answers: Vec<bool>,
}

impl PreviewSource for BrokenFrames {
    fn name(&self) -> &str {
        "broken"
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
        if !kept {
            return Ok(None);
        }
        Ok(Some(RawFrame::new(4, 4, vec![0; 5])))
    }
}

#[test]
fn a_frame_that_fails_to_encode_spends_the_slot_3() {
    // Otherwise a shape that fails on every frame is built and encoded at the full feed rate:
    // the CPU burn this ticket fixed, back again.
    let cfg = PreviewConfig {
        fps: 0.1,
        ..PreviewConfig::default()
    };
    let store = PreviewStore::new();
    let mut decim = Decimator::new(cfg.fps);
    let mut feed = BrokenFrames {
        answers: Vec::new(),
    };

    run_source("cam1", &mut feed, &cfg, &store, &mut decim);

    assert_eq!(
        feed.answers.iter().filter(|k| **k).count(),
        1,
        "one attempt per slot"
    );
    assert!(store.get("cam1").is_none());
}

#[test]
fn a_rebuilt_source_gets_a_frame_at_once_3() {
    // run_forever keeps one Decimator across reconnects while each run_source starts a new
    // clock at 0. A slot stamped on the old clock must not freeze the new connection's preview
    // for as long as the old one ran.
    let cfg = PreviewConfig {
        fps: 5.0,
        ..PreviewConfig::default()
    };
    let store = PreviewStore::new();
    let mut decim = Decimator::new(cfg.fps);
    decim.mark_emitted(36_000_000); // the last frame of a 10 h connection

    let mut feed = FastFeed {
        calls: 0,
        limit: 20,
        built: 0,
    };
    run_source("cam1", &mut feed, &cfg, &store, &mut decim);

    assert_eq!(feed.built, 1, "the new connection keeps its first frame");
    assert!(store.get("cam1").is_some());
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
fn ndi_lost_link_rebuilds_after_a_window_without_video_3() {
    use bkshading::preview::ndi_source::{LostLink, LostStep, LOST_REBUILD_AFTER_MS};
    let w = LOST_REBUILD_AFTER_MS;
    let mut link = LostLink::default();
    // The SDK reports a lost link, then timeouts while it reconnects: Lost, None, Lost, ...
    // Timeouts do not reset the window, so it still ends in a rebuild. Only the first error of
    // a window is news (warn); the rest wait quietly.
    assert_eq!(link.on_lost(0), LostStep::Opened);
    assert_eq!(link.on_lost(w / 2), LostStep::Waiting);
    assert_eq!(link.on_lost(w - 1), LostStep::Waiting);
    assert_eq!(
        link.on_lost(w),
        LostStep::Rebuild,
        "no video for the whole window"
    );
    // A video frame closes the window; the next error opens a new one.
    let mut link = LostLink::default();
    assert_eq!(link.on_lost(0), LostStep::Opened);
    link.on_video();
    assert_eq!(link.on_lost(w + 5), LostStep::Opened);
    assert_eq!(link.on_lost(2 * w + 5), LostStep::Rebuild);
}

#[cfg(feature = "ndi")]
#[test]
fn ndi_frame_usable_needs_data_size_stride_and_a_known_fourcc_3() {
    use bkshading::preview::ndi_source::frame_usable;
    let cc = |s: &[u8; 4]| u32::from_le_bytes(*s);
    let bgra = cc(b"BGRA");
    let uyvy = cc(b"UYVY");
    assert_eq!(frame_usable(true, 640, 360, 2560, bgra), Some(4));
    assert_eq!(
        frame_usable(true, 640, 360, 2600, bgra),
        Some(4),
        "padded rows are fine"
    );
    assert_eq!(frame_usable(false, 640, 360, 2560, bgra), None, "null data");
    assert_eq!(frame_usable(true, 0, 360, 2560, bgra), None, "zero width");
    assert_eq!(frame_usable(true, 640, 0, 2560, bgra), None, "zero height");
    assert_eq!(
        frame_usable(true, 640, 360, 2559, bgra),
        None,
        "short stride"
    );
    assert_eq!(frame_usable(true, 640, 360, 1280, uyvy), Some(2));
    // UYVY packs two pixels per four bytes: an odd width cannot convert to a valid frame.
    assert_eq!(
        frame_usable(true, 641, 360, 1282, uyvy),
        None,
        "odd-width UYVY"
    );
    assert_eq!(
        frame_usable(true, 640, 360, 2560, cc(b"NV12")),
        None,
        "unknown FourCC"
    );
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
