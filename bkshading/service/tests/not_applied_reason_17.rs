//! shading#17: the service carries the relay's reason for a refused write. `notAppliedReason`
//! rides in the relay state it already passes on; a failed `PUT /api/params` answers the panel
//! with the relay's `x-bkshading-reason` (and `x-bkshading-camera-status`), or
//! `relay-unreachable` when the relay itself cannot be reached.

use bkshading::aggregator::{camera_view, Aggregator};
use bkshading::config::ServiceConfig;
use bkshading::http::forward_error_response;
use bkshading_proto::wire::{NotAppliedReason, RelayState, SetRequest};

fn config(address: &str) -> ServiceConfig {
    ServiceConfig::from_toml_str(&format!(
        "bind = \"127.0.0.1:0\"\n\n[[camera]]\nid = \"cam6\"\nlabel = \"Cam 6\"\ntransport = \"cambox-relay\"\naddress = \"{address}\"\n"
    ))
    .unwrap()
}

/// A fake relay whose `PUT /api/params` answers `status` with `headers` and a text body.
async fn refusing_relay(status: u16, headers: Vec<(&'static str, &'static str)>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new().route(
        "/api/params",
        axum::routing::put(move || {
            let headers = headers.clone();
            async move {
                let mut resp = axum::response::IntoResponse::into_response((
                    axum::http::StatusCode::from_u16(status).unwrap(),
                    "camera 1.2.3.4:80 refused PUT /control/api/v1/video/iso: status 403",
                ));
                for (k, v) in headers {
                    resp.headers_mut().insert(k, v.parse().unwrap());
                }
                resp
            }
        }),
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr.to_string()
}

fn iso() -> SetRequest {
    SetRequest {
        iso: Some(800),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_relay_refusal_reaches_the_panel_with_its_reason_17() {
    let addr = refusing_relay(
        502,
        vec![
            ("x-bkshading-reason", "camera-refused"),
            ("x-bkshading-camera-status", "403"),
        ],
    )
    .await;
    let cfg = config(&addr);
    let agg = Aggregator::new("0.2.14-test").unwrap();
    let err = agg.forward_set(&cfg.cameras[0], &iso()).await.unwrap_err();
    let resp = forward_error_response(&err);
    assert_eq!(resp.status().as_u16(), 502);
    assert_eq!(resp.headers()["x-bkshading-reason"], "camera-refused");
    assert_eq!(resp.headers()["x-bkshading-camera-status"], "403");
}

#[tokio::test]
async fn an_older_relay_without_a_reason_gets_no_header_17() {
    let addr = refusing_relay(502, vec![]).await;
    let cfg = config(&addr);
    let agg = Aggregator::new("0.2.14-test").unwrap();
    let err = agg.forward_set(&cfg.cameras[0], &iso()).await.unwrap_err();
    let resp = forward_error_response(&err);
    assert_eq!(resp.status().as_u16(), 502);
    assert!(resp.headers().get("x-bkshading-reason").is_none());
}

#[tokio::test]
async fn a_relay_that_does_not_answer_is_relay_unreachable_17() {
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().to_string()
    };
    let cfg = config(&dead);
    let agg = Aggregator::new("0.2.14-test").unwrap();
    let err = agg.forward_set(&cfg.cameras[0], &iso()).await.unwrap_err();
    let resp = forward_error_response(&err);
    assert_eq!(resp.status().as_u16(), 502);
    assert_eq!(resp.headers()["x-bkshading-reason"], "relay-unreachable");
}

#[test]
fn the_state_s_reasons_pass_through_the_camera_view_17() {
    let cfg = config("127.0.0.1:1");
    let mut st = RelayState::offline("0.2.14");
    st.online = true;
    st.not_applied = vec!["apertureStep".into()];
    st.not_applied_reason = [("apertureStep".to_string(), NotAppliedReason::LensEnd)].into();
    let json = serde_json::to_value(camera_view(&cfg.cameras[0], Some(st), false)).unwrap();
    assert_eq!(
        json["state"]["notAppliedReason"],
        serde_json::json!({"apertureStep": "lens-end"})
    );
}

#[tokio::test]
async fn a_newer_relay_s_reason_word_passes_through_unchanged_17() {
    let addr = refusing_relay(502, vec![("x-bkshading-reason", "battery-low")]).await;
    let cfg = config(&addr);
    let agg = Aggregator::new("0.2.14-test").unwrap();
    let err = agg.forward_set(&cfg.cameras[0], &iso()).await.unwrap_err();
    assert_eq!(
        forward_error_response(&err).headers()["x-bkshading-reason"],
        "battery-low"
    );
    // Not a token: dropped, never forwarded.
    let bad = refusing_relay(502, vec![("x-bkshading-reason", "Two Words")]).await;
    let err = agg
        .forward_set(&config(&bad).cameras[0], &iso())
        .await
        .unwrap_err();
    assert!(forward_error_response(&err)
        .headers()
        .get("x-bkshading-reason")
        .is_none());
}

#[tokio::test]
async fn an_unreachable_relay_keeps_the_transport_detail_in_the_text_17() {
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().to_string()
    };
    let agg = Aggregator::new("0.2.14-test").unwrap();
    let err = agg
        .forward_set(&config(&dead).cameras[0], &iso())
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(text.starts_with("relay cam6 unreachable: "), "{text}");
    assert!(text.len() > "relay cam6 unreachable: ".len(), "{text}");
}
