//! Relay logic tests driven by a fake gphoto2 runner — no camera, no gphoto2 binary. This
//! is how the M1 relay is verified while cam1 is running E2E (and before gphoto2 is even
//! installed on the box).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Result};
use bkshading_proto::wire::SetRequest;
use bkshading_relay::transport::{
    build_get_config_many_args, parse_capture_fps_env, parse_first_model,
    parse_min_read_interval_env, read_is_fresh, split_config_blocks, CameraSession, Gphoto2Cli,
    Gphoto2Runner, MonoClock, CORE_CONFIG_KEYS,
};

const AUTO_DETECT: &str = "\
Model                          Port
----------------------------------------------------------
Blackmagic Design Pocket Cinema Camera 4K usb:002,005";

struct FakeRunner {
    detect: String,
    configs: HashMap<String, String>,
    camera_present: bool,
}

impl FakeRunner {
    fn full_camera() -> Self {
        let mut configs = HashMap::new();
        configs.insert(
            "iso".into(),
            "Current: 400\nChoice: 0 0\nChoice: 1 100\nChoice: 2 200\nChoice: 3 400\nChoice: 4 800\nEND".into(),
        );
        configs.insert(
            "f-number".into(),
            "Current: f/5.2\nChoice: 0 f/2.8\nChoice: 1 f/4.0\nChoice: 2 f/5.2\nChoice: 3 f/8.0\nEND".into(),
        );
        configs.insert(
            "d002".into(),
            "Current: 18000\nBottom: 173\nTop: 36000\nEND".into(),
        );
        configs.insert(
            "d004".into(),
            "Current: 5600\nBottom: 2500\nTop: 10000\nEND".into(),
        );
        configs.insert("d005".into(), "Current: 0\nEND".into());
        configs.insert("d006".into(), "Current: 2500\nEND".into());
        configs.insert("d007".into(), "Current: 25\nBottom: 5\nTop: 60\nEND".into());
        FakeRunner {
            detect: AUTO_DETECT.into(),
            configs,
            camera_present: true,
        }
    }

    /// Adds (or overrides) one gphoto2 config block, e.g. the issue-1238 `d003` focus-distance
    /// block that `full_camera()` deliberately omits (so the best-effort read of an ABSENT d003
    /// is also exercised).
    fn with_config(mut self, key: &str, block: &str) -> Self {
        self.configs.insert(key.into(), block.into());
        self
    }
}

impl Gphoto2Runner for FakeRunner {
    fn auto_detect(&self) -> Result<String> {
        if !self.camera_present {
            bail!("no camera detected");
        }
        Ok(self.detect.clone())
    }
    fn get_config(&self, key: &str) -> Result<String> {
        self.configs
            .get(key)
            .cloned()
            .ok_or_else(|| anyhow!("no such config key: {key}"))
    }
    fn set_config(&self, _key: &str, _value: &str) -> Result<()> {
        Ok(()) // reads don't write; the apply test uses RecordingRunner below
    }
}

#[test]
fn parse_first_model_from_auto_detect_table() {
    assert_eq!(
        parse_first_model(AUTO_DETECT).as_deref(),
        Some("Blackmagic Design Pocket Cinema Camera 4K")
    );
    // No camera row -> None (header/separator only).
    assert_eq!(
        parse_first_model("Model                          Port\n-------------"),
        None
    );
}

#[test]
fn read_state_reports_online_camera() {
    let session = CameraSession::new(Box::new(FakeRunner::full_camera()), "1.7.0-dev.516");
    let st = session.read_state();
    assert!(st.online);
    assert_eq!(
        st.camera.as_deref(),
        Some("Blackmagic Design Pocket Cinema Camera 4K")
    );
    assert_eq!(st.params.iso, Some(400));
    assert_eq!(st.params.kelvin, Some(5600)); // d004
    assert_eq!(st.params.tint, Some(0)); // d005 -- distinct from d006 to catch a positional swap
    assert_eq!(st.params.sensor_fps100, Some(2500)); // d006 -- ditto; issue 1229 batch order
    assert_eq!(st.params.shutter, Some(50)); // d002 18000 @ 25fps -> 1/50
    assert!(st.fps_supported);
    // issue 1238: full_camera() deliberately omits d003, so the best-effort focus-distance read
    // degrades to None WITHOUT breaking the online state — the RED/GREEN guard against reading
    // d003 with `?` (which would wrongly degrade the whole read to offline when d003 is absent).
    assert_eq!(st.params.focus_distance, None);
    let caps = st.caps.unwrap();
    assert_eq!(caps.iso_choices, vec![100, 200, 400, 800]);
    assert_eq!(st.version, "1.7.0-dev.516");
}

