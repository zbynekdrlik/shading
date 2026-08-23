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

// --- NDI runtime discovery (issue 1157) ---------------------------------------------------
// The bkshading SERVICE ships to the strih PC (Windows first), so the real-preview receiver's
// libndi discovery MUST look for the Windows runtime DLL, not only the appliance's Linux `.so`
// names. These pin the PURE, default-feature discovery decision (the `#[cfg(feature="ndi")]`
// `NdiLib::load_uncached()` loader consumes it), so they run on CI with no libndi.

use bkshading::preview::ndi_paths::{ndi_search_candidates, NdiOs};

#[test]
fn windows_candidates_include_the_ndi_runtime_dll() {
    let cands = ndi_search_candidates(NdiOs::Windows, |_| None);
    let has_dll = cands
        .iter()
        .any(|p| p.to_string_lossy().contains("Processing.NDI.Lib"));
    assert!(
        has_dll,
        "Windows NDI discovery must try Processing.NDI.Lib*.dll (the strih runtime); got {cands:?}"
    );
}

#[test]
fn windows_candidates_include_the_documented_strih_runtime_dir() {
    let cands = ndi_search_candidates(NdiOs::Windows, |_| None);
    let has_dir = cands.iter().any(|p| {
        let s = p.to_string_lossy();
        s.contains("NDI 6 Tools") && s.contains("Runtime") && s.contains("Processing.NDI.Lib")
    });
    assert!(
        has_dir,
        "Windows discovery must include the documented NDI 6 Tools Runtime dir; got {cands:?}"
    );
}

#[test]
fn linux_candidates_cover_libndi_so_and_bare_fallback() {
    let cands = ndi_search_candidates(NdiOs::Linux, |_| None);
    assert!(
        cands
            .iter()
            .any(|p| p.to_string_lossy().ends_with("libndi.so.6")),
        "Linux discovery must include libndi.so.6"
    );
    assert!(
        cands.iter().any(|p| p.to_string_lossy() == "libndi.so.6"),
        "bare-name dynamic-linker fallback must be present"
    );
}

#[test]
fn env_runtime_dir_is_searched_before_wellknown_dirs() {
    let env = |k: &str| {
        if k == "NDI_RUNTIME_DIR_V6" {
            Some("/custom/ndi".to_string())
        } else {
            None
        }
    };
    let cands = ndi_search_candidates(NdiOs::Linux, env);
    let first_env = cands.iter().position(|p| p.starts_with("/custom/ndi"));
    let first_wellknown = cands.iter().position(|p| p.starts_with("/usr/lib/ndi"));
    assert!(first_env.is_some(), "env dir candidate must exist");
    assert!(
        first_wellknown.is_some(),
        "well-known dir candidate must exist"
    );
    assert!(
        first_env < first_wellknown,
        "env dir must be tried before well-known dirs"
    );
}

// --- NDI runtime lifecycle (issue 808: reconnect-safe init/destroy) -------------------------
//
// The receiver module itself compiles only under `--features ndi`, but its LIFECYCLE contract
// is pinned here on default features from the source text: the NDI runtime (NDIlib_initialize /
// NDIlib_destroy) is APPLICATION-lifetime and process-GLOBAL, so a per-connect load would let
// one camera's routine reconnect destroy the SDK under every other live preview receiver.

/// The real receiver source text (structural pins run without libndi).
const NDI_SOURCE_SRC: &str = include_str!("../src/preview/ndi_source.rs");

#[test]
fn ndi_connect_acquires_the_process_shared_runtime_never_a_per_connect_load() {
    let src = NDI_SOURCE_SRC;
    let connect_start = src.find("pub fn connect").expect("connect fn present");
    let connect_end = src[connect_start..]
        .find("impl PreviewSource for NdiPreviewSource")
        .map(|o| connect_start + o)
        .expect("impl PreviewSource anchor after connect");
    let connect_body = &src[connect_start..connect_end];
    assert!(
        !connect_body.contains("NdiLib::load()"),
        "connect() must NOT load a fresh NDI runtime per connect: a per-source NdiLib means \
         one camera's reconnect (the worker drops its source before every backoff) runs the \
         process-global NDI destroy under every other live preview receiver"
    );
    assert!(
        !connect_body.contains("load_uncached"),
        "connect() must never reach the uncached loader directly — only through the \
         process-shared keep-alive slot"
    );
    assert!(
        connect_body.contains("NdiLib::shared()"),
        "connect() must acquire the process-shared NDI runtime via NdiLib::shared()"
    );
}

