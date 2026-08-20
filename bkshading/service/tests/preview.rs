//! Pure-logic tests for the M2 preview pipeline (issue 808) — frame decimation, per-camera
//! routing/store, JPEG encoding, colour conversion, the test pattern, and the stub source.
//! All run on CI with NO libndi and NO camera (the real NDI receive path is feature-gated).

use std::time::Duration;

use bkshading::preview::convert::{bgra_to_rgb, rgba_to_rgb, uyvy_to_rgb};
use bkshading::preview::decimate::Decimator;
use bkshading::preview::encode::encode_jpeg;
use bkshading::preview::frame::RawFrame;
use bkshading::preview::pattern::test_pattern_rgb;
use bkshading::preview::source::{PreviewSource, StubSource};
use bkshading::preview::store::PreviewStore;
use bkshading::preview::PreviewConfig;

// --- test pattern -------------------------------------------------------------------------

#[test]
fn test_pattern_has_exact_rgb_length_and_is_deterministic() {
    let a = test_pattern_rgb(64, 36, 7);
    let b = test_pattern_rgb(64, 36, 7);
    assert_eq!(a.len(), 64 * 36 * 3);
    assert_eq!(a, b, "same (w,h,tick) must be byte-identical");
}

#[test]
fn test_pattern_moves_between_ticks() {
    let f0 = test_pattern_rgb(64, 36, 0);
    let f1 = test_pattern_rgb(64, 36, 1);
    assert_ne!(f0, f1, "consecutive ticks must differ (visible motion)");
}

#[test]
fn test_pattern_zero_dimension_is_empty() {
    assert!(test_pattern_rgb(0, 10, 3).is_empty());
    assert!(test_pattern_rgb(10, 0, 3).is_empty());
}

// --- decimation ---------------------------------------------------------------------------

#[test]
fn decimator_interval_matches_target_fps() {
    assert_eq!(Decimator::new(5.0).min_interval_ms(), 200);
    assert_eq!(Decimator::new(3.0).min_interval_ms(), 333);
    // Non-positive / non-finite fps floors to 1 fps, never 0-interval.
    assert_eq!(Decimator::new(0.0).min_interval_ms(), 1000);
    assert_eq!(Decimator::new(-4.0).min_interval_ms(), 1000);
}

#[test]
fn decimator_emits_first_then_thins_to_target() {
    let mut d = Decimator::new(5.0); // 200 ms interval
    assert!(d.should_emit(0), "first frame always emits");
    assert!(!d.should_emit(100), "100 ms < 200 ms -> dropped");
    assert!(!d.should_emit(199), "199 ms < 200 ms -> dropped");
    assert!(d.should_emit(200), "200 ms since last -> emit");
    assert!(!d.should_emit(300), "100 ms since last -> dropped");
    assert!(d.should_emit(401), "201 ms since last -> emit");
}

#[test]
fn decimator_thins_a_60fps_stream_to_about_target() {
    // A ~60 fps input (every ~16 ms) thinned to 5 fps should emit ~5 per simulated second.
    let mut d = Decimator::new(5.0);
    let mut emitted = 0;
    for i in 0..60 {
        if d.should_emit(i * 16) {
            emitted += 1;
        }
    }
    // 60*16 = 960 ms window; at 200 ms spacing that's the 1st + at ~200,400,600,800 = 5.
    assert_eq!(emitted, 5, "60fps over ~960ms thinned to 5fps");
}

#[test]
fn decimator_backwards_clock_does_not_burst() {
    let mut d = Decimator::new(5.0);
    assert!(d.should_emit(1000));
    assert!(
        !d.should_emit(900),
        "a backwards timestamp must not emit a burst"
    );
}

// --- JPEG encoding ------------------------------------------------------------------------

#[test]
fn encode_produces_a_valid_jpeg_with_soi_and_eoi() {
    let frame = RawFrame::new(16, 16, test_pattern_rgb(16, 16, 0));
    let jpeg = encode_jpeg(&frame, 55).expect("encode");
    assert!(jpeg.len() > 2);
    assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "JPEG SOI marker");
    assert_eq!(&jpeg[jpeg.len() - 2..], &[0xFF, 0xD9], "JPEG EOI marker");
}

#[test]
fn encode_rejects_an_invalid_frame() {
    // RGB length doesn't match dimensions -> error, never a corrupt JPEG or a panic.
    let bad = RawFrame::new(16, 16, vec![0u8; 10]);
    assert!(!bad.is_valid());
    assert!(encode_jpeg(&bad, 55).is_err());
}

// --- colour conversion --------------------------------------------------------------------

#[test]
fn bgra_converts_to_rgb_dropping_alpha() {
    // one pixel B=10 G=20 R=30 A=255 -> R=30 G=20 B=10
    let rgb = bgra_to_rgb(&[10, 20, 30, 255], 1, 1);
    assert_eq!(rgb, vec![30, 20, 10]);
}

#[test]
fn rgba_converts_to_rgb_dropping_alpha() {
    let rgb = rgba_to_rgb(&[30, 20, 10, 255], 1, 1);
    assert_eq!(rgb, vec![30, 20, 10]);
}

#[test]
fn uyvy_neutral_chroma_is_grey() {
    // U=128 Y0=128 V=128 Y1=128 (neutral chroma, mid luma) -> ~grey for both pixels.
    let rgb = uyvy_to_rgb(&[128, 128, 128, 128], 2, 1);
    assert_eq!(rgb.len(), 2 * 3);
    for c in rgb {
        assert!((120..=136).contains(&c), "neutral UYVY -> grey, got {c}");
    }
}

// --- per-camera store / routing -----------------------------------------------------------

#[test]
fn store_roundtrips_and_routes_per_camera() {
    let store = PreviewStore::new();
    assert!(store.get("cam1").is_none(), "no frame yet -> None");

    store.put("cam1", vec![1, 2, 3], 1000);
    store.put("cam2", vec![9, 9], 1001);

    let a = store.get("cam1").expect("cam1 frame");
    assert_eq!(*a.jpeg, vec![1, 2, 3]);
    assert_eq!(a.seq, 0);
    assert_eq!(store.get("cam2").expect("cam2").jpeg.len(), 2);
    assert!(store.get("cam3").is_none(), "unknown camera -> None");
}

#[test]
fn store_bumps_sequence_on_update() {
    let store = PreviewStore::new();
    store.put("cam1", vec![0], 1);
    store.put("cam1", vec![0], 2);
    assert_eq!(store.get("cam1").unwrap().seq, 1, "seq bumps on each put");
}

// --- stub source --------------------------------------------------------------------------

#[test]
fn stub_source_yields_a_valid_frame() {
    let mut src = StubSource::new("CAM1 (usb)");
    assert_eq!(src.name(), "CAM1 (usb)");
    let frame = src
        .next_frame(Duration::from_millis(100))
        .expect("no source error")
        .expect("a frame");
    assert!(frame.is_valid());
    // and it encodes end to end
    assert!(encode_jpeg(&frame, 55).is_ok());
}

#[test]
fn preview_config_defaults_are_sane() {
    let c = PreviewConfig::default();
    assert!(c.fps >= 2.0 && c.fps <= 5.0, "default preview fps in 2..5");
    assert!(c.jpeg_quality > 0 && c.jpeg_quality <= 100);
}
