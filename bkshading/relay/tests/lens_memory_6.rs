//! shading#6 (0.2.5): faster aperture steps on cam5 (Pocket 4K fw 9.8, zoom at 108 mm, live
//! 10.11.2026). Each +/- press took 4.2-5.9 s: the lens has 1/3-stop steps, the relay probes 1/16
//! stop at a time, and every probe the iris does not follow waited the full 1.2 s although the iris
//! reads back moved within ~340 ms. At 108 mm the wide end 4.4 reads `normalised` 0.0387 (measured
//! against the description min 4.1), so every "-" at 4.4 probed 5 s and answered quietly.
//!
//! The relay now waits 0.5 s per probe, remembers the lens's ladder per focal length (`lens/zoom
//! focalLength`) and writes an observed-adjacent neighbour directly, and learns the lens's ends per
//! focal length: a press toward a learned end is refused at once, and `irisRangePos` reads 0 / 1
//! there. These run the real session against the fake camera in `support/`.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use bkshading_proto::wire::{RelayState, SetRequest};
use bkshading_relay::transport::{
    ApplyOutcome, CameraAddr, CameraLocator, CameraSession, IRIS_PROBE_WAIT_MS,
};
use serde_json::json;
use support::{Cam, FakeCamera};

const VERSION: &str = "0.2.5-test";

fn session(cam: &FakeCamera) -> CameraSession {
    CameraSession::new(
        CameraLocator::Fixed(CameraAddr {
            host: "127.0.0.1".into(),
            port: cam.addr.port(),
        }),
        VERSION,
    )
    .with_aperture_readback_delay_ms(0)
}

fn press(s: &Arc<CameraSession>, dir: i64) -> RelayState {
    match s
        .submit(&SetRequest {
            aperture_step: Some(dir),
            ..Default::default()
        })
        .unwrap()
    {
        ApplyOutcome::Applied { state, .. } => *state,
        ApplyOutcome::Queued => panic!("expected Applied, got Queued"),
    }
}

fn fstop(st: &RelayState) -> f64 {
    let f = 2f64.powf(st.params.aperture_av.unwrap() / 2.0);
    (f * 100.0).round() / 100.0
}

fn iris_puts(cam: &FakeCamera) -> Vec<f64> {
    cam.puts()
        .into_iter()
        .filter(|(p, _)| p == "/lens/iris")
        .map(|(_, b)| b.unwrap()["apertureStop"].as_f64().unwrap())
        .collect()
}

fn iris_range_pos(st: &RelayState) -> serde_json::Value {
    serde_json::to_value(&st.params).unwrap()["irisRangePos"].clone()
}

/// The 108 mm lens without the travel latency, for the walks that only count writes.
fn instant_108mm() -> Cam {
    Cam {
        iris_latency: Duration::ZERO,
        ..support::pocket_4k_zoom_108mm()
    }
}

#[test]
fn the_probe_wait_follows_the_measured_iris_answer_6() {
    // Live: a GET after a PUT lens/iris reads the moved apertureStop within <= 340 ms (4K) and
    // ~220 ms (6K). 500 ms covers that with margin; 1.2 s cost a 1/3-stop press up to 5 s.
    assert_eq!(IRIS_PROBE_WAIT_MS, 500);
}

#[test]
fn a_press_with_one_dead_probe_costs_about_a_second_6() {
    // 5.2 -> 5.6 at 108 mm: the first 1/16-stop probe snaps back to 5.2, the second lands 5.6.
    let mut c = support::pocket_4k_zoom_108mm();
    c.aperture = 5.2;
    let cam = FakeCamera::start(c);
    let s = Arc::new(session(&cam));
    let _ = s.read_state();
    let started = Instant::now();
    let st = press(&s, 1);
    let took = started.elapsed();
    assert_eq!(fstop(&st), 5.6);
    assert!(st.not_applied.is_empty(), "{:?}", st.not_applied);
    assert!(
        took < Duration::from_millis(1400),
        "one dead probe + the landing, at the production wait: {took:?}"
    );
}

