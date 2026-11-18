use super::*;
use crate::behave::holds::{FLASH_MS, VALUE_HOLD_MS};
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
        address: None,
        tally: None,
        control: ControlStatus::Ok,
    }
}

fn with(f: impl FnOnce(&mut ShadingParams)) -> CameraView {
    let mut c = cam();
    f(&mut c.state.as_mut().unwrap().params);
    c
}

fn kv(p: Press) -> (&'static str, serde_json::Value) {
    (p.key, p.value)
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
    assert_eq!(kv(p), ("iso", serde_json::json!(800)));
    // The same (stale) push still shows 400: the panel holds 800, pending, the bar runs.
    let r = row(&s.view(&c, 100.0), Row::Iso);
    assert_eq!((r.text.as_str(), r.pending, r.busy), ("800", true, true));
    assert!(!r.inc.enabled, "800 is the last choice");
    // The confirmation ends the hold.
    let r = row(&s.view(&with(|p| p.iso = Some(800)), 200.0), Row::Iso);
    assert_eq!((r.text.as_str(), r.pending), ("800", false));
}

/// shading#29: a double press, or a press while the row's write is unanswered, writes nothing;
/// after the answer the next press builds on the held target.
#[test]
fn a_row_is_locked_until_its_write_is_answered_then_builds_on_the_held_target() {
    let mut s = Steppers::default();
    let c = cam();
    let first = s.press(Row::Kelvin, 1, &c, 0.0).unwrap();
    assert_eq!(first.value, serde_json::json!(5700));
    assert_eq!(s.press(Row::Kelvin, 1, &c, 10.0), None, "a double press");
    assert_eq!(
        s.press(Row::Kelvin, -1, &c, 1000.0),
        None,
        "− is locked too"
    );
    assert_eq!(
        s.press(Row::Tint, -1, &c, 10.0).unwrap().value,
        serde_json::json!(-1),
        "another row is free"
    );
    s.answered("kelvin", first.id, WriteAnswer::Accepted, 1500.0);
    let r = row(&s.view(&c, 1500.0), Row::Kelvin);
    assert_eq!(
        (r.text.as_str(), r.pending, r.busy),
        ("5700 K", true, false),
        "answered: the bar ends, the target stays against the stale push"
    );
    assert_eq!(
        s.press(Row::Kelvin, 1, &c, 1500.0).unwrap().value,
        serde_json::json!(5800)
    );
    assert_eq!(row(&s.view(&c, 1600.0), Row::Tint).text, "-1");
}

