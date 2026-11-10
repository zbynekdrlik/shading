//! Unit tests for the pure bkshading camera-value math — no camera, no IO. The shutter and
//! aperture-step semantics mirror the dev2 MVP `pybridge/test_mapping.py`; the REST assembly
//! lives in `rest_read_6.rs`.

use bkshading_proto::mapping::*;
use bkshading_proto::wire::Transport;

// --- shutter angle <-> denominator (the same formula both ways) --------------

#[test]
fn shutter_denom_to_angle_at_25fps() {
    // 360 * 25.00fps / 50 = 180 deg = 18000 (x100).
    assert_eq!(convert_angle_or_denom(50, 2500), 18000);
    // 360 * 25.00 / 25 = 360 deg = 36000 (x100), the RANGE max.
    assert_eq!(convert_angle_or_denom(25, 2500), 36000);
}

#[test]
fn shutter_angle_back_to_denom_is_symmetric() {
    // angle 18000 at 25fps -> denominator 50 again.
    assert_eq!(convert_angle_or_denom(18000, 2500), 50);
}

#[test]
fn shutter_zero_or_negative_never_divides_by_zero() {
    assert_eq!(convert_angle_or_denom(0, 2500), 1);
    assert_eq!(convert_angle_or_denom(-5, 2500), 1);
}

#[test]
fn shutter_denom_to_angle100_clamps_to_range() {
    // A very fast denom would exceed neither bound at 25fps, but a degenerate huge denom
    // drives the angle below the RANGE min and is clamped up.
    assert!(shutter_denom_to_angle100(50, 2500) == 18000);
    assert!(shutter_denom_to_angle100(100_000, 2500) >= SHUTTER_ANGLE_MIN);
    assert!(shutter_denom_to_angle100(1, 2500) <= SHUTTER_ANGLE_MAX);
}

// --- aperture -----------------------------------------------------------------

#[test]
fn fnumber_to_av_matches_reference() {
    // AV = 2*log2(f); f/4 -> 4.0, f/2 -> 2.0.
    assert!((fnumber_to_av(4.0).unwrap() - 4.0).abs() < 1e-9);
    assert!((fnumber_to_av(2.0).unwrap() - 2.0).abs() < 1e-9);
    assert_eq!(fnumber_to_av(0.0), None);
    assert_eq!(fnumber_to_av(-1.0), None);
}

#[test]
fn norm_and_index_round_trip() {
    assert_eq!(choices_to_norm(0, 5), 0.0);
    assert_eq!(choices_to_norm(4, 5), 1.0);
    assert_eq!(choices_to_norm(2, 5), 0.5);
    assert_eq!(choices_to_norm(0, 1), 0.0); // single choice, no divide-by-zero
    assert_eq!(norm_to_choice_index(0.0, 5), 0);
    assert_eq!(norm_to_choice_index(1.0, 5), 4);
    assert_eq!(norm_to_choice_index(0.5, 5), 2);
    assert_eq!(norm_to_choice_index(2.0, 5), 4); // clamped
    assert_eq!(norm_to_choice_index(-1.0, 5), 0); // clamped
}

// --- shutter choices fallback ------------------------------------------------

#[test]
fn shutter_choices_exclude_faster_than_frame_rate() {
    let ch = shutter_choices_for_fps(2500); // 25.00 fps
    assert!(!ch.contains(&24), "24 is faster than 25fps -> excluded");
    assert!(ch.contains(&25), "25 is the frame rate itself -> included");
    assert!(
        ch.contains(&2000),
        "2000 stays within the angle range at 25fps"
    );
}

#[test]
fn camera_caps_fnumber_choices_wire_is_camel_case_1304() {
    let caps = bkshading_proto::wire::CameraCaps {
        iso_choices: vec![100, 200],
        fnumber_choices: vec![2.8, 4.0, 5.2, 8.0],
        shutter_choices: vec![50, 60],
        fps_choices: vec![2400, 2500],
        fps_min: 5,
        fps_max: 60,
        kelvin_min: 2500,
        kelvin_max: 10000,
    };
    let json = serde_json::to_string(&caps).unwrap();
    assert!(json.contains("\"fNumberChoices\""), "camelCase wire key");
    // Round-trips, and an OLDER relay that omits the key deserializes to an empty Vec
    // (`#[serde(default)]`), never a deserialize error.
    let back: bkshading_proto::wire::CameraCaps = serde_json::from_str(&json).unwrap();
    assert_eq!(back, caps);
    let legacy = "{\"isoChoices\":[100],\"shutterChoices\":[50],\"fpsMin\":5,\"fpsMax\":60,\"kelvinMin\":2500,\"kelvinMax\":10000}";
    let old: bkshading_proto::wire::CameraCaps = serde_json::from_str(legacy).unwrap();
    assert!(
        old.fnumber_choices.is_empty(),
        "missing key -> empty via serde default"
    );
}

#[test]
fn transport_serialises_kebab_case() {
    assert_eq!(
        serde_json::to_string(&Transport::CamboxRelay).unwrap(),
        "\"cambox-relay\""
    );
    assert_eq!(
        serde_json::to_string(&Transport::SbcRelay).unwrap(),
        "\"sbc-relay\""
    );
    assert_eq!(
        serde_json::to_string(&Transport::EthernetRest).unwrap(),
        "\"ethernet-rest\""
    );
}

