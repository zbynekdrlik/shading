//! The relay's local HTTP API, consumed by the `bkshading` aggregation service.
//!
//! Mirrors the dev2 MVP `pybridge` endpoint shape (`/healthz`, live state, write) so the
//! service can treat a cambox relay and a future SBC relay uniformly. The blocking camera
//! REST calls run in `spawn_blocking` so the async runtime is never stalled.

use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{get, put},
    Router,
};
use bkshading_proto::read::FpsNotSettable;
use bkshading_proto::wire::{
    NotAppliedReason, RelayState, SetRequest, CAMERA_STATUS_HEADER, NOT_APPLIED_REASON_HEADER,
};

use crate::transport::rest::RestError;
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

async fn set_params(State(session): State<Shared>, Json(req): Json<SetRequest>) -> Response {
    let s = session.clone();
    // issue 1309/1337: funnel through the single-flight FIFO gate so rapid SETs never write to the
    // camera at once. An `Applied` write returns 200 with the applied count AND the state read back
    // after it (`{"applied": n, "state": {...}}`) so the service can push an immediate confirmation
    // to the panel; a SET that queued behind an in-flight write returns 202 `{"queued": true}` (it
    // is applied in order by the in-flight worker).
    match tokio::task::spawn_blocking(move || s.submit(&req)).await {
        Ok(Ok(ApplyOutcome::Applied { count, state })) => (
            StatusCode::OK,
            Json(serde_json::json!({ "applied": count, "state": *state })),
        )
            .into_response(),
        Ok(Ok(ApplyOutcome::Queued)) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "queued": true })),
        )
            .into_response(),
        Ok(Err(e)) => set_error_response(&e),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// The answer to a failed `PUT /api/params`: the status of [`set_error_status`], the error's text
/// (both unchanged, the pinned contract), plus the additive `x-bkshading-reason` header and, for a
/// camera refusal, `x-bkshading-camera-status` (shading#17). Pure.
pub fn set_error_response(e: &anyhow::Error) -> Response {
    let (reason, camera_status) = set_error_reason(e);
    let mut resp = (set_error_status(e), e.to_string()).into_response();
    let headers = resp.headers_mut();
    headers.insert(
        HeaderName::from_static(NOT_APPLIED_REASON_HEADER),
        HeaderValue::from_static(reason.as_str()),
    );
    if let Some(status) = camera_status {
        headers.insert(HeaderName::from_static(CAMERA_STATUS_HEADER), status.into());
    }
    resp
}

/// Why a write failed, from its error chain (shading#17): a `fps` the camera does not list =
/// `not-in-choices`; the camera's own non-2xx = `camera-refused` with its status; no answer =
/// `camera-not-answering`; anything else (no camera found, nothing written) = `camera-offline`.
/// Pure.
pub fn set_error_reason(e: &anyhow::Error) -> (NotAppliedReason, Option<u16>) {
    if e.is::<FpsNotSettable>() {
        return (NotAppliedReason::NotInChoices, None);
    }
    match e.downcast_ref::<RestError>() {
        Some(RestError::Status { status, .. }) => (NotAppliedReason::CameraRefused, Some(*status)),
        Some(RestError::Transport(_)) => (NotAppliedReason::CameraNotAnswering, None),
        Some(RestError::Decode(_)) | None => (NotAppliedReason::CameraOffline, None),
    }
}

/// The HTTP status a failed `PUT /api/params` answers with. A `fps` the camera does not list
/// (issue 1402, [`FpsNotSettable`]) is the CLIENT's error, decided before anything reached the
/// camera: `422 Unprocessable Entity`, with the named message listing the camera's own choices.
/// Every other failure is the camera's (offline, no answer, a refused PUT), an upstream failure,
/// not our bug: `502 Bad Gateway`. Pure, so the mapping is unit-tested without an HTTP server.
pub fn set_error_status(e: &anyhow::Error) -> StatusCode {
    if e.is::<FpsNotSettable>() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::BAD_GATEWAY
    }
}
