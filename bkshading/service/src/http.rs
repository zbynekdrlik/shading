//! The service web panel + JSON API.
//!
//! Serves the responsive 4+4 shading panel (top preview placeholder, bottom parameters per
//! camera) and the aggregation API. Web assets are embedded (`include_str!`) so the binary
//! is self-contained on the strih PC. The service version is injected into the served HTML
//! (version-on-dashboard) so it is readable straight from the DOM.

use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::{header, HeaderName, StatusCode},
    response::{Html, IntoResponse, Json, Response},
    routing::{get, put},
    Router,
};
use bkshading_proto::wire::{Aggregate, RelayState, ServerMsg, SetRequest};
use tokio::sync::watch;

use crate::aggregator::{aggregate_with_camera_update, Aggregator};
use crate::config::{CameraConfig, ServiceConfig};
use crate::preview::store::PreviewStore;

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const STYLE_CSS: &str = include_str!("../web/style.css");
const VERSION: &str = env!("CARGO_PKG_VERSION");

// issue 1305: installable web app (PWA) assets, embedded so the binary stays self-contained on
// the strih PC (same include_str!/include_bytes! model as the HTML/JS/CSS above). The generator
// for the icons is bkshading/service/web/gen-icons.py (stdlib, no new dependency).
const MANIFEST_JSON: &str = include_str!("../web/manifest.webmanifest");
const SW_JS: &str = include_str!("../web/sw.js");
const ICON_192_PNG: &[u8] = include_bytes!("../web/icon-192.png");
const ICON_512_PNG: &[u8] = include_bytes!("../web/icon-512.png");
const FAVICON_SVG: &str = include_str!("../web/favicon.svg");

// The Content-Type each PWA asset route serves (issue 1305), exposed so the service tests can
// pin the exact type without standing up an HTTP server (mirrors `rendered_index`).
pub const MANIFEST_CONTENT_TYPE: &str = "application/manifest+json";
pub const SW_JS_CONTENT_TYPE: &str = "text/javascript; charset=utf-8";
pub const PNG_CONTENT_TYPE: &str = "image/png";
pub const SVG_CONTENT_TYPE: &str = "image/svg+xml";

/// The embedded PWA manifest JSON (issue 1305) — exposed for the service route tests.
pub fn manifest_asset() -> &'static str {
    MANIFEST_JSON
}
/// The embedded service worker JS (issue 1305).
pub fn sw_js_asset() -> &'static str {
    SW_JS
}
/// The embedded 192px PWA icon PNG bytes (issue 1305).
pub fn icon_192_asset() -> &'static [u8] {
    ICON_192_PNG
}
/// The embedded 512px PWA icon PNG bytes (issue 1305).
pub fn icon_512_asset() -> &'static [u8] {
    ICON_512_PNG
}
/// The embedded SVG favicon (issue 1305).
pub fn favicon_svg_asset() -> &'static str {
    FAVICON_SVG
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<ServiceConfig>,
    pub agg: Arc<Aggregator>,
    /// Latest JPEG preview frame per camera, written by the per-camera preview workers.
    pub previews: PreviewStore,
    /// The latest aggregate, published by the single background pump task (issue 808 WS
    /// milestone). `/ws` clients subscribe to this; keeping the receiver here means there is
    /// always at least one receiver, so the pump's `send` never fails for "no receivers".
    pub live: watch::Receiver<Arc<Aggregate>>,
    /// The publish handle for the live aggregate (issue 1337), shared with the pump. `set_params`
    /// uses it to push an IMMEDIATE per-camera confirmation over the WS after a successful write,
    /// instead of waiting up to 2 s for the next pump tick (the owner's "číslo sa hneď zmení").
    pub live_tx: Arc<watch::Sender<Arc<Aggregate>>>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/style.css", get(style_css))
        // issue 1305: PWA assets (installable web app).
        .route("/manifest.webmanifest", get(manifest))
        .route("/sw.js", get(service_worker))
        .route("/icon-192.png", get(icon_192))
        .route("/icon-512.png", get(icon_512))
        .route("/favicon.svg", get(favicon_svg))
        .route("/api/version", get(version))
        .route("/api/cameras", get(cameras))
        .route("/api/cameras/:id/params", put(set_params))
        .route("/api/cameras/:id/preview.jpg", get(preview_jpg))
        .route("/ws", get(ws_upgrade))
        .with_state(state)
}

/// Upgrades a `GET /ws` to a WebSocket that receives a live push of the whole aggregate:
/// the current state on connect, then a fresh state on every change (issue 808). Push-only —
/// writes stay on `PUT /api/cameras/:id/params`; this is the single-source-of-truth channel
/// the owner asked for (server pushes, every panel sees the same state).
async fn ws_upgrade(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| ws_push(socket, state.live.clone()))
}