#[test]
fn read_state_reports_d003_focus_distance_1238() {
    // A camera that DOES answer d003 -> the relay reports the raw manual focus distance, and the
    // other params are unaffected by the added best-effort read.
    let session = CameraSession::new(
        Box::new(
            FakeRunner::full_camera()
                .with_config("d003", "Current: 32768\nBottom: 0\nTop: 65536\nEND"),
        ),
        "1.7.0-dev.516",
    );
    let st = session.read_state();
    assert!(st.online);
    assert_eq!(st.params.focus_distance, Some(32768));
    assert_eq!(st.params.iso, Some(400));
}

#[test]
fn read_state_offline_when_camera_absent() {
    let mut fake = FakeRunner::full_camera();
    fake.camera_present = false;
    let session = CameraSession::new(Box::new(fake), "1.7.0-dev.516");
    let st = session.read_state();
    assert!(!st.online);
    assert_eq!(st.camera, None);
    assert_eq!(st.params.iso, None);
}

#[test]
fn parse_capture_fps_env_parses_int_decimal_and_rejects_junk() {
    // issue 809: the box's capture-mode fps from CAMERA_BOX_CAPTURE_FPS.
    assert_eq!(parse_capture_fps_env(Some("60".into())), Some(60));
    assert_eq!(parse_capture_fps_env(Some(" 60 ".into())), Some(60)); // trimmed
    assert_eq!(parse_capture_fps_env(Some("60.0".into())), Some(60));
    assert_eq!(parse_capture_fps_env(Some("59.94".into())), Some(60)); // rounded (integer model)
    assert_eq!(parse_capture_fps_env(Some("0".into())), None); // non-positive -> unknown
    assert_eq!(parse_capture_fps_env(Some("-5".into())), None);
    assert_eq!(parse_capture_fps_env(Some("abc".into())), None); // junk -> unknown
    assert_eq!(parse_capture_fps_env(Some("".into())), None);
    assert_eq!(parse_capture_fps_env(None), None); // env unset -> unknown
                                                   // Review hardening: a non-finite / absurd env must not slip through as a bogus giant rate
                                                   // ("inf" would otherwise saturate to i64::MAX through the cast).
    assert_eq!(parse_capture_fps_env(Some("inf".into())), None);
    assert_eq!(parse_capture_fps_env(Some("NaN".into())), None);
    assert_eq!(parse_capture_fps_env(Some("1e18".into())), None);
}

#[test]
fn with_capture_fps_is_reported_in_state_online_and_offline() {
    // issue 809: the relay reports the box's capture-mode fps in RelayState, both when the
    // camera is online AND when it is offline (it is a box property, not a camera one).
    let online = CameraSession::new(Box::new(FakeRunner::full_camera()), "1.7.0-dev.516")
        .with_capture_fps(Some(60));
    assert_eq!(online.read_state().capture_fps, Some(60));

    let mut fake = FakeRunner::full_camera();
    fake.camera_present = false;
    let offline = CameraSession::new(Box::new(fake), "1.7.0-dev.516").with_capture_fps(Some(60));
    let st = offline.read_state();
    assert!(!st.online);
    assert_eq!(
        st.capture_fps,
        Some(60),
        "box rate reported even camera-offline"
    );

    // Default (no env) -> None, never a bogus value.
    let none = CameraSession::new(Box::new(FakeRunner::full_camera()), "1.7.0-dev.516");
    assert_eq!(none.read_state().capture_fps, None);
}

