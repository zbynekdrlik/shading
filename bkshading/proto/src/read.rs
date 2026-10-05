//! Pure assembly of live shading state from raw gphoto2 `get-config` text, and pure
//! planning of the gphoto2 `set-config` writes for a [`SetRequest`]. No IO — the relay
//! shells out to gphoto2 and hands the captured text/plan here, so every transform stays
//! unit-testable without a camera (and standalone-`rustc`-testable under Tier-0).

use crate::mapping::*;
use crate::wire::{CameraCaps, SetRequest, ShadingParams};

/// Compares each key a write burst WROTE (`written`) against the camera's authoritative readback
/// (`readback`) and returns the WIRE field names whose write the camera did NOT apply (issue 1343).
///
/// The relay fills [`crate::wire::RelayState::not_applied`] with this at the burst idle-close (using
/// the SAME `params_and_caps` readback basis the panel already reconciles against), so a camera that
/// ACKs a `set-config` and silently ignores it — cam1 today: the BMPCC drops aperture AND focus PTP
/// writes while ISO applies — surfaces as a per-key `.not-applied` flag instead of the optimistic
/// value just reverting with no signal.
///
/// Per key. APERTURE (`apertureNorm`) is compared via the CHOICE INDEX, not the raw float norm:
/// the written norm and the readback `aperture_norm` are each mapped onto the SAME f-number choice
/// grid via [`norm_to_choice_index`], and a mismatch (or a missing readback) flags it. This catches
/// both a fully-dropped write and the off-grid case (a current f-number like cam1's f/4 that sits
/// below a 4.5-minimum grid readback-snaps to a different nearest choice than the requested on-grid
/// one); an empty `fnumber_choices` means "cannot judge" (never a false flag). ISO / SHUTTER /
/// KELVIN / TINT / FPS are compared BY VALUE against the readback (`fps` is written to d006 as
/// `fps*100` and compared with the readback `fps100`, the d006 project fps, issue 1402); a `None`
/// readback for a written key flags it (the camera did not report the value it should now hold).
///
/// Only keys PRESENT in `written` are compared — an unwritten key never appears. Pure — no IO, no
/// camera; exhaustively rustc/Tier-0 testable.
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
    /// `d006` — PROJECT fps x100: a MENU of the camera's own timebases (`Current: 6000`,
    /// `Choice: N 2398` …). The rate the camera records and outputs, and the only settable
    /// frame rate (issue 1402).
    pub project_fps: String,
    /// `d007` — OFF-SPEED (sensor) fps, a plain int (RANGE 5..60). Readback only.
    pub sensor_fps: String,
    /// `d003` — manual focus DISTANCE (RANGE, ~0=closest..65536=infinite). The only
    /// focus-related property the BMPCC's PTP space documents (issue 1238); read
    /// best-effort by the relay, so an empty block (a camera that does not answer it)
    /// degrades to a `None` `focus_distance`, never a crash.
    pub focus_distance: String,
    /// `gphoto2 --summary` text, read best-effort in the SAME session as `d003` (issue 1306).
    /// Its `F-Number(0x5007) … value: f/4 (400)` line carries the raw current aperture (x100)
    /// that libgphoto2 OMITS from the `f-number` RADIO `Current:` when the lens is open below the
    /// camera's first enumerated stop (`Current: (null)`). Empty when unavailable — the aperture
    /// then simply falls back to the RADIO `Current:` (i.e. the pre-1306 behaviour), never a crash.
    pub summary: String,
}

fn current_i64(block: &str) -> Option<i64> {
    parse_current(block).and_then(|s| s.parse::<i64>().ok())
}

