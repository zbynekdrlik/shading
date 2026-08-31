//! Pure assembly of live shading state from raw gphoto2 `get-config` text, and pure
//! planning of the gphoto2 `set-config` writes for a [`SetRequest`]. No IO — the relay
//! shells out to gphoto2 and hands the captured text/plan here, so every transform stays
//! unit-testable without a camera (and standalone-`rustc`-testable under Tier-0).

use crate::mapping::*;
use crate::wire::{CameraCaps, SetRequest, ShadingParams};

/// The raw `gphoto2 --get-config <key>` text blocks the relay captured this cycle, one
/// per shading property. Empty strings are tolerated (a property the camera did not
/// answer degrades to `None`, never a crash).
#[derive(Debug, Default, Clone)]
pub struct RawConfigs {
    /// `iso` — RADIO, plain integer Choice values.
    pub iso: String,
    /// `f-number` — RADIO, choice strings like `f/5.2`.
    pub fnumber: String,
    /// `d002` — shutter angle x100 (RANGE 173..36000).
    pub shutter_angle: String,
    /// `d004` — WB Kelvin (RANGE).
    pub kelvin: String,
    /// `d005` — tint (MENU -50..50).
    pub tint: String,
    /// `d006` — sensor fps x100 (readback only).
    pub sensor_fps: String,
    /// `d007` — project fps (settable, 5..60).
    pub project_fps: String,
    /// `d003` — manual focus DISTANCE (RANGE, ~0=closest..65536=infinite). The only
    /// focus-related property the BMPCC's PTP space documents (issue 1238); read
    /// best-effort by the relay, so an empty block (a camera that does not answer it)
    /// degrades to a `None` `focus_distance`, never a crash.
    pub focus_distance: String,
}

fn current_i64(block: &str) -> Option<i64> {
    parse_current(block).and_then(|s| s.parse::<i64>().ok())
}

/// Builds `(ShadingParams, CameraCaps)` from the raw gphoto2 config blocks.
///
/// M1 fps-source choice (documented, refined against the live camera in M2): the working
/// `fps100` for the shutter angle<->denominator conversion is the **project fps** (d007)
/// x100 when known, else the **sensor fps** (d006, already x100), else [`DEFAULT_FPS100`].
/// `sensor_fps100` is reported verbatim from d006 for exact off-speed readback.
pub fn params_and_caps(raw: &RawConfigs) -> (ShadingParams, CameraCaps) {
    let sensor_fps100 = current_i64(&raw.sensor_fps);
    let project_fps = current_i64(&raw.project_fps);
    let fps100 = project_fps
        .map(|f| f * 100)
        .or(sensor_fps100)
        .unwrap_or(DEFAULT_FPS100);

    // Aperture: current f-number choice -> AV + normalised position within the choices.
    let fnumber_choices = parse_choices(&raw.fnumber);
    let current_fnumber = parse_current(&raw.fnumber);
    let (aperture_av, aperture_norm) = match &current_fnumber {
        Some(cur) => {
            let av = parse_fnumber(cur).and_then(fnumber_to_av);
            let norm = fnumber_choices
                .iter()
                .position(|c| c == cur)
                .map(|i| choices_to_norm(i as i64, fnumber_choices.len() as i64));
            (av, norm)
        }
        None => (None, None),
    };

    let iso = current_i64(&raw.iso);
    let shutter =
        current_i64(&raw.shutter_angle).map(|angle| convert_angle_or_denom(angle, fps100));
    let kelvin = current_i64(&raw.kelvin);
    let tint = current_i64(&raw.tint);
    // d003 manual focus distance (issue 1238): reported verbatim as the raw current value.
    // An empty/absent block -> None (the camera did not answer d003 this cycle), never 0.
    let focus_distance = current_i64(&raw.focus_distance);

    let params = ShadingParams {
        aperture_av,
        aperture_norm,
        iso,
        kelvin,
        tint,
        shutter,
        fps100: project_fps.map(|f| f * 100).or(sensor_fps100),
        sensor_fps100,
        focus_distance,
    };

    let (fps_min, fps_max) =
        parse_range(&raw.project_fps).unwrap_or((FPS_MIN_FALLBACK, FPS_MAX_FALLBACK));
    let (kelvin_min, kelvin_max) =
        parse_range(&raw.kelvin).unwrap_or((KELVIN_MIN_FALLBACK, KELVIN_MAX_FALLBACK));
    let caps = CameraCaps {
        iso_choices: parse_iso_choices(&raw.iso),
        shutter_choices: shutter_choices_for_fps(fps100),
        fps_min,
        fps_max,
        kelvin_min,
        kelvin_max,
    };

    (params, caps)
}

/// Whether the project fps (d007) is settable on this camera — the RANGE parsed a
/// Bottom/Top, i.e. the property is exposed.
pub fn fps_supported(raw: &RawConfigs) -> bool {
    parse_range(&raw.project_fps).is_some() || current_i64(&raw.project_fps).is_some()
}

/// Plans the gphoto2 `set-config` writes for a [`SetRequest`] as ordered
/// `(config-key, value-string)` pairs — pure; the relay executes them. `auto_wb` has no
/// PTP equivalent on the USB path and is silently dropped (matching the MVP box-side).
pub fn plan_writes(
    req: &SetRequest,
    fnumber_choices: &[String],
    fps100: i64,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    if let Some(norm) = req.aperture_norm {
        let idx = norm_to_choice_index(norm, fnumber_choices.len() as i64);
        if let Some(choice) = fnumber_choices.get(idx as usize) {
            out.push(("f-number".to_string(), choice.clone()));
        }
    }
    if let Some(iso) = req.iso {
        out.push(("iso".to_string(), iso.to_string()));
    }
    if let Some(shutter) = req.shutter {
        out.push((
            "d002".to_string(),
            shutter_denom_to_angle100(shutter, fps100).to_string(),
        ));
    }
    if let Some(kelvin) = req.kelvin {
        out.push(("d004".to_string(), kelvin.to_string()));
    }
    if let Some(tint) = req.tint {
        out.push(("d005".to_string(), tint.to_string()));
    }
    if let Some(fps) = req.fps {
        out.push(("d007".to_string(), fps.to_string()));
    }
    out
}
