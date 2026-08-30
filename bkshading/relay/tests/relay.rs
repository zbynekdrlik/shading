//! Relay logic tests driven by a fake gphoto2 runner — no camera, no gphoto2 binary. This
//! is how the M1 relay is verified while cam1 is running E2E (and before gphoto2 is even
//! installed on the box).

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{anyhow, bail, Result};
use bkshading_proto::wire::SetRequest;
use bkshading_relay::transport::{
    parse_capture_fps_env, parse_first_model, CameraSession, Gphoto2Runner,
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
    assert_eq!(st.params.kelvin, Some(5600));
    assert_eq!(st.params.shutter, Some(50)); // d002 18000 @ 25fps -> 1/50
    assert!(st.fps_supported);
    let caps = st.caps.unwrap();
    assert_eq!(caps.iso_choices, vec![100, 200, 400, 800]);
    assert_eq!(st.version, "1.7.0-dev.516");
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

/// A [`Gphoto2Runner`] that COUNTS its gphoto2 invocations (via a shared atomic), so a test can
/// assert how many real USB-PTP reads a sequence of `/api/state` calls actually triggered. The
/// `auto_detect` count is the proxy for "one full read cycle" (`read_state` calls it exactly once
/// before the seven `get_config` reads).
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
