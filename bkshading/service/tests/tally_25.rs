//! shading#25: the PGM/PVW tally from the strih tally service. The pure half (parsing, the camera
//! mapping, `apply_tally`, the once-per-transition tracker) and the real tally task against a stub
//! HTTP tally server (the only replaced part: camera-box's `strih_tally.py`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{extract::State, http::StatusCode, routing::get, Router};
use bkshading::aggregator::{aggregate_with_camera_update, camera_view, Aggregator};
use bkshading::config::ServiceConfig;
use bkshading::preview::store::PreviewStore;
use bkshading::tally::{
    apply_tally, camera_tally, parse_tally, run, tally_number, TallyState, TallyTracker,
};
use bkshading_proto::wire::{Aggregate, RelayState, ShadingParams, Tally};
use tokio::sync::watch;

/// Three venue cameras whose relays do not answer (port 9 refuses at once) and a handheld.
const CONFIG: &str = "\
[tally]
url = \"http://127.0.0.1:9240/state\"

[[camera]]
id = \"cam2\"
label = \"Cam 2\"
transport = \"cambox-relay\"
address = \"127.0.0.1:9\"

[[camera]]
id = \"cam3\"
label = \"Cam 3\"
transport = \"cambox-relay\"
address = \"127.0.0.1:9\"

[[camera]]
id = \"cam5\"
label = \"Cam 5\"
transport = \"cambox-relay\"
address = \"127.0.0.1:9\"

[[camera]]
id = \"handheld-1\"
label = \"Handheld 1\"
transport = \"sbc-relay\"
address = \"127.0.0.1:9\"
";

fn config() -> ServiceConfig {
    ServiceConfig::from_toml_str(CONFIG).unwrap()
}

fn aggregate(cfg: &ServiceConfig) -> Aggregate {
    Aggregate {
        version: "0.2.11".into(),
        cameras: cfg
            .cameras
            .iter()
            .map(|c| camera_view(c, None, false))
            .collect(),
    }
}

fn tallies(agg: &Aggregate) -> Vec<(String, Option<Tally>)> {
    agg.cameras
        .iter()
        .map(|c| (c.id.clone(), c.tally))
        .collect()
}

fn t(pgm: u64, pvw: u64, obs: bool) -> TallyState {
    TallyState {
        pgm,
        pvw,
        obs_connected: obs,
    }
}

#[test]
fn the_config_key_turns_the_tally_on_and_absent_or_empty_is_off_25() {
    assert_eq!(
        config().tally.poll_url(),
        Some("http://127.0.0.1:9240/state")
    );
    let off = ServiceConfig::from_toml_str("").unwrap();
    assert_eq!(off.tally.poll_url(), None, "no [tally] table");
    let empty = ServiceConfig::from_toml_str("[tally]\nurl = \"  \"\n").unwrap();
    assert_eq!(empty.tally.poll_url(), None, "an empty url is off");
}

