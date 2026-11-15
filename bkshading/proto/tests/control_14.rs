//! shading#14: whether the panel can control a camera, and why not. Pure, no IO.

use bkshading_proto::control::{control_status, parse_relay_version, relay_too_old};
use bkshading_proto::wire::{CameraView, ControlStatus, OfflineReason, RelayState};

fn offline(version: &str, reason: Option<OfflineReason>) -> RelayState {
    RelayState {
        offline_reason: reason,
        ..RelayState::offline(version)
    }
}

#[test]
fn relay_versions_parse_with_or_without_a_suffix_14() {
    assert_eq!(parse_relay_version("0.1.2"), Some((0, 1, 2)));
    assert_eq!(parse_relay_version("0.2.6"), Some((0, 2, 6)));
    assert_eq!(parse_relay_version("1.7.0-dev.e2e"), Some((1, 7, 0)));
    assert_eq!(parse_relay_version(" 0.2.0 "), Some((0, 2, 0)));
    assert_eq!(parse_relay_version("test"), None);
    assert_eq!(parse_relay_version("0.2"), None);
    assert_eq!(parse_relay_version(""), None);
}

#[test]
fn a_relay_before_the_rest_transport_is_too_old_14() {
    assert!(relay_too_old("0.1.2"), "pp-cam4 runs 0.1.2");
    assert!(relay_too_old("0.1.99"));
    assert!(!relay_too_old("0.2.0"), "0.2.0 is the first REST relay");
    assert!(!relay_too_old("0.2.6"));
    assert!(!relay_too_old("1.7.0-dev.e2e"));
    assert!(
        !relay_too_old("test"),
        "an unreadable version is never judged old"
    );
}

#[test]
fn each_case_gets_its_control_status_14() {
    assert_eq!(control_status(None), ControlStatus::RelayUnreachable);

    let mut online = RelayState::offline("0.2.6");
    online.online = true;
    assert_eq!(control_status(Some(&online)), ControlStatus::Ok);
    // An old relay whose camera IS online still controls it: no warning.
    online.version = "0.1.2".into();
    assert_eq!(control_status(Some(&online)), ControlStatus::Ok);

    assert_eq!(
        control_status(Some(&offline("0.1.2", None))),
        ControlStatus::RelayTooOld
    );
    // The version decides before any reason (an old relay sends none anyway).
    assert_eq!(
        control_status(Some(&offline("0.1.2", Some(OfflineReason::RestOff)))),
        ControlStatus::RelayTooOld
    );
    assert_eq!(
        control_status(Some(&offline("0.2.6", Some(OfflineReason::NoCameraLink)))),
        ControlStatus::NoCameraLink
    );
    assert_eq!(
        control_status(Some(&offline(
            "0.2.6",
            Some(OfflineReason::CameraNotAnswering)
        ))),
        ControlStatus::CameraNotAnswering
    );
    assert_eq!(
        control_status(Some(&offline("0.2.6", Some(OfflineReason::RestOff)))),
        ControlStatus::RestOff
    );
    // A 0.2.0-0.2.5 relay sends no reason; a newer relay may send one we do not know.
    assert_eq!(
        control_status(Some(&offline("0.2.5", None))),
        ControlStatus::CameraOffline
    );
    assert_eq!(
        control_status(Some(&offline("0.2.6", Some(OfflineReason::Unknown)))),
        ControlStatus::CameraOffline
    );
}

#[test]
fn offline_reason_is_additive_on_the_wire_14() {
    let st = offline("0.2.6", Some(OfflineReason::RestOff));
    let json = serde_json::to_value(&st).unwrap();
    assert_eq!(json["offlineReason"], "rest-off");
    for (r, w) in [
        (OfflineReason::NoCameraLink, "no-camera-link"),
        (OfflineReason::CameraNotAnswering, "camera-not-answering"),
        (OfflineReason::RestOff, "rest-off"),
    ] {
        assert_eq!(serde_json::to_value(r).unwrap(), w);
        let back: OfflineReason = serde_json::from_value(serde_json::json!(w)).unwrap();
        assert_eq!(back, r);
    }
    // An older relay (0.2.5) sends no offlineReason: it still deserializes, as None.
    let older = r#"{"online":false,"camera":null,"params":{},"caps":null,"fpsSupported":false,"version":"0.2.5","notApplied":[]}"#;
    let back: RelayState = serde_json::from_str(older).unwrap();
    assert_eq!(back.offline_reason, None);
    // A newer relay's reason this service does not know never breaks the whole state.
    let newer = r#"{"online":false,"camera":null,"params":{},"caps":null,"fpsSupported":false,"version":"0.3.0","offlineReason":"battery-flat"}"#;
    let back: RelayState = serde_json::from_str(newer).unwrap();
    assert_eq!(back.offline_reason, Some(OfflineReason::Unknown));
}

#[test]
fn camera_view_control_is_kebab_and_defaults_to_ok_14() {
    for (c, w) in [
        (ControlStatus::Ok, "ok"),
        (ControlStatus::RelayUnreachable, "relay-unreachable"),
        (ControlStatus::RelayTooOld, "relay-too-old"),
        (ControlStatus::NoCameraLink, "no-camera-link"),
        (ControlStatus::CameraNotAnswering, "camera-not-answering"),
        (ControlStatus::RestOff, "rest-off"),
        (ControlStatus::CameraOffline, "camera-offline"),
    ] {
        assert_eq!(serde_json::to_value(c).unwrap(), w);
    }
    // A view from an older service has no `control`: no warning.
    let older = r#"{"id":"cam1","label":"Cam 1","transport":"cambox-relay","hasPreview":false,"reachable":true,"grabFps":null,"fpsSync":"unknown","state":null}"#;
    let back: CameraView = serde_json::from_str(older).unwrap();
    assert_eq!(back.control, ControlStatus::Ok);
}
