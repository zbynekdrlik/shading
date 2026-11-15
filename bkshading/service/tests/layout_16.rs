//! shading#16: the venue's panel layout in the service — the file next to the config, the save with
//! its revision check, the push channel, the `PUT /api/layout` status contract, and the new panel
//! served at `/next/`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::http::StatusCode;
use bkshading::aggregator::Aggregator;
use bkshading::config::ServiceConfig;
use bkshading::http::{has_content_hash, layout_put_response, panel_file, router, AppState};
use bkshading::layout::{layout_path, load, LayoutStore, SaveError, DEFAULT_LAYOUT_FILE};
use bkshading::preview::store::PreviewStore;
use bkshading::registry::RelayRegistry;
use bkshading_proto::layout::{LayoutDoc, LayoutPut, LayoutRefusal};
use bkshading_proto::wire::Aggregate;

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// A fresh empty directory under the system temp dir (no tempfile dependency).
fn scratch_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "bkshading-layout-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn put(rev: u64, order: &[&str], hidden: &[&str]) -> LayoutPut {
    LayoutPut {
        rev,
        order: ids(order),
        hidden: ids(hidden),
    }
}

#[test]
fn the_layout_file_defaults_next_to_the_config() {
    assert_eq!(
        layout_path(None, Some(Path::new("/etc/bkshading/bkshading.toml"))),
        Some(PathBuf::from("/etc/bkshading").join(DEFAULT_LAYOUT_FILE))
    );
    assert_eq!(
        layout_path(None, Some(Path::new("bkshading.toml"))),
        Some(PathBuf::from(DEFAULT_LAYOUT_FILE))
    );
    assert_eq!(
        layout_path(
            Some(Path::new("/var/lib/x.json")),
            Some(Path::new("/etc/a.toml"))
        ),
        Some(PathBuf::from("/var/lib/x.json"))
    );
    assert_eq!(layout_path(None, None), None, "no config: memory only");
}

#[test]
fn no_file_is_number_order_at_rev_0() {
    let dir = scratch_dir("missing");
    assert_eq!(load(&dir.join("none.json")), LayoutDoc::default());
}

#[test]
fn a_broken_file_is_moved_aside_and_number_order_is_used() {
    let dir = scratch_dir("broken");
    let file = dir.join("layout.json");
    std::fs::write(&file, "{ not json").unwrap();
    assert_eq!(load(&file), LayoutDoc::default());
    assert!(!file.exists(), "the broken file is moved aside");
    assert_eq!(
        std::fs::read_to_string(dir.join("layout.json.bad")).unwrap(),
        "{ not json",
        "kept for a look"
    );
}

#[test]
fn a_save_is_written_published_and_survives_a_restart() {
    let dir = scratch_dir("save");
    let file = dir.join("layout.json");
    let store = LayoutStore::open(Some(file.clone()));
    let mut rx = store.subscribe();
    assert_eq!(rx.borrow_and_update().rev, 0);

    let saved = store
        .save(&put(0, &["cam2", "cam1", "cam3"], &["cam3"]), "10.0.0.5:1")
        .unwrap();
    assert_eq!(saved.rev, 1);
    assert_eq!(store.current(), saved);
    assert!(rx.has_changed().unwrap(), "every /ws socket hears the save");
    assert_eq!(*rx.borrow_and_update().clone(), saved);
    assert!(!dir.join("layout.json.tmp").exists(), "no temporary left");

    // A restart reads the same document back.
    assert_eq!(LayoutStore::open(Some(file)).current(), saved);
}

#[test]
fn a_stale_or_invalid_save_changes_nothing() {
    let dir = scratch_dir("refused");
    let file = dir.join("layout.json");
    let store = LayoutStore::open(Some(file.clone()));
    store.save(&put(0, &["cam1", "cam2"], &[]), "a").unwrap();
    let mut rx = store.subscribe();
    rx.borrow_and_update();

    assert_eq!(
        store.save(&put(0, &["cam2", "cam1"], &[]), "b"),
        Err(SaveError::Refused(LayoutRefusal::Stale)),
        "another device saved rev 1 first"
    );
    assert_eq!(
        store.save(&put(1, &["cam1", "cam1"], &[]), "b"),
        Err(SaveError::Refused(LayoutRefusal::Invalid))
    );
    assert_eq!(store.current().rev, 1);
    assert_eq!(store.current().order, ids(&["cam1", "cam2"]));
    assert!(!rx.has_changed().unwrap(), "nothing pushed");
    assert_eq!(load(&file).order, ids(&["cam1", "cam2"]));
}

