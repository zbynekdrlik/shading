//! Issue 1402: on every BMPCC in the fleet, gphoto2 `d006` is the PROJECT frame rate (a MENU of
//! the camera's own timebases, x100) and `d007` the OFF-SPEED (sensor) frame rate (a plain-int
//! RANGE 5..60). The relay had them the other way round, so cam3 (the Pocket 6K on handheld-1)
//! reported 50 fps while it ran 60, and the "Zosúladiť s grab" write changed the off-speed rate.
//!
//! These tests pin the corrected semantics to the TWO real choice lists read off the fleet:
//! - Pocket Cinema Camera 6K, handheld-1 (issue body): d006 MENU Current 6000, d007 RANGE Current 50;
//! - Pocket Cinema Camera 4K, cam1 (design comment): d006 MENU Current 6000, d007 RANGE Current 60.
//!
//! Both list the same d006 choices `8, 2398, 2400, 2500, 2997, 3000, 5000, 5994`: the current 6000
//! (60.00) is NOT one of them, and `8` is a non-rate entry. Pure, no IO, no camera.

use bkshading_proto::mapping::{fps_settable, parse_fps100_choices, shutter_choices_for_fps};
use bkshading_proto::read::{
    fps_supported, params_and_caps, plan_writes, FpsNotSettable, RawConfigs,
};
use bkshading_proto::wire::{CameraCaps, CameraView, FpsSync, SetRequest, Transport};

/// The Pocket 6K on handheld-1 — `gphoto2 --get-config d006 --get-config d007`, as gphoto2 prints
/// a MENU and a RANGE block (the issue-body read).
const POCKET_6K_D006: &str = "\
Label: PTP Property 0xd006
Readonly: 0
Type: MENU
Current: 6000
Choice: 0 8
Choice: 1 2398
Choice: 2 2400
Choice: 3 2500
Choice: 4 2997
Choice: 5 3000
Choice: 6 5000
Choice: 7 5994
END";
const POCKET_6K_D007: &str = "\
Label: PTP Property 0xd007
Readonly: 0
Type: RANGE
Current: 50
Bottom: 5
Top: 60
Step: 1
END";

/// The Pocket 4K on cam1 — the main's read in the design comment (same d006 list, off-speed 60).
const POCKET_4K_D006: &str = POCKET_6K_D006;
const POCKET_4K_D007: &str = "\
Label: PTP Property 0xd007
Readonly: 0
Type: RANGE
Current: 60
Bottom: 5
Top: 60
Step: 1
END";

/// A 180-degree shutter angle (x100) — what the camera reports in `d002`.
const D002_180: &str =
    "Label: PTP Property 0xd002\nType: RANGE\nCurrent: 18000\nBottom: 173\nTop: 36000\nEND";

/// The d006 choices of both bodies, x100, verbatim and in the camera's order.
const BMPCC_D006: [i64; 8] = [8, 2398, 2400, 2500, 2997, 3000, 5000, 5994];

fn raw(d006: &str, d007: &str) -> RawConfigs {
    RawConfigs {
        shutter_angle: D002_180.to_string(),
        project_fps: d006.to_string(),
        sensor_fps: d007.to_string(),
        ..Default::default()
    }
}

fn fps_req(fps: i64) -> SetRequest {
    SetRequest {
        fps: Some(fps),
        ..Default::default()
    }
}

// --- the read side: fps100 = d006, sensorFps100 = d007 x100 --------------------------------------

#[test]
fn pocket_6k_reports_d006_as_the_project_fps_and_d007_as_off_speed_1402() {
    let (p, caps) = params_and_caps(&raw(POCKET_6K_D006, POCKET_6K_D007));
    // The camera runs 60.00 — the owner's "realne ma 60". Not the off-speed 50 the panel showed.
    assert_eq!(
        p.fps100,
        Some(6000),
        "fps100 is d006 Current (already x100)"
    );
    assert_eq!(
        p.sensor_fps100,
        Some(5000),
        "sensorFps100 is d007 Current x100"
    );
    // The shutter denominator converts at the PROJECT fps: 180 deg at 60.00 = 1/120 s.
    // (At the off-speed 50 it read 1/100 — the swap.)
    assert_eq!(p.shutter, Some(120));
    assert_eq!(caps.shutter_choices, shutter_choices_for_fps(6000));
    // The settable project rates are the d006 choices; fps_min/fps_max are the off-speed RANGE.
    assert_eq!(caps.fps_choices, BMPCC_D006.to_vec());
    assert_eq!((caps.fps_min, caps.fps_max), (5, 60));
}

