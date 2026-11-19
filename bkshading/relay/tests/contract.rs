//! The pinned shading -> camera-box contract (docs/CONTRACT.md, shading#1). camera-box deploys the
//! released relay and its E2E reads the camera through the same REST endpoints the relay uses
//! (shading#6), so a change here is a breaking change for camera-box: bump the version, update
//! docs/CONTRACT.md, tell camera-box.

use std::process::Command;

use bkshading_proto::rest;
use bkshading_relay::transport::{CAMERA_HTTP_PORT, CAMERA_LINK_DRIVER};

fn relay(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_bkshading-relay"))
        .args(args)
        .output()
        .expect("run bkshading-relay");
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn the_rest_endpoints_camera_box_reads_keep_their_paths() {
    // shading#6: the relay drives the camera at http://<camera>:80/control/api/v1/<endpoint>, and
    // camera-box's E2E test-camera step uses the same endpoints while the relay is stopped.
    assert_eq!(rest::API_PREFIX, "/control/api/v1");
    assert_eq!(CAMERA_HTTP_PORT, 80);
    assert_eq!(rest::ISO, "/video/iso");
    assert_eq!(rest::SHUTTER, "/video/shutter");
    assert_eq!(rest::WHITE_BALANCE, "/video/whiteBalance");
    assert_eq!(rest::TINT, "/video/whiteBalanceTint");
    assert_eq!(rest::IRIS, "/lens/iris");
    assert_eq!(rest::FOCUS, "/lens/focus");
    assert_eq!(rest::FORMAT, "/system/format");
    assert_eq!(rest::PRODUCT, "/system/product");
    // The networkd .network camera-box installs matches the camera link by this driver.
    assert_eq!(CAMERA_LINK_DRIVER, "cdc_ncm");
}

#[test]
fn version_is_one_line_with_the_version_as_last_word() {
    let out = relay(&["--version"]);
    assert_eq!(
        out,
        format!("bkshading-relay {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn listens_on_8771_and_reads_its_registration_from_relay_env() {
    let help = relay(&["--help"]);
    assert!(help.contains("[default: 0.0.0.0:8771]"), "{help}");
    // camera-box writes these into the relay.env its unit loads.
    assert!(help.contains("[env: BKSHADING_REGISTER_URL="), "{help}");
    assert!(help.contains("[env: BKSHADING_RELAY_ID="), "{help}");
    // shading#6: the optional fixed camera address; gphoto2 is gone.
    assert!(help.contains("[env: BKSHADING_CAMERA="), "{help}");
    assert!(!help.contains("gphoto2"), "{help}");
}

#[test]
fn the_aperture_step_field_is_aperture_step_on_the_wire_6() {
    // shading#6 (v0.2.1): the +/- press is `{"apertureStep": ±1}` in PUT /api/params.
    let req: bkshading_proto::wire::SetRequest =
        serde_json::from_str(r#"{"apertureStep": -1}"#).unwrap();
    assert_eq!(req.aperture_step, Some(-1));
    let req: bkshading_proto::wire::SetRequest = serde_json::from_str("{}").unwrap();
    assert_eq!(
        req.aperture_step, None,
        "an older client simply leaves it out"
    );
}

#[test]
fn the_offline_reason_field_is_additive_offline_reason_on_the_wire_14() {
    // shading#14 (v0.2.6): `GET /api/state` carries `offlineReason` (null while online, else
    // `no-camera-link` | `camera-not-answering` | `rest-off`). Every other key keeps its name.
    let st = bkshading_proto::wire::RelayState::offline("0.2.6");
    let json = serde_json::to_value(&st).unwrap();
    let keys: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    for k in [
        "online",
        "camera",
        "params",
        "caps",
        "fpsSupported",
        "captureFps",
        "version",
        "notApplied",
        "offlineReason",
    ] {
        assert!(keys.contains(&k), "{k} missing from {keys:?}");
    }
    assert_eq!(json["offlineReason"], serde_json::Value::Null);
    // A reader that predates it (camera-box, an older service) parses the state without it.
    let older: bkshading_proto::wire::RelayState = serde_json::from_str(
        r#"{"online":true,"camera":"x","params":{},"caps":null,"fpsSupported":false,"version":"0.2.5"}"#,
    )
    .unwrap();
    assert_eq!(older.offline_reason, None);
}

#[test]
fn the_not_applied_reason_field_is_additive_17() {
    // shading#17 (v0.2.14): `GET /api/state` carries `notAppliedReason` ({} when nothing was
    // refused) next to `notApplied`; a failed PUT names `x-bkshading-reason`. Every other key keeps
    // its name.
    let st = bkshading_proto::wire::RelayState::offline("0.2.14");
    let json = serde_json::to_value(&st).unwrap();
    assert_eq!(json["notAppliedReason"], serde_json::json!({}));
    assert_eq!(json["notApplied"], serde_json::json!([]));
    assert_eq!(
        bkshading_proto::wire::NOT_APPLIED_REASON_HEADER,
        "x-bkshading-reason"
    );
    assert_eq!(
        bkshading_proto::wire::CAMERA_STATUS_HEADER,
        "x-bkshading-camera-status"
    );
    let older: bkshading_proto::wire::RelayState = serde_json::from_str(
        r#"{"online":true,"camera":"x","params":{},"caps":null,"fpsSupported":false,"version":"0.2.13","notApplied":["iso"]}"#,
    )
    .unwrap();
    assert!(older.not_applied_reason.is_empty());
}
