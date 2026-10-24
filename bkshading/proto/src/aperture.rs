//! Aperture write convergence (issue 1452) — the pure decisions the relay runs after an aperture
//! write inside a write burst. No IO: the relay reads 0x5007 through its paced `gphoto2 --shell`
//! and hands the raw values here.
//!
//! Live 19.10.2026 (issue 1452 comments 6262112518 / 6262566110): a BMPCC moves its iris on a
//! 0x5007 write but does not always land on the requested listed value — cam6 (Pocket 6K, fw 8.1)
//! lands ONE listed step past it (f/4.0 -> request f/4.5 -> f/4.8), cam5 (Pocket 4K, fw 9.8) lands
//! the lens's own wide-open stop outside its list, cam7 (Pocket 6K, fw 8.6) does not move at all.
//! So the relay:
//! 1. reads 0x5007 back until the iris settles ([`aperture_settle`]),
//! 2. corrects an overshoot by ONE listed step back toward the request ([`aperture_correction`]),
//!    never a loop,
//! 3. reports a write that moved the iris as applied, wherever it landed
//!    ([`not_applied_after_aperture_motion`]); only a write that did not move keeps the issue-1343
//!    `notApplied` flag.

use std::cmp::Ordering;

use crate::mapping::{fnumber_to_av, nearest_choice_norm, parse_current, parse_fnumber};

/// The ONE correcting write after an aperture write (issue 1452): `Some(index)` of the listed
/// f-number to write when the camera overshot the request, `None` otherwise.
///
/// All positions are indices into the relay's ascending f-number list of length `n`
/// (`parse_fnumber_labels`). `before` = where the iris was before the write, `landed` = where it
/// settled after it; `None` = not a listed value (or not known).
///
/// An overshoot is a landing PAST the request in the direction the write asked for
/// (`before < requested < landed`, or the mirror). The correction is ONE listed step from `landed`
/// back toward the request — on cam6 that is the request itself. Everything else is `None`:
/// landed on the request, did not move, stopped short, a position outside the list or unknown (no
/// direction to judge by), or landed on the first / last listed value (the lens's own stop, which a
/// step back would not fix). The correction's own result is never corrected again: the relay calls
/// this once per write.
pub fn aperture_correction(
    requested: usize,
    before: Option<usize>,
    landed: Option<usize>,
    n: usize,
) -> Option<usize> {
    let (before, landed) = (before?, landed?);
    if n < 2 || requested >= n || before >= n || landed >= n {
        return None;
    }
    if landed == 0 || landed == n - 1 {
        return None;
    }
    match (requested.cmp(&before), landed.cmp(&requested)) {
        (Ordering::Greater, Ordering::Greater) => Some(landed - 1),
        (Ordering::Less, Ordering::Less) => Some(landed + 1),
        _ => None,
    }
}

/// Whether the 0x5007 read-back after an aperture write has settled (issue 1452): `Some(raw)` =
/// the iris's settled value, `None` = read again.
///
/// `reads` are the raw values (x100) read so far, oldest first; `before_raw` the value before the
/// write. Settled = the last two reads agree AND differ from the start (the iris moved and
/// stopped). Two reads still at the start do not settle it — the iris may not have started yet —
/// so a body that does not move (cam7) ends at `max_reads` with the start value, which the report
/// reads as "did not move". `max_reads` bounds every wait.
pub fn aperture_settle(before_raw: Option<i64>, reads: &[i64], max_reads: usize) -> Option<i64> {
    let last = *reads.last()?;
    let stable = reads.len() >= 2 && reads[reads.len() - 2] == last;
    if (stable && before_raw != Some(last)) || reads.len() >= max_reads {
        Some(last)
    } else {
        None
    }
}

/// Whether an aperture write moved the iris (issue 1452): `landings` are the settled raw values
/// (x100) after the write and after its correction, oldest first. ANY landing away from
/// `before_raw` is a move — a write corrected back onto its starting value still moved twice, it
/// was never ignored. Compared as values, so a landing outside the list (cam5's wide-open f/4.0)
/// counts. `None` when the start is unknown or nothing was read back.
pub fn aperture_moved(before_raw: Option<i64>, landings: &[i64]) -> Option<bool> {
    let before = before_raw?;
    if landings.is_empty() {
        return None;
    }
    Some(landings.iter().any(|raw| *raw != before))
}

/// The issue-1343 `notApplied` list after the issue-1452 motion check: a burst whose last aperture
/// write MOVED the iris (`Some(true)`) drops `apertureNorm` — the camera took the write and landed
/// elsewhere, which the state reports as its real value. A write that did not move (`Some(false)`)
/// or was not measured (`None`) keeps the flag, exactly as before. Other keys are untouched.
pub fn not_applied_after_aperture_motion(
    mut not_applied: Vec<String>,
    aperture_moved: Option<bool>,
) -> Vec<String> {
    if aperture_moved == Some(true) {
        not_applied.retain(|k| k != "apertureNorm");
    }
    not_applied
}

/// The index of the listed f-number whose value x100 is `raw_x100` (issue 1452), e.g. `f/4.8` for
/// `480`; `None` when the value is not on the list.
pub fn fnumber_label_index(labels: &[String], raw_x100: i64) -> Option<usize> {
    labels
        .iter()
        .position(|l| parse_fnumber(l).map(|f| (f * 100.0).round() as i64) == Some(raw_x100))
}

/// The raw aperture (x100) from a `get-config 5007` block's `Current:` (issue 1452); `None` for a
/// missing, junk or non-positive value.
pub fn fnumber_raw_from_block(block: &str) -> Option<i64> {
    parse_current(block)?
        .parse::<i64>()
        .ok()
        .filter(|raw| *raw > 0)
}

/// The panel's `(apertureAv, apertureNorm)` for a raw aperture (x100) the iris landed on (issue
/// 1452): the real f-number's AV, and the nearest listed position for the slider (exact for a
/// listed value) — the same off-grid rule the read path uses (issue 1306).
pub fn landed_aperture(raw_x100: i64, labels: &[String]) -> (Option<f64>, Option<f64>) {
    let f = raw_x100 as f64 / 100.0;
    (fnumber_to_av(f), nearest_choice_norm(f, labels))
}
