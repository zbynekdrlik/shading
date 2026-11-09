//! Pure conversions between the Blackmagic camera's values and the bkshading wire.
//!
//! Since shading#6 the relay drives the camera through Blackmagic's REST API
//! (`/control/api/v1/...`, [`crate::rest`]). What is left here is camera-model math with no
//! transport in it: the aperture choice grid and its normalised position, the shutter angle
//! <-> denominator conversion, the frame-rate string <-> fps x100 conversion and the
//! settable-fps rule. It is dependency-free (`std` only, no IO), so every conversion is
//! unit-tested without a camera.
//!
//! The shutter and fps facts come from the PTP era (dev2 MVP `pybridge/mapping.py`, issue
//! 1402) and still hold over REST: `video/shutter` reports either a speed denominator or an
//! angle x100 (18000 = 180 deg), and the project frame rate is the one the camera records
//! and outputs.

// The documented shutter-angle range x100 (1.73 .. 360 deg).
pub const SHUTTER_ANGLE_MIN: i64 = 173;
pub const SHUTTER_ANGLE_MAX: i64 = 36000;

/// Fallback fps x100 (25.00 fps) for the shutter angle <-> denominator conversion in the
/// window before a read has learned the camera's real project fps. Never reported as the
/// camera's fps: `ShadingParams.fps100` stays `None` until `system/format` is read.
pub const DEFAULT_FPS100: i64 = 2500;

/// BMPCC/Pocket Control shutter-speed denominator granularity (1/N seconds). The fallback
/// shutter list when the camera does not answer `video/supportedShutters`.
pub const STANDARD_SHUTTER_DENOMS: [i64; 23] = [
    24, 25, 30, 40, 50, 60, 80, 100, 120, 125, 160, 200, 240, 250, 320, 400, 500, 640, 800, 1000,
    1250, 1600, 2000,
];

// Fallback ranges when the camera does not describe its off-speed fps / Kelvin range —
// matches the web UI's own hardcoded slider bounds.
pub const FPS_MIN_FALLBACK: i64 = 5;
pub const FPS_MAX_FALLBACK: i64 = 60;
pub const KELVIN_MIN_FALLBACK: i64 = 2500;
pub const KELVIN_MAX_FALLBACK: i64 = 10000;

/// The nominal 1/3-stop f-numbers (shading#6). REST gives a lens's aperture RANGE
/// (`lens/iris/description` `apertureStop.min/max`), not its list of stops, so the relay offers
/// these between the lens's min and max as the panel's aperture grid. The camera snaps every
/// `PUT lens/iris {"apertureStop": x}` to the lens's own step, and the read-back carries the
/// true value.
pub const THIRD_STOP_FNUMBERS: [f64; 28] = [
    1.0, 1.1, 1.2, 1.4, 1.6, 1.8, 2.0, 2.2, 2.5, 2.8, 3.2, 3.5, 4.0, 4.5, 5.0, 5.6, 6.3, 7.1, 8.0,
    9.0, 10.0, 11.0, 13.0, 14.0, 16.0, 18.0, 20.0, 22.0,
];

/// Two f-numbers closer than this ratio (1/6 stop, `2^(1/12)`) are one stop of the grid: a nominal
/// value this close to the lens's own min or max is left out, so a step from the end of the lens
/// moves at least 1/6 stop instead of snapping back onto the end.
pub const FNUMBER_GRID_MIN_RATIO: f64 = 1.059_463_094_359_295_3;

/// Rounds ties towards positive infinity, matching Kotlin's `Double.roundToInt()`
/// (and `mapping.py`'s `_round_half_up`). Rust's `f64::round()` also rounds half away
/// from zero, but only diverges from this on negatives, which these conversions never
/// feed; `floor(x + 0.5)` is kept for exact parity with the reference.
fn round_half_up(x: f64) -> i64 {
    (x + 0.5).floor() as i64
}

// --- aperture normalised (0..1) <-> f-number choice index -------------------

/// Normalised `[0,1]` position of `index` within a list of `choice_count` f-number
/// choices. `0.0` when there is only one choice — never divides by zero.
pub fn choices_to_norm(index: i64, choice_count: i64) -> f64 {
    if choice_count <= 1 {
        return 0.0;
    }
    index as f64 / (choice_count - 1) as f64
}

/// Inverse of [`choices_to_norm`]: nearest choice index for a normalised `value`,
/// clamped into `[0, choice_count - 1]`.
pub fn norm_to_choice_index(value: f64, choice_count: i64) -> i64 {
    if choice_count <= 1 {
        return 0;
    }
    let index = round_half_up(value * (choice_count - 1) as f64);
    index.clamp(0, choice_count - 1)
}

/// AV (aperture value) from an f-number: `AV = 2*log2(fNumber)` — the inverse of the
/// web UI's own `fNumber = sqrt(2^AV)`. `None` for a non-positive f-number (`log2(0)`
/// is undefined) — "skip this cycle", not a crash.
pub fn fnumber_to_av(f_number: f64) -> Option<f64> {
    if f_number <= 0.0 {
        return None;
    }
    Some(2.0 * f_number.log2())
}

