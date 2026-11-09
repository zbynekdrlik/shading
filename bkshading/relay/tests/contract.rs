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