/// Pushes the latest aggregate to one connected panel: send the current state immediately,
/// then block on `changed()` and send each new state. A client that goes away is detected on
/// the next `send`; a dropped pump (`changed()` = `Err`) ends the loop. Inbound frames are not
/// read (push-only) — a browser panel sends nothing.
async fn ws_push(mut socket: WebSocket, mut rx: watch::Receiver<Arc<Aggregate>>) {
    loop {
        // `borrow_and_update` marks the current value seen, so the FIRST `changed()` below
        // only fires on the NEXT pump update (no duplicate initial send).
        let snapshot = rx.borrow_and_update().clone();
        match serde_json::to_string(&ServerMsg::State((*snapshot).clone())) {
            Ok(text) => {
                if socket.send(Message::Text(text)).await.is_err() {
                    break; // client gone
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "ws: serialize aggregate failed");
                break;
            }
        }
        if rx.changed().await.is_err() {
            break; // pump/sender dropped (service shutting down)
        }
    }
}

/// Injects the compiled service version into the served HTML so the panel header shows it
/// straight from the DOM.
pub fn rendered_index() -> String {
    INDEX_HTML.replace("{{VERSION}}", VERSION)
}

async fn index() -> Html<String> {
    Html(rendered_index())
}

async fn app_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        APP_JS,
    )
}

async fn style_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLE_CSS,
    )
}

// issue 1305: PWA asset handlers. Each serves an embedded asset with its exact Content-Type.
async fn manifest() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, MANIFEST_CONTENT_TYPE)],
        MANIFEST_JSON,
    )
}

async fn service_worker() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, SW_JS_CONTENT_TYPE),
            // Allow the SW to control the whole origin even though it is served from /sw.js.
            (HeaderName::from_static("service-worker-allowed"), "/"),
        ],
        SW_JS,
    )
}

async fn icon_192() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, PNG_CONTENT_TYPE)], ICON_192_PNG)
}

async fn icon_512() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, PNG_CONTENT_TYPE)], ICON_512_PNG)
}

async fn favicon_svg() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, SVG_CONTENT_TYPE)], FAVICON_SVG)
}

async fn version() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "version": VERSION }))
}

async fn cameras(State(state): State<AppState>) -> Json<Aggregate> {
    Json(state.agg.snapshot(&state.config).await)
}

async fn set_params(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SetRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let Some(cam) = state.config.cameras.iter().find(|c| c.id == id) else {
        return Err((StatusCode::NOT_FOUND, format!("no camera '{id}'")));
    };
    match state.agg.forward_set(cam, &req).await {
        Ok(relay_state) => {
            // issue 1337: push an IMMEDIATE confirmation over the WS if the relay returned its
            // projected state, instead of waiting for the next ~2 s pump tick. Only THIS camera's
            // view is rebuilt + replaced in the current aggregate (no re-poll of every relay).
            if let Some(rs) = relay_state {
                push_camera_update(&state, cam, rs);
            }
            Ok(Json(serde_json::json!({ "ok": true })))
        }
        Err(e) => Err((StatusCode::BAD_GATEWAY, e.to_string())),
    }
}

/// Pushes an immediate single-camera update over the live WS channel (issue 1337): take the
/// current aggregate, rebuild ONLY `cam`'s view from the relay-returned state, and republish. A
/// no-op if the camera vanished from the config in between. The pump keeps publishing full
/// snapshots on its own cadence; this just shortcuts the confirmation for a just-applied write.
fn push_camera_update(state: &AppState, cam: &CameraConfig, relay_state: RelayState) {
    let current = state.live_tx.borrow().clone();
    let updated = aggregate_with_camera_update(&current, cam, relay_state);
    let _ = state.live_tx.send(Arc::new(updated));
}

/// The latest JPEG preview frame for a camera. The web UI's preview block reloads an `<img>`
/// against this at a few fps (cache-busting query). `404` for an unknown camera; `503` until
/// the first frame is produced (the block shows its placeholder meanwhile). Always `no-store`
/// — a preview is always "now".
async fn preview_jpg(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    if !state.config.cameras.iter().any(|c| c.id == id) {
        return (StatusCode::NOT_FOUND, format!("no camera '{id}'")).into_response();
    }
    match state.previews.get(&id) {
        Some(frame) => (
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            frame.jpeg.to_vec(),
        )
            .into_response(),
        None => (StatusCode::SERVICE_UNAVAILABLE, "no preview frame yet").into_response(),
    }
}