#[test]
fn pocket_4k_reports_d006_as_the_project_fps_and_d007_as_off_speed_1402() {
    let (p, caps) = params_and_caps(&raw(POCKET_4K_D006, POCKET_4K_D007));
    assert_eq!(p.fps100, Some(6000));
    assert_eq!(p.sensor_fps100, Some(6000));
    assert_eq!(p.shutter, Some(120));
    assert_eq!(caps.fps_choices, BMPCC_D006.to_vec());
}

#[test]
fn the_6k_and_the_4k_compare_the_same_against_a_60_grab_1402() {
    // With fps100 from d006 both bodies are Synced against the rig's 60 grab: no mismatch, no
    // button. The old d007 reading put the 6K at 50 -> a false Mismatch and a wrong write offer.
    for (d006, d007) in [
        (POCKET_6K_D006, POCKET_6K_D007),
        (POCKET_4K_D006, POCKET_4K_D007),
    ] {
        let (p, _) = params_and_caps(&raw(d006, d007));
        assert_eq!(FpsSync::classify(p.fps100, Some(60)), FpsSync::Synced);
    }
}

#[test]
fn an_absent_d006_reports_no_project_fps_never_the_off_speed_rate_1402() {
    // Reporting the off-speed rate as the project rate IS the bug. Without d006 the project fps is
    // not known (None -> FpsSync::Unknown), the off-speed rate still reads as sensorFps100.
    let (p, caps) = params_and_caps(&raw("", POCKET_6K_D007));
    assert_eq!(p.fps100, None);
    assert_eq!(p.sensor_fps100, Some(5000));
    assert!(caps.fps_choices.is_empty());
    assert_eq!(FpsSync::classify(p.fps100, Some(60)), FpsSync::Unknown);
}

#[test]
fn fps_choices_keep_the_camera_list_verbatim_including_the_8_entry_1402() {
    // Verbatim, in the camera's order: the `8` non-rate entry is one of the camera's own choices,
    // so it stays in the list the refusal names. 6000 is not listed on either body.
    let c = parse_fps100_choices(POCKET_6K_D006);
    assert_eq!(c, BMPCC_D006.to_vec());
    assert_eq!(c[0], 8);
    assert!(
        !c.contains(&6000),
        "60.00 is not a d006 choice on the BMPCC"
    );
    assert!(parse_fps100_choices("").is_empty());
    assert!(
        parse_fps100_choices(POCKET_6K_D007).is_empty(),
        "a RANGE has no choices"
    );
}

#[test]
fn fps_supported_keys_on_d006_not_on_the_off_speed_range_1402() {
    assert!(fps_supported(&raw(POCKET_6K_D006, POCKET_6K_D007)));
    assert!(fps_supported(&raw(POCKET_6K_D006, "")));
    // d007 alone is the off-speed rate: the project fps is not exposed.
    assert!(!fps_supported(&raw("", POCKET_6K_D007)));
    assert!(!fps_supported(&raw("", "")));
}

// --- fps_settable: d006 present AND the wanted value is one of its choices -----------------------

#[test]
fn fps_settable_only_for_a_listed_choice_1402() {
    for wanted in [2398, 2400, 2500, 2997, 3000, 5000, 5994] {
        assert!(fps_settable(&BMPCC_D006, wanted), "{wanted} is listed");
    }
    // 60.00 is the camera's CURRENT rate, but not one of its choices: not settable.
    assert!(!fps_settable(&BMPCC_D006, 6000));
    // Never a neighbour: 59.94 is listed, 59.95 / 59.93 are not.
    assert!(!fps_settable(&BMPCC_D006, 5995));
    assert!(!fps_settable(&BMPCC_D006, 5993));
    // No d006 -> nothing is settable.
    assert!(!fps_settable(&[], 2500));
    // A non-positive value never is.
    assert!(!fps_settable(&BMPCC_D006, 0));
    assert!(!fps_settable(&BMPCC_D006, -2500));
}

