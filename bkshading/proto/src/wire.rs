//! Shared wire types for the bkshading shading-control protocol.
//!
//! One source of truth for the JSON exchanged between the browser web panel, the
//! `bkshading` aggregation service, and each `bkshading-relay`. Keeping these types in
//! `bkshading-proto` (depended on by BOTH the service and the relay) prevents the exact
//! "duplicate truth" drift the owner flagged in the MVP (Python `mapping.py` vs Kotlin
//! `PtpMapping.kt`). Field names use `camelCase` to stay compatible with the verified
//! dev2 MVP web-UI wire shape (`apertureAv`/`apertureNorm`/`iso`/...).

use serde::{Deserialize, Serialize};

/// How a camera's shading is reached (owner decision 2026-08-20: transports are USB via
/// a relay, or USB-Ethernet REST; Bluetooth is not used at all).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    /// Camera USB -> cambox PC, controlled by a `bkshading-relay` on that cambox.
    CamboxRelay,
    /// Camera USB -> mini SBC on the cage (Pi Zero 2 W), running the SAME relay — a
    /// "mini-cambox without video". Handheld path.
    SbcRelay,
    /// Camera in REST mode via a USB-C->Ethernet adapter (Camera OS >= 8.6). Future
    /// alternative transport; the service treats it as another relay-shaped endpoint.
    EthernetRest,
}

/// Live shading state of a single camera. `None` fields mean "not yet known this poll
/// cycle" (never "zero") — the server is the single source of truth.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShadingParams {
    /// Aperture value: `AV = 2*log2(fNumber)`.
    pub aperture_av: Option<f64>,
    /// Aperture normalised into `[0,1]` across the camera's f-number choices.
    pub aperture_norm: Option<f64>,
    pub iso: Option<i64>,
    pub kelvin: Option<i64>,
    pub tint: Option<i64>,
    /// Shutter speed as a denominator (e.g. `500` == 1/500 s).
    pub shutter: Option<i64>,
    /// Project fps x100 (settable; d007).
    pub fps100: Option<i64>,
    /// Sensor fps x100 (readback only; d006).
    pub sensor_fps100: Option<i64>,
}

/// The camera's own fine-grained value lists/ranges — the web UI rebuilds its ISO and
/// shutter button groups from these when present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraCaps {
    pub iso_choices: Vec<i64>,
    pub shutter_choices: Vec<i64>,
    pub fps_min: i64,
    pub fps_max: i64,
    pub kelvin_min: i64,
    pub kelvin_max: i64,
}

/// A relay's report about the one camera it owns. Served at the relay's `GET /api/state`
/// and forwarded by the service into each web-UI camera block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayState {
    /// Camera detected and answering (gphoto2 auto-detect succeeded this cycle).
    pub online: bool,
    /// Camera model string from `gphoto2 --auto-detect`, when known.
    pub camera: Option<String>,
    pub params: ShadingParams,
    pub caps: Option<CameraCaps>,
    /// Whether the project fps is settable (d007 present) on this camera.
    pub fps_supported: bool,
    /// The relay binary's own version (for diagnostics; the service shows its own).
    pub version: String,
}

impl RelayState {
    /// The state a relay reports when it cannot see its camera this cycle.
    pub fn offline(version: impl Into<String>) -> Self {
        RelayState {
            online: false,
            camera: None,
            params: ShadingParams::default(),
            caps: None,
            fps_supported: false,
            version: version.into(),
        }
    }
}

/// A write request from the web panel — every field optional; only present fields are
/// applied. Aperture is set as a normalised `[0,1]` position (the panel's slider), the
/// relay maps it back to an f-number choice.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetRequest {
    pub aperture_norm: Option<f64>,
    pub iso: Option<i64>,
    pub kelvin: Option<i64>,
    pub tint: Option<i64>,
    /// Shutter denominator (e.g. `500` == 1/500 s).
    pub shutter: Option<i64>,
    /// Project fps (d007).
    pub fps: Option<i64>,
    /// Trigger auto white balance (no PTP equivalent — ignored on the USB path).
    pub auto_wb: Option<bool>,
}

/// Whether a camera's reported frame rate matches the box's grab mode (issue 809). A
/// mismatch produces duplicate/beat artefacts at the source of the capture chain (#674
/// duplicate-frame analysis, #685 capture-rate health), so the panel surfaces it as a
/// visible warning and offers an explicit "align to grab" action (never an auto-write —
/// a camera-side format change can interrupt recording).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FpsSync {
    /// Can't judge: the camera's fps isn't known this cycle (offline / not read) or no
    /// grab mode is configured for this camera.
    #[default]
    Unknown,
    /// The camera's project fps equals the box's grab fps — no beat.
    Synced,
    /// The camera's project fps differs from the box's grab fps — the warning state.
    Mismatch,
}

impl FpsSync {
    /// Classifies a camera's reported project fps (`ShadingParams.fps100`, i.e. fps x100)
    /// against the box's configured grab fps (plain fps, e.g. `60`). Pure — the single
    /// source of truth for the sync verdict, shared by the service and covered by unit
    /// tests. Compares the PROJECT fps (d007), because that is exactly what the "align to
    /// grab" write changes and what the camera's HDMI output follows; `sensor_fps100`
    /// (off-speed d006) stays a separate diagnostic. A non-positive value on either side
    /// (an unread property, a misconfigured `grab_fps = 0`) is treated as "not known" and
    /// yields [`FpsSync::Unknown`] rather than a false mismatch.
    pub fn classify(camera_fps100: Option<i64>, grab_fps: Option<i64>) -> FpsSync {
        match (camera_fps100, grab_fps) {
            (Some(cam), Some(grab)) if cam > 0 && grab > 0 => {
                if cam == grab * 100 {
                    FpsSync::Synced
                } else {
                    FpsSync::Mismatch
                }
            }
            _ => FpsSync::Unknown,
        }
    }
}

/// One camera as the aggregation service presents it to the web panel. A camera with no
/// NDI preview (a handheld, or M1 where preview is not wired yet) has `has_preview =
/// false` and the panel renders a params-only block (no preview area).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraView {
    pub id: String,
    pub label: String,
    pub transport: Transport,
    /// NDI low-quality preview available (presenter integration = M2). Always `false` in
    /// M1 — the block shows a preview placeholder.
    pub has_preview: bool,
    /// The camera's relay was reachable this cycle.
    pub reachable: bool,
    /// The box's grab-mode fps for this camera (issue 809), from config (`60` for cam1).
    /// `None` when the camera's config declares no grab mode — then `fps_sync` is
    /// [`FpsSync::Unknown`] and the panel shows no grab comparison.
    pub grab_fps: Option<i64>,
    /// FPS-vs-grab sync verdict (issue 809) — [`FpsSync::classify`] of the camera's
    /// reported project fps against `grab_fps`. Drives the panel's warning + align button.
    pub fps_sync: FpsSync,
    /// Live state from the relay, when reachable.
    pub state: Option<RelayState>,
}

/// The whole aggregate the service serves at `GET /api/cameras`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Aggregate {
    /// The service's own version (shown in the panel header — version-on-dashboard).
    pub version: String,
    pub cameras: Vec<CameraView>,
}
