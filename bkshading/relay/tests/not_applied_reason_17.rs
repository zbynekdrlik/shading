//! shading#17: a write that is not applied says WHY, per key (`notAppliedReason`), and a failed
//! write names its reason in the additive `x-bkshading-reason` header. Against the same fake
//! camera as `rest_camera_6.rs` (the only replaced part).

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::http::StatusCode;
use bkshading_proto::wire::{NotAppliedReason, RelayState, SetRequest};
use bkshading_relay::http::{set_error_reason, set_error_response};
use bkshading_relay::transport::{ApplyOutcome, CameraAddr, CameraLocator, CameraSession};
use support::{Cam, FakeCamera};

fn session(cam: &FakeCamera) -> Arc<CameraSession> {
    Arc::new(
        CameraSession::new(
            CameraLocator::Fixed(CameraAddr {
                host: "127.0.0.1".into(),
                port: cam.addr.port(),
            }),
            "0.2.14-test",
        )
        .with_aperture_readback_delay_ms(0)
        .with_iris_probe_wait_ms(30),
    )
}

fn submit(s: &Arc<CameraSession>, req: SetRequest) -> RelayState {
    match s.submit(&req).unwrap() {
        ApplyOutcome::Applied { state, .. } => *state,
        ApplyOutcome::Queued => panic!("expected Applied"),
    }
}

fn step(dir: i64) -> SetRequest {
    SetRequest {
        aperture_step: Some(dir),
        ..Default::default()
    }
}

fn reasons(pairs: &[(&str, NotAppliedReason)]) -> BTreeMap<String, NotAppliedReason> {
    pairs.iter().map(|(k, r)| (k.to_string(), *r)).collect()
}

#[test]
fn a_press_at_the_end_of_the_lens_says_lens_end_17() {
    let cam = FakeCamera::start(support::zoom_third_stop_lens());
    let s = session(&cam);
    // Wide open: one more "-" has no step.
    let st = submit(&s, step(-1));
    assert_eq!(st.not_applied, vec!["apertureStep".to_string()]);
    assert_eq!(
        st.not_applied_reason,
        reasons(&[("apertureStep", NotAppliedReason::LensEnd)])
    );
    // The cached state carries it too (the next /api/state poll).
    assert_eq!(s.read_state().not_applied_reason, st.not_applied_reason);
}

#[test]
fn a_lens_that_cannot_be_driven_says_not_controllable_17() {
    let cam = FakeCamera::start(Cam {
        iris_controllable: false,
        ..support::zoom_third_stop_lens()
    });
    let st = submit(&session(&cam), step(1));
    assert_eq!(
        st.not_applied_reason,
        reasons(&[("apertureStep", NotAppliedReason::NotControllable)])
    );
}

#[test]
fn an_iris_that_follows_no_probe_says_iris_not_following_17() {
    let cam = FakeCamera::start(Cam {
        iris_ignores_writes: true,
        ..support::zoom_third_stop_lens()
    });
    let st = submit(&session(&cam), step(1));
    assert_eq!(
        st.not_applied_reason,
        reasons(&[("apertureStep", NotAppliedReason::IrisNotFollowing)])
    );
}

#[test]
fn a_value_the_camera_keeps_says_camera_kept_value_17() {
    let cam = FakeCamera::start(Cam {
        iris_ignores_writes: true,
        ..Default::default()
    });
    let st = submit(
        &session(&cam),
        SetRequest {
            aperture_norm: Some(1.0),
            iso: Some(800),
            ..Default::default()
        },
    );
    assert_eq!(st.not_applied, vec!["apertureNorm".to_string()]);
    assert_eq!(
        st.not_applied_reason,
        reasons(&[("apertureNorm", NotAppliedReason::CameraKeptValue)])
    );
}

#[test]
fn an_applied_write_has_no_reasons_17() {
    let cam = FakeCamera::start(Cam::default());
    let st = submit(
        &session(&cam),
        SetRequest {
            iso: Some(800),
            ..Default::default()
        },
    );
    assert!(st.not_applied.is_empty());
    assert!(st.not_applied_reason.is_empty());
    // On the wire: an empty map, next to the empty list.
    let wire = serde_json::to_value(&st).unwrap();
    assert_eq!(wire["notAppliedReason"], serde_json::json!({}));
}

#[test]
fn a_camera_refusal_is_camera_refused_with_its_status_17() {
    let mut c = Cam::default();
    c.refuse
        .insert("/video/iso".into(), (403, "ISO locked".into()));
    let cam = FakeCamera::start(c);
    let err = session(&cam)
        .submit(&SetRequest {
            iso: Some(800),
            ..Default::default()
        })
        .unwrap_err();
    // The text stays the pinned one.
    assert!(
        err.to_string().contains("PUT /control/api/v1/video/iso"),
        "{err}"
    );
    assert_eq!(
        set_error_reason(&err),
        (NotAppliedReason::CameraRefused, Some(403))
    );
    let resp = set_error_response(&err);
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(resp.headers()["x-bkshading-reason"], "camera-refused");
    assert_eq!(resp.headers()["x-bkshading-camera-status"], "403");
}

#[test]
fn an_unlisted_fps_is_not_in_choices_17() {
    let cam = FakeCamera::start(Cam::default());
    let err = session(&cam)
        .submit(&SetRequest {
            fps: Some(48),
            ..Default::default()
        })
        .unwrap_err();
    let resp = set_error_response(&err);
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(resp.headers()["x-bkshading-reason"], "not-in-choices");
    assert!(resp.headers().get("x-bkshading-camera-status").is_none());
}

#[test]
fn no_camera_is_camera_offline_and_a_silent_one_camera_not_answering_17() {
    let offline =
        anyhow::anyhow!("camera offline: no camera answers its REST API, nothing written");
    assert_eq!(
        set_error_reason(&offline),
        (NotAppliedReason::CameraOffline, None)
    );
    let silent = anyhow::Error::new(bkshading_relay::transport::rest::RestError::Transport(
        "timed out".into(),
    ))
    .context("camera 127.0.0.1:1 did not answer PUT /control/api/v1/video/iso {}");
    assert_eq!(
        set_error_reason(&silent),
        (NotAppliedReason::CameraNotAnswering, None)
    );
    assert!(silent
        .to_string()
        .starts_with("camera 127.0.0.1:1 did not answer"));
}

#[test]
fn reasons_are_kebab_case_and_an_unknown_one_reads_as_unknown_17() {
    for r in NotAppliedReason::KNOWN {
        let wire = serde_json::to_value(r).unwrap();
        assert_eq!(wire, serde_json::json!(r.as_str()));
        assert_eq!(NotAppliedReason::from_wire(r.as_str()), r);
    }
    assert_eq!(
        NotAppliedReason::from_wire("from-a-newer-relay"),
        NotAppliedReason::Unknown
    );
    let st: RelayState = serde_json::from_value(serde_json::json!({
        "online": true, "camera": null, "params": {}, "caps": null, "fpsSupported": false,
        "version": "0.3.0", "notApplied": ["iso"], "notAppliedReason": {"iso": "brand-new"}
    }))
    .unwrap();
    assert_eq!(st.not_applied_reason["iso"], NotAppliedReason::Unknown);
    // An older relay sends no map: empty.
    let old: RelayState = serde_json::from_value(serde_json::json!({
        "online": true, "camera": null, "params": {}, "caps": null, "fpsSupported": false,
        "version": "0.2.13", "notApplied": ["iso"]
    }))
    .unwrap();
    assert!(old.not_applied_reason.is_empty());
}
