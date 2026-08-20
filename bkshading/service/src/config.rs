//! Service configuration — the camera list.
//!
//! Per the owner architecture (issue 808, 2026-08-20): the camera list is NOT tied to
//! camboxes. A camera is a record with an `id`, a `transport` (cambox-USB relay / SBC
//! relay / ethernet REST), and the relay `address`. `ndi_preview` is OPTIONAL — a handheld
//! with no NDI feed simply has no preview name, and the panel renders a params-only block.

use bkshading_proto::wire::Transport;
use serde::Deserialize;

use crate::preview::PreviewConfig;

fn default_bind() -> String {
    "0.0.0.0:8770".to_string()
}

/// The whole service config (a TOML file).
#[derive(Debug, Clone, Deserialize)]
pub struct ServiceConfig {
    /// Address:port the service web panel binds on.
    #[serde(default = "default_bind")]
    pub bind: String,
    /// The cameras this service aggregates (`[[camera]]` tables).
    #[serde(default, rename = "camera")]
    pub cameras: Vec<CameraConfig>,
    /// Live-preview tuning (the optional `[preview]` table). Defaults are sensible, so it may
    /// be omitted entirely.
    #[serde(default)]
    pub preview: PreviewConfig,
}

/// One camera the service controls through its relay.
#[derive(Debug, Clone, Deserialize)]
pub struct CameraConfig {
    /// Stable id used in the API path and the panel.
    pub id: String,
    /// Human label shown in the panel block.
    pub label: String,
    /// How the camera's shading is reached.
    pub transport: Transport,
    /// `host:port` of the relay serving this camera (for `ethernet-rest`, the camera's own
    /// REST endpoint — M1 treats every transport as a relay-shaped `/api/state` endpoint).
    pub address: String,
    /// Optional NDI source name for the preview. Present => the panel shows a preview block
    /// (an M1 placeholder; real NDI via presenter tech is M2). Absent => params-only block
    /// (a handheld without a video feed).
    #[serde(default)]
    pub ndi_preview: Option<String>,
    /// The box's grab-mode fps this camera should match (issue 809, e.g. `60` for a cam
    /// grabbed at 60 fps). When set, the panel compares the camera's reported project fps
    /// against it and warns on a mismatch (a mismatch makes duplicate/beat artefacts at
    /// the capture source). Absent => no grab comparison for this camera. A static config
    /// field for now (the follow-up is deriving it from the box's live capture_fps).
    #[serde(default)]
    pub grab_fps: Option<i64>,
}

impl ServiceConfig {
    pub fn from_toml_str(s: &str) -> anyhow::Result<Self> {
        Ok(toml::from_str(s)?)
    }

    pub fn from_path(path: &std::path::Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("read config {}: {e}", path.display()))?;
        Self::from_toml_str(&text)
    }

    /// An empty config (no cameras) — the default when no config file is given, so the
    /// service still starts and serves the panel (empty grid).
    pub fn empty() -> Self {
        ServiceConfig {
            bind: default_bind(),
            cameras: Vec::new(),
            preview: PreviewConfig::default(),
        }
    }
}