#[test]
fn apply_writes_expected_gphoto2_config() {
    // Keep a raw pointer to inspect the recorded sets after moving the fake into the session.
    let fake = FakeRunner::full_camera();
    // We can't share the Mutex out of the Box easily, so re-read via a fresh apply and a
    // wrapper that exposes the recorded writes.
    let recorded = std::sync::Arc::new(Mutex::new(Vec::<(String, String)>::new()));

    struct RecordingRunner {
        inner: FakeRunner,
        recorded: std::sync::Arc<Mutex<Vec<(String, String)>>>,
    }
    impl Gphoto2Runner for RecordingRunner {
        fn auto_detect(&self) -> Result<String> {
            self.inner.auto_detect()
        }
        fn get_config(&self, key: &str) -> Result<String> {
            self.inner.get_config(key)
        }
        fn set_config(&self, key: &str, value: &str) -> Result<()> {
            self.recorded
                .lock()
                .unwrap()
                .push((key.to_string(), value.to_string()));
            Ok(())
        }
    }

    let session = CameraSession::new(
        Box::new(RecordingRunner {
            inner: fake,
            recorded: recorded.clone(),
        }),
        "1.7.0-dev.516",
    );

    let req = SetRequest {
        aperture_norm: Some(1.0), // last f-number choice -> f/8.0
        iso: Some(800),
        kelvin: Some(6500),
        tint: Some(10),
        shutter: Some(50), // @ 25fps -> d002 angle 18000
        fps: Some(30),
        auto_wb: Some(true), // dropped (no PTP equivalent)
    };
    let n = session.apply(&req).expect("apply ok");
    assert_eq!(n, 6); // 6 real writes, auto_wb dropped
    let writes = recorded.lock().unwrap().clone();
    assert!(writes.contains(&("f-number".into(), "f/8.0".into())));
    assert!(writes.contains(&("iso".into(), "800".into())));
    assert!(writes.contains(&("d002".into(), "18000".into())));
    assert!(writes.contains(&("d004".into(), "6500".into())));
    assert!(writes.contains(&("d005".into(), "10".into())));
    assert!(writes.contains(&("d007".into(), "30".into())));
    assert!(!writes.iter().any(|(k, _)| k.contains("wb")));
}