#[test]
fn a_failed_write_keeps_the_stored_layout() {
    let dir = scratch_dir("nowrite");
    // The file's directory does not exist: the write fails.
    let store = LayoutStore::with_doc(
        Some(dir.join("missing-dir").join("layout.json")),
        LayoutDoc::default(),
    );
    let mut rx = store.subscribe();
    rx.borrow_and_update();
    assert!(matches!(
        store.save(&put(0, &["cam1"], &[]), "a"),
        Err(SaveError::Write(_))
    ));
    assert_eq!(store.current(), LayoutDoc::default());
    assert!(!rx.has_changed().unwrap());
}

#[test]
fn a_memory_only_store_still_saves_and_pushes() {
    let store = LayoutStore::open(None);
    assert_eq!(store.path(), None);
    let doc = store.save(&put(0, &["cam1"], &[]), "a").unwrap();
    assert_eq!(doc.rev, 1);
    assert_eq!(store.current(), doc);
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 16)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn put_layout_answers_200_409_422_and_500() {
    let store = LayoutStore::with_doc(None, LayoutDoc::default());
    let ok = layout_put_response(&store, &put(0, &["cam1", "cam2"], &["cam2"]), "p");
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(body_json(ok).await, serde_json::json!({"rev": 1}));

    let stale = layout_put_response(&store, &put(0, &["cam2", "cam1"], &[]), "p");
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(stale).await,
        serde_json::json!({"error": "stale"})
    );

    let invalid = layout_put_response(&store, &put(1, &["cam1"], &["cam9"]), "p");
    assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body_json(invalid).await,
        serde_json::json!({"error": "invalid"})
    );

    let dir = scratch_dir("http500");
    let broken = LayoutStore::with_doc(Some(dir.join("no").join("l.json")), LayoutDoc::default());
    let failed = layout_put_response(&broken, &put(0, &["cam1"], &[]), "p");
    assert_eq!(failed.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        body_json(failed).await,
        serde_json::json!({"error": "write"})
    );
}

#[test]
fn content_hashed_panel_files_are_recognised() {
    assert!(has_content_hash("bkshading-panel-3f2a9c81d4e6b7a0_bg.wasm"));
    assert!(has_content_hash("x/bkshading-panel-0123456789ab.js"));
    assert!(!has_content_hash("sw.js"));
    assert!(!has_content_hash("manifest.webmanifest"));
    assert!(!has_content_hash("icon-192.png"));
}

#[test]
fn the_panel_page_is_served_and_a_missing_file_is_404() {
    let page = panel_file("index.html");
    assert_eq!(page.status(), StatusCode::OK);
    assert!(page.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/html"));
    assert_eq!(page.headers()["cache-control"], "no-cache");
    assert_eq!(
        panel_file("no-such-file.js").status(),
        StatusCode::NOT_FOUND
    );
}

/// The real router on a free port, with the given layout store.
async fn serve(layout: LayoutStore) -> SocketAddr {
    let cfg = Arc::new(ServiceConfig::empty());
    let registry = RelayRegistry::new();
    let agg = Arc::new(Aggregator::with_registry("test", registry.clone()).unwrap());
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
        layout,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router(state).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });
    addr
}

#[tokio::test]
async fn the_layout_api_and_the_panel_routes_answer_over_http() {
    let store = LayoutStore::with_doc(None, LayoutDoc::default());
    let addr = serve(store.clone()).await;
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let url = |p: &str| format!("http://{addr}{p}");

    let doc: LayoutDoc = http
        .get(url("/api/layout"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(doc, LayoutDoc::default());

    let r = http
        .put(url("/api/layout"))
        .json(&put(0, &["cam3", "cam1"], &["cam1"]))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(store.current().order, ids(&["cam3", "cam1"]));

    let r = http
        .put(url("/api/layout"))
        .json(&put(0, &["cam1"], &[]))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 409);

    let r = http.get(url("/next")).send().await.unwrap();
    assert_eq!(r.status(), 308);
    assert_eq!(r.headers()["location"], "/next/");
    let r = http.get(url("/next/")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let r = http.get(url("/next/some/route")).send().await.unwrap();
    assert_eq!(r.status(), 200, "a path without a dot is the page");
    let r = http.get(url("/next/nope.wasm")).send().await.unwrap();
    assert_eq!(r.status(), 404);
}