// --- the write side: d006 = the exact listed choice, everything else REFUSED ---------------------

#[test]
fn a_listed_fps_writes_exactly_that_d006_choice_1402() {
    let w = plan_writes(&fps_req(50), &[], 6000, &BMPCC_D006).expect("5000 is listed");
    assert_eq!(w, vec![("d006".to_string(), "5000".to_string())]);
    let w = plan_writes(&fps_req(25), &[], 6000, &BMPCC_D006).expect("2500 is listed");
    assert_eq!(w, vec![("d006".to_string(), "2500".to_string())]);
}

#[test]
fn a_shutter_planned_with_an_fps_write_converts_at_the_new_project_fps_1402() {
    // d002 is the shutter ANGLE, written after d006 (next test), so the camera reads it at the NEW
    // project rate. A request setting the project fps to 50.00 AND a 1/100 shutter must therefore
    // convert at 50.00: 180 deg = 18000, never at the old 25.00 (9000 = 1/200 at 50).
    let req = SetRequest {
        shutter: Some(100),
        fps: Some(50),
        ..Default::default()
    };
    let w = plan_writes(&req, &[], 2500, &BMPCC_D006).expect("5000 is listed");
    assert!(
        w.contains(&("d002".to_string(), "18000".to_string())),
        "{w:?}"
    );
    assert!(
        w.contains(&("d006".to_string(), "5000".to_string())),
        "{w:?}"
    );
    // Without an fps write the shutter still converts at the camera's current project fps.
    let w = plan_writes(
        &SetRequest {
            shutter: Some(100),
            ..Default::default()
        },
        &[],
        2500,
        &BMPCC_D006,
    )
    .expect("no fps");
    assert_eq!(w, vec![("d002".to_string(), "9000".to_string())]);
}

#[test]
fn the_project_fps_is_written_before_every_other_value_1402() {
    // Review round 2: whether a BMPCC keeps the shutter ANGLE or the shutter SPEED across a project
    // frame-rate change is not verified. Writing d006 FIRST makes the d002 angle (converted at the
    // new rate) land while the camera already runs that rate, so it is right either way, and every
    // other value lands at the rate the camera then runs.
    let req = SetRequest {
        aperture_norm: Some(0.0),
        iso: Some(800),
        kelvin: Some(5600),
        tint: Some(0),
        shutter: Some(100),
        fps: Some(50),
        auto_wb: None,
    };
    let labels = ["f/2.8".to_string(), "f/4.0".to_string()];
    let w = plan_writes(&req, &labels, 2500, &BMPCC_D006).expect("5000 is listed");
    assert_eq!(
        w[0],
        ("d006".to_string(), "5000".to_string()),
        "the frame rate goes first: {w:?}"
    );
    assert_eq!(w.len(), 6, "{w:?}");
}

#[test]
fn a_60_fps_write_is_refused_with_a_named_error_listing_the_choices_1402() {
    let err = plan_writes(&fps_req(60), &[], 6000, &BMPCC_D006).expect_err("6000 is not listed");
    assert_eq!(
        err,
        FpsNotSettable {
            fps: 60,
            choices: BMPCC_D006.to_vec(),
        }
    );
    let msg = err.to_string();
    assert!(msg.starts_with("fps-not-settable:"), "named error: {msg}");
    assert!(msg.contains("6000"), "names the refused value x100: {msg}");
    assert!(
        msg.contains("[8, 2398, 2400, 2500, 2997, 3000, 5000, 5994]"),
        "lists the camera's own choices verbatim: {msg}"
    );
    // A std error, so the relay can carry and downcast it.
    let _: &dyn std::error::Error = &err;
}

#[test]
fn a_refused_fps_refuses_the_whole_request_nothing_is_planned_1402() {
    // An fps the camera would refuse takes the rest of its request with it: no partial write.
    let req = SetRequest {
        iso: Some(800),
        shutter: Some(120),
        fps: Some(60),
        ..Default::default()
    };
    assert!(plan_writes(&req, &[], 6000, &BMPCC_D006).is_err());
}

