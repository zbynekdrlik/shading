//! Service config-parsing + camera-view assembly tests — pure, no HTTP, no relay.

use bkshading::aggregator::{aggregate_with_camera_update, camera_view};
use bkshading::config::ServiceConfig;
use bkshading_proto::wire::{Aggregate, FpsSync, RelayState, ShadingParams, Transport};

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
fn immediate_confirm_push_updates_only_the_target_camera_1337() {
    // issue 1337 Layer 2: after a shading write the service pushes an IMMEDIATE confirmation by
    // rebuilding ONLY the target camera's view from the relay-returned state — every other camera
    // is untouched (no re-poll of the whole fleet).
    let cfg = ServiceConfig::from_toml_str(EXAMPLE).unwrap();
    let online_iso = |iso: i64| RelayState {
        online: true,
        camera: Some("Blackmagic Design Pocket Cinema Camera 4K".into()),
        params: ShadingParams {
            iso: Some(iso),
            ..Default::default()
        },
        caps: None,
        fps_supported: true,
        capture_fps: None,
        version: "1.7.0-dev.640".into(),
        not_applied: Vec::new(),
    };
    let cam1_before = camera_view(&cfg.cameras[0], Some(online_iso(400)), false);
    let handheld = camera_view(&cfg.cameras[1], None, false);
    let agg = Aggregate {
        version: "1.7.0-dev.640".into(),
        cameras: vec![cam1_before, handheld.clone()],
    };
    // A write applied to cam1 returned iso 800.
    let updated = aggregate_with_camera_update(&agg, &cfg.cameras[0], online_iso(800));
    let cam1 = updated.cameras.iter().find(|c| c.id == "cam1").unwrap();
    assert_eq!(
        cam1.state.as_ref().unwrap().params.iso,
        Some(800),
        "target camera reflects the applied write immediately"
    );
    let hh = updated
        .cameras
        .iter()
        .find(|c| c.id == "handheld-1")
        .unwrap();
    assert_eq!(*hh, handheld, "other cameras are untouched (no re-poll)");
    assert_eq!(
        updated.version, "1.7.0-dev.640",
        "aggregate version preserved"
    );
}

