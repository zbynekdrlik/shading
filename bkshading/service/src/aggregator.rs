//! Relay aggregation — the service polls each camera's relay and assembles the panel view.
//!
//! M1 model: poll on demand when the panel asks (`GET /api/cameras`). A background WS push
//! of the aggregate is M2. Each relay is reached over plain HTTP on the LAN; a relay that
//! does not answer within the timeout is reported `reachable: false` (never a panic — the
//! panel greys that block out).

use std::time::{Duration, Instant};

use bkshading_proto::wire::{
    resolve_grab, summarize_set_request, Aggregate, CameraView, FpsSync, RelayState, SetRequest,
};

use crate::config::{CameraConfig, ServiceConfig};

/// Pure assembly of a [`CameraView`] from a camera's config and its (optional) relay state.
/// Split out so the mapping — including "has NDI preview iff `ndi_preview` is configured"
/// and the issue-809 fps-vs-grab sync verdict — is unit-testable without any HTTP.
pub fn camera_view(cam: &CameraConfig, state: Option<RelayState>) -> CameraView {
    // issue 809: compare the camera's reported project fps against the box's grab mode.
    // A camera-offline / unreachable state carries `fps100 = None`, which classifies as
    // Unknown (never a false mismatch).
    let camera_fps100 = state.as_ref().and_then(|s| s.params.fps100);
    // issue 809: DERIVE the effective grab from the box's live reported capture rate when the
    // relay reports one (the ACTUAL capture mode — a box-side mode change is then followed
    // automatically), else fall back to the static config; and VALIDATE the two against each
    // other so a stale config surfaces (`desync`) instead of silently mis-comparing.
    let reported_capture = state.as_ref().and_then(|s| s.capture_fps);
    let resolution = resolve_grab(cam.grab_fps, reported_capture);
    let fps_sync = FpsSync::classify(camera_fps100, resolution.effective);
    CameraView {
        id: cam.id.clone(),
        label: cam.label.clone(),
        transport: cam.transport,
        // M1: preview is a placeholder; a camera is preview-capable iff it has an NDI source
        // configured (a handheld without a feed has none -> params-only block).
        has_preview: cam.ndi_preview.is_some(),
        reachable: state.is_some(),
        grab_fps: resolution.effective,
        grab_fps_desync: resolution.desync,
        fps_sync,
        state,
    }
}

/// Polls relays and assembles the aggregate the panel renders.
#[derive(Clone)]
pub struct Aggregator {
    client: reqwest::Client,
    version: String,
}

impl Aggregator {
    pub fn new(version: impl Into<String>) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(1500))
            .build()?;
        Ok(Aggregator {
            client,
            version: version.into(),
        })
    }

    /// One aggregate snapshot across every configured camera.
    pub async fn snapshot(&self, cfg: &ServiceConfig) -> Aggregate {
        // Poll every relay CONCURRENTLY: /api/cameras latency is bounded by the SLOWEST relay
        // (~one client timeout), not the SUM — the normal M1 state has several relays down, and
        // each unreachable relay would otherwise burn a full connect timeout in series.
        let states =
            futures::future::join_all(cfg.cameras.iter().map(|cam| self.poll_state(cam))).await;
        let cameras = cfg
            .cameras
            .iter()
            .zip(states)
            .map(|(cam, state)| camera_view(cam, state))
            .collect();
        Aggregate {
            version: self.version.clone(),
            cameras,
        }
    }

    /// Reads one relay's `/api/state`, returning `None` if unreachable / malformed.
    async fn poll_state(&self, cam: &CameraConfig) -> Option<RelayState> {
        let url = format!("http://{}/api/state", cam.address);
        match self.client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => resp.json::<RelayState>().await.ok(),
            Ok(resp) => {
                // issue 1309: DEBUG, not WARN — the per-poll (~2 s) reachability state is logged
                // ONCE per transition by `monitor::reach_transitions` in the pump; a per-poll WARN
                // here is the 365 KB/run "relay unreachable" spam the ticket measured.
                tracing::debug!(id = %cam.id, status = %resp.status(), "relay state non-200");
                None
            }
            Err(e) => {
                tracing::debug!(id = %cam.id, error = %e, "relay unreachable");
                None
            }
        }
    }

    /// Forwards a shading write to the camera's relay (`PUT /api/params`). issue 1309: logs EVERY
    /// forward — camera id, the params requested, the relay HTTP status + latency, and the relay's
    /// error body on failure — so a "crash after two SETs" is reconstructible from the strih log
    /// alone (before this, a `PUT /api/params` left no trace on the service side).
    pub async fn forward_set(&self, cam: &CameraConfig, req: &SetRequest) -> anyhow::Result<()> {
        let url = format!("http://{}/api/params", cam.address);
        let params = summarize_set_request(req);
        let start = Instant::now();
        let result = self.client.put(&url).json(req).send().await;
        let latency_ms = start.elapsed().as_millis() as u64;
        match result {
            Ok(resp) if resp.status().is_success() => {
                let status = resp.status();
                tracing::info!(id = %cam.id, params = %params, status = %status, latency_ms,
                    "PUT /api/params -> relay ok");
                Ok(())
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                let body_tail: String = body.chars().take(500).collect();
                tracing::error!(id = %cam.id, params = %params, status = %status, latency_ms,
                    body = %body_tail, "PUT /api/params -> relay non-2xx");
                anyhow::bail!("relay {} returned {}", cam.id, status);
            }
            Err(e) => {
                tracing::error!(id = %cam.id, params = %params, latency_ms, error = %e,
                    "PUT /api/params -> relay unreachable");
                Err(e.into())
            }
        }
    }
}
