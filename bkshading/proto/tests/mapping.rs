//! Unit tests for the pure bkshading mapping/assembly — no camera, no IO. These mirror
//! the assertions in the dev2 MVP `pybridge/test_mapping.py` so the Rust port keeps the
//! byte-verified gphoto2 semantics.

use bkshading_proto::mapping::*;
use bkshading_proto::read::{params_and_caps, plan_writes, RawConfigs};
use bkshading_proto::wire::{SetRequest, Transport};

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

// --- gphoto2 text parsing -----------------------------------------------------

const ISO_BLOCK: &str = "\
Label: ISO
Type: RADIO
Current: 400
Choice: 0 0
Choice: 1 100
Choice: 2 200
Choice: 3 400
Choice: 4 800
END";

const FNUMBER_BLOCK: &str = "\
Label: F-Number
Type: RADIO
Current: f/5.2
Choice: 0 f/2.8
Choice: 1 f/4.0
Choice: 2 f/5.2
Choice: 3 f/8.0
END";

const D002_BLOCK: &str =
    "Label: PTP Property 0xd002\nType: RANGE\nCurrent: 18000\nBottom: 173\nTop: 36000\nEND";
const D004_BLOCK: &str =
    "Label: PTP Property 0xd004\nType: RANGE\nCurrent: 5600\nBottom: 2500\nTop: 10000\nEND";
const D005_BLOCK: &str = "Label: PTP Property 0xd005\nType: MENU\nCurrent: 0\nEND";
const D006_BLOCK: &str = "Label: PTP Property 0xd006\nType: MENU\nCurrent: 2500\nEND";
const D007_BLOCK: &str =
    "Label: PTP Property 0xd007\nType: RANGE\nCurrent: 25\nBottom: 5\nTop: 60\nEND";
// issue 1238: d003 manual focus distance (RANGE, ~0=closest..65536=infinite).
const D003_BLOCK: &str =
    "Label: PTP Property 0xd003\nType: RANGE\nCurrent: 32768\nBottom: 0\nTop: 65536\nEND";

#[test]
fn parse_current_extracts_value() {
    assert_eq!(parse_current(ISO_BLOCK).as_deref(), Some("400"));
    assert_eq!(parse_current(FNUMBER_BLOCK).as_deref(), Some("f/5.2"));
    assert_eq!(parse_current("Label: x\nType: RANGE\nEND"), None);
}

#[test]
fn parse_iso_choices_drops_junk_low_values() {
    // The "0" choice is junk (< MIN_VALID_ISO 25) and must be dropped; the rest ascending.
    assert_eq!(parse_iso_choices(ISO_BLOCK), vec![100, 200, 400, 800]);
}

#[test]
fn parse_choices_orders_by_index() {
    assert_eq!(
        parse_choices(FNUMBER_BLOCK),
        vec!["f/2.8", "f/4.0", "f/5.2", "f/8.0"]
    );
}

#[test]
fn parse_fnumber_accepts_only_f_slash_number() {
    assert_eq!(parse_fnumber("f/5.2"), Some(5.2));
    assert_eq!(parse_fnumber("f/8"), Some(8.0));
    assert_eq!(parse_fnumber("auto"), None);
    assert_eq!(parse_fnumber("f/"), None);
    assert_eq!(parse_fnumber("f/5.2.1"), None);
    assert_eq!(parse_fnumber("f/5."), None); // reference regex requires a digit after the dot
    assert_eq!(parse_fnumber("f/.5"), None); // and a digit before it
}

#[test]
fn parse_range_reads_bottom_top() {
    assert_eq!(parse_range(D007_BLOCK), Some((5, 60)));
    assert_eq!(parse_range("Current: 5\nEND"), None);
}

#[test]
fn shutter_choices_exclude_faster_than_frame_rate() {
    let ch = shutter_choices_for_fps(2500); // 25.00 fps
    assert!(!ch.contains(&24), "24 is faster than 25fps -> excluded");
    assert!(ch.contains(&25), "25 is the frame rate itself -> included");
    assert!(
        ch.contains(&2000),
        "2000 stays within the d002 angle RANGE at 25fps"
    );
}

