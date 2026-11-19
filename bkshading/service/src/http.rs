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
        ConnectInfo, Path, State,
    },
    http::{header, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{get, post, put},
    Router,
};
use bkshading_proto::layout::{LayoutDoc, LayoutPut};
use bkshading_proto::wire::{
    Aggregate, RelayState, ServerMsg, SetRequest, CAMERA_STATUS_HEADER, NOT_APPLIED_REASON_HEADER,
};
use rust_embed::RustEmbed;
use tokio::sync::watch;

use crate::aggregator::{aggregate_with_camera_update, Aggregator, ForwardError};
use crate::config::{CameraConfig, ServiceConfig};
use crate::layout::{LayoutStore, SaveError};
use crate::preview::store::{wall_clock_ms, PreviewStore};
use crate::registry::{announced_address, valid_relay_id, Registration, RelayRegistry};

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
    /// The publish handle for the live aggregate (issue 1337), shared with the pump. `set_params`
    /// uses it to push an IMMEDIATE per-camera confirmation over the WS after a successful write,
    /// instead of waiting up to 2 s for the next pump tick (the owner's "číslo sa hneď zmení").
    pub live_tx: Arc<watch::Sender<Arc<Aggregate>>>,
    /// Self-registered relays (issue 808): a handheld SBC announces itself here.
    pub registry: RelayRegistry,
    /// The venue's panel layout (shading#16): `GET`/`PUT /api/layout`, pushed over `/ws`.
    pub layout: LayoutStore,
}

/// The operator panel (shading#16, Leptos CSR): Trunk's `bkshading/panel/dist/`, embedded at build
/// time so the service stays one self-contained binary. Served at `/` (the switch, v0.2.10: the old
/// JS panel is gone; `/next/` redirects here). The dist carries the page, the WASM, the manifest,
/// the service worker and the icons. CI builds the real bundle before every service build that
/// ships; native-only jobs create a placeholder `dist/index.html` first (the folder must exist).
#[derive(RustEmbed)]
#[folder = "../panel/dist/"]
pub struct PanelAssets;

/// Where the panel is served (Trunk `public_url`).
pub const PANEL_PREFIX: &str = "/";

/// The panel's phase-1 address (shading#16): an old bookmark or an app installed from it is sent
/// to [`PANEL_PREFIX`].
pub const OLD_PANEL_PREFIX: &str = "/next";

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(|| async { panel_file("index.html") }))
        .route("/api/version", get(version))
        .route("/api/cameras", get(cameras))
        .route("/api/cameras/:id/params", put(set_params))
        .route("/api/cameras/:id/preview.jpg", get(preview_jpg))
        .route("/api/preview/viewer", post(preview_viewer))
        .route("/api/relays/register", post(register_relay))
        .route("/api/layout", get(get_layout).put(put_layout))
        .route("/ws", get(ws_upgrade))
        // shading#16's switch: the panel moved from /next/ to /.
        .route("/next", get(to_panel))
        .route("/next/", get(to_panel))
        .route("/next/*path", get(to_panel))
        // The panel's files, and the page for a client-side address.
        .route("/*path", get(panel_path))
        .with_state(state)
}

/// `POST /api/relays/register` (issue 808): a relay announces `{id, port}`; the service keeps the
/// address the request came FROM, so a handheld on venue WiFi is found whatever DHCP gave it.
async fn register_relay(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Json(reg): Json<Registration>,
) -> Response {
    if !valid_relay_id(&reg.id) || reg.port == 0 {
        tracing::warn!(peer = %peer, "relay registration refused: bad id or port");
        return StatusCode::BAD_REQUEST.into_response();
    }
    let address = announced_address(peer.ip(), reg.port);
    let now = std::time::Instant::now();
    let fresh = state.registry.lookup(&reg.id, now).is_none();
    match state.registry.register(&reg.id, address.clone(), now) {
        Some(old) => tracing::info!(id = %reg.id, from = %old, to = %address, "relay moved"),
        None if fresh => tracing::info!(id = %reg.id, address = %address, "relay registered"),
        None => {}
    }
    Json(serde_json::json!({ "id": reg.id, "address": address })).into_response()
}

/// Upgrades a `GET /ws` to a WebSocket that receives a live push of the whole aggregate:
/// the current state on connect, then a fresh state on every change (issue 808). Push-only —
/// writes stay on `PUT /api/cameras/:id/params`; this is the single-source-of-truth channel
/// the owner asked for (server pushes, every panel sees the same state).
async fn ws_upgrade(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| ws_push(socket, state.live.clone(), state.layout.subscribe()))
}

/// Sends one push message; false when the client is gone or the message did not serialize.
async fn ws_send(socket: &mut WebSocket, msg: &ServerMsg) -> bool {
    match serde_json::to_string(msg) {
        Ok(text) => socket.send(Message::Text(text)).await.is_ok(),
        Err(e) => {
            tracing::warn!(error = %e, "ws: serialize push failed");
            false
        }
    }
}