#[test]
fn shading_params_wire_is_camel_case() {
    let p = bkshading_proto::wire::ShadingParams {
        aperture_av: Some(4.0),
        aperture_norm: Some(0.5),
        iso: Some(400),
        kelvin: Some(5600),
        tint: Some(0),
        shutter: Some(50),
        fps100: Some(2500),
        sensor_fps100: Some(2500),
        focus_distance: None,
        focus_norm: Some(0.25),
        iris_range_pos: Some(0.5),
    };
    let json = serde_json::to_string(&p).unwrap();
    assert!(json.contains("\"apertureAv\""));
    assert!(json.contains("\"apertureNorm\""));
    assert!(json.contains("\"sensorFps100\""));
    // shading#6: the REST focus position serialises camelCase; an older reader ignores it.
    assert!(json.contains("\"focusNorm\""));
    // shading#6 (0.2.3): the iris's place in the lens range.
    assert!(json.contains("\"irisRangePos\""));
    // round-trips
    let back: bkshading_proto::wire::ShadingParams = serde_json::from_str(&json).unwrap();
    assert_eq!(back, p);
}

// issue 1337: step the aperture from the camera's REAL current f-number across the enumerated
// choice grid — NEVER a nearest-snap to idx 0 for an off-grid lens (the owner's "clona sa
// nezdvihne" bug). "+" = the first choice ABOVE the current f-number, "-" = the last choice BELOW
// it; a lens open below the first enumerated stop clamps to the min choice from either direction.
#[test]
fn step_choice_from_off_grid_and_on_grid_1337() {
    // cam1: lens at f/4.0, choices start at f/4.5 (below the first enumerated stop). The first "+"
    // must move to the FIRST real stop (f/4.5 = idx 0), not the SECOND (the pre-fix idx-0-snap +1).
    let cam1 = [4.5_f64, 5.0, 5.6, 6.3, 7.1, 8.0];
    assert_eq!(
        step_choice(4.0, &cam1, 1),
        Some(0),
        "off-grid below: + -> first stop (min)"
    );
    assert_eq!(
        step_choice(4.0, &cam1, -1),
        Some(0),
        "off-grid below: - clamps to min"
    );

    // cam3: lens at f/3.36 (further below the grid) — "-" must reach the min choice (not stay dead).
    let cam3 = [4.5_f64, 5.0, 5.6, 6.3];
    assert_eq!(
        step_choice(3.36, &cam3, -1),
        Some(0),
        "off-grid below: - -> min choice"
    );
    assert_eq!(
        step_choice(3.36, &cam3, 1),
        Some(0),
        "off-grid below: + -> first stop (min)"
    );

    // on-grid: exactly at a choice, +/- move by one index.
    let g = [2.8_f64, 4.0, 5.2, 8.0];
    assert_eq!(step_choice(5.2, &g, 1), Some(3), "on-grid: + -> next index");
    assert_eq!(
        step_choice(5.2, &g, -1),
        Some(1),
        "on-grid: - -> prev index"
    );

    // bounds: at the max, "+" stays at max; at the min, "-" stays at min (no move -> the panel
    // disables the button, but the mapper never indexes out of range).
    assert_eq!(step_choice(8.0, &g, 1), Some(3), "at max: + stays at max");
    assert_eq!(step_choice(2.8, &g, -1), Some(0), "at min: - stays at min");

    // between two choices: "+" to the choice above, "-" to the choice below.
    assert_eq!(
        step_choice(4.6, &g, 1),
        Some(2),
        "between: + -> next choice above"
    );
    assert_eq!(
        step_choice(4.6, &g, -1),
        Some(1),
        "between: - -> next choice below"
    );

    // above the whole grid: "+" clamps to max, "-" to the choice below (the max).
    assert_eq!(
        step_choice(9.0, &g, 1),
        Some(3),
        "above grid: + clamps to max"
    );
    assert_eq!(
        step_choice(9.0, &g, -1),
        Some(3),
        "above grid: - -> last below (max)"
    );

    // degenerate: no / single choice.
    assert_eq!(step_choice(4.0, &[], 1), None, "no choices -> None");
    assert_eq!(
        step_choice(4.0, &[5.6], 1),
        Some(0),
        "single choice -> idx 0"
    );
}

/// shading#6 (0.2.3): a step toward the end `lens/iris normalised` says the iris is at.
#[test]
fn the_iris_is_at_the_end_a_step_heads_for_6() {
    use bkshading_proto::mapping::{iris_at_end, iris_range_pos, IRIS_END_EPS};
    assert!(iris_at_end(Some(0.0), -1), "wide open, -");
    assert!(!iris_at_end(Some(0.0), 1), "wide open, +");
    assert!(iris_at_end(Some(1.0), 1), "closed, +");
    assert!(!iris_at_end(Some(1.0), -1), "closed, -");
    assert!(
        iris_at_end(Some(IRIS_END_EPS / 2.0), -1),
        "float noise at the end"
    );
    assert!(iris_at_end(Some(1.0 - IRIS_END_EPS / 2.0), 1));
    assert!(
        !iris_at_end(Some(0.03), -1),
        "one 1/6 stop in: a step is left"
    );
    assert!(!iris_at_end(Some(0.97), 1));
    assert!(!iris_at_end(None, -1), "unknown: probe");
    assert!(!iris_at_end(None, 1));
    assert!(!iris_at_end(Some(0.0), 0));
    assert_eq!(iris_range_pos(Some(-0.2)), Some(0.0));
    assert_eq!(iris_range_pos(Some(1.5)), Some(1.0));
    assert_eq!(iris_range_pos(Some(0.4)), Some(0.4));
    assert_eq!(iris_range_pos(Some(f64::NAN)), None);
    assert_eq!(iris_range_pos(None), None);
}
