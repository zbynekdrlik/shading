//! The Blackmagic camera REST API (shading#6) — the endpoint paths the relay uses and the JSON
//! shapes they answer, as plain serde types.
//!
//! Source: Blackmagic's "REST API for Blackmagic Cameras" developer manual
//! (`documents.blackmagicdesign.com/DeveloperManuals/RESTAPIforBlackmagicCameras.pdf`), the
//! camera's own `/control/documentation.html`, and live reads on SNV cam5 (Pocket 4K, fw 9.8). The
//! camera serves them under [`API_PREFIX`] over its USB-C CDC-NCM link, with no authentication.
//!
//! Every field is optional and unknown fields are ignored: the manual says a camera fills only
//! the properties it implements, so a missing value reads as "not known this cycle", never as a
//! failed read. Nothing here does IO; the relay fetches and decodes, [`crate::read`] assembles.
//!
//! These paths are a pinned contract with camera-box (`docs/CONTRACT.md`).

use serde::{Deserialize, Deserializer};

/// A JSON number the camera may send as an integer or with a fraction (`400` or `400.0`). The
/// manual types these fields `integer`, but one float would otherwise fail the WHOLE endpoint's
/// decode, so integer fields read either and round.
#[derive(Deserialize)]
#[serde(untagged)]
enum Num {
    Int(i64),
    Float(f64),
}

impl Num {
    fn round(self) -> Option<i64> {
        match self {
            Num::Int(i) => Some(i),
            Num::Float(f) if f.is_finite() && f.abs() < 1e15 => Some(f.round() as i64),
            Num::Float(_) => None,
        }
    }
}

fn int_opt<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    Ok(Option::<Num>::deserialize(d)?.and_then(Num::round))
}

fn int_vec<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<i64>, D::Error> {
    Ok(Vec::<Num>::deserialize(d)?
        .into_iter()
        .filter_map(Num::round)
        .collect())
}

/// Every endpoint below lives under this prefix (`http://<camera>/control/api/v1/...`).
pub const API_PREFIX: &str = "/control/api/v1";

/// `GET` `{deviceName, productName, softwareVersion}` — confirms the address is a camera.
pub const PRODUCT: &str = "/system/product";
/// `GET`/`PUT` `{iso}`.
pub const ISO: &str = "/video/iso";
/// `GET` `{supportedISOs: [int]}`.
pub const SUPPORTED_ISOS: &str = "/video/supportedISOs";
/// `GET`/`PUT` `{shutterSpeed}` or `{shutterAngle}` (x100), whichever measurement the camera's
/// setup uses; `GET` also carries `continuousShutterAutoExposure` (not read).
pub const SHUTTER: &str = "/video/shutter";
/// `GET` `{shutterSpeeds: [int]}` or `{shutterAngles: [number]}` for the current setup.
pub const SUPPORTED_SHUTTERS: &str = "/video/supportedShutters";
/// `GET`/`PUT` `{whiteBalance}` (Kelvin).
pub const WHITE_BALANCE: &str = "/video/whiteBalance";
/// `GET` `{whiteBalance: {min, max}}`.
pub const WHITE_BALANCE_DESCRIPTION: &str = "/video/whiteBalance/description";
/// `PUT` (no body) — run auto white balance.
pub const WHITE_BALANCE_AUTO: &str = "/video/whiteBalance/doAuto";
/// `GET`/`PUT` `{whiteBalanceTint}`.
pub const TINT: &str = "/video/whiteBalanceTint";
/// `GET` `{apertureStop, normalised, apertureNumber, continuousApertureAutoExposure}` (the relay
/// reads `apertureStop`);
/// `PUT` `{apertureStop}`. Only an `apertureStop` write moves the iris on cam5 (an
/// `apertureNumber` or `normalised` write answers 204 and does nothing).
pub const IRIS: &str = "/lens/iris";
/// `GET` `{controllable, apertureStop: {min, max}}`.
pub const IRIS_DESCRIPTION: &str = "/lens/iris/description";
/// `GET` `{normalised}` — read only here (a focus write during a take is unsafe, issue 1238).
pub const FOCUS: &str = "/lens/focus";
/// `GET` `{codec, frameRate, offSpeedEnabled, offSpeedFrameRate, minOffSpeedFrameRate,
/// maxOffSpeedFrameRate, recordResolution, sensorResolution}`; `PUT` `{frameRate}`.
pub const FORMAT: &str = "/system/format";
/// `GET` `{supportedFormats: [{codecs, frameRates, recordResolution, ...}]}`.
pub const SUPPORTED_FORMATS: &str = "/system/supportedFormats";

