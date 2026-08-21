//! Service config-parsing + camera-view assembly tests — pure, no HTTP, no relay.

use bkshading::aggregator::camera_view;
use bkshading::config::ServiceConfig;
use bkshading_proto::wire::{FpsSync, RelayState, ShadingParams, Transport};

const EXAMPLE: &str = "\
bind = \"0.0.0.0:8770\"

[[camera]]
id = \"cam1\"
label = \"Cam 1\"
transport = \"cambox-relay\"
address = \"cam1.lan:8771\"
ndi_preview = \"CAM1 (usb)\"

[[camera]]
id = \"handheld-1\"
label = \"Handheld 1\"
transport = \"sbc-relay\"
address = \"10.77.9.60:8771\"
";

#[test]
fn parses_camera_list_with_transports() {
    let cfg = ServiceConfig::from_toml_str(EXAMPLE).expect("parse config");
    assert_eq!(cfg.bind, "0.0.0.0:8770");
    assert_eq!(cfg.cameras.len(), 2);

    let cam1 = &cfg.cameras[0];
    assert_eq!(cam1.id, "cam1");
    assert_eq!(cam1.transport, Transport::CamboxRelay);
    assert_eq!(cam1.address, "cam1.lan:8771");
    assert_eq!(cam1.ndi_preview.as_deref(), Some("CAM1 (usb)"));

    let handheld = &cfg.cameras[1];
    assert_eq!(handheld.transport, Transport::SbcRelay);
    assert_eq!(handheld.ndi_preview, None); // no preview -> params-only block
}

#[test]
fn empty_config_starts_clean() {
    let cfg = ServiceConfig::from_toml_str("").expect("empty parse");
    assert!(cfg.cameras.is_empty());
    assert_eq!(cfg.bind, "0.0.0.0:8770"); // default bind applies
}

#[test]
fn camera_with_ndi_preview_has_preview_block() {
    let cfg = ServiceConfig::from_toml_str(EXAMPLE).unwrap();
    let state = RelayState {
        online: true,
        camera: Some("Blackmagic Design Pocket Cinema Camera 4K".into()),
        params: ShadingParams {
            iso: Some(400),
            ..Default::default()
        },
        caps: None,
        fps_supported: true,
        capture_fps: None,
        version: "1.7.0-dev.516".into(),
    };
    let view = camera_view(&cfg.cameras[0], Some(state));
    assert!(
        view.has_preview,
        "cam1 has an NDI preview name -> preview block"
    );
    assert!(view.reachable);
    assert_eq!(view.state.unwrap().params.iso, Some(400));
}

#[test]
fn handheld_without_preview_is_params_only_and_offline_when_unreachable() {
    let cfg = ServiceConfig::from_toml_str(EXAMPLE).unwrap();
    let view = camera_view(&cfg.cameras[1], None);
    assert!(
        !view.has_preview,
        "handheld without NDI preview -> params-only block"
    );
    assert!(!view.reachable);
    assert!(view.state.is_none());
}

#[test]
fn served_index_injects_the_version() {
    // The panel header must carry the compiled version straight in the DOM
    // (version-on-dashboard), not a placeholder.
    let html = bkshading::http::rendered_index();
    assert!(
        html.contains(concat!("v", env!("CARGO_PKG_VERSION"))),
        "served index must show v{}",
        env!("CARGO_PKG_VERSION")
    );
    assert!(
        !html.contains("{{VERSION}}"),
        "placeholder must be replaced"
    );
    assert!(html.contains("data-testid=\"version\""));
}

#[test]
fn parses_preview_table_and_defaults_when_absent() {
    // M2: the optional [preview] table deserializes into PreviewConfig (fps is an f64, so the
    // shipped example uses 3.0 — this pins the deserialize path in CI).
    let cfg = ServiceConfig::from_toml_str("[preview]\nfps = 3.0\njpeg_quality = 40\n")
        .expect("parse [preview]");
    assert!((cfg.preview.fps - 3.0).abs() < 1e-9);
    assert_eq!(cfg.preview.jpeg_quality, 40);

    // Absent [preview] -> sensible defaults (never a parse error).
    let d = ServiceConfig::from_toml_str("").expect("empty parse");
    assert!(d.preview.fps > 0.0);
    assert!(d.preview.jpeg_quality > 0);
}