// --- full assembly + write planning ------------------------------------------

fn full_raw() -> RawConfigs {
    RawConfigs {
        iso: ISO_BLOCK.to_string(),
        fnumber: FNUMBER_BLOCK.to_string(),
        shutter_angle: D002_BLOCK.to_string(),
        kelvin: D004_BLOCK.to_string(),
        tint: D005_BLOCK.to_string(),
        sensor_fps: D006_BLOCK.to_string(),
        project_fps: D007_BLOCK.to_string(),
        focus_distance: D003_BLOCK.to_string(),
        summary: String::new(),
    }
}

#[test]
fn params_and_caps_from_full_camera() {
    let (params, caps) = params_and_caps(&full_raw());
    assert_eq!(params.iso, Some(400));
    assert_eq!(params.kelvin, Some(5600));
    assert_eq!(params.tint, Some(0));
    assert_eq!(params.sensor_fps100, Some(2500));
    // project fps 25 -> fps100 2500; d002 18000 at 2500 -> shutter denom 50.
    assert_eq!(params.fps100, Some(2500));
    assert_eq!(params.shutter, Some(50));
    // issue 1238: d003 manual focus distance lifts through verbatim.
    assert_eq!(params.focus_distance, Some(32768));
    // f/5.2 is choice index 2 of 4 -> norm 2/3.
    let norm = params.aperture_norm.unwrap();
    assert!((norm - (2.0 / 3.0)).abs() < 1e-9, "norm was {norm}");
    // caps
    assert_eq!(caps.iso_choices, vec![100, 200, 400, 800]);
    assert_eq!(caps.fps_min, 5);
    assert_eq!(caps.fps_max, 60);
    assert_eq!(caps.kelvin_min, 2500);
    assert_eq!(caps.kelvin_max, 10000);
    assert!(!caps.shutter_choices.is_empty());
    // issue 1304: the f-number choices lift through as plain numbers, same order/count as the
    // RADIO choice list, so the panel's aperture +/- step indexes the identical list.
    assert_eq!(caps.fnumber_choices.len(), 4);
    for (got, want) in caps.fnumber_choices.iter().zip([2.8, 4.0, 5.2, 8.0]) {
        assert!((got - want).abs() < 1e-9, "fnumber choice {got} != {want}");
    }
}

#[test]
fn fnumber_choices_parity_drops_unparseable_1304() {
    // A pathological RADIO list with a non-f/N.N entry ("Auto"): the caps count, the readback
    // aperture_norm basis, AND the relay's plan_writes basis must ALL use the SAME parseable
    // subset — otherwise the panel's +/- step maps to the wrong f-stop (review finding, #1304).
    let block =
        "Current: f/4.0\nChoice: 0 Auto\nChoice: 1 f/2.8\nChoice: 2 f/4.0\nChoice: 3 f/8.0\nEND";
    let raw = RawConfigs {
        fnumber: block.to_string(),
        ..Default::default()
    };
    let (params, caps) = params_and_caps(&raw);
    // "Auto" is dropped -> three parseable choices.
    assert_eq!(caps.fnumber_choices.len(), 3);
    // The write-path basis (what transport.rs feeds plan_writes) has the SAME count.
    let labels = parse_fnumber_labels(block);
    assert_eq!(labels.len(), caps.fnumber_choices.len(), "count parity");
    // f/4.0 is index 1 of the 3-entry parseable list -> norm 0.5, and plan_writes round-trips it
    // back to f/4.0 over the same basis (the panel's +/- step round-trip).
    assert!((params.aperture_norm.unwrap() - 0.5).abs() < 1e-9);
    let writes = plan_writes(
        &SetRequest {
            aperture_norm: Some(0.5),
            ..Default::default()
        },
        &labels,
        2500,
    );
    assert!(writes.contains(&("f-number".to_string(), "f/4.0".to_string())));
}

fn assert_f64_vec(got: &[f64], want: &[f64]) {
    assert_eq!(got.len(), want.len(), "len {got:?} != {want:?}");
    for (g, w) in got.iter().zip(want) {
        assert!((g - w).abs() < 1e-9, "{got:?} != {want:?}");
    }
}