/// What one read cycle GETs after [`PRODUCT`] confirmed the camera, in this order.
pub const READ_ENDPOINTS: [&str; 12] = [
    ISO,
    SUPPORTED_ISOS,
    SHUTTER,
    SUPPORTED_SHUTTERS,
    WHITE_BALANCE,
    WHITE_BALANCE_DESCRIPTION,
    TINT,
    IRIS,
    IRIS_DESCRIPTION,
    FOCUS,
    FORMAT,
    SUPPORTED_FORMATS,
];

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Product {
    pub device_name: Option<String>,
    pub product_name: Option<String>,
    pub software_version: Option<String>,
}

impl Product {
    /// The model string the relay reports (`RelayState.camera`): the product name, else the
    /// device name. `None` when the answer names neither (not a camera we can talk to).
    pub fn model(&self) -> Option<String> {
        self.product_name
            .as_deref()
            .or(self.device_name.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Iso {
    #[serde(deserialize_with = "int_opt")]
    pub iso: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct SupportedIsos {
    #[serde(rename = "supportedISOs", deserialize_with = "int_vec")]
    pub supported_isos: Vec<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Shutter {
    /// 1/N seconds, when the camera measures shutter as a speed.
    #[serde(deserialize_with = "int_opt")]
    pub shutter_speed: Option<i64>,
    /// Degrees x100 (18000 = 180 deg), when the camera measures shutter as an angle.
    pub shutter_angle: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SupportedShutters {
    #[serde(deserialize_with = "int_vec")]
    pub shutter_speeds: Vec<i64>,
    /// Degrees x100.
    pub shutter_angles: Vec<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WhiteBalance {
    #[serde(deserialize_with = "int_opt")]
    pub white_balance: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct IntRange {
    #[serde(deserialize_with = "int_opt")]
    pub min: Option<i64>,
    #[serde(deserialize_with = "int_opt")]
    pub max: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct NumberRange {
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WhiteBalanceDescription {
    pub white_balance: Option<IntRange>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Tint {
    #[serde(deserialize_with = "int_opt")]
    pub white_balance_tint: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Iris {
    /// The f-number the camera displays (`7.3`).
    pub aperture_stop: Option<f64>,
    /// The iris's place in the lens's range: 0.0 = wide open, 1.0 = closed (LensControl.yaml
    /// min 0.0 max 1.0; cam3 at 24 mm read 0.0 at its widest f/4.0). READ only: a `normalised`
    /// write answers 204 and does not move the iris (shading#6, 0.2.3).
    pub normalised: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IrisDescription {
    pub controllable: Option<bool>,
    pub aperture_stop: Option<NumberRange>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Focus {
    pub normalised: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Resolution {
    #[serde(deserialize_with = "int_opt")]
    pub width: Option<i64>,
    #[serde(deserialize_with = "int_opt")]
    pub height: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Format {
    pub codec: Option<String>,
    /// The project frame rate as the camera writes it (`"23.98"`, `"24"`, `"59.94"`).
    pub frame_rate: Option<String>,
    pub off_speed_enabled: Option<bool>,
    pub off_speed_frame_rate: Option<f64>,
    pub min_off_speed_frame_rate: Option<f64>,
    pub max_off_speed_frame_rate: Option<f64>,
    pub record_resolution: Option<Resolution>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SupportedFormat {
    pub codecs: Vec<String>,
    pub frame_rates: Vec<String>,
    pub record_resolution: Option<Resolution>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SupportedFormats {
    pub supported_formats: Vec<SupportedFormat>,
}

/// Everything one read cycle got from the camera. `None` = that endpoint did not answer with a
/// usable body this cycle (not implemented, an error status, a body that did not decode).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RestRead {
    pub iso: Option<Iso>,
    pub supported_isos: Option<SupportedIsos>,
    pub shutter: Option<Shutter>,
    pub supported_shutters: Option<SupportedShutters>,
    pub white_balance: Option<WhiteBalance>,
    pub white_balance_description: Option<WhiteBalanceDescription>,
    pub tint: Option<Tint>,
    pub iris: Option<Iris>,
    pub iris_description: Option<IrisDescription>,
    pub focus: Option<Focus>,
    pub format: Option<Format>,
    pub supported_formats: Option<SupportedFormats>,
}
