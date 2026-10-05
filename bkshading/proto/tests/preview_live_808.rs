//! Unit tests for the issue-808 `CameraView.preview_live` wire flag. The panel loads a preview
//! frame only while the service reports the camera's NDI feed live, so the flag must ride the
//! wire as camelCase `previewLive`, and a view from an older service that does not send it must
//! still deserialize, as NOT live (the panel then shows its placeholder, never a stale frame).
//! Pure serde, no IO.

use bkshading_proto::wire::{CameraView, FpsSync, Transport};

fn view(preview_live: bool) -> CameraView {
    CameraView {
        id: "cam1".into(),
        label: "Cam 1".into(),
        transport: Transport::CamboxRelay,
        has_preview: true,
        preview_live,
        reachable: true,
        grab_fps: None,
        grab_fps_desync: false,
        fps_sync: FpsSync::Unknown,
        fps_align_settable: false,
        state: None,
    }
}

#[test]
fn camera_view_wire_carries_preview_live_camel_case_808() {
    for live in [true, false] {
        let v = view(live);
        let json = serde_json::to_string(&v).unwrap();
        assert!(
            json.contains(&format!("\"previewLive\":{live}")),
            "previewLive in wire: {json}"
        );
        assert!(
            json.contains("\"hasPreview\":true"),
            "hasPreview kept: {json}"
        );
        let back: CameraView = serde_json::from_str(&json).unwrap();
        assert_eq!(back, v, "round-trips unchanged");
    }
}

#[test]
fn an_older_view_without_preview_live_reads_as_not_live_808() {
    // A CameraView as a service built before the field existed serialized it.
    let older = r#"{"id":"cam1","label":"Cam 1","transport":"cambox-relay","hasPreview":true,"reachable":false,"grabFps":null,"fpsSync":"unknown","state":null}"#;
    let back: CameraView = serde_json::from_str(older).unwrap();
    assert!(back.has_preview, "the configured preview is kept");
    assert!(
        !back.preview_live,
        "an absent previewLive must read as NOT live (placeholder), never as live"
    );
}