/// Builds `(ShadingParams, CameraCaps)` from the raw gphoto2 config blocks.
///
/// Frame rates (issue 1402, read live on both BMPCC bodies in the fleet): `fps100` is the
/// **project fps** = the d006 MENU `Current:` (already x100). It is what the camera records
/// and outputs, what the shutter angle <-> denominator conversion runs at, and what the
/// issue-809 sync compares against the grab. `sensor_fps100` is the **off-speed fps** = the
/// d007 RANGE `Current:` x100, a diagnostic only. The two never stand in for each other: with
/// no d006 the project fps is `None` (not known), never the off-speed rate — reporting the
/// off-speed rate as the project rate is exactly the issue-1402 bug. The conversion then runs
/// at [`DEFAULT_FPS100`] for that cycle.
pub fn params_and_caps(raw: &RawConfigs) -> (ShadingParams, CameraCaps) {
    let project_fps100 = current_i64(&raw.project_fps);
    // checked: a junk Current must degrade to "not known", never panic a read.
    let sensor_fps100 = current_i64(&raw.sensor_fps).and_then(|f| f.checked_mul(100));
    let fps100 = project_fps100.unwrap_or(DEFAULT_FPS100);

    // Aperture: current f-number -> AV + normalised position within the choices.
    // Use the PARSEABLE-ONLY choice list as the single canonical basis (issue 1304): the readback
    // `aperture_norm`, the caps `fnumber_choices` below, and the relay's `plan_writes` all count
    // from THIS same list, so the panel's +/- step index can never diverge from the write index.
    let fnumber_labels = parse_fnumber_labels(&raw.fnumber);
    // The RADIO `Current:` only when it names a real f-number; libgphoto2 prints `Current: (null)`
    // (-> parse_fnumber None) when the lens is open below the camera's first enumerated stop.
    let current_label = parse_current(&raw.fnumber).filter(|c| parse_fnumber(c).is_some());
    // Fall back to the `--summary` raw (0x5007 value / 100) when the RADIO current is absent —
    // the only honest current-aperture signal in the off-grid case (issue 1306).
    let summary_fnumber =
        parse_summary_fnumber_raw(&raw.summary).map(|raw_x100| raw_x100 as f64 / 100.0);
    let current_fnumber_value = current_label
        .as_deref()
        .and_then(parse_fnumber)
        .or(summary_fnumber);
    let aperture_av = current_fnumber_value.and_then(fnumber_to_av);
    let aperture_norm = match &current_label {
        // An exact enumerated choice -> its exact position in the (filtered) choice list. If the
        // reported `Current:` string is NOT among the filtered choices (e.g. a `f/4` vs `f/4.0`
        // spelling drift, or a value that got junk-filtered), fall back to the NEAREST choice by
        // f-number so the slider is never dead while `aperture_av` is known (review finding #1306).
        Some(cur) => fnumber_labels
            .iter()
            .position(|c| c == cur)
            .map(|i| choices_to_norm(i as i64, fnumber_labels.len() as i64))
            .or_else(|| {
                current_fnumber_value.and_then(|v| nearest_choice_norm(v, &fnumber_labels))
            }),
        // No exact choice (the summary-derived off-grid case) -> the NEAREST choice by f-number,
        // so the slider still shows a sensible position rather than staying dead (issue 1306).
        None => current_fnumber_value.and_then(|v| nearest_choice_norm(v, &fnumber_labels)),
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
        fps100: project_fps100,
        sensor_fps100,
        focus_distance,
    };

    // The OFF-SPEED d007 RANGE bounds; the settable PROJECT rates are `fps_choices` below.
    let (fps_min, fps_max) =
        parse_range(&raw.sensor_fps).unwrap_or((FPS_MIN_FALLBACK, FPS_MAX_FALLBACK));
    let (kelvin_min, kelvin_max) =
        parse_range(&raw.kelvin).unwrap_or((KELVIN_MIN_FALLBACK, KELVIN_MAX_FALLBACK));
    let caps = CameraCaps {
        iso_choices: parse_iso_choices(&raw.iso),
        // issue 1304: expose the f-number choices as plain numbers, from the SAME parseable-only
        // `fnumber_labels` basis used for `aperture_norm` above and for the relay's `plan_writes`
        // (transport.rs also derives its write list via `parse_fnumber_labels`). Every label here
        // parses by construction, so this count EQUALS the write-path count — the panel's +/- step
        // index can never diverge and step to the wrong f-stop.
        fnumber_choices: fnumber_labels
            .iter()
            .filter_map(|c| parse_fnumber(c))
            .collect(),
        shutter_choices: shutter_choices_for_fps(fps100),
        // issue 1402: the d006 project-fps choices, verbatim — the list a `fps` write must hit
        // exactly (`fps_settable`), carried to the service for its "align to grab" offer.
        fps_choices: parse_fps100_choices(&raw.project_fps),
        fps_min,
        fps_max,
        kelvin_min,
        kelvin_max,
    };

    (params, caps)
}

/// Whether the camera exposes its PROJECT fps — the d006 MENU answered with a `Current:` or
/// a choice list (issue 1402). Exposed is not "settable to any rate": a `fps` write is
/// accepted only for a value in the d006 choices ([`fps_settable`]). The off-speed d007
/// RANGE alone does not count — it is not the rate a `fps` write sets.
pub fn fps_supported(raw: &RawConfigs) -> bool {
    current_i64(&raw.project_fps).is_some() || !parse_fps100_choices(&raw.project_fps).is_empty()
}

/// A `fps` write the camera cannot take (issue 1402): `fps * 100` is not one of the camera's
/// own d006 project-fps choices. [`plan_writes`] returns it BEFORE planning anything, so the
/// whole request is refused and nothing reaches the camera. The rate is never rounded to a
/// neighbour (60 is not written as 5994) and never redirected to the off-speed d007. The relay
/// answers it as a client error (422) carrying this message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FpsNotSettable {
    /// The requested project fps, as sent (plain fps, e.g. `60`).
    pub fps: i64,
    /// The camera's own d006 choices (x100), verbatim — empty when it exposes no d006.
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
            "fps-not-settable: fps {} ({} x100) is not one of the camera's d006 project-fps \
             choices [{}] (x100); refused, never rounded to a neighbour, never written to d007",
            self.fps,
            wanted,
            choices.join(", ")
        )
    }
}

impl std::error::Error for FpsNotSettable {}

/// Plans the gphoto2 `set-config` writes for a [`SetRequest`] as ordered
/// `(config-key, value-string)` pairs — pure; the relay executes them. `auto_wb` has no
/// PTP equivalent on the USB path and is silently dropped (matching the MVP box-side).
///
/// `fps` (issue 1402) is written to the PROJECT fps d006 as exactly `fps * 100`, and only
/// when that value is one of `fps_choices` (the camera's d006 choices, [`fps_settable`]).
/// Any other value refuses the WHOLE request with [`FpsNotSettable`] before anything is
/// planned — a request is applied whole or not at all. The off-speed d007 is never written.
pub fn plan_writes(
    req: &SetRequest,
    fnumber_choices: &[String],
    fps100: i64,
    fps_choices: &[i64],
) -> Result<Vec<(String, String)>, FpsNotSettable> {
    // Decide the fps write FIRST, so a refused rate leaves the plan empty.
    let fps_write = match req.fps {
        Some(fps) => match fps.checked_mul(100) {
            Some(wanted) if fps_settable(fps_choices, wanted) => Some(wanted),
            _ => {
                return Err(FpsNotSettable {
                    fps,
                    choices: fps_choices.to_vec(),
                })
            }
        },
        None => None,
    };
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
    if let Some(fps100_value) = fps_write {
        out.push(("d006".to_string(), fps100_value.to_string()));
    }
    Ok(out)
}