// --- shutter angle (x100) <-> speed denominator -----------------------

/// Converts between a shutter-speed denominator (e.g. `50` for 1/50s) and a shutter
/// angle x100 (e.g. `18000` = 180 deg) at a given `fps100` — the SAME formula both
/// ways, since `angle = 360 * fps / denom` rearranges to `denom = 360 * fps / angle`.
/// Guards against a zero/negative `value` (never divides by zero); result is at least 1.
pub fn convert_angle_or_denom(value: i64, fps100: i64) -> i64 {
    if value <= 0 {
        return 1;
    }
    round_half_up(360.0 * fps100 as f64 / value as f64).max(1)
}

/// [`convert_angle_or_denom`] specialised for denom -> angle100, additionally clamped
/// to the camera's documented angle range ([`SHUTTER_ANGLE_MIN`]..[`SHUTTER_ANGLE_MAX`]).
pub fn shutter_denom_to_angle100(denom: i64, fps100: i64) -> i64 {
    convert_angle_or_denom(denom, fps100).clamp(SHUTTER_ANGLE_MIN, SHUTTER_ANGLE_MAX)
}

/// The fine-grained shutter-speed denominator list ("shutterChoices" caps) at a given
/// fps: every [`STANDARD_SHUTTER_DENOMS`] entry that is both no faster than the frame
/// rate itself (`denom >= fps`) AND representable within the camera's documented
/// angle RANGE at this fps. Ascending.
pub fn shutter_choices_for_fps(fps100: i64) -> Vec<i64> {
    let fps = fps100 as f64 / 100.0;
    let mut choices = Vec::new();
    for &denom in STANDARD_SHUTTER_DENOMS.iter() {
        if (denom as f64) < fps {
            continue;
        }
        let angle100 = convert_angle_or_denom(denom, fps100);
        if (SHUTTER_ANGLE_MIN..=SHUTTER_ANGLE_MAX).contains(&angle100) {
            choices.push(denom);
        }
    }
    choices
}

// --- aperture grid (REST: a range, not a list of stops) ----------------------

/// The aperture grid between a lens's min and max f-number (shading#6): the lens's own `min` and
/// `max`, plus every [`THIRD_STOP_FNUMBERS`] value strictly between them that is at least 1/6 stop
/// ([`FNUMBER_GRID_MIN_RATIO`]) away from both ends. Ascending. With no usable range (the camera
/// did not describe its iris, or a nonsense range) the whole nominal list is the grid.
pub fn fnumber_grid(min: Option<f64>, max: Option<f64>) -> Vec<f64> {
    let (lo, hi) = match (min, max) {
        (Some(lo), Some(hi)) if lo > 0.0 && hi >= lo && lo.is_finite() && hi.is_finite() => {
            (lo, hi)
        }
        _ => return THIRD_STOP_FNUMBERS.to_vec(),
    };
    let mut grid = vec![lo];
    for &f in THIRD_STOP_FNUMBERS.iter() {
        if f >= lo * FNUMBER_GRID_MIN_RATIO && f * FNUMBER_GRID_MIN_RATIO <= hi {
            grid.push(f);
        }
    }
    if hi > lo {
        grid.push(hi);
    }
    grid
}

/// The normalised slider position of the grid choice NEAREST to `value` (an f-number). The lens
/// lands on its own step, which need not be a grid value, so the slider shows the closest one.
/// `None` for an empty grid.
pub fn nearest_choice_norm(value: f64, choices: &[f64]) -> Option<f64> {
    nearest_choice_index(value, choices).map(|i| choices_to_norm(i as i64, choices.len() as i64))
}

