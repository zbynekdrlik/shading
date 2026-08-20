//! Relay aggregation — the service polls each camera's relay and assembles the panel view.
//!
//! M1 model: poll on demand when the panel asks (`GET /api/cameras`). A background WS push
//! of the aggregate is M2. Each relay is reached over plain HTTP on the LAN; a relay that
//! does not answer within the timeout is reported `reachable: false` (never a panic — the
//! panel greys that block out).

use std::time::Duration;

use bkshading_proto::wire::{Aggregate, CameraView, RelayState, SetRequest};

use crate::config::{CameraConfig, ServiceConfig};

/// Pure assembly of a [`CameraView`] from a camera's config and its (optional) relay state.
/// Split out so the mapping — including "has NDI preview iff `ndi_preview` is configured" —
/// is unit-testable without any HTTP.
pub fn camera_view(cam: &CameraConfig, state: Option<RelayState>) -> CameraView {
    CameraView {
        id: cam.id.clone(),
        label: cam.label.clone(),
        transport: cam.transport,
        // M1: preview is a placeholder; a camera is preview-capable iff it has an NDI source
        // configured (a handheld without a feed has none -> params-only block).
        has_preview: cam.ndi_preview.is_some(),
        reachable: state.is_some(),
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
        let mut cameras = Vec::with_capacity(cfg.cameras.len());
        for cam in &cfg.cameras {
            let state = self.poll_state(cam).await;
            cameras.push(camera_view(cam, state));
        }
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
                tracing::warn!(id = %cam.id, status = %resp.status(), "relay state non-200");
                None
            }
            Err(e) => {
                tracing::warn!(id = %cam.id, error = %e, "relay unreachable");
                None
            }
        }
    }

    /// Forwards a shading write to the camera's relay (`PUT /api/params`).
    pub async fn forward_set(&self, cam: &CameraConfig, req: &SetRequest) -> anyhow::Result<()> {
        let url = format!("http://{}/api/params", cam.address);
        let resp = self.client.put(&url).json(req).send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("relay {} returned {}", cam.id, resp.status());
        }
        Ok(())
    }
}
