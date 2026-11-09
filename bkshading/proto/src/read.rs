//! Pure assembly of live shading state from the camera's REST answers ([`RestRead`]), and pure
//! planning of the REST writes for a [`SetRequest`] (shading#6). No IO: the relay fetches the
//! JSON and executes the plan, so every transform is unit-tested without a camera.

use crate::mapping::*;
use crate::rest::{self, Format, RestRead, SupportedFormats};
use crate::wire::{CameraCaps, SetRequest, ShadingParams};

/// Compares each key a write WROTE (`written`) against the camera's read-back (`readback`) and
/// returns the WIRE field names whose write the camera did NOT apply (issue 1343).
///
/// Per key. APERTURE (`apertureNorm`) is compared via the CHOICE INDEX, not the raw float norm:
/// the written norm and the read-back `aperture_norm` are each mapped onto the SAME f-number grid
/// via [`norm_to_choice_index`], and a mismatch (or a missing read-back) flags it; an empty grid
/// means "cannot judge" (never a false flag). The relay drops the aperture flag again when the
/// iris moved (it landed on the lens's own step, which is the camera applying the write). ISO /
/// SHUTTER / KELVIN / TINT / FPS are compared BY VALUE (`fps` against the read-back `fps100` as
/// `fps*100`); a `None` read-back for a written key flags it.
///
/// Only keys PRESENT in `written` are compared — an unwritten key never appears.
pub fn not_applied_keys(
    written: &SetRequest,
    readback: &ShadingParams,
    fnumber_choices: &[f64],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(w_norm) = written.aperture_norm {
        let n = fnumber_choices.len() as i64;
        if n >= 1 {
            let w_idx = norm_to_choice_index(w_norm, n);
            let r_idx = readback.aperture_norm.map(|r| norm_to_choice_index(r, n));
            if r_idx != Some(w_idx) {
                out.push("apertureNorm".to_string());
            }
        }
    }
    if let Some(w) = written.iso {
        if readback.iso != Some(w) {
            out.push("iso".to_string());
        }
    }
    if let Some(w) = written.shutter {
        if readback.shutter != Some(w) {
            out.push("shutter".to_string());
        }
    }
    if let Some(w) = written.kelvin {
        if readback.kelvin != Some(w) {
            out.push("kelvin".to_string());
        }
    }
    if let Some(w) = written.tint {
        if readback.tint != Some(w) {
            out.push("tint".to_string());
        }
    }
    if let Some(w) = written.fps {
        if readback.fps100 != Some(w * 100) {
            out.push("fps".to_string());
        }
    }
    out
}

fn round_i64(v: f64) -> Option<i64> {
    (v.is_finite() && v.abs() < 1e12).then(|| v.round() as i64)
}

/// The camera's project frame rate x100, from `system/format` `frameRate`.
fn project_fps100(read: &RestRead) -> Option<i64> {
    read.format
        .as_ref()
        .and_then(|f| f.frame_rate.as_deref())
        .and_then(parse_frame_rate100)
}

/// One frame rate the camera lists for its current setup: x100 and the camera's own string,
/// which is what a `PUT system/format` sends back verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameRateChoice {
    pub fps100: i64,
    pub label: String,
}

