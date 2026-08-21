//! Unit tests for the issue-809 FPS-vs-grab sync verdict — pure, no IO, no camera. The
//! classifier is the single source of truth for the panel's mismatch warning + align
//! button, so its edge cases (unknown fps, missing/invalid grab, exact match) are pinned
//! here, plus the `CameraView` wire round-trip carrying the new `grabFps`/`fpsSync` fields.

use bkshading_proto::wire::{CameraView, FpsSync, Transport};

#[test]
fn classify_matching_project_fps_is_synced() {
    // Camera project fps 60.00 (fps100 = 6000) against a 60 fps grab -> Synced.
    assert_eq!(FpsSync::classify(Some(6000), Some(60)), FpsSync::Synced);
    // 25.00 fps camera against a 25 fps grab -> Synced (the MVP's default rate).
    assert_eq!(FpsSync::classify(Some(2500), Some(25)), FpsSync::Synced);
}

#[test]
fn classify_off_grab_project_fps_is_mismatch() {
    // A 50 fps camera while the box grabs 60 -> Mismatch (this is the beat-artefact case).
    assert_eq!(FpsSync::classify(Some(5000), Some(60)), FpsSync::Mismatch);
    // 30 fps camera vs 60 grab.
    assert_eq!(FpsSync::classify(Some(3000), Some(60)), FpsSync::Mismatch);
}

#[test]
fn classify_unknown_when_camera_fps_absent() {
    // Camera offline / fps not read this cycle -> can't judge, never a false mismatch.
    assert_eq!(FpsSync::classify(None, Some(60)), FpsSync::Unknown);
}

#[test]
fn classify_unknown_when_no_grab_configured() {
    // No grab mode configured for this camera -> no comparison.
    assert_eq!(FpsSync::classify(Some(6000), None), FpsSync::Unknown);
    assert_eq!(FpsSync::classify(None, None), FpsSync::Unknown);
}

#[test]
fn classify_unknown_on_nonpositive_values() {
    // A misconfigured grab_fps = 0 or an unread (0) camera reading is "not known", never a
    // spurious mismatch.
    assert_eq!(FpsSync::classify(Some(6000), Some(0)), FpsSync::Unknown);
    assert_eq!(FpsSync::classify(Some(0), Some(60)), FpsSync::Unknown);
    assert_eq!(FpsSync::classify(Some(-1), Some(60)), FpsSync::Unknown);
}

#[test]
fn default_fps_sync_is_unknown() {
    assert_eq!(FpsSync::default(), FpsSync::Unknown);
}

#[test]
fn fps_sync_serialises_kebab_case() {
    assert_eq!(
        serde_json::to_string(&FpsSync::Unknown).unwrap(),
        "\"unknown\""
    );
    assert_eq!(
        serde_json::to_string(&FpsSync::Synced).unwrap(),
        "\"synced\""
    );
    assert_eq!(
        serde_json::to_string(&FpsSync::Mismatch).unwrap(),
        "\"mismatch\""
    );
    // round-trips back to the same variant.
    let back: FpsSync = serde_json::from_str("\"mismatch\"").unwrap();
    assert_eq!(back, FpsSync::Mismatch);
}

#[test]
fn camera_view_wire_carries_grab_and_sync_camel_case() {
    let view = CameraView {
        id: "cam1".into(),
        label: "Cam 1".into(),
        transport: Transport::CamboxRelay,
        has_preview: true,
        reachable: true,
        grab_fps: Some(60),
        grab_fps_desync: false,
        fps_sync: FpsSync::Mismatch,
        state: None,
    };
    let json = serde_json::to_string(&view).unwrap();
    assert!(json.contains("\"grabFps\":60"), "grabFps in wire: {json}");
    assert!(
        json.contains("\"fpsSync\":\"mismatch\""),
        "fpsSync kebab in wire: {json}"
    );
    // round-trips unchanged.
    let back: CameraView = serde_json::from_str(&json).unwrap();
    assert_eq!(back, view);
}
