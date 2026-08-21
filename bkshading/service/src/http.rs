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
    http::{header, StatusCode},
    response::{Html, IntoResponse, Json, Response},
    routing::{get, put},
    Router,
};
use bkshading_proto::wire::{Aggregate, ServerMsg, SetRequest};
use tokio::sync::watch;

use crate::aggregator::Aggregator;
use crate::config::ServiceConfig;
use crate::preview::store::PreviewStore;

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const STYLE_CSS: &str = include_str!("../web/style.css");
const VERSION: &str = env!("CARGO_PKG_VERSION");

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
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/style.css", get(style_css))
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
        Ok(()) => Ok(Json(serde_json::json!({ "ok": true }))),
        Err(e) => Err((StatusCode::BAD_GATEWAY, e.to_string())),
    }
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
