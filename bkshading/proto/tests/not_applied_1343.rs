//! Write-not-applied comparison tests (issue 1343): `not_applied_keys` returns the WIRE field
//! names whose write the camera did NOT apply (compared against the authoritative readback). This
//! is the pure core the relay fills `RelayState.not_applied` from at the burst idle-close, so a
//! camera that ACKs a `set-config` and silently ignores it (cam1 today: aperture + focus dropped at
//! PTP while ISO applies) surfaces per-key on the panel instead of the optimistic value reverting.

use bkshading_proto::read::not_applied_keys;
use bkshading_proto::wire::{SetRequest, ShadingParams};

/// The camera's four f-number choices for these cases (ascending, like `parse_fnumber_labels`).
fn choices() -> Vec<f64> {
    vec![2.8, 4.0, 5.2, 8.0]
}

#[test]
fn dropped_aperture_write_is_flagged_1343() {
    // Wrote apertureNorm targeting choice index 3 (f/8.0), but the readback still sits at index 2
    // (f/5.2) — the camera dropped the aperture write. Nothing else was written.
    let written = SetRequest {
        aperture_norm: Some(1.0), // idx 3 of 4
        ..Default::default()
    };
    let readback = ShadingParams {
        aperture_norm: Some(2.0 / 3.0), // idx 2 of 4 (unchanged)
        ..Default::default()
    };
    assert_eq!(
        not_applied_keys(&written, &readback, &choices()),
        vec!["apertureNorm".to_string()],
        "a dropped aperture write flags exactly apertureNorm"
    );
}

#[test]
fn applied_iso_write_is_empty_1343() {
    // Wrote iso 800 and the readback reads back 800 — the camera applied it. No aperture written.
    let written = SetRequest {
        iso: Some(800),
        ..Default::default()
    };
    let readback = ShadingParams {
        iso: Some(800),
        ..Default::default()
    };
    assert!(
        not_applied_keys(&written, &readback, &choices()).is_empty(),
        "an applied ISO write flags nothing"
    );
}

#[test]
fn applied_aperture_write_is_empty_1343() {
    // The high-value false-positive guard: an aperture write the camera DID apply (readback choice
    // index == requested choice index) must NOT be flagged. Locks the exact `choices_to_norm` <->
    // `norm_to_choice_index` round-trip against a future mapping regression.
    let written = SetRequest {
        aperture_norm: Some(2.0 / 3.0), // idx 2 of 4 requested
        ..Default::default()
    };
    let readback = ShadingParams {
        aperture_norm: Some(2.0 / 3.0), // camera applied it -> same idx
        ..Default::default()
    };
    assert!(
        not_applied_keys(&written, &readback, &choices()).is_empty(),
        "an applied aperture write flags nothing"
    );
}

#[test]
fn off_grid_current_fnumber_with_on_grid_request_is_flagged_1343() {
    // cam1 today: the lens sits at f/4, which is BELOW the camera's 4.5-minimum enumerated grid, so
    // params_and_caps snaps the readback aperture_norm to the NEAREST choice (index 0). The operator
    // requested an ON-GRID choice (index 2). The two choice indices differ -> the write is flagged,
    // even though the camera never reported an exact match.
    let grid = vec![4.5, 4.8, 5.2, 8.0];
    let written = SetRequest {
        aperture_norm: Some(2.0 / 3.0), // idx 2 (f/5.2) requested
        ..Default::default()
    };
    let readback = ShadingParams {
        aperture_norm: Some(0.0), // off-grid f/4 -> nearest choice idx 0
        ..Default::default()
    };
    assert_eq!(
        not_applied_keys(&written, &readback, &grid),
        vec!["apertureNorm".to_string()],
        "an off-grid current f-number that never reaches the requested on-grid choice is flagged"
    );
}

#[test]
fn only_the_dropped_key_is_flagged_when_others_apply_1343() {
    // Wrote aperture (dropped) + iso (applied to its current value) — only apertureNorm is flagged.
    let written = SetRequest {
        aperture_norm: Some(1.0), // idx 3 requested
        iso: Some(400),           // matches the readback
        ..Default::default()
    };
    let readback = ShadingParams {
        aperture_norm: Some(2.0 / 3.0), // idx 2 (dropped)
        iso: Some(400),                 // applied
        ..Default::default()
    };
    assert_eq!(
        not_applied_keys(&written, &readback, &choices()),
        vec!["apertureNorm".to_string()],
        "an applied ISO is not falsely flagged alongside a dropped aperture"
    );
}

#[test]
fn unwritten_keys_are_never_flagged_1343() {
    // A readback whose values differ from an EMPTY request flags nothing — only WRITTEN keys count.
    let written = SetRequest::default();
    let readback = ShadingParams {
        aperture_norm: Some(0.0),
        iso: Some(100),
        shutter: Some(50),
        kelvin: Some(3200),
        tint: Some(-5),
        fps100: Some(2500),
        ..Default::default()
    };
    assert!(
        not_applied_keys(&written, &readback, &choices()).is_empty(),
        "no writes -> nothing flagged"
    );
}

#[test]
fn dropped_iso_shutter_kelvin_tint_fps_are_each_flagged_by_value_1343() {
    // Every non-aperture key is compared BY VALUE; a readback that differs flags it. fps is written
    // as project fps and compared against the readback fps100 (= fps*100).
    let written = SetRequest {
        iso: Some(800),
        shutter: Some(125),
        kelvin: Some(6500),
        tint: Some(10),
        fps: Some(30),
        ..Default::default()
    };
    // The camera dropped every one (readback holds different / absent values).
    let readback = ShadingParams {
        iso: Some(400),
        shutter: Some(50),
        kelvin: Some(5600),
        tint: None,
        fps100: Some(2500),
        ..Default::default()
    };
    let got = not_applied_keys(&written, &readback, &choices());
    for key in ["iso", "shutter", "kelvin", "tint", "fps"] {
        assert!(
            got.contains(&key.to_string()),
            "{key} should be flagged: {got:?}"
        );
    }
    assert_eq!(got.len(), 5, "exactly the five dropped keys: {got:?}");
}

#[test]
fn no_choices_never_falsely_flags_aperture_1343() {
    // With no f-number choices the aperture cannot be judged (the relay wrote nothing meaningful) —
    // never a false flag.
    let written = SetRequest {
        aperture_norm: Some(0.5),
        ..Default::default()
    };
    let readback = ShadingParams::default();
    assert!(
        not_applied_keys(&written, &readback, &[]).is_empty(),
        "no choice grid -> aperture is not judged"
    );
}