#[test]
fn a_known_neighbour_is_written_directly_with_one_put_6() {
    let cam = FakeCamera::start(support::pocket_4k_zoom_108mm());
    let s = Arc::new(session(&cam));
    let _ = s.read_state();
    // The first "-" walks the lens (unknown neighbour) and learns 6.2 <-> 6.7.
    assert_eq!(fstop(&press(&s, -1)), 6.2);
    cam.clear_seen();
    let started = Instant::now();
    let st = press(&s, 1);
    let took = started.elapsed();
    assert_eq!(fstop(&st), 6.7);
    assert!(st.not_applied.is_empty(), "{:?}", st.not_applied);
    assert_eq!(iris_puts(&cam), vec![6.7], "ONE PUT of the known stop");
    assert!(took < Duration::from_millis(900), "no probe wait: {took:?}");
    cam.clear_seen();
    assert_eq!(fstop(&press(&s, -1)), 6.2);
    assert_eq!(iris_puts(&cam), vec![6.2], "and back, one PUT again");
}

#[test]
fn a_walk_learns_the_whole_ladder_and_never_skips_the_108mm_steps_6() {
    let cam = FakeCamera::start(instant_108mm());
    let stops = cam.cam().lens_stops.clone();
    let s = Arc::new(session(&cam).with_iris_probe_wait_ms(30));
    let at = stops.iter().position(|f| *f == 6.7).unwrap();
    for want in stops[..at].iter().rev() {
        assert_eq!(fstop(&press(&s, -1)), *want);
    }
    cam.clear_seen();
    for want in &stops[1..=at + 1] {
        assert_eq!(fstop(&press(&s, 1)), *want);
    }
    // Back up over known neighbours: one PUT per press, each the next native stop. 7.3 was never
    // seen next to 6.7 before, so that last press probes.
    let puts = iris_puts(&cam);
    assert_eq!(&puts[..at], &stops[1..=at], "{puts:?}");
    assert!(puts.len() > at + 1, "the unknown 6.7 -> 7.3 probed: {puts:?}");
}

#[test]
fn the_wide_end_is_learned_and_a_second_minus_is_refused_without_a_put_6() {
    let mut c = instant_108mm();
    c.aperture = 4.4;
    let cam = FakeCamera::start(c);
    let s = Arc::new(session(&cam).with_iris_probe_wait_ms(30));
    let st = s.read_state();
    let p = iris_range_pos(&st).as_f64().unwrap();
    assert!(p > 0.0, "the camera never says 0 at tele: {p}");
    // The first "-" probes down to the description min 4.1 and finds no wider step: the end. It is
    // reported (no longer quiet) and the panel gets irisRangePos 0.
    let st = press(&s, -1);
    assert_eq!(fstop(&st), 4.4);
    assert_eq!(st.not_applied, vec!["apertureStep".to_string()]);
    assert_eq!(iris_range_pos(&st), json!(0.0), "a learned wide end");
    assert!(!iris_puts(&cam).is_empty(), "the first press probed");
    // The second "-" is refused at once: no PUT at all.
    cam.clear_seen();
    let started = Instant::now();
    let st = press(&s, -1);
    assert!(iris_puts(&cam).is_empty(), "{:?}", cam.puts());
    assert_eq!(st.not_applied, vec!["apertureStep".to_string()]);
    assert_eq!(fstop(&st), 4.4);
    assert_eq!(iris_range_pos(&st), json!(0.0));
    assert!(started.elapsed() < Duration::from_millis(500));
    // "+" still walks, and away from the end the camera's own position is reported again.
    let st = press(&s, 1);
    assert_eq!(fstop(&st), 5.2);
    assert!(st.not_applied.is_empty(), "{:?}", st.not_applied);
    let p = iris_range_pos(&st).as_f64().unwrap();
    assert!(p > 0.0 && p < 1.0, "{p}");
    // Back at 4.4 (one PUT, a known neighbour) the learned end reads 0 again.
    let st = press(&s, -1);
    assert_eq!(fstop(&st), 4.4);
    assert_eq!(iris_range_pos(&st), json!(0.0));
    assert!(st.not_applied.is_empty(), "{:?}", st.not_applied);
}