/// The index of the grid choice nearest to `value`, by stops (`log2`), so 4.4 vs 4.0/5.0 is judged
/// the way the eye reads an aperture. `None` for an empty grid or a non-positive value.
pub fn nearest_choice_index(value: f64, choices: &[f64]) -> Option<usize> {
    if choices.is_empty() || value <= 0.0 || !value.is_finite() {
        return None;
    }
    let mut best = 0usize;
    let mut best_d = f64::INFINITY;
    for (i, &c) in choices.iter().enumerate() {
        if c <= 0.0 {
            continue;
        }
        let d = (c / value).log2().abs();
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    Some(best)
}

/// Target choice INDEX when stepping the aperture from the camera's REAL current f-number across
/// the enumerated choice grid, in direction `dir` (+1 = one stop up in f-number, -1 = one down).
/// `choices` are the camera's f-number choices, ascending (the [`fnumber_grid`] order).
/// "+" = the FIRST choice STRICTLY ABOVE `current_fnumber`; "-" = the LAST choice STRICTLY BELOW it.
///
/// The point (issue 1337): step from the camera's actual f-number, NOT from a normalised slider
/// position. A lens open BELOW the first enumerated stop (cam1 f/4.0 vs a grid from f/4.5; cam3
/// f/3.36 — issue 1306) reports `aperture_norm` = idx 0, so the old panel step (`round(norm*(n-1))`
/// then `±1`) sent the SECOND stop on the first "+" and disabled "-". Here that off-grid lens
/// clamps to the MIN choice from EITHER direction, so the FIRST step always moves it onto the grid
/// (the min), never past it. On-grid it is a plain `±1`; at a bound it stays put (the panel then
/// disables that button, but this never indexes out of range). `None` for an empty choice list.
///
/// f-number is strictly monotonic in AV (`AV = 2*log2(f)`), so stepping by f-number value and by
/// AV give the same ordering — the panel passes the f-number it derives from `apertureAv`. This is
/// the pure spec the panel's JS `stepAperture` mirrors (it then sends the ABSOLUTE
/// `apertureNorm = idx/(n-1)`, the exact inverse of [`norm_to_choice_index`], keeping the wire
/// byte-compatible). Pure — Tier-0 tested + JS-mirror-pinned.
pub fn step_choice(current_fnumber: f64, choices: &[f64], dir: i32) -> Option<usize> {
    let n = choices.len();
    if n == 0 {
        return None;
    }
    if n == 1 {
        return Some(0);
    }
    if dir > 0 {
        // First choice strictly above the current f-number; at/above the max, stay at the max.
        Some(
            choices
                .iter()
                .position(|&c| c > current_fnumber)
                .unwrap_or(n - 1),
        )
    } else if dir < 0 {
        // Last choice strictly below the current f-number; at/below the min (incl. off-grid), the
        // min. So an off-grid lens moves onto the grid at idx 0 rather than staying dead.
        Some(
            choices
                .iter()
                .rposition(|&c| c < current_fnumber)
                .unwrap_or(0),
        )
    } else {
        Some(0)
    }
}

/// How far each `apertureStep` probe moves the target, in stops (shading#6, 0.2.1). The camera
/// snaps a written `apertureStop` to the NEAREST native lens step, so a probe only lands past the
/// next step when it crosses the midpoint between the next step and the one after it. An increment
/// below half the sum of two adjacent native gaps can therefore never skip a step. The smallest
/// gaps seen live are about 0.09 stop (the PP 6K's 3.1 -> 3.2 -> 3.3, as the camera reports them to
/// 0.1, so the true gaps may be a little smaller), so 1/16 stop (0.0625) is safe there. It is also
/// below the coordinator's "about 1/12". The argument assumes the iris shows a move within the
/// relay's probe wait; an iris slower than that could see the next probe written mid-travel.
pub const APERTURE_PROBE_STOPS: f64 = 1.0 / 16.0;

/// The f-number of the `k`-th probe (1-based) of an `apertureStep` from `current` (the camera's
/// TRUE f-stop) in direction `dir` (sign only): `current * 2^(k * APERTURE_PROBE_STOPS / 2)` (one
/// stop is a factor sqrt 2 in f-number), clamped to the lens `range` from `lens/iris/description`,
/// rounded to 0.01. `None` when this probe makes no progress (the previous one already reached the
/// end of the range, or `current` is already there): the lens has no further step that way.
pub fn aperture_probe_target(
    current: f64,
    dir: i64,
    k: u32,
    range: Option<(f64, f64)>,
) -> Option<f64> {
    if !(current.is_finite() && current > 0.0) || dir == 0 || k == 0 {
        return None;
    }
    let (lo, hi) = match range {
        Some((lo, hi)) if lo > 0.0 && hi >= lo => (lo, hi),
        _ => (0.0, f64::INFINITY),
    };
    let sign = dir.signum() as f64;
    let at =
        |k: u32| (current * 2f64.powf(sign * k as f64 * APERTURE_PROBE_STOPS / 2.0)).clamp(lo, hi);
    let target = at(k);
    let previous = if k == 1 { current } else { at(k - 1) };
    let progresses = if sign > 0.0 {
        target > previous + 1e-9 && target > current
    } else {
        target < previous - 1e-9 && target < current
    };
    // Round to 0.01, then clamp again, so a range end like 16.666 is never overshot by the rounding.
    progresses.then(|| ((target * 100.0).round() / 100.0).clamp(lo, hi))
}

// --- frame rate --------------------------------------------------------------

/// A Blackmagic REST frame-rate string (`"23.98"`, `"24"`, `"24.00"`, `"59.94"`, `"120"`) as fps
/// x100 (`2398`, `2400`, `2400`, `5994`, `12000`). `None` for anything that is not a positive
/// decimal number.
pub fn parse_frame_rate100(s: &str) -> Option<i64> {
    let t = s.trim();
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    let v: f64 = t.parse().ok()?;
    let x100 = round_half_up(v * 100.0);
    (x100 > 0).then_some(x100)
}

/// Whether the project fps can be set to `wanted_fps100` (fps x100) — the ONE rule both the
/// relay's `fps` write and the service's "align to grab" offer use: the camera lists a
/// frame rate (non-empty `fps_choices`) AND `wanted_fps100` is EXACTLY one of them. A value the
/// camera does not list is never settable: it is not rounded to a neighbour (`5994` for 60) and
/// never written as an off-speed rate. A non-positive value never is.
pub fn fps_settable(fps_choices: &[i64], wanted_fps100: i64) -> bool {
    wanted_fps100 > 0 && fps_choices.contains(&wanted_fps100)
}
