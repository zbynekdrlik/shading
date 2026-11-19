//! shading#19: an NDI preview receiver runs only while someone watches.
//!
//! Live on strih-pp (v0.2.13, no operator): the service used 22 % of a core, all of it the four
//! NDI preview receivers. A LOWEST-bandwidth receiver costs ~5-6 % of a core even when no frame is
//! ever captured (the SDK decodes the 60 fps proxy regardless), and the workers never stopped. So
//! the store now knows when a viewer was last seen (a preview fetch, or the panel's viewer call),
//! and a worker with no viewer within the demand window disconnects its receiver and parks.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bkshading::aggregator::Aggregator;
use bkshading::config::ServiceConfig;
use bkshading::http::{preview_response, router, AppState};
use bkshading::preview::decimate::Decimator;
use bkshading::preview::frame::RawFrame;
use bkshading::preview::pattern::test_pattern_rgb;
use bkshading::preview::source::PreviewSource;
use bkshading::preview::store::{wall_clock_ms, PreviewStore};
use bkshading::preview::worker::{cycle, run_source, RepeatGate};
use bkshading::preview::{PreviewConfig, PREVIEW_DEMAND_MS};
use bkshading::registry::RelayRegistry;
use bkshading_proto::wire::Aggregate;

const CONFIG: &str = r#"
[[camera]]
id = "cam1"
label = "Cam 1"
transport = "cambox-relay"
address = "127.0.0.1:9"
ndi_preview = "CAM1 (usb)"
"#;

/// A feed that delivers a frame on every call after `delay`, and ends after `limit` calls.
struct SlowFeed {
    calls: u32,
    limit: u32,
    delay: Duration,
}

impl PreviewSource for SlowFeed {
    fn name(&self) -> &str {
        "slow"
    }

    fn next_frame(
        &mut self,
        _timeout: Duration,
        keep: &mut dyn FnMut() -> bool,
    ) -> anyhow::Result<Option<RawFrame>> {
        self.calls += 1;
        if self.calls > self.limit {
            anyhow::bail!("feed ended");
        }
        std::thread::sleep(self.delay);
        if !keep() {
            return Ok(None);
        }
        Ok(Some(RawFrame::new(64, 36, test_pattern_rgb(64, 36, 0))))
    }
}

#[test]
fn the_demand_window_follows_the_last_viewer_19() {
    assert_eq!(PREVIEW_DEMAND_MS, 10_000);
    let store = PreviewStore::on_demand(PREVIEW_DEMAND_MS);
    let t = 1_800_000_000_000;
    assert!(!store.wanted(t), "nobody has looked yet");
    store.viewer_seen(t);
    assert!(store.wanted(t + 9_999));
    assert!(store.wanted(t + 10_000));
    assert!(!store.wanted(t + 10_001), "the viewer left");
    // A clock stepped back reads as a viewer just seen, never as an underflow.
    assert!(store.wanted(t - 5_000));

    // A store without a demand window (the tests' and tools' store) always wants frames.
    assert!(PreviewStore::new().wanted(t));
}

#[test]
fn a_preview_fetch_counts_as_a_viewer_19() {
    let cfg = ServiceConfig::from_toml_str(CONFIG).unwrap();
    let store = PreviewStore::on_demand(PREVIEW_DEMAND_MS);
    let now = wall_clock_ms();
    let _ = preview_response(&cfg, &store, "no-such-cam", now);
    assert!(!store.wanted(now), "an unknown camera id is not a viewer");
    let _ = preview_response(&cfg, &store, "cam1", now);
    assert!(store.wanted(now), "a fetch of a known camera's preview is");
}

#[test]
fn with_no_viewer_the_source_is_never_asked_for_a_frame_19() {
    let cfg = PreviewConfig::default();
    let store = PreviewStore::on_demand(PREVIEW_DEMAND_MS);
    let mut feed = SlowFeed {
        calls: 0,
        limit: 1000,
        delay: Duration::ZERO,
    };
    let end = run_source(
        "cam1",
        &mut feed,
        &cfg,
        &store,
        &mut Decimator::new(cfg.fps),
        &mut RepeatGate::default(),
    );
    assert!(end.parked, "no viewer: the session parks");
    assert_eq!(feed.calls, 0, "the receiver is not read at all");
}

#[test]
fn the_session_parks_when_the_viewer_leaves_19() {
    let cfg = PreviewConfig::default();
    // A 300 ms window, the viewer seen once at the start.
    let store = PreviewStore::on_demand(300);
    store.viewer_seen(wall_clock_ms());
    let mut feed = SlowFeed {
        calls: 0,
        limit: 1000,
        delay: Duration::from_millis(20),
    };
    let started = std::time::Instant::now();
    let end = run_source(
        "cam1",
        &mut feed,
        &cfg,
        &store,
        &mut Decimator::new(cfg.fps),
        &mut RepeatGate::default(),
    );
    assert!(end.parked, "it ended because nobody watches: {}", end.error);
    assert!(end.stored, "it delivered while the viewer was there");
    assert!(
        feed.calls < 100,
        "it stopped within the window, not at the feed's end ({} calls)",
        feed.calls
    );
    assert!(started.elapsed() < Duration::from_secs(3));
}

static BUILDS: AtomicU32 = AtomicU32::new(0);

fn counted(_: &str) -> anyhow::Result<Box<dyn PreviewSource>> {
    BUILDS.fetch_add(1, Ordering::SeqCst);
    Ok(Box::new(SlowFeed {
        calls: 0,
        limit: 1,
        delay: Duration::ZERO,
    }))
}

#[test]
fn a_cycle_with_no_viewer_connects_nothing_and_warns_nothing_19() {
    let cfg = PreviewConfig::default();
    let store = PreviewStore::on_demand(PREVIEW_DEMAND_MS);
    let mut trouble = RepeatGate::default();
    let log = cycle(
        "cam1",
        "CAM1 (usb)",
        &cfg,
        &store,
        counted,
        &mut Decimator::new(cfg.fps),
        &mut trouble,
    );
    assert_eq!(BUILDS.load(Ordering::SeqCst), 0, "no NDI receiver is built");
    assert!(!log.warned, "parked is not a failure");
    assert!(!trouble.in_streak());

    store.viewer_seen(wall_clock_ms());
    let _ = cycle(
        "cam1",
        "CAM1 (usb)",
        &cfg,
        &store,
        counted,
        &mut Decimator::new(cfg.fps),
        &mut trouble,
    );
    assert_eq!(BUILDS.load(Ordering::SeqCst), 1, "a viewer connects it");
}

/// The real router on an ephemeral port with `previews`.
async fn service(previews: PreviewStore) -> SocketAddr {
    let cfg = Arc::new(ServiceConfig::from_toml_str(CONFIG).unwrap());
    let registry = RelayRegistry::new();
    let agg = Arc::new(Aggregator::with_registry("test", registry.clone()).unwrap());
    let (live_tx, live_rx) = tokio::sync::watch::channel(Arc::new(Aggregate {
        version: "test".into(),
        cameras: vec![],
    }));
    let state = AppState {
        config: cfg,
        agg,
        previews,
        live: live_rx,
        live_tx: Arc::new(live_tx),
        registry,
        layout: bkshading::layout::LayoutStore::with_doc(None, Default::default()),
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
async fn the_panel_viewer_call_wakes_the_previews_19() {
    let store = PreviewStore::on_demand(PREVIEW_DEMAND_MS);
    let addr = service(store.clone()).await;
    assert!(!store.wanted(wall_clock_ms()));
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/api/preview/viewer"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 204);
    assert!(store.wanted(wall_clock_ms()), "a visible panel is a viewer");
}