/// The frame rates the camera can switch to WITHOUT changing codec or resolution: the
/// `frameRates` of every `supportedFormats` entry for the current `recordResolution` whose
/// `codecs` list the current codec. When no entry names the codec the resolution alone decides;
/// when no entry matches the resolution either, nothing is settable (an empty list refuses every
/// `fps` write, which is safer than a rate that would also change the resolution). Ascending,
/// one entry per rate (the first string the camera gave for it).
pub fn frame_rate_choices(format: &Format, supported: &SupportedFormats) -> Vec<FrameRateChoice> {
    let res = format.record_resolution;
    let same_res: Vec<&rest::SupportedFormat> = supported
        .supported_formats
        .iter()
        .filter(|s| res.is_none() || s.record_resolution == res)
        .collect();
    let with_codec: Vec<&rest::SupportedFormat> = match format.codec.as_deref() {
        Some(codec) => same_res
            .iter()
            .copied()
            .filter(|s| s.codecs.iter().any(|c| c == codec))
            .collect(),
        None => Vec::new(),
    };
    let pick = if with_codec.is_empty() {
        same_res
    } else {
        with_codec
    };
    let mut out: Vec<FrameRateChoice> = Vec::new();
    for s in pick {
        for label in &s.frame_rates {
            if let Some(fps100) = parse_frame_rate100(label) {
                if !out.iter().any(|c| c.fps100 == fps100) {
                    out.push(FrameRateChoice {
                        fps100,
                        label: label.clone(),
                    });
                }
            }
        }
    }
    out.sort_by_key(|c| c.fps100);
    out
}

fn read_frame_rates(read: &RestRead) -> Vec<FrameRateChoice> {
    match (&read.format, &read.supported_formats) {
        (Some(f), Some(s)) => frame_rate_choices(f, s),
        _ => Vec::new(),
    }
}

/// The aperture grid for this camera's lens ([`fnumber_grid`] over `lens/iris/description`).
pub fn read_fnumber_grid(read: &RestRead) -> Vec<f64> {
    let range = read.iris_description.as_ref().and_then(|d| d.aperture_stop);
    fnumber_grid(range.and_then(|r| r.min), range.and_then(|r| r.max))
}

/// How the camera measures its shutter (`video/shutter` answers one or the other).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutterMeasure {
    /// `shutterSpeed`, 1/N s — the wire's own unit.
    Speed,
    /// `shutterAngle` x100 — converted at the project fps.
    Angle,
}

fn shutter_measure(read: &RestRead) -> ShutterMeasure {
    match read.shutter.as_ref() {
        Some(s) if s.shutter_speed.is_none() && s.shutter_angle.is_some() => ShutterMeasure::Angle,
        _ => ShutterMeasure::Speed,
    }
}

/// The camera's supported shutter ANGLES as `(denominator, angle x100)` at `fps100`, so a write of a
/// listed denominator sends the camera's exact angle back (a round trip through the formula could
/// miss it by a hundredth of a degree).
fn angle_choices(read: &RestRead, fps100: i64) -> Vec<(i64, i64)> {
    let mut out: Vec<(i64, i64)> = Vec::new();
    if let Some(s) = read.supported_shutters.as_ref() {
        for &a in &s.shutter_angles {
            if let Some(a100) = round_i64(a).filter(|v| *v > 0) {
                let denom = convert_angle_or_denom(a100, fps100);
                if !out.iter().any(|(d, _)| *d == denom) {
                    out.push((denom, a100));
                }
            }
        }
    }
    out.sort_unstable();
    out
}

/// The shutter denominators the panel steps over: the camera's `shutterSpeeds`, else its
/// `shutterAngles` converted at the project fps, else [`shutter_choices_for_fps`]. Ascending.
fn shutter_choices(read: &RestRead, fps100: i64) -> Vec<i64> {
    let mut speeds: Vec<i64> = read
        .supported_shutters
        .as_ref()
        .map(|s| {
            s.shutter_speeds
                .iter()
                .copied()
                .filter(|v| *v > 0)
                .collect()
        })
        .unwrap_or_default();
    if speeds.is_empty() {
        speeds = angle_choices(read, fps100)
            .into_iter()
            .map(|(d, _)| d)
            .collect();
    }
    if speeds.is_empty() {
        return shutter_choices_for_fps(fps100);
    }
    speeds.sort_unstable();
    speeds.dedup();
    speeds
}

