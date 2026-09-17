//! Pure PTP <-> wire mapping for a Blackmagic Pocket Cinema Camera 4K (gphoto2
//! 2.5.28, firmware 8.1).
//!
//! Ported byte-for-byte from the dev2 bkshading MVP `pybridge/mapping.py`, which
//! itself mirrors the verified Kotlin `PtpMapping.kt` gphoto2 semantics. Keeping the
//! conversions here — dependency-free (`std` only, no serde, no IO) — makes them
//! independently unit-testable WITHOUT spawning gphoto2 or standing up a camera, and
//! (per the camera-box Tier-0 build policy, issue 557) compilable/runnable with a
//! standalone `rustc --test` when cargo build is unavailable locally.
//!
//! The `mapping.py` KDoc's "Verified PTP facts" carry over unchanged:
//!   - `iso`       RADIO, plain integer values (100..25600, camera clamps)
//!   - `f-number`  RADIO, choice strings like `f/5.2`
//!   - `d002`      shutter angle x100, RANGE 173..36000 (18000 = 180 deg)
//!   - `d004`      WB Kelvin (RANGE)
//!   - `d005`      tint (MENU -50..50)
//!   - `d006`      sensor fps x100 (MENU, e.g. 2500 = 25.00 fps) — readback only
//!   - `d007`      project fps (plain int 5..60) — the settable frame rate

// d002's documented gphoto2 RANGE.
pub const SHUTTER_ANGLE_MIN: i64 = 173;
pub const SHUTTER_ANGLE_MAX: i64 = 36000;

/// Fallback fps x100 (25.00 fps) for the tiny startup window before the first poll
/// cycle has learned the camera's real d006 — matches PtpTransport.kt's DEFAULT_FPS100.
pub const DEFAULT_FPS100: i64 = 2500;

/// Values below this are treated as gphoto2 junk/placeholder ISO Choice entries
/// rather than real camera sensitivities.
pub const MIN_VALID_ISO: i64 = 25;

/// BMPCC/Pocket Control shutter-speed denominator granularity (1/N seconds).
pub const STANDARD_SHUTTER_DENOMS: [i64; 23] = [
    24, 25, 30, 40, 50, 60, 80, 100, 120, 125, 160, 200, 240, 250, 320, 400, 500, 640, 800, 1000,
    1250, 1600, 2000,
];

// Fallback fps/kelvin ranges when a camera's d007/d004 RANGE has no parseable
// Bottom/Top — matches the web UI's own hardcoded slider bounds.
pub const FPS_MIN_FALLBACK: i64 = 5;
pub const FPS_MAX_FALLBACK: i64 = 60;
pub const KELVIN_MIN_FALLBACK: i64 = 2500;
pub const KELVIN_MAX_FALLBACK: i64 = 10000;

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

// --- shutter angle (d002, x100) <-> speed denominator -----------------------

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
/// to the camera's documented `d002` RANGE ([`SHUTTER_ANGLE_MIN`]..[`SHUTTER_ANGLE_MAX`]).
pub fn shutter_denom_to_angle100(denom: i64, fps100: i64) -> i64 {
    convert_angle_or_denom(denom, fps100).clamp(SHUTTER_ANGLE_MIN, SHUTTER_ANGLE_MAX)
}

/// The fine-grained shutter-speed denominator list ("shutterChoices" caps) at a given
/// fps: every [`STANDARD_SHUTTER_DENOMS`] entry that is both no faster than the frame
/// rate itself (`denom >= fps`) AND representable within the camera's documented `d002`
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

// --- gphoto2 "get-config" text-block parsing --------------------------------

/// Extracts the value after `Current:` from one `get-config` response block, or `None`
/// if no such line is present (malformed/empty gphoto2 output).
pub fn parse_current(output: &str) -> Option<String> {
    for raw in output.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("Current:") {
            let value = rest.trim();
            return if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
        }
    }
    None
}

/// Splits a trimmed `Choice: N label` body into `(N, label)` if `N` is an integer and
/// there is at least a whitespace separator — mirrors `mapping.py`'s
/// `_CHOICE_LINE = r"Choice:\s*(\d+)\s+(.*)"` (a bare `Choice: 3` with no label does
/// not match).
fn parse_choice_line(line: &str) -> Option<(i64, String)> {
    let rest = line.trim().strip_prefix("Choice:")?;
    // The regex requires `\s+` between the index and the (possibly empty) label, so the
    // body after "Choice:" must contain a whitespace-separated index token first.
    let rest = rest.trim_start();
    let mut it = rest.splitn(2, char::is_whitespace);
    let n: i64 = it.next()?.parse().ok()?;
    let label = it.next()?.trim().to_string();
    Some((n, label))
}

/// Extracts every `Choice: N label` line's label, ordered by `N` ascending.
pub fn parse_choices(output: &str) -> Vec<String> {
    let mut pairs: Vec<(i64, String)> = output.lines().filter_map(parse_choice_line).collect();
    pairs.sort_by_key(|p| p.0);
    pairs.into_iter().map(|(_, label)| label).collect()
}