/// Pushes to one connected panel: the venue layout first (shading#16), then the current
/// aggregate, then whichever changes next — a new aggregate from the pump, or a new layout after a
/// save. A client that goes away is detected on the next `send`; a dropped sender (`changed()` =
/// `Err`) ends the loop. Inbound frames are not read (push-only) — a browser panel sends nothing.
async fn ws_push(
    mut socket: WebSocket,
    mut rx: watch::Receiver<Arc<Aggregate>>,
    mut layout_rx: watch::Receiver<Arc<LayoutDoc>>,
) {
    // `borrow_and_update` marks the current values seen, so `changed()` below only fires on the
    // NEXT update (no duplicate initial send).
    let layout = layout_rx.borrow_and_update().clone();
    if !ws_send(&mut socket, &ServerMsg::Layout((*layout).clone())).await {
        return;
    }
    let snapshot = rx.borrow_and_update().clone();
    if !ws_send(&mut socket, &ServerMsg::State((*snapshot).clone())).await {
        return;
    }
    loop {
        let msg = tokio::select! {
            // Push-only: an inbound frame is never acted on, but a Close (or the end of the
            // stream) ends this loop at once instead of at the next send.
            inbound = socket.recv() => match inbound {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => continue,
            },
            changed = rx.changed() => {
                if changed.is_err() {
                    break; // pump dropped (service shutting down)
                }
                ServerMsg::State((*rx.borrow_and_update().clone()).clone())
            }
            changed = layout_rx.changed() => {
                if changed.is_err() {
                    break; // layout store dropped
                }
                ServerMsg::Layout((*layout_rx.borrow_and_update().clone()).clone())
            }
        };
        if !ws_send(&mut socket, &msg).await {
            break; // client gone
        }
    }
}

/// `GET /api/layout` (shading#16): the venue's panel layout.
async fn get_layout(State(state): State<AppState>) -> Json<LayoutDoc> {
    Json(state.layout.current())
}

/// `PUT /api/layout {rev, order, hidden}` (shading#16): saves the panel's ARRANGE edit. 200
/// `{rev}`; 200 `{"error":"stale"}` when `rev` is not the current revision (another device saved
/// first); 200 `{"error":"invalid"}` for a repeated id or a hidden id outside the order (a refusal
/// is an answer, never a 4xx: Chromium logs those as console errors, shading#16); 500
/// `{"error":"write"}` when the file write failed (nothing changed). On a taken save every `/ws` client
/// gets the new layout.
async fn put_layout(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Json(put): Json<LayoutPut>,
) -> Response {
    // The save writes and flushes a file: off the async workers.
    let store = state.layout.clone();
    match tokio::task::spawn_blocking(move || layout_put_response(&store, &put, &peer.to_string()))
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!(error = %e, "layout: the save task failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "write" })),
            )
                .into_response()
        }
    }
}

/// The pure decision of `PUT /api/layout`, split out so its status contract is tested without an
/// HTTP server.
pub fn layout_put_response(store: &LayoutStore, put: &LayoutPut, peer: &str) -> Response {
    match store.save(put, peer) {
        Ok(doc) => Json(serde_json::json!({ "rev": doc.rev })).into_response(),
        // shading#16: a refusal (stale revision, invalid document) is an ANSWER, 200 with its word:
        // Chromium logs every 4xx as a console error, and the panel's console must stay clean.
        Err(SaveError::Refused(refusal)) => {
            Json(serde_json::json!({ "error": refusal.text() })).into_response()
        }
        Err(SaveError::Write(_)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "write" })),
        )
            .into_response(),
    }
}

/// `GET /next...` (the panel's phase-1 address): a permanent redirect to the panel at `/`.
async fn to_panel() -> Response {
    axum::response::Redirect::permanent(PANEL_PREFIX).into_response()
}

/// `GET /<path>`: an embedded panel file, or the panel page for a path without a dot (a reload on
/// a client-side address). An unknown `api/...` path stays a 404 (never the page as an API answer).
async fn panel_path(Path(path): Path<String>) -> Response {
    if path == "api" || path.starts_with("api/") {
        tracing::debug!(path, "no such api route");
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    if path.contains('.') {
        panel_file(&path)
    } else {
        panel_file("index.html")
    }
}

/// Whether a file name carries a content hash (12 or more hex characters in a row, as Trunk names
/// the panel's JS and WASM): such a file never changes and may be cached for good.
pub fn has_content_hash(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.as_bytes()
        .windows(12)
        .any(|w| w.iter().all(u8::is_ascii_hexdigit))
}

/// One embedded panel file with its type and cache rule, or 404. The page, the service worker and
/// the manifest are `no-cache` (always revalidated: a new release shows at the next load);
/// content-hashed files are immutable.
pub fn panel_file(path: &str) -> Response {
    let Some(file) = <PanelAssets as RustEmbed>::get(path) else {
        tracing::debug!(path, "panel: no such file");
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let mime = mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string();
    let cache = if has_content_hash(path) {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, cache.to_string()),
        ],
        file.data.into_owned(),
    )
        .into_response()
}