#[test]
fn the_state_body_parses_and_ignores_the_other_fields_25() {
    let body = r#"{"pgm": 6, "pvw": 3, "program_scene": "CAM6", "preview_scene": "CAM3",
        "studio_mode": true, "obs_connected": true}"#;
    assert_eq!(parse_tally(body), Ok(t(6, 3, true)));
    assert!(parse_tally("not json").is_err());
    assert_eq!(
        parse_tally(r#"{"pgm": null, "pvw": -1, "obs_connected": true}"#),
        Ok(t(0, 0, true)),
        "null / negative = no single camera, never a failed poll"
    );
    assert_eq!(
        parse_tally(r#"{"obs_connected": false}"#),
        Ok(t(0, 0, false))
    );
    assert!(
        parse_tally(r#"{"pgm": 2, "pvw": 3}"#).is_err(),
        "without obs_connected the numbers mean nothing"
    );
    assert!(
        parse_tally(r#"{"pgm": 1}"#).is_err(),
        "a body without pvw/obs"
    );
}

#[test]
fn only_cam_n_ids_map_to_tally_numbers_and_pgm_wins_25() {
    assert_eq!(tally_number("cam6"), Some(6));
    assert_eq!(tally_number("cam10"), Some(10));
    assert_eq!(tally_number("handheld-1"), None);
    assert_eq!(tally_number("cam"), None);
    assert_eq!(tally_number("camx6"), None);
    let live = t(6, 3, true);
    assert_eq!(camera_tally("cam6", Some(&live)), Some(Tally::Pgm));
    assert_eq!(camera_tally("cam3", Some(&live)), Some(Tally::Pvw));
    assert_eq!(camera_tally("cam5", Some(&live)), None);
    assert_eq!(camera_tally("cam6", Some(&t(6, 6, true))), Some(Tally::Pgm));
    assert_eq!(
        camera_tally("cam6", Some(&t(6, 3, false))),
        None,
        "OBS away"
    );
    assert_eq!(camera_tally("cam6", None), None, "no tally known");
    assert_eq!(
        camera_tally("cam0", Some(&t(0, 0, true))),
        None,
        "0 = no camera"
    );
}

#[test]
fn apply_tally_sets_every_camera_and_clears_with_none_25() {
    let cfg = config();
    let agg = aggregate(&cfg);
    let on = apply_tally(&agg, Some(&t(2, 3, true)));
    assert_eq!(
        tallies(&on),
        vec![
            ("cam2".into(), Some(Tally::Pgm)),
            ("cam3".into(), Some(Tally::Pvw)),
            ("cam5".into(), None),
            ("handheld-1".into(), None),
        ]
    );
    let off = apply_tally(&on, None);
    assert!(off.cameras.iter().all(|c| c.tally.is_none()));
}

#[test]
fn a_write_confirmation_keeps_the_cameras_tally_25() {
    let cfg = config();
    let agg = apply_tally(&aggregate(&cfg), Some(&t(2, 3, true)));
    let state = RelayState {
        online: true,
        camera: Some("Blackmagic Pocket Cinema Camera 4K".into()),
        params: ShadingParams {
            iso: Some(800),
            ..Default::default()
        },
        caps: None,
        fps_supported: true,
        capture_fps: None,
        version: "0.2.11".into(),
        not_applied: Vec::new(),
        offline_reason: None,
        not_applied_reason: Default::default(),
    };
    let updated = aggregate_with_camera_update(&agg, &cfg.cameras[0], state);
    assert_eq!(updated.cameras[0].tally, Some(Tally::Pgm));
}

#[test]
fn the_tracker_logs_once_per_transition_and_reports_only_changes_25() {
    let mut tr = TallyTracker::default();
    let s = tr.observe(Ok(t(2, 3, true)));
    assert_eq!(s.changed, Some(Some(t(2, 3, true))));
    assert_eq!(
        s.info,
        vec![
            "tally: live (OBS connected)".to_string(),
            "tally: pgm 2 pvw 3".to_string()
        ]
    );
    let s = tr.observe(Ok(t(2, 3, true)));
    assert_eq!(s, Default::default(), "no change, no line");
    let s = tr.observe(Ok(t(3, 2, true)));
    assert_eq!(s.changed, Some(Some(t(3, 2, true))));
    assert_eq!(s.info, vec!["tally: pgm 3 pvw 2".to_string()]);
    let s = tr.observe(Err("connection refused".into()));
    assert_eq!(s.changed, Some(None), "an error clears the tally");
    assert_eq!(
        s.warn,
        vec!["tally: poll failed (connection refused); tally cleared".to_string()]
    );
    let s = tr.observe(Err("connection refused".into()));
    assert_eq!(s, Default::default(), "a failing source is logged once");
    let s = tr.observe(Ok(t(0, 0, false)));
    assert_eq!(s.changed, None, "still cleared");
    assert_eq!(s.warn.len(), 1, "OBS away is its own transition");
    let s = tr.observe(Ok(t(0, 0, false)));
    assert_eq!(s, Default::default());
}

/// The stub tally server's answer: status + body.
type Answer = Arc<Mutex<(StatusCode, String)>>;

async fn stub_state(State(answer): State<Answer>) -> (StatusCode, String) {
    answer.lock().unwrap().clone()
}

/// Starts the stub tally server on a free port; returns its `/state` url and the answer handle.
async fn stub_tally_server() -> (String, Answer) {
    let answer: Answer = Arc::new(Mutex::new((
        StatusCode::OK,
        r#"{"pgm": 2, "pvw": 3, "obs_connected": true}"#.to_string(),
    )));
    let app = Router::new()
        .route("/state", get(stub_state))
        .with_state(answer.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/state"), answer)
}

/// Waits (bounded) until the live aggregate's tallies equal `want`.
async fn wait_for(
    rx: &mut watch::Receiver<Arc<Aggregate>>,
    want: &[(&str, Option<Tally>)],
) -> Aggregate {
    let want: Vec<(String, Option<Tally>)> =
        want.iter().map(|(id, t)| (id.to_string(), *t)).collect();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let now = rx.borrow_and_update().clone();
        if tallies(&now) == want {
            return (*now).clone();
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !left.is_zero(),
            "live tally {:?}, wanted {want:?}",
            tallies(&now)
        );
        let _ = tokio::time::timeout(left, rx.changed()).await;
    }
}

#[tokio::test]
async fn the_tally_task_follows_the_tally_service_and_clears_on_failure_25() {
    let cfg = config();
    let (url, answer) = stub_tally_server().await;
    let agg = Arc::new(Aggregator::new("0.2.11").unwrap());
    let (tx, mut rx) = watch::channel(Arc::new(aggregate(&cfg)));
    let tx = Arc::new(tx);
    let task = tokio::spawn(run(url, Duration::from_millis(20), agg.clone(), tx.clone()));

    // The service's first answer: cam2 in program, cam3 in preview; republished at once.
    wait_for(
        &mut rx,
        &[
            ("cam2", Some(Tally::Pgm)),
            ("cam3", Some(Tally::Pvw)),
            ("cam5", None),
            ("handheld-1", None),
        ],
    )
    .await;
    // Every snapshot (the pump, GET /api/cameras) carries the stored tally too.
    let snap = agg.snapshot(&cfg, &PreviewStore::new()).await;
    assert_eq!(snap.cameras[0].tally, Some(Tally::Pgm));
    assert_eq!(snap.cameras[1].tally, Some(Tally::Pvw));

    // A cut.
    *answer.lock().unwrap() = (
        StatusCode::OK,
        r#"{"pgm": 5, "pvw": 2, "obs_connected": true}"#.to_string(),
    );
    wait_for(
        &mut rx,
        &[
            ("cam2", Some(Tally::Pvw)),
            ("cam3", None),
            ("cam5", Some(Tally::Pgm)),
            ("handheld-1", None),
        ],
    )
    .await;

    // The tally service loses OBS: cleared.
    *answer.lock().unwrap() = (
        StatusCode::OK,
        r#"{"pgm": 0, "pvw": 0, "obs_connected": false}"#.to_string(),
    );
    let cleared = [
        ("cam2", None),
        ("cam3", None),
        ("cam5", None),
        ("handheld-1", None),
    ];
    wait_for(&mut rx, &cleared).await;

    // Back, then the service fails (500): cleared again.
    *answer.lock().unwrap() = (
        StatusCode::OK,
        r#"{"pgm": 3, "pvw": 0, "obs_connected": true}"#.to_string(),
    );
    wait_for(
        &mut rx,
        &[
            ("cam2", None),
            ("cam3", Some(Tally::Pgm)),
            ("cam5", None),
            ("handheld-1", None),
        ],
    )
    .await;
    *answer.lock().unwrap() = (StatusCode::INTERNAL_SERVER_ERROR, "boom".to_string());
    wait_for(&mut rx, &cleared).await;
    assert_eq!(agg.tally(), None, "the stored tally is cleared too");
    task.abort();
}

/// v0.2.11 deep review: a `pgm`/`pvw` sent as a numeric string or a float with no fraction is the
/// camera number; any other value is 0 ("no single camera"). Neither fails the whole body, so one
/// odd value never turns a live tally into a failed poll that clears it.
#[test]
fn a_numeric_string_or_whole_float_reads_as_the_number_and_garbage_as_none_25() {
    assert_eq!(
        parse_tally(r#"{"pgm": "3", "pvw": 2.0, "obs_connected": true}"#),
        Ok(t(3, 2, true))
    );
    assert_eq!(
        parse_tally(r#"{"pgm": " 4 ", "pvw": "5.0", "obs_connected": true}"#),
        Ok(t(4, 5, true))
    );
    assert_eq!(
        parse_tally(r#"{"pgm": "x", "pvw": 2.5, "obs_connected": true}"#),
        Ok(t(0, 0, true))
    );
    assert_eq!(
        parse_tally(r#"{"pgm": [1], "pvw": {"n": 2}, "obs_connected": true}"#),
        Ok(t(0, 0, true))
    );

    let mut tr = TallyTracker::default();
    tr.observe(Ok(t(3, 2, true)));
    let s = tr.observe(parse_tally(
        r#"{"pgm": 3, "pvw": "garbage", "obs_connected": true}"#,
    ));
    assert!(
        s.warn.is_empty(),
        "a garbage value is not a failed poll: {s:?}"
    );
    assert_eq!(s.changed, Some(Some(t(3, 0, true))), "pgm stays live");
}