/// Builds `(ShadingParams, CameraCaps)` from one read cycle's REST answers.
///
/// - aperture: `apertureAv = 2*log2(apertureStop)` (the true displayed f-number), `apertureNorm` =
///   the nearest grid choice ([`read_fnumber_grid`]); the grid is `caps.fNumberChoices`.
/// - `fps100` = the project frame rate (`system/format frameRate`); `sensorFps100` = the off-speed
///   rate x100, a diagnostic; `caps.fpsChoices` = [`frame_rate_choices`] x100;
///   `caps.fpsMin/fpsMax` = the off-speed range.
/// - shutter: the speed denominator, or the angle converted at the project fps (at
///   [`DEFAULT_FPS100`] when the format is unknown this cycle).
/// - `focusNorm` = `lens/focus normalised` (read only); `focusDistance` (PTP d003) is not reported.
pub fn params_and_caps(read: &RestRead) -> (ShadingParams, CameraCaps) {
    let project = project_fps100(read);
    let fps100 = project.unwrap_or(DEFAULT_FPS100);
    let grid = read_fnumber_grid(read);
    let stop = read
        .iris
        .as_ref()
        .and_then(|i| i.aperture_stop)
        .filter(|f| *f > 0.0 && f.is_finite());
    let shutter = read.shutter.as_ref().and_then(|s| {
        s.shutter_speed.filter(|v| *v > 0).or_else(|| {
            s.shutter_angle
                .and_then(round_i64)
                .filter(|v| *v > 0)
                .map(|a| convert_angle_or_denom(a, fps100))
        })
    });
    let format = read.format.as_ref();
    let params = ShadingParams {
        aperture_av: stop.and_then(fnumber_to_av),
        aperture_norm: stop.and_then(|f| nearest_choice_norm(f, &grid)),
        iso: read.iso.as_ref().and_then(|v| v.iso),
        kelvin: read.white_balance.as_ref().and_then(|v| v.white_balance),
        tint: read.tint.as_ref().and_then(|v| v.white_balance_tint),
        shutter,
        fps100: project,
        sensor_fps100: format
            .and_then(|f| f.off_speed_frame_rate)
            .and_then(|v| round_i64(v * 100.0))
            .filter(|v| *v > 0),
        focus_distance: None,
        focus_norm: read
            .focus
            .as_ref()
            .and_then(|f| f.normalised)
            .filter(|v| v.is_finite()),
    };
    let mut iso_choices: Vec<i64> = read
        .supported_isos
        .as_ref()
        .map(|s| s.supported_isos.clone())
        .unwrap_or_default();
    iso_choices.sort_unstable();
    iso_choices.dedup();
    let wb = read
        .white_balance_description
        .as_ref()
        .and_then(|d| d.white_balance);
    let caps = CameraCaps {
        iso_choices,
        fnumber_choices: grid,
        shutter_choices: shutter_choices(read, fps100),
        fps_choices: read_frame_rates(read)
            .into_iter()
            .map(|c| c.fps100)
            .collect(),
        fps_min: format
            .and_then(|f| f.min_off_speed_frame_rate)
            .and_then(round_i64)
            .unwrap_or(FPS_MIN_FALLBACK),
        fps_max: format
            .and_then(|f| f.max_off_speed_frame_rate)
            .and_then(round_i64)
            .unwrap_or(FPS_MAX_FALLBACK),
        kelvin_min: wb.and_then(|r| r.min).unwrap_or(KELVIN_MIN_FALLBACK),
        kelvin_max: wb.and_then(|r| r.max).unwrap_or(KELVIN_MAX_FALLBACK),
    };
    (params, caps)
}

/// Whether the camera exposes its project frame rate: `system/format` named one, or it lists rates
/// to switch to.
pub fn fps_supported(read: &RestRead) -> bool {
    project_fps100(read).is_some() || !read_frame_rates(read).is_empty()
}

