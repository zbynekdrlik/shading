use super::*;
use bkshading_proto::wire::{
    CameraCaps, CameraView, ControlStatus, FpsSync, RelayState, ShadingParams, Transport,
};

/// cam1 online at f/5.2 (AV 4.78), ISO 400, 5600 K, tint 0, 1/60, 60.00 fps — the JS panel
/// E2E's fixture.
pub(crate) fn cam() -> CameraView {
    CameraView {
        id: "cam1".into(),
        label: "Cam 1".into(),
        transport: Transport::CamboxRelay,
        has_preview: false,
        preview_live: false,
        reachable: true,
        grab_fps: None,
        grab_fps_desync: false,
        fps_sync: FpsSync::Unknown,
        fps_align_settable: false,
        state: Some(RelayState {
            online: true,
            camera: Some("Pocket 4K".into()),
            params: ShadingParams {
                aperture_av: Some(4.78),
                aperture_norm: Some(2.0 / 3.0),
                iso: Some(400),
                kelvin: Some(5600),
                tint: Some(0),
                shutter: Some(60),
                fps100: Some(6000),
                sensor_fps100: Some(6000),
                focus_distance: None,
                focus_norm: None,
                iris_range_pos: None,
            },
            caps: Some(CameraCaps {
                iso_choices: vec![100, 200, 400, 800],
                fnumber_choices: vec![2.8, 4.0, 5.2, 8.0],
                shutter_choices: vec![60, 100, 125],
                fps_choices: vec![8, 2398, 2400, 2500, 2997, 3000, 5000, 5994, 6000],
                fps_min: 5,
                fps_max: 60,
                kelvin_min: 2500,
                kelvin_max: 10000,
            }),
            fps_supported: true,
            capture_fps: None,
            version: "0.2.7".into(),
            not_applied: vec![],
            offline_reason: None,
        }),
        registration: None,
        control: ControlStatus::Ok,
    }
}

fn with(f: impl FnOnce(&mut ShadingParams)) -> CameraView {
    let mut c = cam();
    f(&mut c.state.as_mut().unwrap().params);
    c
}

fn row(views: &[RowView], r: Row) -> RowView {
    views.iter().find(|v| v.row == r).unwrap().clone()
}

#[test]
fn every_row_shows_the_cameras_value() {
    let v = Steppers::default().view(&cam(), 0.0);
    let texts: Vec<String> = v.iter().map(|r| r.text.clone()).collect();
    assert_eq!(texts, ["f/5.2", "400", "5600 K", "0", "1/60", "60.00"]);
    assert!(v.iter().all(|r| !r.pending && !r.not_applied));
}

#[test]
fn an_offline_camera_shows_dashes_and_no_step() {
    let mut c = cam();
    c.state.as_mut().unwrap().online = false;
    let v = Steppers::default().view(&c, 0.0);
    assert!(v.iter().all(|r| r.text.ends_with('—')), "{v:?}");
    assert!(v.iter().all(|r| !r.dec.enabled && !r.inc.enabled));
}

#[test]
fn step_choice_mirrors_the_proto_rule() {
    let list = [60.0, 100.0, 125.0];
    assert_eq!(
        step_choice(50.0, &list, 1),
        Some(0),
        "off-grid below: onto the grid"
    );
    assert_eq!(step_choice(50.0, &list, -1), Some(0));
    assert_eq!(step_choice(100.0, &list, 1), Some(2));
    assert_eq!(step_choice(100.0, &list, -1), Some(0));
    assert_eq!(step_choice(125.0, &list, 1), Some(2), "stays on the end");
    assert_eq!(
        step_choice(110.0, &list, -1),
        Some(1),
        "between: the last below"
    );
    assert_eq!(step_choice(1.0, &[], 1), None);
    assert_eq!(step_choice(1.0, &[5.0], -1), Some(0));
}

#[test]
fn iso_plus_writes_the_next_listed_value_and_holds_it() {
    let mut s = Steppers::default();
    let c = cam();
    let p = s.press(Row::Iso, 1, &c, 0.0).unwrap();
    assert_eq!(
        p,
        Press {
            key: "iso",
            value: serde_json::json!(800)
        }
    );
    // The same (stale) push still shows 400: the panel holds 800, pending.
    let r = row(&s.view(&c, 100.0), Row::Iso);
    assert_eq!((r.text.as_str(), r.pending), ("800", true));
    assert!(!r.inc.enabled, "800 is the last choice");
    // The confirmation ends the hold.
    let r = row(&s.view(&with(|p| p.iso = Some(800)), 200.0), Row::Iso);
    assert_eq!((r.text.as_str(), r.pending), ("800", false));
}

