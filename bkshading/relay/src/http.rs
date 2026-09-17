//! The relay's local HTTP API, consumed by the `bkshading` aggregation service.
//!
//! Mirrors the dev2 MVP `pybridge` endpoint shape (`/healthz`, live state, write) so the
//! service can treat a cambox relay and a future SBC relay uniformly. Blocking gphoto2
//! work runs in `spawn_blocking` so the async runtime is never stalled.

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
    routing::{get, put},
    Router,
};
use bkshading_proto::wire::{RelayState, SetRequest};

use crate::transport::{ApplyOutcome, CameraSession};

type Shared = Arc<CameraSession>;

/// Builds the relay router. `GET /healthz` liveness, `GET /api/detect` model probe,
/// `GET /api/state` live shading state, `PUT /api/params` write.
pub fn router(session: Shared) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/detect", get(detect))
        .route("/api/state", get(state))
        .route("/api/params", put(set_params))
        .with_state(session)
}

async fn healthz() -> &'static str {
    "ok"
}

async fn detect(State(session): State<Shared>) -> Json<serde_json::Value> {
    let s = session.clone();
    let camera = tokio::task::spawn_blocking(move || s.detect())
        .await
        .unwrap_or(None);
    Json(serde_json::json!({ "camera": camera }))
}

async fn state(State(session): State<Shared>) -> Json<RelayState> {
    let s = session.clone();
    let st = tokio::task::spawn_blocking(move || s.read_state())
        .await
        .unwrap_or_else(|_| RelayState::offline(session.version()));
    Json(st)
}

async fn set_params(
    State(session): State<Shared>,
    Json(req): Json<SetRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, String)> {
    let s = session.clone();
    // issue 1309/1337: funnel through the single-flight FIFO gate + write-burst so rapid SETs never
    // fork two concurrent gphoto2 processes. An `Applied` write returns 200 with the applied count
    // AND the projected resulting state (`{"applied": n, "state": {...}}`) so the service can push
    // an immediate confirmation to the panel; a SET that queued behind an in-flight burst returns
    // 202 `{"queued": true}` (it is applied in order by the in-flight worker).
    match tokio::task::spawn_blocking(move || s.submit(&req)).await {
        Ok(Ok(ApplyOutcome::Applied { count, state })) => Ok((
            StatusCode::OK,
            Json(serde_json::json!({ "applied": count, "state": *state })),
        )),
        Ok(Ok(ApplyOutcome::Queued)) => Ok((
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "queued": true })),
        )),
        // A gphoto2 error (camera unplugged / busy) is an upstream failure, not our bug.
        Ok(Err(e)) => Err((StatusCode::BAD_GATEWAY, e.to_string())),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}