// --- issue 809: camera fps <-> box grab-mode sync ----------------------------

/// An online relay state reporting a given project fps (x100), for the sync tests.
fn online_state_with_fps100(fps100: Option<i64>) -> RelayState {
    online_state_with_fps_and_capture(fps100, None)
}

/// An online relay state reporting a project fps (x100) AND a box capture-mode fps (issue 809).
fn online_state_with_fps_and_capture(fps100: Option<i64>, capture_fps: Option<i64>) -> RelayState {
    RelayState {
        online: true,
        camera: Some("Blackmagic Design Pocket Cinema Camera 4K".into()),
        params: ShadingParams {
            fps100,
            ..Default::default()
        },
        caps: None,
        fps_supported: true,
        capture_fps,
        version: "1.7.0-dev.516".into(),
    }
}

#[test]
fn parses_grab_fps_when_present_and_defaults_none() {
    let cfg = ServiceConfig::from_toml_str(
        "\
[[camera]]
id = \"cam1\"
label = \"Cam 1\"
transport = \"cambox-relay\"
address = \"cam1.lan:8771\"
grab_fps = 60

[[camera]]
id = \"cam2\"
label = \"Cam 2\"
transport = \"cambox-relay\"
address = \"cam2.lan:8771\"
",
    )
    .expect("parse grab_fps");
    assert_eq!(cfg.cameras[0].grab_fps, Some(60));
    assert_eq!(cfg.cameras[1].grab_fps, None); // omitted -> no grab comparison
}

#[test]
fn camera_view_syncs_and_flags_mismatch_against_grab() {
    let cfg = ServiceConfig::from_toml_str(
        "\
[[camera]]
id = \"cam1\"
label = \"Cam 1\"
transport = \"cambox-relay\"
address = \"cam1.lan:8771\"
grab_fps = 60
",
    )
    .unwrap();
    let cam = &cfg.cameras[0];

    // Camera at 60.00 fps matches the 60 fps grab -> Synced.
    let v = camera_view(cam, Some(online_state_with_fps100(Some(6000))));
    assert_eq!(v.grab_fps, Some(60));
    assert_eq!(v.fps_sync, FpsSync::Synced);

    // Camera at 50.00 fps against a 60 fps grab -> Mismatch (the beat-artefact warning).
    let v = camera_view(cam, Some(online_state_with_fps100(Some(5000))));
    assert_eq!(v.fps_sync, FpsSync::Mismatch);

    // Reachable but fps not read this cycle -> Unknown, never a false mismatch.
    let v = camera_view(cam, Some(online_state_with_fps100(None)));
    assert_eq!(v.fps_sync, FpsSync::Unknown);

    // Relay unreachable -> Unknown, but the configured grab is still surfaced.
    let v = camera_view(cam, None);
    assert_eq!(v.fps_sync, FpsSync::Unknown);
    assert_eq!(v.grab_fps, Some(60));
}

#[test]
fn camera_view_without_grab_config_is_unknown_sync() {
    let cfg = ServiceConfig::from_toml_str(
        "\
[[camera]]
id = \"cam2\"
label = \"Cam 2\"
transport = \"cambox-relay\"
address = \"cam2.lan:8771\"
",
    )
    .unwrap();
    // Even a perfectly good 60.00 fps reading is Unknown when no grab mode is configured
    // (nothing to compare against).
    let v = camera_view(&cfg.cameras[0], Some(online_state_with_fps100(Some(6000))));
    assert_eq!(v.grab_fps, None);
    assert_eq!(v.fps_sync, FpsSync::Unknown);
}

// --- issue 809 remainder: derive/validate grab against the box's live capture rate ----------

const CAM1_GRAB60: &str = "\
[[camera]]
id = \"cam1\"
label = \"Cam 1\"
transport = \"cambox-relay\"
address = \"cam1.lan:8771\"
grab_fps = 60
";