#[test]
fn a_second_tap_builds_on_the_held_target() {
    let mut s = Steppers::default();
    let c = cam();
    assert_eq!(
        s.press(Row::Kelvin, 1, &c, 0.0).unwrap().value,
        serde_json::json!(5700)
    );
    assert_eq!(
        s.press(Row::Kelvin, 1, &c, 10.0).unwrap().value,
        serde_json::json!(5800)
    );
    assert_eq!(
        s.press(Row::Tint, -1, &c, 10.0).unwrap().value,
        serde_json::json!(-1)
    );
    assert_eq!(row(&s.view(&c, 20.0), Row::Tint).text, "-1");
}

#[test]
fn linear_steps_clamp_and_disable_at_the_ends() {
    let mut s = Steppers::default();
    let c = with(|p| {
        p.kelvin = Some(10000);
        p.tint = Some(-50);
    });
    let v = s.view(&c, 0.0);
    assert!(!row(&v, Row::Kelvin).inc.enabled);
    assert!(row(&v, Row::Kelvin).dec.enabled);
    assert!(!row(&v, Row::Tint).dec.enabled);
    assert_eq!(
        s.press(Row::Kelvin, 1, &c, 0.0),
        None,
        "a disabled + writes nothing"
    );
    let c = with(|p| p.kelvin = Some(9950));
    assert_eq!(
        Steppers::default()
            .press(Row::Kelvin, 1, &c, 0.0)
            .unwrap()
            .value,
        serde_json::json!(10000)
    );
}

#[test]
fn shutter_off_the_grid_steps_onto_it() {
    let c = with(|p| p.shutter = Some(50));
    let mut s = Steppers::default();
    let v = row(&s.view(&c, 0.0), Row::Shutter);
    assert!(v.dec.enabled && v.inc.enabled, "off-grid: both directions");
    assert_eq!(
        s.press(Row::Shutter, 1, &c, 0.0).unwrap().value,
        serde_json::json!(60)
    );
}

#[test]
fn the_aperture_step_sends_the_direction_and_holds_the_shown_f_stop() {
    let mut s = Steppers::default();
    let c = cam();
    assert_eq!(
        s.press(Row::Aperture, 1, &c, 0.0).unwrap(),
        Press {
            key: "apertureStep",
            value: serde_json::json!(1)
        }
    );
    let r = row(&s.view(&c, 100.0), Row::Aperture);
    assert_eq!((r.text.as_str(), r.pending), ("f/5.2", true));
    // The lens moved: the hold ends and the true f-stop shows.
    let r = row(
        &s.view(&with(|p| p.aperture_av = Some(6.0)), 200.0),
        Row::Aperture,
    );
    assert_eq!((r.text.as_str(), r.pending), ("f/8.0", false));
}

#[test]
fn the_aperture_stops_at_the_lens_ends() {
    // Wide open at f/4.0 (not the grid's first entry): only irisRangePos 0 says so.
    let wide = with(|p| {
        p.aperture_av = Some(4.0);
        p.iris_range_pos = Some(0.0);
    });
    let r = row(&Steppers::default().view(&wide, 0.0), Row::Aperture);
    assert_eq!(r.text, "f/4.0");
    assert!(!r.dec.enabled && r.inc.enabled && r.at_end);
    assert_eq!(r.dec.title, "Objektív je na najotvorenejšej clone");
    let closed = with(|p| p.iris_range_pos = Some(1.0));
    let r = row(&Steppers::default().view(&closed, 0.0), Row::Aperture);
    assert!(r.dec.enabled && !r.inc.enabled && r.at_end);
    let mid = with(|p| p.iris_range_pos = Some(0.4));
    let r = row(&Steppers::default().view(&mid, 0.0), Row::Aperture);
    assert!(r.dec.enabled && r.inc.enabled && !r.at_end);
    assert_eq!(r.pos, Some(0.4));
}

/// shading#22 (PP cam1, 18.11.2026): without `lens/iris/description` the grid is the default
/// 2.8..22 until the relay SEES a wider stop, while the lens opens to f/1.75. The grid's ends are not
/// the lens's: with the camera's own `irisRangePos` only that gates the steps.
#[test]
fn the_grid_ends_do_not_gate_the_aperture_when_the_camera_gives_its_lens_position() {
    let at_grid_first = with(|p| {
        p.aperture_av = Some(2.0 * 2.8f64.log2());
        p.iris_range_pos = Some(0.2);
    });
    let r = row(&Steppers::default().view(&at_grid_first, 0.0), Row::Aperture);
    assert_eq!(r.text, "f/2.8");
    assert!(r.dec.enabled && r.inc.enabled && !r.at_end);
    let at_grid_last = with(|p| {
        p.aperture_av = Some(6.0);
        p.iris_range_pos = Some(0.8);
    });
    let r = row(&Steppers::default().view(&at_grid_last, 0.0), Row::Aperture);
    assert_eq!(r.text, "f/8.0");
    assert!(r.dec.enabled && r.inc.enabled && !r.at_end);
    // Without the camera's word the grid's ends still gate (nothing better is known).
    let no_pos = with(|p| p.aperture_av = Some(2.0 * 2.8f64.log2()));
    let r = row(&Steppers::default().view(&no_pos, 0.0), Row::Aperture);
    assert!(!r.dec.enabled && r.inc.enabled);
}

