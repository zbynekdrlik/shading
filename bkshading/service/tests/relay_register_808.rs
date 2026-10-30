//! Issue 808 (owner ruling 28.10.2026): a handheld relay registers itself with the service, the
//! service reaches it through the announced address, and no SBC IP is ever configured.
//!
//! End to end over real sockets: the REAL relay announcer (`bkshading_relay::register`) posts to
//! the REAL service router (with `ConnectInfo`), and the REAL aggregator then polls a fake relay
//! `/api/state` through the address the registration recorded.

use std::net::SocketAddr;
use std::sync::Arc;

use bkshading::aggregator::Aggregator;
use bkshading::config::ServiceConfig;
use bkshading::http::{router, AppState};
use bkshading::preview::store::PreviewStore;
use bkshading::registry::RelayRegistry;
use bkshading_proto::wire::{Aggregate, RelayState, ShadingParams};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CONFIG: &str = r#"
[[camera]]
id = "cam4"
label = "Cam 4"
transport = "sbc-relay"
address = "register:handheld-1"
"#;

/// A fake relay answering every request with one online `/api/state` body.
async fn fake_relay() -> SocketAddr {
    let state = RelayState {
        online: true,
        camera: Some("USB PTP Class Camera".into()),
        params: ShadingParams {
            iso: Some(1250),
            ..Default::default()
        },
        caps: None,
        fps_supported: true,
        capture_fps: None,
        version: "test".into(),
        not_applied: vec![],
    };
    let body = serde_json::to_string(&state).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    addr
}

/// The real service router on an ephemeral port, sharing `registry` with `agg`.
async fn service(
    registry: RelayRegistry,
    cfg: Arc<ServiceConfig>,
    agg: Arc<Aggregator>,
) -> SocketAddr {
    let (live_tx, live_rx) = tokio::sync::watch::channel(Arc::new(Aggregate {
        version: "test".into(),
        cameras: vec![],
    }));
    let state = AppState {
        config: cfg,
        agg,
        previews: PreviewStore::new(),
        live: live_rx,
        live_tx: Arc::new(live_tx),
        registry,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router(state).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    addr
}

#[tokio::test]
async fn a_registered_handheld_is_reached_at_the_address_it_announced_from_808() {
    let cfg = Arc::new(ServiceConfig::from_toml_str(CONFIG).unwrap());
    let registry = RelayRegistry::new();
    let agg = Arc::new(Aggregator::with_registry("test", registry.clone()).unwrap());
    let svc = service(registry.clone(), cfg.clone(), agg.clone()).await;
    let relay = fake_relay().await;

    // Before the handheld announces itself the camera is unreachable: no address is guessed.
    let before = agg.snapshot(&cfg, &PreviewStore::new()).await;
    assert!(!before.cameras[0].reachable, "no registration yet");
    assert_eq!(
        before.cameras[0].registration, None,
        "nothing announced, nothing shown"
    );

    // The REAL relay announcer posts {id, port} to the service.
    let target = bkshading_relay::register::parse_register_url(&format!(
        "http://127.0.0.1:{}/api/relays/register",
        svc.port()
    ))
    .unwrap();
    let status = bkshading_relay::register::announce_once(&target, "handheld-1", relay.port())
        .await
        .unwrap();
    assert_eq!(status, 200);

    // The service recorded the PEER address + the announced port, and the aggregator reaches the
    // camera through it.
    assert_eq!(
        registry
            .lookup("handheld-1", std::time::Instant::now())
            .as_deref(),
        Some(format!("127.0.0.1:{}", relay.port()).as_str())
    );
    let after = agg.snapshot(&cfg, &PreviewStore::new()).await;
    let cam = &after.cameras[0];
    assert!(cam.reachable, "reached through the registration");
    // The panel sees the registration: who, from where, just now.
    let reg = cam
        .registration
        .as_ref()
        .expect("the view carries the registration");
    assert_eq!(reg.relay_id, "handheld-1");
    assert_eq!(reg.address, format!("127.0.0.1:{}", relay.port()));
    assert!(reg.fresh && reg.age_s <= 2, "{reg:?}");
    let state = cam.state.as_ref().unwrap();
    assert!(state.online);
    assert_eq!(state.params.iso, Some(1250));
}

#[tokio::test]
async fn a_bad_registration_is_refused_and_records_nothing_808() {
    let cfg = Arc::new(ServiceConfig::from_toml_str(CONFIG).unwrap());
    let registry = RelayRegistry::new();
    let agg = Arc::new(Aggregator::with_registry("test", registry.clone()).unwrap());
    let svc = service(registry.clone(), cfg, agg).await;
    let client = reqwest::Client::new();
    for body in [
        r#"{"id":"bad id","port":8771}"#,
        r#"{"id":"handheld-1","port":0}"#,
        r#"{"id":"","port":8771}"#,
    ] {
        let resp = client
            .post(format!(
                "http://127.0.0.1:{}/api/relays/register",
                svc.port()
            ))
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 400, "{body}");
    }
    assert_eq!(
        registry.lookup("handheld-1", std::time::Instant::now()),
        None
    );
}

/// Issue 1431: both strih boxes declare all seven cameras. A venue camera is register-first with its
/// literal relay as the fallback; a visiting camera is listed once its relay announced itself.
const JOINT_CONFIG: &str = r#"
[[camera]]
id = "cam2"
label = "Cam 2"
transport = "cambox-relay"
address = "register:cam2"
fallback_address = "FALLBACK"

[[camera]]
id = "cam1"
label = "Cam 1"
transport = "cambox-relay"
address = "register:cam1"
visiting = true
"#;

#[tokio::test]
async fn a_visiting_camera_appears_once_its_relay_registers_and_a_venue_camera_falls_back_1431() {
    let home_relay = fake_relay().await;
    let visiting_relay = fake_relay().await;
    let cfg = Arc::new(
        ServiceConfig::from_toml_str(
            &JOINT_CONFIG.replace("FALLBACK", &format!("127.0.0.1:{}", home_relay.port())),
        )
        .unwrap(),
    );
    let registry = RelayRegistry::new();
    let agg = Arc::new(Aggregator::with_registry("test", registry.clone()).unwrap());
    let svc = service(registry.clone(), cfg.clone(), agg.clone()).await;

    // A normal event: only the venue camera is listed, reached through its literal relay (its relay
    // has not registered: an older relay binary, or before the first announcement).
    let before = agg.snapshot(&cfg, &PreviewStore::new()).await;
    let ids: Vec<&str> = before.cameras.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["cam2"], "the other venue's camera is not listed");
    assert!(before.cameras[0].reachable, "reached through the fallback");

    // Joint event: the visiting cambox's relay announces itself to this strih (strih.lan points here).
    let target = bkshading_relay::register::parse_register_url(&format!(
        "http://127.0.0.1:{}/api/relays/register",
        svc.port()
    ))
    .unwrap();
    let status = bkshading_relay::register::announce_once(&target, "cam1", visiting_relay.port())
        .await
        .unwrap();
    assert_eq!(status, 200);

    let after = agg.snapshot(&cfg, &PreviewStore::new()).await;
    let ids: Vec<&str> = after.cameras.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["cam2", "cam1"], "config order kept");
    let cam1 = &after.cameras[1];
    assert!(
        cam1.reachable,
        "the visiting camera is driven through its registration"
    );
    assert_eq!(
        cam1.registration.as_ref().map(|r| r.address.clone()),
        Some(format!("127.0.0.1:{}", visiting_relay.port()))
    );
}