#[test]
fn no_integer_fps_is_ever_rounded_redirected_or_mapped_onto_the_8_entry_1402() {
    // Every integer fps a request can carry: only an EXACT listed choice is written, always to d006
    // as fps*100 — 24 -> 2400 (never 23.98), 30 -> 3000 (never 29.97), 60 refused (never 59.94),
    // and the `8` entry is unreachable. Nothing ever goes to d007.
    let mut accepted = Vec::new();
    for fps in 0..=240 {
        match plan_writes(&fps_req(fps), &[], 6000, &BMPCC_D006) {
            Ok(w) => {
                assert_eq!(w, vec![("d006".to_string(), (fps * 100).to_string())]);
                accepted.push(fps);
            }
            Err(e) => assert_eq!(e.fps, fps),
        }
    }
    assert_eq!(accepted, vec![24, 25, 30, 50]);
}

#[test]
fn a_camera_without_d006_refuses_every_fps_and_still_takes_other_writes_1402() {
    let err = plan_writes(&fps_req(25), &[], 2500, &[]).expect_err("no d006 choices");
    assert!(err.choices.is_empty());
    assert!(
        err.to_string().contains("[]"),
        "names the empty list: {err}"
    );
    // A request without fps is unaffected by the missing d006.
    let w = plan_writes(
        &SetRequest {
            iso: Some(800),
            ..Default::default()
        },
        &[],
        2500,
        &[],
    )
    .expect("no fps -> nothing to refuse");
    assert_eq!(w, vec![("iso".to_string(), "800".to_string())]);
}

#[test]
fn an_overflowing_fps_is_refused_never_a_panic_1402() {
    let err = plan_writes(&fps_req(i64::MAX), &[], 6000, &BMPCC_D006).expect_err("not listed");
    assert_eq!(err.fps, i64::MAX);
    assert!(err.to_string().starts_with("fps-not-settable:"));
}

// --- wire: the choice list and the align flag travel camelCase, default empty/false --------------

#[test]
fn camera_caps_carries_fps_choices_camel_case_and_defaults_empty_1402() {
    let caps = CameraCaps {
        iso_choices: vec![400],
        fnumber_choices: vec![],
        shutter_choices: vec![120],
        fps_choices: BMPCC_D006.to_vec(),
        fps_min: 5,
        fps_max: 60,
        kelvin_min: 2500,
        kelvin_max: 10000,
    };
    let json = serde_json::to_string(&caps).unwrap();
    assert!(
        json.contains("\"fpsChoices\":[8,2398,2400,2500,2997,3000,5000,5994]"),
        "{json}"
    );
    let back: CameraCaps = serde_json::from_str(&json).unwrap();
    assert_eq!(back, caps);
    // An older relay that does not send it deserializes to an empty list (nothing settable).
    let legacy = "{\"isoChoices\":[400],\"shutterChoices\":[120],\"fpsMin\":5,\"fpsMax\":60,\"kelvinMin\":2500,\"kelvinMax\":10000}";
    let old: CameraCaps = serde_json::from_str(legacy).unwrap();
    assert!(old.fps_choices.is_empty());
}

#[test]
fn camera_view_carries_fps_align_settable_camel_case_and_defaults_false_1402() {
    let view = CameraView {
        id: "cam3".into(),
        label: "Cam 3".into(),
        transport: Transport::SbcRelay,
        has_preview: false,
        preview_live: false,
        reachable: true,
        grab_fps: Some(50),
        grab_fps_desync: false,
        fps_sync: FpsSync::Mismatch,
        fps_align_settable: true,
        state: None,
    };
    let json = serde_json::to_string(&view).unwrap();
    assert!(json.contains("\"fpsAlignSettable\":true"), "{json}");
    let back: CameraView = serde_json::from_str(&json).unwrap();
    assert_eq!(back, view);
    // An older service that does not send it -> false: the panel offers no align button.
    let legacy = "{\"id\":\"cam3\",\"label\":\"Cam 3\",\"transport\":\"sbc-relay\",\"hasPreview\":false,\"reachable\":true,\"grabFps\":50,\"fpsSync\":\"mismatch\",\"state\":null}";
    let old: CameraView = serde_json::from_str(legacy).unwrap();
    assert!(!old.fps_align_settable);
}