/// Sub-`f/0.5` f-number choices are gphoto2/PTP junk placeholders (`f/0`, `f/0.2`) that no real
/// lens exposes; the threshold keeps genuine fast lenses (an `f/0.95`). Used to drop them from the
/// canonical choice grid (issue 1306).
pub const MIN_VALID_FNUMBER: f64 = 0.5;

/// The f-number RADIO choices that parse as REAL f-numbers, preserving order — any non-`f/N.N`
/// entry (a stray `Auto`/`Unknown`/`—`) AND any junk placeholder below [`MIN_VALID_FNUMBER`]
/// (`f/0`, `f/0.2`, issue 1306) is dropped. This is the ONE canonical basis for the aperture
/// choice index (issue 1304): the readback `aperture_norm`, the caps `fnumber_choices`, AND the
/// relay's write `plan_writes` all derive their count from THIS same filtered list, so the panel's
/// +/- step count can never diverge from the write count (and a step near 0 can never write `f/0`).
pub fn parse_fnumber_labels(output: &str) -> Vec<String> {
    parse_choices(output)
        .into_iter()
        .filter(|c| parse_fnumber(c).is_some_and(|f| f >= MIN_VALID_FNUMBER))
        .collect()
}

/// Extracts the raw F-Number value from a gphoto2 `--summary` `F-Number(0x5007)` line, e.g.
/// `F-Number(0x5007):(readwrite) (type=0x4) Enumeration [...] value: f/4 (400)` -> `400`. The raw
/// is the PTP F-Number x100 (`400` = f/4.0). Returns `None` if the line or its trailing
/// parenthesised integer is absent (fail-safe). This is the ONLY honest current-aperture signal
/// when libgphoto2 prints `Current: (null)` for the `f-number` RADIO because the lens sits open
/// below the camera's first enumerated stop (issue 1306).
pub fn parse_summary_fnumber_raw(summary: &str) -> Option<i64> {
    for line in summary.lines() {
        if line.contains("0x5007") {
            // The value is the LAST parenthesised integer on the line: `... value: f/4 (400)`.
            let open = line.rfind('(')?;
            let inner = &line[open + 1..];
            let close = inner.find(')')?;
            return inner[..close].trim().parse::<i64>().ok();
        }
    }
    None
}

/// The normalised slider position of the choice whose f-number is NEAREST to `value` — used when
/// the camera's current aperture (read from `--summary`) is NOT one of its enumerated choices
/// (the lens is open below the first enumerated stop, issue 1306), so the slider still shows a
/// sensible position. `labels` must be the parsed f-number choice labels (e.g. from
/// [`parse_fnumber_labels`]); `None` when there are no parseable choices.
pub fn nearest_choice_norm(value: f64, labels: &[String]) -> Option<f64> {
    let n = labels.len();
    if n == 0 {
        return None;
    }
    let mut best_i = 0usize;
    let mut best_d = f64::INFINITY;
    for (i, c) in labels.iter().enumerate() {
        if let Some(f) = parse_fnumber(c) {
            let d = (f - value).abs();
            if d < best_d {
                best_d = d;
                best_i = i;
            }
        }
    }
    Some(choices_to_norm(best_i as i64, n as i64))
}

/// Target choice INDEX when stepping the aperture from the camera's REAL current f-number across
/// the enumerated choice grid, in direction `dir` (+1 = one stop up in f-number, -1 = one down).
/// `choices` are the camera's f-number choices, ascending (the [`parse_fnumber_labels`] order).
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

/// Parses an f-number choice string like `"f/5.2"` into `5.2`, or `None` if unparseable
/// — matches `mapping.py`'s `re.fullmatch(r"f/(\d+(?:\.\d+)?)")`.
pub fn parse_fnumber(s: &str) -> Option<f64> {
    let num = s.trim().strip_prefix("f/")?;
    // fullmatch of `\d+(\.\d+)?`: digits, optionally a dot followed by more digits.
    // Rejects "5." and ".5" (which the reference regex also rejects), and "5.2.1".
    let (int_part, frac_part) = match num.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (num, None),
    };
    if int_part.is_empty() || !int_part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if let Some(frac) = frac_part {
        if frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    num.parse::<f64>().ok()
}

/// Extracts every ISO `Choice: N value` line's value as an int, ascending — drops
/// anything unparseable and anything below [`MIN_VALID_ISO`].
pub fn parse_iso_choices(output: &str) -> Vec<i64> {
    let mut values: Vec<i64> = output
        .lines()
        .filter_map(parse_choice_line)
        .filter_map(|(_, label)| label.trim().parse::<i64>().ok())
        .filter(|&v| v >= MIN_VALID_ISO)
        .collect();
    values.sort_unstable();
    values
}

/// Extracts `(Bottom, Top)` from a RANGE property's `get-config` output (d007 fps,
/// d004 Kelvin), or `None` if either bound line is missing/unparseable.
pub fn parse_range(output: &str) -> Option<(i64, i64)> {
    let mut bottom: Option<i64> = None;
    let mut top: Option<i64> = None;
    for raw in output.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("Bottom:") {
            bottom = rest.trim().parse().ok();
        } else if let Some(rest) = line.strip_prefix("Top:") {
            top = rest.trim().parse().ok();
        }
    }
    Some((bottom?, top?))
}
