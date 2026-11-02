//! The pinned shading -> camera-box contract (docs/CONTRACT.md, shading#1). camera-box deploys the
//! released relay and its E2E reads the camera through the relay's own key names, so a change here
//! is a breaking change for camera-box: bump the version, update docs/CONTRACT.md, tell camera-box.

use std::process::Command;

use bkshading_relay::transport::{CORE_CONFIG_KEYS, FOCUS_DISTANCE_KEY};

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
fn the_gphoto2_keys_camera_box_reads_keep_their_names() {
    // camera-box E2E: read+set iso, d002 (shutter angle x100), d004 (WB K), f-number, d005;
    // read-only d006 (project fps x100); d007 (off-speed fps) is read by the relay, never by E2E.
    assert_eq!(
        CORE_CONFIG_KEYS,
        ["iso", "f-number", "d002", "d004", "d005", "d006", "d007"]
    );
    assert_eq!(FOCUS_DISTANCE_KEY, "d003");
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
}