#[test]
fn aperture_from_summary_when_current_null_cam1_1306() {
    // cam1 live repro: the lens is open at f/4.0, BELOW the camera's first enumerated stop (f/4.5),
    // so gphoto2 prints `Current: (null)` for the f-number RADIO. The current aperture must then
    // come from the --summary raw (0x5007 value/100 = 4.0), the slider snaps to the NEAREST choice,
    // and the junk choices f/0.2/f/0 are filtered out of the grid. Bug #1306 — RED until GREEN.
    let raw = RawConfigs {
        fnumber: "Current: (null)\nChoice: 0 f/0.2\nChoice: 1 f/0\nChoice: 2 f/4.5\nChoice: 3 f/4.8\nChoice: 4 f/5.6\nEND".to_string(),
        summary: "F-Number(0x5007):(readwrite) (type=0x4) Enumeration [20,0,450,480,560] value: f/4 (400)".to_string(),
        ..Default::default()
    };
    let (params, caps) = params_and_caps(&raw);
    // aperture_av from the summary raw 400 -> f/4.0 -> 2*log2(4.0)
    assert!(
        (params.aperture_av.unwrap() - 2.0 * 4.0_f64.log2()).abs() < 1e-9,
        "aperture_av {:?}",
        params.aperture_av
    );
    // the grid drops f/0.2 (0.2) and f/0 (0.0); nearest to 4.0 is f/4.5 = index 0 -> norm 0.0
    assert_f64_vec(&caps.fnumber_choices, &[4.5, 4.8, 5.6]);
    assert!(
        params.aperture_norm.unwrap().abs() < 1e-9,
        "aperture_norm {:?}",
        params.aperture_norm
    );
}

#[test]
fn aperture_from_summary_when_current_null_cam2_1306() {
    // cam2 live repro: lens at f/2.0, below the first enumerated stop f/2.6 -> Current: (null).
    let raw = RawConfigs {
        fnumber: "Current: (null)\nChoice: 0 f/0.2\nChoice: 1 f/0\nChoice: 2 f/2.6\nChoice: 3 f/2.8\nChoice: 4 f/3.2\nEND".to_string(),
        summary: "F-Number(0x5007):(readwrite) (type=0x4) Enumeration [20,0,260,280,320] value: f/2 (200)".to_string(),
        ..Default::default()
    };
    let (params, caps) = params_and_caps(&raw);
    assert!((params.aperture_av.unwrap() - 2.0 * 2.0_f64.log2()).abs() < 1e-9);
    assert_f64_vec(&caps.fnumber_choices, &[2.6, 2.8, 3.2]);
    assert!(params.aperture_norm.unwrap().abs() < 1e-9);
}

#[test]
fn aperture_norm_falls_back_to_nearest_when_current_string_off_list_1306() {
    // Review hardening: Current is a valid f-number ("f/4") whose STRING is not among the choices
    // ("f/4.0"); aperture_norm must fall back to the nearest choice (never a dead slider) while
    // aperture_av is known.
    let raw = RawConfigs {
        fnumber: "Current: f/4\nChoice: 0 f/2.8\nChoice: 1 f/4.0\nChoice: 2 f/5.6\nEND".to_string(),
        ..Default::default()
    };
    let (params, _caps) = params_and_caps(&raw);
    assert!((params.aperture_av.unwrap() - 2.0 * 4.0_f64.log2()).abs() < 1e-9);
    // nearest to 4.0 is f/4.0 (index 1 of 3) -> norm 0.5, not None.
    assert!((params.aperture_norm.unwrap() - 0.5).abs() < 1e-9);
}

#[test]
fn empty_fnumber_block_yields_no_choices_1304() {
    // A relay that does not report f-number choices (empty block) -> empty `fnumber_choices`,
    // never a fabricated entry. The panel then disables the aperture +/- step.
    let (_params, caps) = params_and_caps(&RawConfigs::default());
    assert!(caps.fnumber_choices.is_empty());
}