#[test]
fn a_fast_answer_still_ignores_the_second_press_of_a_double_click() {
    let mut s = Steppers::default();
    let c = cam();
    let p = s.press(Row::Iso, -1, &c, 0.0).unwrap();
    s.answered("iso", p.id, WriteAnswer::Accepted, 40.0);
    assert_eq!(s.press(Row::Iso, -1, &c, 120.0), None);
    assert!(s.press(Row::Iso, -1, &c, 300.0).is_some());
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
fn the_aperture_step_sends_the_direction_and_shows_the_next_grid_stop() {
    let mut s = Steppers::default();
    let c = cam();
    assert_eq!(
        kv(s.press(Row::Aperture, 1, &c, 0.0).unwrap()),
        ("apertureStep", serde_json::json!(1))
    );
    let r = row(&s.view(&c, 100.0), Row::Aperture);
    assert_eq!(
        (r.text.as_str(), r.pending, r.busy),
        ("f/8.0", true, true),
        "the grid's next stop at once"
    );
    // The lens moved to its own next step (5.6, between the grid's 5.2 and 8.0): the hold ends
    // and the camera's real f-stop shows.
    let r = row(
        &s.view(&with(|p| p.aperture_av = Some(2.0 * 5.6f64.log2())), 200.0),
        Row::Aperture,
    );
    assert_eq!(
        (r.text.as_str(), r.pending, r.flash),
        ("f/5.6", false, false)
    );
}

#[test]
fn the_next_aperture_stop_is_the_grids_else_a_third_stop() {
    let grid = [2.8, 4.0, 5.2, 8.0];
    // A pushed f-number is never exactly a grid entry (f/5.2 = AV 4.78 = 5.198...).
    let f52 = f_number_from_av(4.78);
    assert_eq!(aperture_next(f52, &grid, 1), 8.0);
    assert_eq!(aperture_next(f52, &grid, -1), 4.0);
    assert_eq!(
        aperture_next(4.5, &grid, -1),
        4.0,
        "off the grid: the one below"
    );
    // shading#22: the lens opens past the grid's first entry; a third of a stop on.
    let wider = aperture_next(2.8, &grid, -1);
    assert!((wider - 2.8 / 2f64.powf(1.0 / 6.0)).abs() < 1e-9, "{wider}");
    assert!((aperture_next(8.0, &grid, 1) - 8.0 * 2f64.powf(1.0 / 6.0)).abs() < 1e-9);
    assert!((aperture_next(4.0, &[], 1) - 4.0 * 2f64.powf(1.0 / 6.0)).abs() < 1e-9);
}

#[test]
fn an_aperture_press_past_the_grid_shows_a_third_stop_and_an_unknown_f_still_steps() {
    let at_first = with(|p| {
        p.aperture_av = Some(2.0 * 2.8f64.log2());
        p.iris_range_pos = Some(0.2);
    });
    let mut s = Steppers::default();
    s.press(Row::Aperture, -1, &at_first, 0.0).unwrap();
    assert_eq!(row(&s.view(&at_first, 1.0), Row::Aperture).text, "f/2.5");
    let unknown = with(|p| p.aperture_av = None);
    let mut s = Steppers::default();
    assert_eq!(
        s.press(Row::Aperture, 1, &unknown, 0.0).unwrap().value,
        serde_json::json!(1)
    );
    assert_eq!(row(&s.view(&unknown, 1.0), Row::Aperture).text, "f/—");
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
    assert!(!r.dec.enabled && r.dec.end && r.inc.enabled && !r.inc.end);
    assert_eq!(r.dec.title, "Objektív je na najotvorenejšej clone");
    let closed = with(|p| p.iris_range_pos = Some(1.0));
    let r = row(&Steppers::default().view(&closed, 0.0), Row::Aperture);
    assert!(r.dec.enabled && !r.inc.enabled && r.inc.end);
    let mid = with(|p| p.iris_range_pos = Some(0.4));
    let r = row(&Steppers::default().view(&mid, 0.0), Row::Aperture);
    assert!(r.dec.enabled && r.inc.enabled && !r.dec.end && !r.inc.end);
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
    let r = row(
        &Steppers::default().view(&at_grid_first, 0.0),
        Row::Aperture,
    );
    assert_eq!(r.text, "f/2.8");
    assert!(r.dec.enabled && r.inc.enabled && !r.dec.end && !r.inc.end);
    let at_grid_last = with(|p| {
        p.aperture_av = Some(6.0);
        p.iris_range_pos = Some(0.8);
    });
    let r = row(&Steppers::default().view(&at_grid_last, 0.0), Row::Aperture);
    assert_eq!(r.text, "f/8.0");
    assert!(r.dec.enabled && r.inc.enabled && !r.dec.end && !r.inc.end);
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
    assert!(ap.not_applied && !ap.pending && !ap.busy);
    assert_eq!(ap.text, "f/5.2", "reverts to the camera's value");
    assert!(ap.flash, "and flashes red");
    assert!(!row(&s.view(&c, 50.0 + FLASH_MS), Row::Aperture).flash);
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
        kv(s.press(Row::Fps, -1, &c, 0.0).unwrap()),
        ("fps", serde_json::json!(50))
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
fn a_failed_write_reverts_and_flashes() {
    let mut s = Steppers::default();
    let c = cam();
    let p = s.press(Row::Kelvin, 1, &c, 0.0).unwrap();
    s.answered("kelvin", p.id, WriteAnswer::Failed, 1.0);
    let r = row(&s.view(&c, 1.0), Row::Kelvin);
    assert_eq!((r.text.as_str(), r.busy, r.flash), ("5600 K", false, true));
    // The slider's aperture write answers under `apertureNorm`: the aperture row's hold.
    let mut s = Steppers::default();
    let p = s.slide(Row::Aperture, 3.0, &c, 0.0).unwrap();
    assert!(row(&s.view(&c, 1.0), Row::Aperture).busy);
    s.answered("apertureNorm", p.id, WriteAnswer::Accepted, 2.0);
    assert!(!row(&s.view(&c, 2.0), Row::Aperture).busy);
    s.answered("autoWb", 0, WriteAnswer::Failed, 3.0);
    assert!(
        s.view(&c, 3.0).iter().all(|r| !r.flash),
        "no row holds autoWb"
    );
}

#[test]
fn an_unanswered_write_times_out_red_an_answered_one_settles_quietly() {
    let c = cam();
    let mut s = Steppers::default();
    s.press(Row::Tint, 1, &c, 0.0).unwrap();
    let r = row(&s.view(&c, VALUE_HOLD_MS), Row::Tint);
    assert_eq!((r.text.as_str(), r.busy, r.flash), ("0", false, true));
    let mut s = Steppers::default();
    let p = s.press(Row::Tint, 1, &c, 0.0).unwrap();
    s.answered("tint", p.id, WriteAnswer::Accepted, 10.0);
    let r = row(&s.view(&c, VALUE_HOLD_MS), Row::Tint);
    assert_eq!((r.text.as_str(), r.flash), ("0", false));
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
    let p = s.press(Row::Kelvin, 1, &c, 5.0).unwrap();
    let r = row(&s.view(&c, 10.0), Row::Kelvin);
    assert_eq!(
        (r.text.as_str(), r.pending),
        ("5700 K", true),
        "the same view (a push about another camera, the clock) cannot refuse the new tap"
    );
    // A newer view of this camera (the tally moved) still carries the EARLIER refusal: not this
    // write's, until its answer is in (shading#29 review).
    c.state.as_mut().unwrap().params.iso = Some(800);
    let r = row(&s.view(&c, 20.0), Row::Kelvin);
    assert!(r.pending && r.busy, "{r:?}");
    s.answered("kelvin", p.id, WriteAnswer::Accepted, 30.0);
    let r = row(&s.view(&c, 30.0), Row::Kelvin);
    assert!(!r.pending && r.not_applied && r.flash);
}

/// shading#24: a button off at a range END carries the end flag (the card lays `min`/`max` over
/// it); a button off because the value or the steps are unknown does not.
#[test]
fn only_a_reached_end_draws_the_end_overlay() {
    let low = with(|p| p.iso = Some(100));
    let r = row(&Steppers::default().view(&low, 0.0), Row::Iso);
    assert!(
        !r.dec.enabled && r.dec.end,
        "ISO 100 = the first listed choice"
    );
    assert!(r.inc.enabled && !r.inc.end);
    let high = with(|p| p.kelvin = Some(10000));
    let r = row(&Steppers::default().view(&high, 0.0), Row::Kelvin);
    assert!(!r.inc.enabled && r.inc.end && !r.dec.end);
    let unknown = with(|p| p.tint = None);
    let r = row(&Steppers::default().view(&unknown, 0.0), Row::Tint);
    assert!(!r.dec.enabled && !r.dec.end && !r.inc.enabled && !r.inc.end);
    let mut offline = cam();
    offline.reachable = false;
    for r in Steppers::default().view(&offline, 0.0) {
        assert!(
            !r.dec.end && !r.inc.end,
            "{:?}: an offline camera has no end",
            r.row
        );
    }
    let mut no_caps = cam();
    no_caps.state.as_mut().unwrap().caps = None;
    let r = row(&Steppers::default().view(&no_caps, 0.0), Row::Shutter);
    assert!(!r.dec.enabled && !r.dec.end, "no steps is not an end");
}

/// shading#24: a refused key keeps its button enabled and never flags an end.
#[test]
fn a_refused_enabled_button_is_not_an_end() {
    let c = cam();
    let mut refused = c.clone();
    refused.state.as_mut().unwrap().not_applied = vec!["iso".into()];
    let r = row(&Steppers::default().view(&refused, 0.0), Row::Iso);
    assert!(r.dec.enabled && !r.dec.end);
    assert_eq!(r.dec.title, NOT_APPLIED_TITLE);
}

#[test]
fn the_rows_sit_in_the_exp_and_sys_panes_with_sentence_case_labels() {
    let exp: Vec<&str> = Row::ALL
        .iter()
        .filter(|r| r.pane() == Pane::Exp)
        .map(|r| r.label())
        .collect();
    let sys: Vec<&str> = Row::ALL
        .iter()
        .filter(|r| r.pane() == Pane::Sys)
        .map(|r| r.label())
        .collect();
    assert_eq!(exp, ["Clona", "ISO", "Biely bod", "Tint"]);
    assert_eq!(sys, ["Uzávierka", "FPS"]);
    assert_eq!(Pane::default(), Pane::Exp);
    let words: Vec<(&str, &str)> = Pane::ALL.iter().map(|p| (p.label(), p.word())).collect();
    assert_eq!(words, [("EXP", "exp"), ("SYS", "sys")]);
}

/// Review findings: the slider respects the row lock; the fps align button (a write no hold owns,
/// id 0) never touches a pending fps press; a Step hold's guess is only shown, the next press
/// builds on the camera's f-stop.
#[test]
fn the_slider_waits_for_the_rows_answer_and_foreign_writes_leave_the_hold_alone() {
    let c = cam();
    let mut s = Steppers::default();
    let p = s.press(Row::Iso, -1, &c, 0.0).unwrap();
    assert_eq!(
        s.slide(Row::Iso, 0.0, &c, 500.0),
        None,
        "locked: the thumb goes back"
    );
    s.answered("iso", p.id, WriteAnswer::Accepted, 600.0);
    assert!(s.slide(Row::Iso, 0.0, &c, 700.0).is_some());

    let mut s = Steppers::default();
    s.press(Row::Fps, -1, &c, 0.0).unwrap();
    s.answered("fps", 0, WriteAnswer::Failed, 10.0);
    let r = row(&s.view(&c, 20.0), Row::Fps);
    assert_eq!((r.text.as_str(), r.busy, r.flash), ("50.00", true, false));
}

#[test]
fn the_next_aperture_press_builds_on_the_cameras_f_stop_not_the_guess() {
    let c = cam();
    let mut s = Steppers::default();
    let p = s.press(Row::Aperture, -1, &c, 0.0).unwrap();
    assert_eq!(row(&s.view(&c, 1.0), Row::Aperture).text, "f/4.0");
    s.answered("apertureStep", p.id, WriteAnswer::Accepted, 100.0);
    // No push moved the lens yet: the next − guesses from f/5.2 again, never from the guess f/4.0.
    s.press(Row::Aperture, -1, &c, 500.0).unwrap();
    assert_eq!(row(&s.view(&c, 501.0), Row::Aperture).text, "f/4.0");
}

/// Review finding: a lens below the grid's start (f/1.75 on the default 2.8.. grid, shading#22)
/// shows a third of a stop, not the grid's f/2.8 more than a stop away; so does one above its end.
#[test]
fn outside_the_grid_the_guess_is_a_third_of_a_stop() {
    let grid = [2.8, 4.0, 5.6, 8.0];
    let g = aperture_next(1.75, &grid, 1);
    assert!((g - 1.75 * 2f64.powf(1.0 / 6.0)).abs() < 1e-9, "{g}");
    assert_eq!(
        aperture_next(2.8, &grid, 1),
        4.0,
        "inside the grid: its next entry"
    );
    let g = aperture_next(11.0, &grid, -1);
    assert!(
        (g - 11.0 / 2f64.powf(1.0 / 6.0)).abs() < 1e-9,
        "above the grid: {g}"
    );
}
