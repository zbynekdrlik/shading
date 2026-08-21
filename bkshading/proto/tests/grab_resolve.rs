//! Unit tests for issue-809 remainder: DERIVE/VALIDATE the effective grab fps against the box's
//! live reported capture rate (`resolve_grab`), the mismatch/desync telemetry notes, and the new
//! wire fields (`RelayState.capture_fps`, `CameraView.grab_fps_desync`). Pure — no IO, no camera.

use bkshading_proto::wire::{
    fps_mismatch_note, grab_desync_note, resolve_grab, CameraView, FpsSync, GrabResolution,
    RelayState, ShadingParams, Transport,
};

#[test]
fn resolve_prefers_live_capture_rate_over_config() {
    // Relay reports the box's live capture rate -> that is the ACTUAL grab, config only a hint.
    let r = resolve_grab(Some(60), Some(50));
    assert_eq!(
        r,
        GrabResolution {
            effective: Some(50),
            desync: true
        },
        "live capture rate wins and the stale config is flagged"
    );
    // Config and live rate agree -> no desync.
    assert_eq!(
        resolve_grab(Some(60), Some(60)),
        GrabResolution {
            effective: Some(60),
            desync: false
        }
    );
}

#[test]
fn resolve_falls_back_to_config_when_no_live_rate() {
    // Relay reports nothing (env unset / older relay) -> use the static config, never a desync.
    assert_eq!(
        resolve_grab(Some(60), None),
        GrabResolution {
            effective: Some(60),
            desync: false
        }
    );
    // No config either -> nothing to compare against.
    assert_eq!(
        resolve_grab(None, None),
        GrabResolution {
            effective: None,
            desync: false
        }
    );
}

#[test]
fn resolve_uses_live_rate_when_config_absent() {
    // A camera with no configured grab but a box that reports its capture rate -> derive it.
    assert_eq!(
        resolve_grab(None, Some(60)),
        GrabResolution {
            effective: Some(60),
            desync: false
        }
    );
}

#[test]
fn resolve_treats_nonpositive_as_unknown_never_a_false_desync() {
    // A misconfigured 0 / negative on either side is "not known", never a spurious desync.
    assert_eq!(
        resolve_grab(Some(0), Some(60)),
        GrabResolution {
            effective: Some(60),
            desync: false
        }
    );
    assert_eq!(
        resolve_grab(Some(60), Some(0)),
        GrabResolution {
            effective: Some(60),
            desync: false
        }
    );
    assert_eq!(
        resolve_grab(Some(-1), Some(-1)),
        GrabResolution {
            effective: None,
            desync: false
        }
    );
}

#[test]
fn mismatch_note_names_rates_and_cross_references_capture_health() {
    let note = fps_mismatch_note("cam1", Some(5000), Some(60));
    assert!(note.contains("cam1"), "names the camera: {note}");
    assert!(
        note.contains("50.00"),
        "camera fps in fps (from x100): {note}"
    );
    assert!(note.contains("60"), "box grab fps: {note}");
    // The #809 cross-reference to the grabber-side capture-rate health + duplicate analysis.
    assert!(
        note.contains("capture_rate_health"),
        "cross-ref capture health: {note}"
    );
    assert!(
        note.contains("656") && note.contains("685"),
        "cross-ref 656/685: {note}"
    );
    assert!(
        note.contains("674"),
        "cross-ref 674 duplicate-frame: {note}"
    );
    // An unread camera fps renders as "?" (never a bogus 0.00).
    assert!(fps_mismatch_note("cam1", None, Some(60)).contains("?"));
}

#[test]
fn desync_note_names_the_live_rate_and_config() {
    let note = grab_desync_note("cam1", Some(50));
    assert!(note.contains("cam1"), "names the camera: {note}");
    assert!(note.contains("50"), "live capture rate: {note}");
    assert!(
        note.to_lowercase().contains("config"),
        "flags the stale config: {note}"
    );
}

#[test]
fn relay_state_capture_fps_is_camel_case_and_defaults_none() {
    let st = RelayState {
        online: true,
        camera: Some("BMPCC".into()),
        params: ShadingParams::default(),
        caps: None,
        fps_supported: true,
        capture_fps: Some(60),
        version: "1.7.0-dev.530".into(),
    };
    let json = serde_json::to_string(&st).unwrap();
    assert!(
        json.contains("\"captureFps\":60"),
        "captureFps in wire: {json}"
    );
    // An older relay that omits captureFps still deserializes (serde default -> None).
    let older = "{\"online\":false,\"camera\":null,\"params\":{},\"caps\":null,\"fpsSupported\":false,\"version\":\"x\"}";
    let back: RelayState = serde_json::from_str(older).unwrap();
    assert_eq!(back.capture_fps, None);
}

#[test]
fn camera_view_carries_grab_fps_desync_camel_case() {
    let view = CameraView {
        id: "cam1".into(),
        label: "Cam 1".into(),
        transport: Transport::CamboxRelay,
        has_preview: true,
        reachable: true,
        grab_fps: Some(50),
        grab_fps_desync: true,
        fps_sync: FpsSync::Mismatch,
        state: None,
    };
    let json = serde_json::to_string(&view).unwrap();
    assert!(
        json.contains("\"grabFpsDesync\":true"),
        "grabFpsDesync in wire: {json}"
    );
    let back: CameraView = serde_json::from_str(&json).unwrap();
    assert_eq!(back, view);
}