/// What a write is planned against, taken from the latest read (shading#6): the aperture grid and
/// where the iris is, the project fps the shutter angle converts at, the frame rates an `fps`
/// write may pick, and how the camera measures its shutter.
#[derive(Debug, Clone, PartialEq)]
pub struct WriteBasis {
    pub fnumber_choices: Vec<f64>,
    /// The iris's f-number at the read (`apertureStop`), to tell whether a write moved it.
    pub aperture_stop: Option<f64>,
    /// The lens's f-number range (`lens/iris/description` `apertureStop.min/max`): where an
    /// `apertureStep` stops widening.
    pub aperture_range: Option<(f64, f64)>,
    /// `lens/iris/description controllable` (`false` = the iris cannot be driven).
    pub aperture_controllable: Option<bool>,
    pub fps100: i64,
    pub frame_rates: Vec<FrameRateChoice>,
    pub shutter_measure: ShutterMeasure,
    /// `(denominator, angle x100)` of the camera's supported angles at `fps100`.
    pub shutter_angles: Vec<(i64, i64)>,
}

impl WriteBasis {
    pub fn from_read(read: &RestRead) -> Self {
        let fps100 = project_fps100(read).unwrap_or(DEFAULT_FPS100);
        WriteBasis {
            fnumber_choices: read_fnumber_grid(read),
            aperture_stop: read.iris.as_ref().and_then(|i| i.aperture_stop),
            aperture_range: read
                .iris_description
                .as_ref()
                .and_then(|d| d.aperture_stop)
                .and_then(|r| Some((r.min?, r.max?))),
            aperture_controllable: read.iris_description.as_ref().and_then(|d| d.controllable),
            fps100,
            frame_rates: read_frame_rates(read),
            shutter_measure: shutter_measure(read),
            shutter_angles: angle_choices(read, fps100),
        }
    }

    /// The settable frame rates x100 (the `fps` write rule, [`fps_settable`]).
    pub fn fps_choices(&self) -> Vec<i64> {
        self.frame_rates.iter().map(|c| c.fps100).collect()
    }
}

/// A `fps` write the camera cannot take (issue 1402, shading#6): `fps * 100` is not one of the
/// frame rates the camera lists for its current codec and resolution. [`plan_writes`] returns it
/// BEFORE planning anything, so the whole request is refused and nothing reaches the camera. The
/// rate is never rounded to a neighbour (60 is not written as 59.94). The relay answers it as a
/// client error (422) carrying this message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FpsNotSettable {
    /// The requested project fps, as sent (plain fps, e.g. `60`).
    pub fps: i64,
    /// The camera's own frame rates x100 — empty when it lists none.
    pub choices: Vec<i64>,
}

impl std::fmt::Display for FpsNotSettable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let wanted = self
            .fps
            .checked_mul(100)
            .map_or_else(|| "out of range".to_string(), |v| v.to_string());
        let choices: Vec<String> = self.choices.iter().map(|c| c.to_string()).collect();
        write!(
            f,
            "fps-not-settable: fps {} ({} x100) is not one of the camera's project frame rates \
             [{}] (x100) for its current codec and resolution; refused, never rounded to a \
             neighbour",
            self.fps,
            wanted,
            choices.join(", ")
        )
    }
}

impl std::error::Error for FpsNotSettable {}

/// One REST write. [`RestWrite::path`] is the endpoint; the relay builds the JSON body.
#[derive(Debug, Clone, PartialEq)]
pub enum RestWrite {
    /// `PUT system/format {"frameRate": <the camera's own string>}`.
    FrameRate(String),
    /// `PUT lens/iris {"apertureStop": f}` — the camera snaps it to the lens's step.
    ApertureStop(f64),
    /// `PUT video/iso {"iso": n}`.
    Iso(i64),
    /// `PUT video/shutter {"shutterSpeed": n}` (1/n s).
    ShutterSpeed(i64),
    /// `PUT video/shutter {"shutterAngle": n}` (degrees x100).
    ShutterAngle(i64),
    /// `PUT video/whiteBalance {"whiteBalance": k}`.
    WhiteBalance(i64),
    /// `PUT video/whiteBalanceTint {"whiteBalanceTint": t}`.
    Tint(i64),
    /// `PUT video/whiteBalance/doAuto` (no body).
    AutoWhiteBalance,
}