#[test]
fn not_applied_is_carried_through_the_camera_view_and_pushed_1343() {
    // issue 1343: the relay's per-key `not_applied` (a camera that ACKed + ignored an aperture
    // write) must flow UNTOUCHED through the service's camera-view assembly onto the pushed wire, as
    // an additive `notApplied` array. The service is a pure pass-through — this proves the panel
    // receives the flag on both the immediate-confirm push and the aggregate.
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
        version: "1.7.0-dev.643".into(),
        not_applied: vec!["apertureNorm".to_string()],
    };
    let view = camera_view(&cfg.cameras[0], Some(state), false);
    assert_eq!(
        view.state.as_ref().unwrap().not_applied,
        vec!["apertureNorm".to_string()],
        "the not_applied flag rides through camera_view"
    );
    // It serializes as the additive camelCase `notApplied` array on the pushed aggregate.
    let agg = Aggregate {
        version: "1.7.0-dev.643".into(),
        cameras: vec![view],
    };
    let json = serde_json::to_string(&agg).unwrap();
    assert!(
        json.contains("\"notApplied\":[\"apertureNorm\"]"),
        "notApplied pushed on the wire: {json}"
    );
    // An older relay that omits the field still deserializes (serde default -> empty), so an
    // aggregate built from it carries an empty notApplied (old clients unaffected).
    let older = "{\"online\":true,\"camera\":\"x\",\"params\":{},\"caps\":null,\"fpsSupported\":false,\"version\":\"y\"}";
    let back: RelayState = serde_json::from_str(older).unwrap();
    assert!(
        back.not_applied.is_empty(),
        "absent notApplied -> empty default"
    );
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
        not_applied: Vec::new(),
    };
    let view = camera_view(&cfg.cameras[0], Some(state), false);
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
    let view = camera_view(&cfg.cameras[1], None, false);
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
        not_applied: Vec::new(),
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
    let v = camera_view(cam, Some(online_state_with_fps100(Some(6000))), false);
    assert_eq!(v.grab_fps, Some(60));
    assert_eq!(v.fps_sync, FpsSync::Synced);

    // Camera at 50.00 fps against a 60 fps grab -> Mismatch (the beat-artefact warning).
    let v = camera_view(cam, Some(online_state_with_fps100(Some(5000))), false);
    assert_eq!(v.fps_sync, FpsSync::Mismatch);

    // Reachable but fps not read this cycle -> Unknown, never a false mismatch.
    let v = camera_view(cam, Some(online_state_with_fps100(None)), false);
    assert_eq!(v.fps_sync, FpsSync::Unknown);

    // Relay unreachable -> Unknown, but the configured grab is still surfaced.
    let v = camera_view(cam, None, false);
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
    let v = camera_view(
        &cfg.cameras[0],
        Some(online_state_with_fps100(Some(6000))),
        false,
    );
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
        false,
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
        false,
    );
    assert!(!v.grab_fps_desync);
    assert_eq!(v.grab_fps, Some(60));
    assert_eq!(v.fps_sync, FpsSync::Mismatch);

    // Relay reports no capture rate (env unset / older relay) -> fall back to the static config,
    // never a desync (current behaviour, no regression).
    let v = camera_view(
        cam,
        Some(online_state_with_fps_and_capture(Some(6000), None)),
        false,
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
    let mismatch = vec![camera_view(
        cam,
        Some(online_state_with_fps100(Some(5000))),
        false,
    )];
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
    let synced = vec![camera_view(
        cam,
        Some(online_state_with_fps100(Some(6000))),
        false,
    )];
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
        false,
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

// --- issue 1305: installable web app (PWA) assets + routes -------------------

#[test]
fn pwa_manifest_is_valid_standalone_1305() {
    let m = bkshading::http::manifest_asset();
    let v: serde_json::Value = serde_json::from_str(m).expect("manifest is valid JSON");
    assert_eq!(v["display"], "standalone");
    assert_eq!(v["start_url"], "/");
    assert_eq!(v["scope"], "/");
    let icons = v["icons"].as_array().expect("icons array");
    let srcs: Vec<&str> = icons.iter().filter_map(|i| i["src"].as_str()).collect();
    assert!(srcs.contains(&"/icon-192.png"), "192 icon listed");
    assert!(srcs.contains(&"/icon-512.png"), "512 icon listed");
    // a maskable entry is present (install-quality icon on Android/Chrome).
    assert!(
        icons.iter().any(|i| i["purpose"]
            .as_str()
            .is_some_and(|p| p.contains("maskable"))),
        "a maskable icon entry is present"
    );
    assert_eq!(
        bkshading::http::MANIFEST_CONTENT_TYPE,
        "application/manifest+json"
    );
}

#[test]
fn pwa_service_worker_has_no_cache_1305() {
    // server-truth: the SW must be a pure passthrough, never a caching SW (no stale UI/state).
    let sw = bkshading::http::sw_js_asset();
    assert!(
        !sw.contains("caches"),
        "sw.js must not use the Cache Storage API"
    );
    assert!(
        sw.contains("fetch(event.request)"),
        "sw.js is a passthrough"
    );
    assert_eq!(
        bkshading::http::SW_JS_CONTENT_TYPE,
        "text/javascript; charset=utf-8"
    );
}

#[test]
fn pwa_icons_are_png_and_favicon_is_svg_1305() {
    assert!(
        bkshading::http::icon_192_asset().starts_with(b"\x89PNG\r\n\x1a\n"),
        "icon-192 is a PNG"
    );
    assert!(
        bkshading::http::icon_512_asset().starts_with(b"\x89PNG\r\n\x1a\n"),
        "icon-512 is a PNG"
    );
    assert!(
        bkshading::http::favicon_svg_asset().contains("<svg"),
        "favicon is an SVG"
    );
    assert_eq!(bkshading::http::PNG_CONTENT_TYPE, "image/png");
    assert_eq!(bkshading::http::SVG_CONTENT_TYPE, "image/svg+xml");
}

#[test]
fn index_links_the_pwa_assets_1305() {
    let html = bkshading::http::rendered_index();
    assert!(
        html.contains("rel=\"manifest\""),
        "index links the manifest"
    );
    assert!(html.contains("/manifest.webmanifest"));
    assert!(
        html.contains("name=\"theme-color\""),
        "index has theme-color"
    );
    assert!(
        html.contains("apple-touch-icon"),
        "index has apple-touch-icon"
    );
}

#[test]
fn reach_transitions_log_once_per_flip_and_heartbeat_counts() {
    use bkshading::monitor::{reach_heartbeat_line, reach_transitions};
    use std::collections::HashMap;

    let cfg = ServiceConfig::from_toml_str(CAM1_GRAB60).unwrap();
    let cam = &cfg.cameras[0];
    // camera_view(cam, Some(state), _) -> reachable=true; camera_view(cam, None, _) -> reachable=false.
    let up = vec![camera_view(
        cam,
        Some(online_state_with_fps100(Some(6000))),
        false,
    )];
    let down = vec![camera_view(cam, None, false)];

    let mut state: HashMap<String, bool> = HashMap::new();
    // First sighting already down -> logged once.
    let l = reach_transitions(&mut state, &down);
    assert_eq!(l.len(), 1);
    assert!(l[0].contains("cam1") && l[0].contains("unreachable"));
    // Still down -> no re-log (no per-poll spam).
    assert!(reach_transitions(&mut state, &down).is_empty());
    // Comes back -> one "reachable again" line.
    let l = reach_transitions(&mut state, &up);
    assert_eq!(l.len(), 1);
    assert!(l[0].contains("reachable again"));
    // Steady up -> silent.
    assert!(reach_transitions(&mut state, &up).is_empty());

    // Heartbeat counts up/down without a transition.
    assert!(reach_heartbeat_line(&up).contains("1/1 reachable"));
    let hb_down = reach_heartbeat_line(&down);
    assert!(hb_down.contains("0/1 reachable") && hb_down.contains("cam1"));
}

// --- issue 808: a preview camera with no / a stale NDI frame (slice A) -------------------

#[test]
fn preview_live_rides_the_camera_view_and_needs_a_preview_source_808() {
    // The panel loads a preview frame ONLY while `previewLive`, so camera_view carries the flag the
    // pump computed from the preview store, unchanged, for a preview-capable camera.
    let cfg = ServiceConfig::from_toml_str(EXAMPLE).unwrap();
    let live = camera_view(&cfg.cameras[0], None, true);
    assert!(live.has_preview);
    assert!(
        live.preview_live,
        "a fresh frame in the store -> previewLive"
    );
    let idle = camera_view(&cfg.cameras[0], None, false);
    assert!(idle.has_preview, "the preview block stays configured");
    assert!(
        !idle.preview_live,
        "no/stale frame -> not live (placeholder)"
    );
    // A camera WITHOUT an NDI preview can never be live, whatever the caller passes: previewLive
    // implies hasPreview, so the panel never fetches a preview it has no block for.
    let handheld = camera_view(&cfg.cameras[1], None, true);
    assert!(!handheld.has_preview);
    assert!(
        !handheld.preview_live,
        "a params-only camera is never preview-live"
    );
    // It rides the wire as camelCase `previewLive` (the field app.js reads).
    let json = serde_json::to_string(&live).unwrap();
    assert!(json.contains("\"previewLive\":true"), "wire: {json}");
}

#[test]
fn immediate_confirm_push_keeps_the_preview_live_flag_808() {
    // The issue-1337 immediate per-camera push rebuilds the view from the RELAY state only; preview
    // liveness comes from the preview store at pump time, so the rebuild must carry it over instead
    // of flipping a live preview to the placeholder for up to one pump tick after every click.
    let cfg = ServiceConfig::from_toml_str(EXAMPLE).unwrap();
    let state = |iso: i64| RelayState {
        online: true,
        camera: Some("Blackmagic Design Pocket Cinema Camera 4K".into()),
        params: ShadingParams {
            iso: Some(iso),
            ..Default::default()
        },
        caps: None,
        fps_supported: true,
        capture_fps: None,
        version: "1.7.0-dev.761".into(),
        not_applied: Vec::new(),
    };
    for live in [true, false] {
        let agg = Aggregate {
            version: "1.7.0-dev.761".into(),
            cameras: vec![camera_view(&cfg.cameras[0], Some(state(400)), live)],
        };
        let updated = aggregate_with_camera_update(&agg, &cfg.cameras[0], state(800));
        let cam1 = &updated.cameras[0];
        assert_eq!(cam1.state.as_ref().unwrap().params.iso, Some(800));
        assert_eq!(
            cam1.preview_live, live,
            "the immediate push keeps the pump's previewLive ({live})"
        );
    }
}

#[test]
fn preview_endpoint_serves_only_a_fresh_frame_else_204_808() {
    use axum::http::{header, StatusCode};
    use bkshading::http::preview_response;
    use bkshading::preview::store::PreviewStore;

    let cfg = ServiceConfig::from_toml_str(EXAMPLE).unwrap();
    let store = PreviewStore::new();
    let max_age = cfg.preview.max_frame_age_ms();
    let now: u64 = 1_800_000_000_000;
    let cache = |r: &axum::response::Response| {
        r.headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };

    // An unknown camera id is the ONLY 404.
    let r = preview_response(&cfg, &store, "no-such-cam", now);
    assert_eq!(r.status(), StatusCode::NOT_FOUND);

    // A preview camera whose feed never delivered a frame: 204 No Content + no-store, never a
    // 4xx/5xx (Chromium logs every 4xx/5xx resource load as a console error).
    let r = preview_response(&cfg, &store, "cam1", now);
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    assert_eq!(cache(&r).as_deref(), Some("no-store"));

    // A configured camera without an NDI preview: also 204, never 404/5xx.
    let r = preview_response(&cfg, &store, "handheld-1", now);
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    assert_eq!(cache(&r).as_deref(), Some("no-store"));

    // A fresh frame is served as a JPEG, no-store.
    store.put("cam1", vec![0xFF, 0xD8, 0xFF, 0xD9], now - 100);
    let r = preview_response(&cfg, &store, "cam1", now);
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(
        r.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("image/jpeg")
    );
    assert_eq!(cache(&r).as_deref(), Some("no-store"));

    // The same frame once it is older than the bound (the feed stopped): 204, never the frozen
    // frame served as if live.
    let stale_at = now - 100 + max_age + 1;
    let r = preview_response(&cfg, &store, "cam1", stale_at);
    assert_eq!(
        r.status(),
        StatusCode::NO_CONTENT,
        "a stale frame is not served"
    );
    assert_eq!(cache(&r).as_deref(), Some("no-store"));
    // Exactly at the bound it is still fresh.
    let r = preview_response(&cfg, &store, "cam1", now - 100 + max_age);
    assert_eq!(r.status(), StatusCode::OK);
}

#[test]
fn preview_transitions_log_each_live_stale_flip_once_808() {
    // A feed that stops leaves the preview worker on quiet capture timeouts; the pump logs ONE
    // line per live/stale flip (the issue-1309 transition model), never one per ~2 s cycle, and
    // ignores a camera without an NDI preview.
    use bkshading::monitor::preview_transitions;
    use std::collections::HashMap;

    let cfg = ServiceConfig::from_toml_str(EXAMPLE).unwrap();
    let views = |live: bool| {
        vec![
            camera_view(&cfg.cameras[0], None, live),
            camera_view(&cfg.cameras[1], None, false),
        ]
    };
    let mut state: HashMap<String, bool> = HashMap::new();
    // First sighting without a frame: one line for cam1, nothing for the params-only handheld.
    let l = preview_transitions(&mut state, &views(false));
    assert_eq!(l.len(), 1, "{l:?}");
    assert!(
        l[0].contains("cam1") && l[0].contains("no fresh NDI frame"),
        "{l:?}"
    );
    // Unchanged -> silent.
    assert!(preview_transitions(&mut state, &views(false)).is_empty());
    // The feed comes up -> one line; stays up -> silent.
    let l = preview_transitions(&mut state, &views(true));
    assert_eq!(l.len(), 1, "{l:?}");
    assert!(l[0].contains("cam1") && l[0].contains("live"), "{l:?}");
    assert!(preview_transitions(&mut state, &views(true)).is_empty());
    // The feed stops -> one stale line.
    let l = preview_transitions(&mut state, &views(false));
    assert_eq!(l.len(), 1, "{l:?}");
    assert!(l[0].contains("cam1") && l[0].contains("stale"), "{l:?}");
    // A camera first seen already live logs nothing.
    let mut fresh: HashMap<String, bool> = HashMap::new();
    assert!(preview_transitions(&mut fresh, &views(true)).is_empty());
}