#[test]
fn ndi_runtime_is_kept_alive_process_wide_via_shared_runtime_static() {
    let src = NDI_SOURCE_SRC;
    assert!(
        src.contains("SharedRuntime<NdiLib>"),
        "ndi_source.rs must hold the runtime in a process-wide SharedRuntime<NdiLib> static \
         (load-once keep-alive: initialize once per process, never a mid-flight destroy)"
    );
}

// --- shared runtime keeper (pure, default features) -----------------------------------------

#[test]
fn shared_runtime_loads_once_and_shares_the_same_instance() {
    use bkshading::preview::shared_runtime::SharedRuntime;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let rt: SharedRuntime<String> = SharedRuntime::new();
    let loads = AtomicUsize::new(0);
    let a = rt
        .acquire(|| -> Result<Arc<String>, ()> {
            loads.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new("ndi".to_string()))
        })
        .expect("first load");
    let b = rt
        .acquire(|| -> Result<Arc<String>, ()> {
            loads.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new("other".to_string()))
        })
        .expect("second acquire");
    assert!(Arc::ptr_eq(&a, &b), "every acquire must share ONE instance");
    assert_eq!(loads.load(Ordering::SeqCst), 1, "loader runs exactly once");
}

#[test]
fn shared_runtime_keeps_the_instance_alive_after_all_handles_drop() {
    use bkshading::preview::shared_runtime::SharedRuntime;
    use std::sync::Arc;

    let rt: SharedRuntime<u32> = SharedRuntime::new();
    let first = rt
        .acquire(|| -> Result<Arc<u32>, ()> { Ok(Arc::new(7)) })
        .expect("first load");
    let first_ptr = Arc::as_ptr(&first);
    // The worker drops its source (and with it every outer handle) before each reconnect
    // backoff — the runtime must survive that and be reused, never reloaded/destroyed.
    drop(first);
    let again = rt
        .acquire(|| -> Result<Arc<u32>, ()> { panic!("must NOT reload — keep-alive slot") })
        .expect("reacquire");
    assert_eq!(
        Arc::as_ptr(&again),
        first_ptr,
        "reconnect must reuse the SAME live runtime instance"
    );
}

#[test]
fn shared_runtime_recovers_from_a_poisoned_lock() {
    use bkshading::preview::shared_runtime::SharedRuntime;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::Arc;

    let rt: SharedRuntime<u32> = SharedRuntime::new();
    // A loader that panics unwinds while the slot lock is held -> the mutex poisons.
    let panicked = catch_unwind(AssertUnwindSafe(|| {
        let _ = rt.acquire(|| -> Result<Arc<u32>, ()> { panic!("loader panicked mid-acquire") });
    }));
    assert!(panicked.is_err(), "the loader panic must propagate");
    // A one-off panic must self-heal: the next reconnect's acquire still works (the slot is
    // only written after a fully successful load, so the guarded data stayed valid).
    let ok = rt
        .acquire(|| -> Result<Arc<u32>, ()> { Ok(Arc::new(9)) })
        .expect("acquire after a poisoned lock must recover, not panic forever");
    assert_eq!(*ok, 9);
}

#[test]
fn shared_runtime_does_not_cache_a_failed_load() {
    use bkshading::preview::shared_runtime::SharedRuntime;
    use std::sync::Arc;

    let rt: SharedRuntime<u32> = SharedRuntime::new();
    let err = rt.acquire(|| -> Result<Arc<u32>, &str> { Err("no runtime found") });
    assert_eq!(err.expect_err("load must fail"), "no runtime found");
    let ok = rt
        .acquire(|| -> Result<Arc<u32>, &str> { Ok(Arc::new(1)) })
        .expect("retry after failure");
    assert_eq!(*ok, 1, "a failed load must not poison later loads");
}
