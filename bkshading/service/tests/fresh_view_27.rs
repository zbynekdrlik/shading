//! shading#27: a pump snapshot whose relay polls started BEFORE a write finished must not replace
//! the post-write view the write's confirmation already published (live PP cam6, v0.2.10: f/3.8
//! flicked back to f/3.7 ~0.9 s after the press). Pure: `merge_fresher` decides per camera.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bkshading::aggregator::{camera_view, merge_fresher};
use bkshading::config::ServiceConfig;
use bkshading_proto::wire::{Aggregate, RelayState, ShadingParams};

const CONFIG: &str = "\
[[camera]]
id = \"cam6\"
label = \"Cam 6\"
transport = \"cambox-relay\"
address = \"10.77.8.66:8771\"

[[camera]]
id = \"cam7\"
label = \"Cam 7\"
transport = \"cambox-relay\"
address = \"10.77.8.67:8771\"
";

fn state(iso: i64) -> RelayState {
    RelayState {
        online: true,
        camera: Some("Blackmagic Pocket Cinema Camera 6K".into()),
        params: ShadingParams {
            iso: Some(iso),
            ..Default::default()
        },
        caps: None,
        fps_supported: true,
        capture_fps: None,
        version: "0.2.10".into(),
        not_applied: Vec::new(),
        offline_reason: None,
    }
}

fn agg(cam6_iso: i64, cam7_iso: i64) -> Aggregate {
    let cfg = ServiceConfig::from_toml_str(CONFIG).unwrap();
    Aggregate {
        version: "0.2.11".into(),
        cameras: vec![
            camera_view(&cfg.cameras[0], Some(state(cam6_iso)), false),
            camera_view(&cfg.cameras[1], Some(state(cam7_iso)), false),
        ],
    }
}

fn iso(a: &Aggregate, id: &str) -> Option<i64> {
    a.cameras
        .iter()
        .find(|c| c.id == id)
        .and_then(|c| c.state.as_ref())
        .and_then(|s| s.params.iso)
}

#[test]
fn a_snapshot_polled_before_a_write_keeps_the_post_write_view_27() {
    let polled_from = Instant::now();
    // cam6's write finished while the snapshot's polls were in flight.
    let writes = HashMap::from([("cam6".to_string(), polled_from + Duration::from_millis(400))]);
    let current = agg(800, 400); // the confirmation push: cam6 at 800
    let snapshot = agg(400, 200); // the polls read cam6 before the write; cam7 changed on its own
    let merged = merge_fresher(&current, snapshot, polled_from, &writes);
    assert_eq!(iso(&merged, "cam6"), Some(800), "no rollback of the written camera");
    assert_eq!(iso(&merged, "cam7"), Some(200), "every other camera comes from the snapshot");
}

#[test]
fn a_snapshot_polled_after_the_write_replaces_the_view_27() {
    let write_done = Instant::now();
    let polled_from = write_done + Duration::from_millis(10);
    let writes = HashMap::from([("cam6".to_string(), write_done)]);
    let current = agg(800, 400);
    let snapshot = agg(1600, 400); // the camera moved on after the write (e.g. on the body)
    let merged = merge_fresher(&current, snapshot, polled_from, &writes);
    assert_eq!(iso(&merged, "cam6"), Some(1600), "a post-write poll is the truth");
}

#[test]
fn a_camera_new_in_the_snapshot_is_taken_even_with_a_late_write_stamp_27() {
    let polled_from = Instant::now();
    let writes = HashMap::from([("cam7".to_string(), polled_from + Duration::from_millis(5))]);
    let mut current = agg(800, 400);
    current.cameras.retain(|c| c.id == "cam6"); // cam7 not in the current view (e.g. visiting)
    let snapshot = agg(800, 200);
    let merged = merge_fresher(&current, snapshot, polled_from, &writes);
    assert_eq!(iso(&merged, "cam7"), Some(200), "nothing current to keep: the snapshot's view");
    assert_eq!(merged.cameras.len(), 2, "the snapshot's camera list is kept");
}
