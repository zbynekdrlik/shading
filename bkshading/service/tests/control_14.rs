//! shading#14: the service fills each block's `control` status from what its relay answered, so
//! the panel can warn and say what to switch on. Pure `camera_view`, plus the real aggregator
//! polling a relay that is down, too old or reports a reason.

use bkshading::aggregator::{camera_view, Aggregator};
use bkshading::config::ServiceConfig;
use bkshading::preview::store::PreviewStore;
use bkshading_proto::wire::{ControlStatus, OfflineReason, RelayState};

fn config(address: &str) -> ServiceConfig {
    ServiceConfig::from_toml_str(&format!(
        "bind = \"127.0.0.1:0\"\n\n[[camera]]\nid = \"cam7\"\nlabel = \"Cam 7\"\ntransport = \"cambox-relay\"\naddress = \"{address}\"\n"
    ))
    .unwrap()
}

fn offline(version: &str, reason: Option<OfflineReason>) -> RelayState {
    RelayState {
        offline_reason: reason,
        ..RelayState::offline(version)
    }
}

#[test]
fn camera_view_derives_the_control_status_14() {
    let cfg = config("127.0.0.1:1");
    let cam = &cfg.cameras[0];
    assert_eq!(
        camera_view(cam, None, false).control,
        ControlStatus::RelayUnreachable
    );
    let mut online = RelayState::offline("0.2.6");
    online.online = true;
    assert_eq!(
        camera_view(cam, Some(online), false).control,
        ControlStatus::Ok
    );
    assert_eq!(
        camera_view(cam, Some(offline("0.1.2", None)), false).control,
        ControlStatus::RelayTooOld
    );
    let v = camera_view(
        cam,
        Some(offline("0.2.6", Some(OfflineReason::RestOff))),
        false,
    );
    assert_eq!(v.control, ControlStatus::RestOff);
    let json = serde_json::to_value(&v).unwrap();
    assert_eq!(json["control"], "rest-off", "{json}");
    assert_eq!(json["state"]["offlineReason"], "rest-off", "{json}");
}

/// A one-shot fake relay: answers every `GET /api/state` with `body`.
async fn fake_relay(body: String) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new().route(
        "/api/state",
        axum::routing::get(move || {
            let b = body.clone();
            async move { ([("content-type", "application/json")], b) }
        }),
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr.to_string()
}

#[tokio::test]
async fn the_aggregator_reports_each_relay_case_14() {
    let previews = PreviewStore::default();
    let agg = Aggregator::new("0.2.6-test").unwrap();

    // Nothing listens at the relay's address.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().to_string()
    };
    let snap = agg.snapshot(&config(&dead), &previews).await;
    assert_eq!(snap.cameras[0].control, ControlStatus::RelayUnreachable);
    assert!(!snap.cameras[0].reachable);

    // pp-cam4: a 0.1.2 relay (no offlineReason, gphoto2 era) with its camera offline.
    let old = fake_relay(
        r#"{"online":false,"camera":null,"params":{},"caps":null,"fpsSupported":false,"version":"0.1.2"}"#
            .to_string(),
    )
    .await;
    let snap = agg.snapshot(&config(&old), &previews).await;
    assert_eq!(snap.cameras[0].control, ControlStatus::RelayTooOld);

    // A current relay whose camera has Web media manager off.
    let rest_off =
        fake_relay(serde_json::to_string(&offline("0.2.6", Some(OfflineReason::RestOff))).unwrap())
            .await;
    let snap = agg.snapshot(&config(&rest_off), &previews).await;
    assert_eq!(snap.cameras[0].control, ControlStatus::RestOff);

    // A current relay with no camera on USB.
    let no_link = fake_relay(
        serde_json::to_string(&offline("0.2.6", Some(OfflineReason::NoCameraLink))).unwrap(),
    )
    .await;
    let snap = agg.snapshot(&config(&no_link), &previews).await;
    assert_eq!(snap.cameras[0].control, ControlStatus::NoCameraLink);
}