async fn version() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "version": VERSION }))
}

async fn cameras(State(state): State<AppState>) -> Json<Aggregate> {
    Json(state.agg.snapshot(&state.config, &state.previews).await)
}

async fn set_params(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SetRequest>,
) -> Response {
    let Some(cam) = state.config.cameras.iter().find(|c| c.id == id) else {
        return (StatusCode::NOT_FOUND, format!("no camera '{id}'")).into_response();
    };
    match state.agg.forward_set(cam, &req).await {
        Ok(relay_state) => {
            // issue 1337: push an IMMEDIATE confirmation over the WS if the relay returned its
            // projected state, instead of waiting for the next ~2 s pump tick. Only THIS camera's
            // view is rebuilt + replaced in the current aggregate (no re-poll of every relay).
            if let Some(rs) = relay_state {
                push_camera_update(&state, cam, rs);
            }
            Json(serde_json::json!({ "ok": true })).into_response()
        }
        Err(e) => forward_error_response(&e),
    }
}

/// The panel's answer to a write that failed on its way to the camera: `502` with the error text
/// (unchanged), plus the additive `x-bkshading-reason` (and `x-bkshading-camera-status`) headers
/// the relay gave, or `relay-unreachable` when the relay could not be reached (shading#17). Pure.
pub fn forward_error_response(e: &anyhow::Error) -> Response {
    let mut resp = (StatusCode::BAD_GATEWAY, e.to_string()).into_response();
    if let Some(fe) = e.downcast_ref::<ForwardError>() {
        let headers = resp.headers_mut();
        if let Some(reason) = fe.reason {
            headers.insert(
                NOT_APPLIED_REASON_HEADER,
                header::HeaderValue::from_static(reason.as_str()),
            );
        }
        if let Some(status) = fe.camera_status {
            headers.insert(CAMERA_STATUS_HEADER, status.into());
        }
    }
    resp
}

/// Pushes an immediate single-camera update over the live WS channel (issue 1337): take the
/// current aggregate, rebuild ONLY `cam`'s view from the relay-returned state, and republish. A
/// no-op if the camera vanished from the config in between. The pump keeps publishing full
/// snapshots on its own cadence; this just shortcuts the confirmation for a just-applied write.
fn push_camera_update(state: &AppState, cam: &CameraConfig, relay_state: RelayState) {
    // Under the channel's lock (shading#27): the pump's merge and this update never interleave.
    state.live_tx.send_modify(|current| {
        *current = Arc::new(aggregate_with_camera_update(current, cam, relay_state));
    });
}

/// `GET /api/cameras/:id/preview.jpg` — the camera's latest preview frame, served only while it
/// is fresh. The panel fetches it a few times a second while the camera view says `previewLive`.
async fn preview_jpg(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    preview_response(&state.config, &state.previews, &id, wall_clock_ms())
}

/// `POST /api/preview/viewer` (shading#19): a VISIBLE panel calls it every 2 s, so the NDI preview
/// receivers connect (and stay connected) only while someone can see the pictures. `204`.
async fn preview_viewer(State(state): State<AppState>) -> StatusCode {
    state.previews.viewer_seen(wall_clock_ms());
    StatusCode::NO_CONTENT
}

/// The pure preview-endpoint decision (issue 808), split out so its status contract is Tier-0
/// tested without an HTTP server:
/// - an unknown camera id -> `404`, the ONLY error status;
/// - a frame at most `config.preview.max_frame_age_ms()` old at `now_ms` -> `200` `image/jpeg`;
/// - no frame yet, a stale frame (the feed stopped), or a camera without an NDI preview ->
///   `204 No Content`, never a 4xx/5xx. Chromium logs every 4xx/5xx resource load as a console
///   error, and a stale frame served as live would freeze the operator's picture.
///
/// Every answer for a known camera is `Cache-Control: no-store`: a preview is always "now". A
/// fetch of a known camera counts as a viewer (shading#19): it keeps the receivers connected.
pub fn preview_response(
    config: &ServiceConfig,
    previews: &PreviewStore,
    id: &str,
    now_ms: u64,
) -> Response {
    if !config.cameras.iter().any(|c| c.id == id) {
        return (StatusCode::NOT_FOUND, format!("no camera '{id}'")).into_response();
    }
    previews.viewer_seen(now_ms);
    match previews.get_fresh(id, now_ms, config.preview.max_frame_age_ms()) {
        Some(frame) => (
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            frame.jpeg.to_vec(),
        )
            .into_response(),
        None => (
            StatusCode::NO_CONTENT,
            [(header::CACHE_CONTROL, "no-store")],
        )
            .into_response(),
    }
}