#[test]
fn parse_summary_fnumber_raw_1306() {
    assert_eq!(
        parse_summary_fnumber_raw(
            "F-Number(0x5007):(readwrite) (type=0x4) Enumeration [20,0,450] value: f/4 (400)"
        ),
        Some(400)
    );
    assert_eq!(
        parse_summary_fnumber_raw("F-Number(0x5007): ... value: f/2 (200)"),
        Some(200)
    );
    assert_eq!(parse_summary_fnumber_raw("no 0x5007 line here"), None);
    // present line but no parseable trailing integer -> None (fail-safe).
    assert_eq!(
        parse_summary_fnumber_raw("F-Number(0x5007): value: f/4 ()"),
        None
    );
}

#[test]
fn parse_fnumber_labels_drops_sub_half_junk_1306() {
    // f/0 and f/0.2 are gphoto2 placeholders (< MIN_VALID_FNUMBER 0.5); a real f/0.95 lens stays.
    let block = "Choice: 0 f/0.2\nChoice: 1 f/0\nChoice: 2 f/4.5\nChoice: 3 f/0.95\nEND";
    assert_eq!(parse_fnumber_labels(block), vec!["f/4.5", "f/0.95"]);
}

#[test]
fn nearest_choice_norm_1306() {
    let labels = vec![
        "f/4.5".to_string(),
        "f/4.8".to_string(),
        "f/5.6".to_string(),
    ];
    assert_eq!(nearest_choice_norm(4.0, &labels), Some(0.0)); // nearest = f/4.5 (idx 0)
    assert_eq!(nearest_choice_norm(5.0, &labels), Some(0.5)); // nearest = f/4.8 (idx 1)
    assert!(nearest_choice_norm(4.0, &[]).is_none());
}

#[test]
fn camera_caps_fnumber_choices_wire_is_camel_case_1304() {
    let caps = bkshading_proto::wire::CameraCaps {
        iso_choices: vec![100, 200],
        fnumber_choices: vec![2.8, 4.0, 5.2, 8.0],
        shutter_choices: vec![50, 60],
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
fn params_degrade_gracefully_on_empty_blocks() {
    let (params, caps) = params_and_caps(&RawConfigs::default());
    assert_eq!(params.iso, None);
    assert_eq!(params.aperture_norm, None);
    assert_eq!(params.shutter, None);
    // fallbacks fire, never a panic.
    assert_eq!(caps.fps_min, FPS_MIN_FALLBACK);
    assert_eq!(caps.kelvin_max, KELVIN_MAX_FALLBACK);
}

#[test]
fn plan_writes_maps_every_field() {
    let choices: Vec<String> = parse_choices(FNUMBER_BLOCK);
    let req = SetRequest {
        aperture_norm: Some(1.0), // last choice -> f/8.0
        iso: Some(800),
        kelvin: Some(6500),
        tint: Some(10),
        shutter: Some(50), // at 2500 -> angle 18000
        fps: Some(30),
        auto_wb: Some(true), // no PTP equivalent -> dropped
    };
    let writes = plan_writes(&req, &choices, 2500);
    assert!(writes.contains(&("f-number".to_string(), "f/8.0".to_string())));
    assert!(writes.contains(&("iso".to_string(), "800".to_string())));
    assert!(writes.contains(&("d002".to_string(), "18000".to_string())));
    assert!(writes.contains(&("d004".to_string(), "6500".to_string())));
    assert!(writes.contains(&("d005".to_string(), "10".to_string())));
    assert!(writes.contains(&("d007".to_string(), "30".to_string())));
    // auto_wb never produces a write.
    assert!(!writes.iter().any(|(k, _)| k == "auto-wb" || k == "d008"));
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
        focus_distance: Some(32768),
    };
    let json = serde_json::to_string(&p).unwrap();
    assert!(json.contains("\"apertureAv\""));
    assert!(json.contains("\"apertureNorm\""));
    assert!(json.contains("\"sensorFps100\""));
    // issue 1238: the new focus-distance field serialises camelCase.
    assert!(json.contains("\"focusDistance\""));
    // round-trips
    let back: bkshading_proto::wire::ShadingParams = serde_json::from_str(&json).unwrap();
    assert_eq!(back, p);
}