#[test]
fn camera_view_derives_effective_grab_from_live_capture_rate() {
    let cfg = ServiceConfig::from_toml_str(CAM1_GRAB60).unwrap();
    let cam = &cfg.cameras[0];

    // The box actually grabs 50 (relay-reported) while the static config still says 60: derive
    // the LIVE rate (50) and flag the stale config; a camera at 50.00 is then Synced to the
    // real grab, not spuriously Mismatched against the stale 60.
    let v = camera_view(
        cam,
        Some(online_state_with_fps_and_capture(Some(5000), Some(50))),
    );
    assert_eq!(
        v.grab_fps,
        Some(50),
        "effective grab derived from the live capture rate"
    );
    assert!(v.grab_fps_desync, "config 60 != live 50 -> desync flagged");
    assert_eq!(
        v.fps_sync,
        FpsSync::Synced,
        "camera 50.00 matches the live grab 50"
    );

    // Config and live rate agree -> no desync; a camera off that rate is a genuine mismatch.
    let v = camera_view(
        cam,
        Some(online_state_with_fps_and_capture(Some(5000), Some(60))),
    );
    assert!(!v.grab_fps_desync);
    assert_eq!(v.grab_fps, Some(60));
    assert_eq!(v.fps_sync, FpsSync::Mismatch);

    // Relay reports no capture rate (env unset / older relay) -> fall back to the static config,
    // never a desync (current behaviour, no regression).
    let v = camera_view(
        cam,
        Some(online_state_with_fps_and_capture(Some(6000), None)),
    );
    assert!(!v.grab_fps_desync);
    assert_eq!(v.grab_fps, Some(60));
    assert_eq!(v.fps_sync, FpsSync::Synced);
}

#[test]
fn fps_alert_transitions_logs_mismatch_once_per_transition() {
    use bkshading::monitor::fps_alert_transitions;
    use std::collections::HashMap;

    let cfg = ServiceConfig::from_toml_str(CAM1_GRAB60).unwrap();
    let cam = &cfg.cameras[0];
    let mut state: HashMap<String, (FpsSync, bool)> = HashMap::new();

    // Camera at 50.00 vs grab 60 -> Mismatch: logs ONCE on entry, with the cross-reference.
    let mismatch = vec![camera_view(cam, Some(online_state_with_fps100(Some(5000))))];
    let lines = fps_alert_transitions(&mut state, &mismatch);
    assert_eq!(lines.len(), 1, "one mismatch line on transition");
    assert!(lines[0].contains("cam1"));
    assert!(
        lines[0].contains("capture_rate_health"),
        "cross-ref present: {}",
        lines[0]
    );

    // Same state again -> no re-log (a chronic mismatch is logged once, not every cycle).
    assert!(fps_alert_transitions(&mut state, &mismatch).is_empty());

    // Recover to Synced -> no line; then back to Mismatch -> logs afresh.
    let synced = vec![camera_view(cam, Some(online_state_with_fps100(Some(6000))))];
    assert!(fps_alert_transitions(&mut state, &synced).is_empty());
    assert_eq!(fps_alert_transitions(&mut state, &mismatch).len(), 1);
}

#[test]
fn fps_alert_transitions_logs_grab_config_desync_once() {
    use bkshading::monitor::fps_alert_transitions;
    use std::collections::HashMap;

    let cfg = ServiceConfig::from_toml_str(CAM1_GRAB60).unwrap();
    let cam = &cfg.cameras[0];
    let mut state: HashMap<String, (FpsSync, bool)> = HashMap::new();

    // Box live-captures 50 while config says 60, camera at 50.00 -> Synced to the live rate but
    // the static config is out of sync: logs a desync line ONCE.
    let desync = vec![camera_view(
        cam,
        Some(online_state_with_fps_and_capture(Some(5000), Some(50))),
    )];
    let lines = fps_alert_transitions(&mut state, &desync);
    assert_eq!(lines.len(), 1, "one desync line on transition");
    assert!(
        lines[0].to_lowercase().contains("desync"),
        "desync note: {}",
        lines[0]
    );
    assert!(
        fps_alert_transitions(&mut state, &desync).is_empty(),
        "chronic desync silent"
    );
}