/// A [`Gphoto2Runner`] that COUNTS its `auto_detect` invocations (via a shared atomic), so a test
/// can assert how many real USB-PTP READ CYCLES a sequence of `/api/state` calls actually
/// triggered. `read_state` calls `auto_detect` exactly once per uncached read cycle (before the
/// coalesced core `get_config_many` batch + the best-effort `d003`), so its count is the read-cycle
/// proxy — used by the min-interval-floor tests (issue 1229). It deliberately does NOT override
/// `get_config_many`, so a floor test measures read CYCLES, not USB sessions per read (that is the
/// separate `SessionCountingRunner`).
struct CountingRunner {
    inner: FakeRunner,
    detect_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Gphoto2Runner for CountingRunner {
    fn auto_detect(&self) -> Result<String> {
        self.detect_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.auto_detect()
    }
    fn get_config(&self, key: &str) -> Result<String> {
        self.inner.get_config(key)
    }
    fn set_config(&self, key: &str, value: &str) -> Result<()> {
        self.inner.set_config(key, value)
    }
}

#[test]
fn read_state_burst_coalesces_to_one_gphoto2_read_1229() {
    // #1229 root cause: the relay shelled `gphoto2` on EVERY `GET /api/state` (a fresh USB-PTP
    // session: open/enumerate/close), and the service pump polls every 2 s -> continuous bus
    // contention that crashed the grabber's capture rate on the shared xHCI bus, tripping the
    // #663 self-heal USB reset every 600 s (~10 s frozen picture, live during production).
    // The relay must coalesce a rapid burst of `/api/state` reads into AT MOST ONE real gphoto2
    // read (served from a min-interval-floored cache). With the default floor (>=10 s), five
    // reads in the same instant must trigger exactly ONE auto-detect. On the pre-fix code (no
    // cache/floor) this triggers five -> the test is RED until the floor lands.
    let detect_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let session = CameraSession::new(
        Box::new(CountingRunner {
            inner: FakeRunner::full_camera(),
            detect_calls: detect_calls.clone(),
        }),
        "1.7.0-dev.516",
    );
    for _ in 0..5 {
        let _ = session.read_state();
    }
    assert_eq!(
        detect_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a burst of 5 rapid /api/state reads must hit gphoto2 at most once (min-interval floor)"
    );
}

/// A [`MonoClock`] whose monotonic ms a test controls directly, so the read-throttle floor
/// (issue 1229) is exercised without real sleeps.
struct FakeClock(Arc<AtomicU64>);

impl MonoClock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[test]
fn read_state_reads_again_after_floor_elapses_1229() {
    // The floor caps the read RATE: within `min_read_interval_ms` of the last read, `/api/state`
    // is cached (no gphoto2); once the floor elapses, the next read hits gphoto2 again — so a
    // panel left open still gets fresh readback, just at most once per floor on the shared bus.
    let detect_calls = Arc::new(AtomicUsize::new(0));
    let clock = Arc::new(AtomicU64::new(0));
    let session = CameraSession::new(
        Box::new(CountingRunner {
            inner: FakeRunner::full_camera(),
            detect_calls: detect_calls.clone(),
        }),
        "1.7.0-dev.516",
    )
    .with_min_read_interval_ms(10_000)
    .with_clock(Box::new(FakeClock(clock.clone())));

    let _ = session.read_state(); // t=0 -> real read #1
    assert_eq!(detect_calls.load(Ordering::SeqCst), 1);

    clock.store(9_999, Ordering::SeqCst); // still within the floor
    let _ = session.read_state();
    assert_eq!(
        detect_calls.load(Ordering::SeqCst),
        1,
        "within the floor -> served from cache, no gphoto2"
    );

    clock.store(10_000, Ordering::SeqCst); // floor elapsed (>=)
    let _ = session.read_state();
    assert_eq!(
        detect_calls.load(Ordering::SeqCst),
        2,
        "floor elapsed -> a fresh real read"
    );
}

#[test]
fn apply_invalidates_read_cache_1229() {
    // A write (SetRequest) is user-initiated and rare, so it stays per-invocation; but after it
    // succeeds the cached read is stale, so the NEXT /api/state must read fresh (even inside the
    // same floor window) so the panel reflects the change instead of the pre-write cache.
    let detect_calls = Arc::new(AtomicUsize::new(0));
    let clock = Arc::new(AtomicU64::new(0));
    let session = CameraSession::new(
        Box::new(CountingRunner {
            inner: FakeRunner::full_camera(),
            detect_calls: detect_calls.clone(),
        }),
        "1.7.0-dev.516",
    )
    .with_min_read_interval_ms(10_000)
    .with_clock(Box::new(FakeClock(clock.clone())));

    let _ = session.read_state(); // real read #1
    let _ = session.read_state(); // cached (same instant)
    assert_eq!(detect_calls.load(Ordering::SeqCst), 1);

    session
        .apply(&SetRequest {
            aperture_norm: None,
            iso: Some(200),
            kelvin: None,
            tint: None,
            shutter: None,
            fps: None,
            auto_wb: None,
        })
        .expect("apply ok");

    // Same instant, but the write invalidated the cache -> fresh read.
    let _ = session.read_state();
    assert_eq!(
        detect_calls.load(Ordering::SeqCst),
        2,
        "a successful write must invalidate the read cache"
    );
}

/// A runner whose `set_config` always FAILS (camera busy/unplugged mid-apply), so a test can
/// prove a partially-failed write still invalidates the cache. Detect + get_config succeed.
struct FailWriteRunner {
    inner: FakeRunner,
    detect_calls: Arc<AtomicUsize>,
}

impl Gphoto2Runner for FailWriteRunner {
    fn auto_detect(&self) -> Result<String> {
        self.detect_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.auto_detect()
    }
    fn get_config(&self, key: &str) -> Result<String> {
        self.inner.get_config(key)
    }
    fn set_config(&self, _key: &str, _value: &str) -> Result<()> {
        bail!("camera busy (simulated mid-apply failure)")
    }
}

#[test]
fn apply_failed_write_also_invalidates_read_cache_1229() {
    // Review finding (issue 1229): a write that FAILS partway still leaves the camera dirty
    // (earlier writes landed), so the cached pre-write snapshot is stale and must NOT be served
    // for up to a floor. A partially-failed apply must invalidate the cache too, not only success.
    let detect_calls = Arc::new(AtomicUsize::new(0));
    let clock = Arc::new(AtomicU64::new(0));
    let session = CameraSession::new(
        Box::new(FailWriteRunner {
            inner: FakeRunner::full_camera(),
            detect_calls: detect_calls.clone(),
        }),
        "1.7.0-dev.516",
    )
    .with_min_read_interval_ms(10_000)
    .with_clock(Box::new(FakeClock(clock.clone())));

    let _ = session.read_state(); // real read #1 populates the cache
    let _ = session.read_state(); // cached (same instant)
    assert_eq!(detect_calls.load(Ordering::SeqCst), 1);

    let res = session.apply(&SetRequest {
        aperture_norm: None,
        iso: Some(200),
        kelvin: None,
        tint: None,
        shutter: None,
        fps: None,
        auto_wb: None,
    });
    assert!(
        res.is_err(),
        "a failing set_config must propagate the error"
    );

    // Same instant, but the failed write must have invalidated the cache -> fresh read.
    let _ = session.read_state();
    assert_eq!(
        detect_calls.load(Ordering::SeqCst),
        2,
        "a partially-failed write must also invalidate the read cache"
    );
}

#[test]
fn read_is_fresh_pure_1229() {
    assert!(!read_is_fresh(None, 0, 10_000)); // no prior read -> never fresh
    assert!(read_is_fresh(Some(0), 0, 10_000)); // same instant -> fresh
    assert!(read_is_fresh(Some(0), 9_999, 10_000)); // within floor -> fresh
    assert!(!read_is_fresh(Some(0), 10_000, 10_000)); // at floor (>=) -> stale
    assert!(!read_is_fresh(Some(0), 20_000, 10_000)); // past floor -> stale
                                                      // A (monotonic-impossible) backwards step saturates to 0 -> treated fresh: serve cache
                                                      // rather than hammer the bus.
    assert!(read_is_fresh(Some(100), 50, 10_000));
}

/// A [`Gphoto2Runner`] that counts every real gphoto2 PROCESS SPAWN on the read path — one per
/// `auto_detect`, one per `get_config`, and ONE per `get_config_many` (a batched multi-key call is
/// a SINGLE USB-PTP session, so it must count as one). This measures the shared-bus footprint of a
/// single read cycle (issue 1229): the residual capture dips on cam1 come from the number of
/// USB open/enumerate/close cycles a read fires on the xHCI bus it shares with the grabber.
struct SessionCountingRunner {
    inner: FakeRunner,
    sessions: Arc<AtomicUsize>,
}

impl Gphoto2Runner for SessionCountingRunner {
    fn auto_detect(&self) -> Result<String> {
        self.sessions.fetch_add(1, Ordering::SeqCst);
        self.inner.auto_detect()
    }
    fn get_config(&self, key: &str) -> Result<String> {
        self.sessions.fetch_add(1, Ordering::SeqCst);
        self.inner.get_config(key)
    }
    fn get_config_many(&self, keys: &[&str]) -> Result<Vec<String>> {
        // A batched call is ONE USB session regardless of key count: count once, and read the
        // per-key blocks from the inner fake DIRECTLY (not via self.get_config) so it stays one.
        self.sessions.fetch_add(1, Ordering::SeqCst);
        keys.iter().map(|k| self.inner.get_config(k)).collect()
    }
    fn set_config(&self, key: &str, value: &str) -> Result<()> {
        self.inner.set_config(key, value)
    }
}

#[test]
fn read_cycle_uses_at_most_three_usb_sessions_1229() {
    // #1229 residual (post-floor): even at one read per floor, a single read cycle shells gphoto2
    // NINE times — `detect()` (1x --auto-detect) + `read_raw()` (8x --get-config: the 7 shading
    // keys + the d003 focus distance) — each a separate USB open/enumerate/close on the xHCI bus
    // the relay shares with the grabber, which is what still dips the grabber's isochronous capture.
    // The fix coalesces the shading reads into ONE multi `--get-config` session, so a full read is
    // at most THREE USB sessions: detect + one core batch + the best-effort d003 (kept separate so
    // an unanswered d003 can never abort the core batch). On the pre-fix code this is NINE -> RED.
    let sessions = Arc::new(AtomicUsize::new(0));
    let session = CameraSession::new(
        Box::new(SessionCountingRunner {
            inner: FakeRunner::full_camera()
                .with_config("d003", "Current: 32768\nBottom: 0\nTop: 65536\nEND"),
            sessions: sessions.clone(),
        }),
        "1.7.0-dev.516",
    );
    let st = session.read_state();
    assert!(st.online, "the read must still produce a full online state");
    assert_eq!(st.params.focus_distance, Some(32768), "d003 still read");
    assert_eq!(st.params.iso, Some(400), "core keys still read");
    let n = sessions.load(Ordering::SeqCst);
    assert!(
        n <= 3,
        "one read cycle must use at most 3 USB-PTP sessions (detect + core batch + d003), got {n}"
    );
}

/// A [`Gphoto2Runner`] that mirrors the REAL [`Gphoto2Cli`] batched read (issue 1229): its
/// `get_config_many` JOINS the per-key gphoto2 stdout blocks into one combined string (as a single
/// multi-`--get-config` process would print) and then re-splits it with `split_config_blocks`. This
/// proves the combine→split round-trip recovers the correct per-key blocks in order, so a coalesced
/// read yields the SAME shading state as the per-key path — without a camera or the gphoto2 binary.
struct CoalescingFakeRunner {
    inner: FakeRunner,
}

impl Gphoto2Runner for CoalescingFakeRunner {
    fn auto_detect(&self) -> Result<String> {
        self.inner.auto_detect()
    }
    fn get_config(&self, key: &str) -> Result<String> {
        self.inner.get_config(key)
    }
    fn get_config_many(&self, keys: &[&str]) -> Result<Vec<String>> {
        let combined = keys
            .iter()
            .map(|k| self.inner.get_config(k))
            .collect::<Result<Vec<_>>>()?
            .join("\n");
        split_config_blocks(&combined, keys.len())
            .ok_or_else(|| anyhow!("split_config_blocks: block/key count mismatch"))
    }
    fn set_config(&self, key: &str, value: &str) -> Result<()> {
        self.inner.set_config(key, value)
    }
}

#[test]
fn coalesced_read_yields_same_state_1229() {
    // The batched (combine→split) read must produce byte-identical shading state to the per-key
    // read — the coalesce is a bus-footprint change ONLY, never a semantic one.
    let d003 = "Current: 32768\nBottom: 0\nTop: 65536\nEND";
    let plain = CameraSession::new(
        Box::new(FakeRunner::full_camera().with_config("d003", d003)),
        "1.7.0-dev.516",
    );
    let coalesced = CameraSession::new(
        Box::new(CoalescingFakeRunner {
            inner: FakeRunner::full_camera().with_config("d003", d003),
        }),
        "1.7.0-dev.516",
    );
    let a = plain.read_state();
    let b = coalesced.read_state();
    // Whole-state equality: RelayState derives PartialEq, so this pins EVERY field (camera, all
    // params incl. aperture/tint/sensor_fps, caps, fps_supported, version) at once — a positional
    // mis-map or a dropped field in the coalesced path fails here, not just a hand-picked subset.
    assert_eq!(a, b, "coalesced read must equal the per-key read exactly");
    assert!(b.online);
    assert_eq!(b.params.focus_distance, Some(32768));
}

#[test]
fn split_config_blocks_1229() {
    let combined = "Label: ISO\nCurrent: 400\nEND\nLabel: F\nCurrent: f/5.2\nEND";
    let got = split_config_blocks(combined, 2).expect("two blocks");
    assert_eq!(got.len(), 2);
    assert!(got[0].contains("Current: 400"));
    assert!(got[1].contains("Current: f/5.2"));
    // A trailing newline after the last END is tolerated (no spurious empty block).
    assert!(split_config_blocks("A\nEND\nB\nEND\n", 2).is_some());
    // Count mismatch (excess or shortfall) -> None -> fail-safe (read degrades to offline, never a
    // block mis-assigned to the wrong key).
    assert!(split_config_blocks(combined, 3).is_none());
    assert!(split_config_blocks(combined, 1).is_none());
    // A missing terminating END -> the final block is incomplete -> shortfall -> None.
    assert!(split_config_blocks("A\nEND\nB", 2).is_none());
    // Zero keys / empty input.
    assert_eq!(split_config_blocks("", 0), Some(vec![]));
}

#[cfg(unix)]
#[test]
fn gphoto2_cli_get_config_many_batches_and_fails_safe_1229() {
    // Exercise the REAL `Gphoto2Cli::get_config_many` glue (argv build -> one process -> split ->
    // fail-safe) with a stand-in `gphoto2` shell script, no camera. The `bkshading` CI job runs on
    // Linux (/bin/sh present); `#[cfg(unix)]` keeps it off the `bkshading-windows` compile.
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("bksh-1229-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // A fake gphoto2 that prints TWO END-delimited blocks (mirrors a real multi `--get-config`).
    let ok = dir.join("gphoto2-ok.sh");
    std::fs::write(
        &ok,
        "#!/bin/sh\nprintf 'Label: ISO\\nCurrent: 400\\nEND\\nLabel: F\\nCurrent: f/5.2\\nEND\\n'\n",
    )
    .unwrap();
    std::fs::set_permissions(&ok, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cli = Gphoto2Cli {
        binary: ok.to_string_lossy().into_owned(),
    };
    let blocks = cli
        .get_config_many(&["iso", "f-number"])
        .expect("two blocks");
    assert_eq!(blocks.len(), 2);
    assert!(blocks[0].contains("Current: 400"));
    assert!(blocks[1].contains("Current: f/5.2"));
    // Count mismatch (2 printed blocks, 3 keys) -> Err (fail-safe -> offline), never a mis-parse.
    assert!(cli.get_config_many(&["iso", "f-number", "d002"]).is_err());
    // Empty keys -> Ok(empty), never a bare `gphoto2` usage-error spawn.
    assert!(cli.get_config_many(&[]).unwrap().is_empty());

    // A non-zero gphoto2 exit -> Err (a failed read degrades to offline).
    let bad = dir.join("gphoto2-bad.sh");
    std::fs::write(&bad, "#!/bin/sh\necho boom >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cli_bad = Gphoto2Cli {
        binary: bad.to_string_lossy().into_owned(),
    };
    assert!(cli_bad.get_config_many(&["iso"]).is_err());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_get_config_many_args_1229() {
    // One `--get-config` token per key -> a SINGLE gphoto2 process reads every key in ONE USB
    // session (the coalesce), not one process per key.
    let args = build_get_config_many_args(&CORE_CONFIG_KEYS);
    assert_eq!(args.len(), CORE_CONFIG_KEYS.len() * 2);
    assert_eq!(
        args.iter().filter(|a| a.as_str() == "--get-config").count(),
        CORE_CONFIG_KEYS.len()
    );
    assert_eq!(&args[0], "--get-config");
    assert_eq!(&args[1], "iso");
    assert_eq!(&args[12], "--get-config");
    assert_eq!(&args[13], "d007");
}

#[test]
fn parse_min_read_interval_env_1229() {
    assert_eq!(
        parse_min_read_interval_env(Some("15000".into())),
        Some(15_000)
    );
    assert_eq!(
        parse_min_read_interval_env(Some(" 15000 ".into())),
        Some(15_000)
    ); // trimmed
    assert_eq!(parse_min_read_interval_env(Some("0".into())), None); // never disable the floor
    assert_eq!(parse_min_read_interval_env(Some("-5".into())), None);
    assert_eq!(parse_min_read_interval_env(Some("12.5".into())), None); // non-integer
    assert_eq!(parse_min_read_interval_env(Some("abc".into())), None);
    assert_eq!(parse_min_read_interval_env(Some("".into())), None);
    assert_eq!(parse_min_read_interval_env(None), None); // unset -> caller uses the default
                                                         // Review finding: reject an absurd value (a units mistake would otherwise freeze readback).
    assert_eq!(
        parse_min_read_interval_env(Some("3600000".into())),
        Some(3_600_000)
    ); // at the 1 h cap -> ok
    assert_eq!(parse_min_read_interval_env(Some("3600001".into())), None); // over cap -> default
    assert_eq!(
        parse_min_read_interval_env(Some("99999999999".into())),
        None
    ); // absurd -> default
}