#[test]
fn the_wide_end_press_at_the_production_wait_is_fast_the_second_time_6() {
    let mut c = support::pocket_4k_zoom_108mm();
    c.aperture = 4.4;
    let cam = FakeCamera::start(c);
    let s = Arc::new(session(&cam));
    let _ = s.read_state();
    let started = Instant::now();
    let st = press(&s, -1);
    assert_eq!(st.not_applied, vec!["apertureStep".to_string()]);
    assert!(
        started.elapsed() < Duration::from_millis(2600),
        "4 dead probes at 0.5 s, was 5 s: {:?}",
        started.elapsed()
    );
    let started = Instant::now();
    let st = press(&s, -1);
    assert_eq!(st.not_applied, vec!["apertureStep".to_string()]);
    assert!(started.elapsed() < Duration::from_millis(300));
}

#[test]
fn another_focal_length_does_not_reuse_the_ladder_or_the_end_6() {
    let mut c = instant_108mm();
    c.aperture = 4.4;
    let cam = FakeCamera::start(c);
    let s = Arc::new(session(&cam).with_iris_probe_wait_ms(30));
    // At 108 mm: learn the wide end and the neighbour 4.4 -> 5.2.
    assert_eq!(press(&s, -1).not_applied, vec!["apertureStep".to_string()]);
    assert_eq!(fstop(&press(&s, 1)), 5.2);
    assert_eq!(fstop(&press(&s, -1)), 4.4);
    // Zoom out: the lens has a 4.8 now and a wider end.
    {
        let mut c = cam.cam();
        c.focal_length = Some(70.0);
        c.lens_stops = vec![
            4.0, 4.4, 4.8, 5.2, 5.6, 6.2, 6.7, 7.3, 8.0, 8.7, 9.5, 11.0, 16.0, 22.0,
        ];
    }
    // Each press reads its start (f-stop, position, focal length) from the camera.
    cam.clear_seen();
    let st = press(&s, 1);
    assert_eq!(fstop(&st), 4.8, "no direct write of the 108 mm neighbour 5.2");
    assert!(st.not_applied.is_empty(), "{:?}", st.not_applied);
    assert_ne!(iris_puts(&cam), vec![5.2]);
    assert_eq!(fstop(&press(&s, -1)), 4.4);
    let st = press(&s, -1);
    assert_eq!(fstop(&st), 4.0, "the 108 mm end does not refuse at 70 mm");
    assert!(st.not_applied.is_empty(), "{:?}", st.not_applied);
    let p = iris_range_pos(&st).as_f64().unwrap();
    assert!(p < 1.0);
}

#[test]
fn a_known_neighbour_the_iris_does_not_take_falls_back_to_probing_6() {
    let cam = FakeCamera::start(instant_108mm());
    let s = Arc::new(session(&cam).with_iris_probe_wait_ms(30));
    // Learn 6.2 <-> 6.7.
    assert_eq!(fstop(&press(&s, -1)), 6.2);
    // The lens changed under the same focal length key: 6.7 is gone, a write of 6.7 snaps to 6.2.
    {
        let mut c = cam.cam();
        c.lens_stops.retain(|f| *f != 6.7);
    }
    cam.clear_seen();
    let st = press(&s, 1);
    assert_eq!(fstop(&st), 7.3, "the probe walk found the real next step");
    assert!(st.not_applied.is_empty(), "{:?}", st.not_applied);
    let puts = iris_puts(&cam);
    assert_eq!(puts[0], 6.7, "the direct write came first: {puts:?}");
    assert!(puts.len() > 1, "then the probe walk: {puts:?}");
}

#[test]
fn a_dropped_link_forgets_the_learned_lens_6() {
    let mut c = instant_108mm();
    c.aperture = 4.4;
    let cam = FakeCamera::start(c);
    let s = Arc::new(
        session(&cam)
            .with_iris_probe_wait_ms(30)
            .with_min_read_interval_ms(1),
    );
    assert_eq!(press(&s, -1).not_applied, vec!["apertureStep".to_string()]);
    // The camera stops answering: the read fails and the link is dropped.
    cam.cam()
        .get_delay
        .insert("/video/iso".into(), Duration::from_millis(1500));
    std::thread::sleep(Duration::from_millis(5));
    assert!(!s.read_state().online);
    cam.cam().get_delay.clear();
    std::thread::sleep(Duration::from_millis(5));
    let st = s.read_state();
    assert!(st.online);
    let p = iris_range_pos(&st).as_f64().unwrap();
    assert!(p > 0.0, "no learned end after the link drop: {p}");
    cam.clear_seen();
    let _ = press(&s, -1);
    assert!(!iris_puts(&cam).is_empty(), "the end is probed again");
}