impl RestWrite {
    pub fn path(&self) -> &'static str {
        match self {
            RestWrite::FrameRate(_) => rest::FORMAT,
            RestWrite::ApertureStop(_) => rest::IRIS,
            RestWrite::Iso(_) => rest::ISO,
            RestWrite::ShutterSpeed(_) | RestWrite::ShutterAngle(_) => rest::SHUTTER,
            RestWrite::WhiteBalance(_) => rest::WHITE_BALANCE,
            RestWrite::Tint(_) => rest::TINT,
            RestWrite::AutoWhiteBalance => rest::WHITE_BALANCE_AUTO,
        }
    }
}

/// Plans the REST writes for a [`SetRequest`], in order — pure; the relay executes them.
///
/// - `fps` goes FIRST, and only as one of the camera's own frame rates ([`FpsNotSettable`] refuses
///   the WHOLE request otherwise, before anything is planned). Everything after it lands while the
///   camera runs the new rate, and a shutter ANGLE in the same request converts at that rate.
/// - `apertureNorm` picks a grid choice ([`norm_to_choice_index`]) and writes its f-number as
///   `apertureStop`.
/// - `shutter` (a denominator) is written as a speed, or as an angle when the camera measures in
///   angles (the camera's exact listed angle when the denominator is one of its own).
/// - `autoWb: true` runs auto white balance, after any manual Kelvin/tint in the same request.
pub fn plan_writes(req: &SetRequest, basis: &WriteBasis) -> Result<Vec<RestWrite>, FpsNotSettable> {
    let fps_write = match req.fps {
        Some(fps) => {
            let choices = basis.fps_choices();
            let found = fps
                .checked_mul(100)
                .filter(|wanted| fps_settable(&choices, *wanted))
                .and_then(|wanted| basis.frame_rates.iter().find(|c| c.fps100 == wanted));
            match found {
                Some(choice) => Some(choice.clone()),
                None => return Err(FpsNotSettable { fps, choices }),
            }
        }
        None => None,
    };
    let mut out: Vec<RestWrite> = Vec::new();
    if let Some(choice) = &fps_write {
        out.push(RestWrite::FrameRate(choice.label.clone()));
    }
    if let Some(norm) = req.aperture_norm {
        let idx = norm_to_choice_index(norm, basis.fnumber_choices.len() as i64);
        if let Some(&f) = basis.fnumber_choices.get(idx as usize) {
            out.push(RestWrite::ApertureStop(f));
        }
    }
    if let Some(iso) = req.iso {
        out.push(RestWrite::Iso(iso));
    }
    if let Some(denom) = req.shutter {
        match basis.shutter_measure {
            ShutterMeasure::Speed => out.push(RestWrite::ShutterSpeed(denom)),
            ShutterMeasure::Angle => {
                let angle = match &fps_write {
                    Some(choice) => shutter_denom_to_angle100(denom, choice.fps100),
                    None => basis
                        .shutter_angles
                        .iter()
                        .find(|(d, _)| *d == denom)
                        .map(|(_, a)| *a)
                        .unwrap_or_else(|| shutter_denom_to_angle100(denom, basis.fps100)),
                };
                out.push(RestWrite::ShutterAngle(angle));
            }
        }
    }
    if let Some(kelvin) = req.kelvin {
        out.push(RestWrite::WhiteBalance(kelvin));
    }
    if let Some(tint) = req.tint {
        out.push(RestWrite::Tint(tint));
    }
    if req.auto_wb == Some(true) {
        out.push(RestWrite::AutoWhiteBalance);
    }
    Ok(out)
}

/// Whether a write moved the iris: the f-number before and after differ by more than display
/// rounding (0.05). `None` when either side is unknown.
pub fn iris_moved(before: Option<f64>, after: Option<f64>) -> Option<bool> {
    match (before, after) {
        (Some(b), Some(a)) => Some((a - b).abs() > 0.05),
        _ => None,
    }
}