#[test]
fn no_choices_disable_with_a_reason() {
    let mut c = cam();
    c.state
        .as_mut()
        .unwrap()
        .caps
        .as_mut()
        .unwrap()
        .fnumber_choices
        .clear();
    c.state
        .as_mut()
        .unwrap()
        .caps
        .as_mut()
        .unwrap()
        .iso_choices
        .clear();
    let v = Steppers::default().view(&c, 0.0);
    let ap = row(&v, Row::Aperture);
    assert!(!ap.dec.enabled && !ap.inc.enabled);
    assert!(ap.dec.title.contains("Kroky clony"));
    assert!(!row(&v, Row::Iso).inc.enabled);
}

#[test]
fn a_not_applied_key_flags_its_row_and_ends_its_hold() {
    let mut s = Steppers::default();
    s.press(Row::Aperture, 1, &cam(), 0.0).unwrap();
    let mut c = cam();
    c.state.as_mut().unwrap().not_applied = vec!["apertureStep".into()];
    let v = s.view(&c, 50.0);
    let ap = row(&v, Row::Aperture);
    assert!(ap.not_applied && !ap.pending);
    assert_eq!(ap.inc.title, NOT_APPLIED_TITLE);
    assert!(!row(&v, Row::Iso).not_applied, "only the refused key");
}

#[test]
fn fps_steps_only_over_whole_listed_rates() {
    let c = cam();
    assert_eq!(
        whole_fps_choices(c.state.as_ref().unwrap().caps.as_ref()),
        vec![24.0, 25.0, 30.0, 50.0, 60.0],
        "the 8 entry and 23.98/29.97/59.94 are not steps"
    );
    let mut s = Steppers::default();
    assert_eq!(
        s.press(Row::Fps, -1, &c, 0.0).unwrap(),
        Press {
            key: "fps",
            value: serde_json::json!(50)
        }
    );
    assert_eq!(row(&s.view(&c, 1.0), Row::Fps).text, "50.00");
    let mut none = cam();
    none.state
        .as_mut()
        .unwrap()
        .caps
        .as_mut()
        .unwrap()
        .fps_choices
        .clear();
    let r = row(&Steppers::default().view(&none, 0.0), Row::Fps);
    assert!(!r.dec.enabled && !r.inc.enabled);
}

#[test]
fn a_failed_write_drops_the_hold() {
    let mut s = Steppers::default();
    let c = cam();
    s.press(Row::Kelvin, 1, &c, 0.0).unwrap();
    s.write_failed("kelvin");
    assert_eq!(row(&s.view(&c, 1.0), Row::Kelvin).text, "5600 K");
}

#[test]
fn the_bar_sits_at_the_value() {
    let v = Steppers::default().view(&cam(), 0.0);
    assert_eq!(row(&v, Row::Iso).pos, Some(2.0 / 3.0));
    assert_eq!(row(&v, Row::Tint).pos, Some(0.5));
    assert_eq!(row(&v, Row::Shutter).pos, Some(0.0));
    assert_eq!(nearest_index(&[60.0, 100.0, 125.0], 110.0), 1);
}

#[test]
fn tint_and_f_numbers_read_like_the_mockup() {
    assert_eq!(tint_text(3), "+3");
    assert_eq!(tint_text(0), "0");
    assert_eq!(tint_text(-2), "-2");
    assert!((f_number_from_av(4.0) - 4.0).abs() < 1e-9);
    assert!(iris_at_end(Some(0.0005), -1));
    assert!(!iris_at_end(None, -1));
    assert!(!iris_at_end(Some(0.5), 0));
}

#[test]
fn a_retap_against_a_view_that_still_carries_a_refusal_stays_pending() {
    let mut c = cam();
    c.state.as_mut().unwrap().not_applied = vec!["kelvin".into()];
    let mut s = Steppers::default();
    s.view(&c, 0.0);
    s.press(Row::Kelvin, 1, &c, 5.0).unwrap();
    let r = row(&s.view(&c, 10.0), Row::Kelvin);
    assert_eq!(
        (r.text.as_str(), r.pending),
        ("5700 K", true),
        "the same view (a push about another camera, the clock) cannot refuse the new tap"
    );
    // A newer view of this camera still refusing kelvin ends the hold.
    c.state.as_mut().unwrap().params.iso = Some(800);
    let r = row(&s.view(&c, 20.0), Row::Kelvin);
    assert!(!r.pending && r.not_applied);
}
