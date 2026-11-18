//! The six steppers of a camera block (clona, ISO, biely bod, tint, uzávierka, fps): what each
//! shows, where its bar sits, when its −/+ are disabled, and what a tap writes. Ported from the JS
//! panel (issues 1304, 1337, 1343, 1350, shading#6) and the approved mockup arrange-v1 (shading#16,
//! the sixth stepper: fps).
//!
//! One tap = one PUT of one key, never an auto-repeat (the issue-1229 one-write rule). A tap steps
//! from the camera's REAL current value (or the target of a tap still held), shows the expected
//! next value at once (shading#29), and sends:
//! - clona: `{apertureStep: ±1}`, the relay walks the lens's own steps (shading#6); the panel shows
//!   the next stop of the f-number grid ([`aperture_next`]) until the camera says where it landed;
//! - ISO, uzávierka, fps: the absolute next listed choice;
//! - biely bod ±100 K and tint ±1: the absolute value, clamped to the range.

use bkshading_proto::wire::{CameraCaps, CameraView, ShadingParams};

use super::holds::{HoldKind, Holds, Seen, Tap, WriteAnswer};

/// Kelvin per tap.
pub const KELVIN_STEP: i64 = 100;
/// Tint units per tap.
pub const TINT_STEP: i64 = 1;
/// The white balance range when the relay sends none.
pub const KELVIN_RANGE: (i64, i64) = (2500, 10000);
/// The tint range (the camera's own).
pub const TINT_RANGE: (i64, i64) = (-50, 50);
/// How far apart two f-numbers must be to count as different stops (the panel shows one decimal; a
/// pushed f-number comes from `apertureAv` and is never exactly a grid entry).
pub const F_NUMBER_EPS: f64 = 0.05;
/// Mirror of the proto `mapping::IRIS_END_EPS`: how close to 0 / 1 `irisRangePos` is a lens end.
pub const IRIS_END_EPS: f64 = 1e-3;

/// The title of a value the camera did not apply (issue 1343).
pub const NOT_APPLIED_TITLE: &str = "Kamera tento zápis neprijala";
const NO_APERTURE_STEPS: &str = "Kroky clony nie sú dostupné (relay neposiela voľby clony)";
const NO_ISO_STEPS: &str = "Kroky ISO nie sú dostupné (relay neposiela voľby ISO)";
const NO_SHUTTER_STEPS: &str = "Kroky uzávierky nie sú dostupné (relay neposiela voľby uzávierky)";
const NO_FPS_STEPS: &str =
    "Snímková frekvencia sa z panelu nedá meniť (kamera neposiela celé frekvencie)";
const UNKNOWN_VALUE: &str = "Hodnota z kamery nie je známa";
const AT_WIDE: &str = "Objektív je na najotvorenejšej clone";
const AT_CLOSED: &str = "Objektív je na najzatvorenejšej clone";
const AT_END: &str = "Koniec rozsahu";

/// The six steppers, in the block's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Aperture,
    Iso,
    Kelvin,
    Tint,
    Shutter,
    Fps,
}

impl Row {
    pub const ALL: [Row; 6] = [
        Row::Aperture,
        Row::Iso,
        Row::Kelvin,
        Row::Tint,
        Row::Shutter,
        Row::Fps,
    ];

    /// The label in the block (mockup theme-v4d: sentence case).
    pub fn label(self) -> &'static str {
        match self {
            Row::Aperture => "Clona",
            Row::Iso => "ISO",
            Row::Kelvin => "Biely bod",
            Row::Tint => "Tint",
            Row::Shutter => "Uzávierka",
            Row::Fps => "FPS",
        }
    }

    /// The card pane the row lives in (theme-v4d): EXP = exposure and colour, SYS = capture.
    pub fn pane(self) -> Pane {
        match self {
            Row::Aperture | Row::Iso | Row::Kelvin | Row::Tint => Pane::Exp,
            Row::Shutter | Row::Fps => Pane::Sys,
        }
    }

    /// The `data-role` prefix of the −/+ buttons (`<role>-dec`, `<role>-inc`).
    pub fn role(self) -> &'static str {
        match self {
            Row::Aperture => "aperture",
            Row::Iso => "iso",
            Row::Kelvin => "kelvin",
            Row::Tint => "tint",
            Row::Shutter => "shutter",
            Row::Fps => "fps",
        }
    }

    /// The `data-role` of the value (the JS panel's names, kept for the E2E).
    pub fn value_role(self) -> &'static str {
        match self {
            Row::Aperture => "fnum",
            Row::Iso => "iso-val",
            Row::Kelvin => "kelvin-val",
            Row::Tint => "tint-val",
            Row::Shutter => "shutter-val",
            Row::Fps => "fps-val",
        }
    }

    /// The wire key a tap writes (and its hold's key).
    pub fn key(self) -> &'static str {
        match self {
            Row::Aperture => "apertureStep",
            Row::Iso => "iso",
            Row::Kelvin => "kelvin",
            Row::Tint => "tint",
            Row::Shutter => "shutter",
            Row::Fps => "fps",
        }
    }

    /// The `notApplied` keys that flag this row.
    fn refused_keys(self) -> &'static [&'static str] {
        match self {
            Row::Aperture => &["apertureNorm", "apertureStep"],
            Row::Iso => &["iso"],
            Row::Kelvin => &["kelvin"],
            Row::Tint => &["tint"],
            Row::Shutter => &["shutter"],
            Row::Fps => &["fps"],
        }
    }
}

/// A card's two panes (shading#24 theme-v4d): a per-card EXP/SYS tablist switches between them,
/// and both share one grid cell, so the card's height never changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pane {
    /// Clona, ISO, biely bod, tint.
    #[default]
    Exp,
    /// Uzávierka, FPS, Auto WB.
    Sys,
}

impl Pane {
    pub const ALL: [Pane; 2] = [Pane::Exp, Pane::Sys];

    /// The tab's text and the `data-pane` word.
    pub fn label(self) -> &'static str {
        match self {
            Pane::Exp => "EXP",
            Pane::Sys => "SYS",
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Pane::Exp => "exp",
            Pane::Sys => "sys",
        }
    }

    /// The tab's tooltip: what the pane holds.
    pub fn title(self) -> &'static str {
        match self {
            Pane::Exp => "expozícia a farba: clona, ISO, biely bod, tint",
            Pane::Sys => "snímanie: uzávierka, FPS, auto WB",
        }
    }
}

/// Whether one of the −/+ buttons can be pressed, and its title. `end`: it is off because the
/// value reached the end of its range in that direction (the card lays the `min`/`max` overlay
/// over it), not because the value or the steps are unknown.
#[derive(Debug, Clone, PartialEq)]
pub struct Gate {
    pub enabled: bool,
    pub end: bool,
    pub title: String,
}

impl Gate {
    fn on() -> Self {
        Gate {
            enabled: true,
            end: false,
            title: String::new(),
        }
    }
    fn off(title: &str) -> Self {
        Gate {
            enabled: false,
            end: false,
            title: title.to_string(),
        }
    }
    fn at_end(title: &str) -> Self {
        Gate {
            enabled: false,
            end: true,
            title: title.to_string(),
        }
    }
    fn end_if(reached: bool, title: &str) -> Self {
        if reached {
            Gate::at_end(title)
        } else {
            Gate::on()
        }
    }
}

/// What one stepper shows.
#[derive(Debug, Clone, PartialEq)]
pub struct RowView {
    pub row: Row,
    pub text: String,
    /// A tap is held: the value is the target, not yet confirmed.
    pub pending: bool,
    /// The tap's write is unanswered: the row is locked and the bar under the value runs
    /// (shading#29).
    pub busy: bool,
    /// The value was just refused or timed out: it flashes red (shading#29).
    pub flash: bool,
    /// The camera did not apply the last write of this key (issue 1343).
    pub not_applied: bool,
    /// The bar's position in `[0, 1]`, or none when unknown.
    pub pos: Option<f64>,
    pub dec: Gate,
    pub inc: Gate,
    /// The slider a tap on the value opens (shading#24), or none when it cannot write.
    pub slider: Option<super::slider::SliderSpec>,
}

/// A tap's write: the PUT body's one key and value.
#[derive(Debug, Clone, PartialEq)]
pub struct Press {
    pub key: &'static str,
    pub value: serde_json::Value,
    /// The id of the write's hold: its answer carries it back to [`Steppers::answered`].
    pub id: u64,
}

/// The f-number of an aperture value: `sqrt(2^AV)`.
pub fn f_number_from_av(av: f64) -> f64 {
    2f64.powf(av).sqrt()
}

/// Mirror of the proto `mapping::step_choice`: the index of the next choice from `current` in
/// direction `dir` over ascending `choices`. "+" = the first choice strictly above, "−" = the last
/// strictly below; past an end it stays on the end; a value off the grid steps ONTO it.
pub fn step_choice(current: f64, choices: &[f64], dir: i32) -> Option<usize> {
    let n = choices.len();
    if n == 0 {
        return None;
    }
    if n == 1 || dir == 0 {
        return Some(0);
    }
    if dir > 0 {
        Some(choices.iter().position(|&c| c > current).unwrap_or(n - 1))
    } else {
        Some(choices.iter().rposition(|&c| c < current).unwrap_or(0))
    }
}

/// The index of the choice closest to `value` (the bar of an off-grid value).
pub fn nearest_index(choices: &[f64], value: f64) -> usize {
    choices
        .iter()
        .enumerate()
        .min_by(|a, b| (a.1 - value).abs().total_cmp(&(b.1 - value).abs()))
        .map_or(0, |(i, _)| i)
}

/// The f-stop an aperture press in `dir` is expected to reach from `f` (shading#29, shown at once
/// until the camera answers): the next entry of the f-number grid the gates use, or one third of
/// a stop on when `f` lies outside the grid or the grid ends before the lens does (shading#22: the
/// default grid starts at 2.8, a lens may open to 1.75). The relay walks the lens's own steps, so the camera's answer may
/// differ; it is the truth.
pub fn aperture_next(f: f64, grid: &[f64], dir: i32) -> f64 {
    let next = if dir > 0 {
        grid.iter().copied().find(|&c| c > f + F_NUMBER_EPS)
    } else {
        grid.iter().copied().rev().find(|&c| c < f - F_NUMBER_EPS)
    };
    // Outside the grid's span the grid says nothing about the lens's steps (a lens below the
    // default grid's 2.8 moves ~1/6 stop, not to f/2.8): a third of a stop then.
    let inside = match (grid.first(), grid.last()) {
        (Some(&lo), Some(&hi)) => f >= lo - F_NUMBER_EPS && f <= hi + F_NUMBER_EPS,
        _ => false,
    };
    next.filter(|_| inside)
        .unwrap_or_else(|| f * 2f64.powf(f64::from(dir.signum()) / 6.0))
}

/// Whether the iris is at the lens end a move in `dir` heads for (`irisRangePos`: 0 = wide open,
/// 1 = closed). An unknown position is never an end.
pub fn iris_at_end(pos: Option<f64>, dir: i32) -> bool {
    match pos {
        Some(p) if p.is_finite() => {
            if dir < 0 {
                p <= IRIS_END_EPS
            } else if dir > 0 {
                p >= 1.0 - IRIS_END_EPS
            } else {
                false
            }
        }
        _ => false,
    }
}

/// The settable whole frame rates of a camera: its `fpsChoices` (x100) that are whole rates, as
/// plain fps. A write is accepted only for one of these (issue 1402: `fps * 100` must be listed),
/// so 23.98 / 29.97 / 59.94 and the BMPCC's `8` entry are not steps.
pub fn whole_fps_choices(caps: Option<&CameraCaps>) -> Vec<f64> {
    let Some(caps) = caps else {
        return Vec::new();
    };
    let mut out: Vec<f64> = caps
        .fps_choices
        .iter()
        .filter(|&&c| c >= 100 && c % 100 == 0)
        .map(|&c| (c / 100) as f64)
        .collect();
    out.sort_by(f64::total_cmp);
    out.dedup();
    out
}

/// The tint as the mockup shows it: `+3`, `0`, `-2`.
pub fn tint_text(t: i64) -> String {
    if t > 0 {
        format!("+{t}")
    } else {
        t.to_string()
    }
}

pub(super) fn fps_text(fps: f64) -> String {
    format!("{fps:.2}")
}

/// A camera's live values, as far as this push knows them.
pub(super) struct Live<'a> {
    pub(super) params: Option<&'a ShadingParams>,
    pub(super) caps: Option<&'a CameraCaps>,
    not_applied: &'a [String],
}

impl<'a> Live<'a> {
    pub(super) fn of(cam: &'a CameraView) -> Self {
        let state = cam.state.as_ref().filter(|s| cam.reachable && s.online);
        Live {
            params: state.map(|s| &s.params),
            caps: state.and_then(|s| s.caps.as_ref()),
            not_applied: state.map_or(&[], |s| s.not_applied.as_slice()),
        }
    }
    fn refused(&self, row: Row) -> bool {
        row.refused_keys()
            .iter()
            .any(|k| self.not_applied.iter().any(|n| n == k))
    }
}

/// One stepper's live value: its text and number from the push.
pub(super) fn pushed(row: Row, live: &Live) -> (String, Option<f64>) {
    let p = live.params;
    let num = |v: Option<i64>| v.map(|v| v as f64);
    match row {
        Row::Aperture => {
            let f = p.and_then(|p| p.aperture_av).map(f_number_from_av);
            (f.map_or("f/—".into(), |f| format!("f/{f:.1}")), f)
        }
        Row::Iso => {
            let v = p.and_then(|p| p.iso);
            (v.map_or("—".into(), |v| v.to_string()), num(v))
        }
        Row::Kelvin => {
            let v = p.and_then(|p| p.kelvin);
            (v.map_or("—".into(), |v| format!("{v} K")), num(v))
        }
        Row::Tint => {
            let v = p.and_then(|p| p.tint);
            (v.map_or("—".into(), tint_text), num(v))
        }
        Row::Shutter => {
            let v = p.and_then(|p| p.shutter);
            (v.map_or("—".into(), |v| format!("1/{v}")), num(v))
        }
        Row::Fps => {
            let v = p.and_then(|p| p.fps100).map(|f| f as f64 / 100.0);
            (v.map_or("—".into(), fps_text), v)
        }
    }
}

/// The listed choices a row steps over (aperture: the slider grid that bounds the ±; kelvin/tint:
/// none, they are linear).
pub(super) fn choices(row: Row, caps: Option<&CameraCaps>) -> Vec<f64> {
    let ints = |v: &[i64]| v.iter().map(|&x| x as f64).collect::<Vec<f64>>();
    match (row, caps) {
        (Row::Aperture, Some(c)) => c.fnumber_choices.clone(),
        (Row::Iso, Some(c)) => ints(&c.iso_choices),
        (Row::Shutter, Some(c)) => ints(&c.shutter_choices),
        (Row::Fps, c) => whole_fps_choices(c),
        _ => Vec::new(),
    }
}

/// The linear range of biely bod / tint.
pub(super) fn linear_range(row: Row, caps: Option<&CameraCaps>) -> (i64, i64) {
    match row {
        Row::Kelvin => caps
            .filter(|c| c.kelvin_min < c.kelvin_max)
            .map_or(KELVIN_RANGE, |c| (c.kelvin_min, c.kelvin_max)),
        _ => TINT_RANGE,
    }
}

/// A camera block's steppers, with the taps it holds. One per block; the component keeps it.
#[derive(Debug, Clone, Default)]
pub struct Steppers {
    pub(super) holds: Holds,
    /// The camera's view last drawn, and how many different views this block has seen: a tap's
    /// hold is confirmed or refused only by a view of THIS camera newer than the tap (a push about
    /// another camera, or the clock, changes nothing here).
    last: Option<CameraView>,
    pub(super) seq: u64,
}

impl Steppers {
    /// The six steppers for `cam` at `now`, reconciling the held taps with it (only a view of the
    /// camera that changed since a tap confirms or refuses it; the clock only times it out).
    pub fn view(&mut self, cam: &CameraView, now: f64) -> Vec<RowView> {
        if self.last.as_ref() != Some(cam) {
            self.seq += 1;
            self.last = Some(cam.clone());
        }
        let seq = self.seq;
        let live = Live::of(cam);
        Row::ALL
            .iter()
            .map(|&row| self.row_view(row, &live, seq, now))
            .collect()
    }

    fn row_view(&mut self, row: Row, live: &Live, seq: u64, now: f64) -> RowView {
        let (pushed_text, pushed_value) = pushed(row, live);
        let refused = live.refused(row);
        let held = self
            .holds
            .reconcile(
                row.key(),
                Seen {
                    pushed: &pushed_text,
                    refused,
                    seq,
                    now,
                },
            )
            .cloned();
        let (text, base) = match &held {
            Some(h) => (h.text.clone(), h.value.or(pushed_value)),
            None => (pushed_text, pushed_value),
        };
        let (dec, inc, pos) = gates(row, live, base);
        let flag = |g: Gate| {
            if refused && g.enabled {
                Gate {
                    enabled: true,
                    end: false,
                    title: NOT_APPLIED_TITLE.into(),
                }
            } else {
                g
            }
        };
        RowView {
            row,
            text,
            pending: held.is_some(),
            busy: self.holds.is_pending(row.key()),
            flash: self.holds.flashing(row.key(), now),
            not_applied: refused,
            pos,
            dec: flag(dec),
            inc: flag(inc),
            slider: super::slider::spec(row, live, base),
        }
    }

    /// A tap on `row`'s − (`dir` −1) or + (`dir` 1) of camera `cam`: the write, or none when the
    /// button would be disabled, the row's last write is unanswered, or the last accepted press
    /// was under [`super::holds::DEBOUNCE_MS`] ago (shading#29). The tap is held (its expected
    /// value shown at once) until a newer view of the camera confirms or refuses it.
    pub fn press(&mut self, row: Row, dir: i32, cam: &CameraView, now: f64) -> Option<Press> {
        let current = self.view(cam, now).into_iter().find(|v| v.row == row)?;
        let seq = self.seq;
        let gate = if dir < 0 { &current.dec } else { &current.inc };
        let key = row.key();
        if !gate.enabled || !self.holds.accepts(key, now) {
            return None;
        }
        let live = Live::of(cam);
        let (from, pushed_value) = pushed(row, &live);
        // `view` above reconciled the holds with this push: one still standing is the base.
        let base = self.holds.get(key).and_then(|h| h.value).or(pushed_value);
        let (kind, text, held, write) = match row {
            Row::Aperture => {
                // The guess is only shown: the relay steps from the camera's f-stop, so the next
                // press builds on the pushed one (`value` none). An unknown f-stop still steps
                // (the relay knows it); the dash stays shown.
                let grid = choices(row, live.caps);
                let text = base.map_or_else(
                    || current.text.clone(),
                    |f| format!("f/{:.1}", aperture_next(f, &grid, dir)),
                );
                (HoldKind::Step, text, None, serde_json::json!(dir.signum()))
            }
            Row::Iso | Row::Shutter | Row::Fps => {
                let list = choices(row, live.caps);
                let idx = step_choice(base?, &list, dir)?;
                let value = list[idx];
                let text = match row {
                    Row::Iso => format!("{value}"),
                    Row::Shutter => format!("1/{value}"),
                    _ => fps_text(value),
                };
                (
                    HoldKind::Value,
                    text,
                    Some(value),
                    serde_json::json!(value as i64),
                )
            }
            Row::Kelvin | Row::Tint => {
                let (lo, hi) = linear_range(row, live.caps);
                let step = if row == Row::Kelvin {
                    KELVIN_STEP
                } else {
                    TINT_STEP
                };
                let next = (base?.round() as i64 + dir.signum() as i64 * step).clamp(lo, hi);
                let text = if row == Row::Kelvin {
                    format!("{next} K")
                } else {
                    tint_text(next)
                };
                (
                    HoldKind::Value,
                    text,
                    Some(next as f64),
                    serde_json::json!(next),
                )
            }
        };
        let tap = Tap {
            kind,
            text,
            from,
            value: held,
            was_refused: live.refused(row),
        };
        let id = self.holds.hold(key, tap, seq, now);
        Some(Press {
            key,
            value: write,
            id,
        })
    }

    /// The service answered write `id` of wire key `key` at `now` (the slider's `apertureNorm`
    /// is the aperture row's hold): a 2xx unlocks the row, anything else reverts it with a red
    /// flash (shading#29). Only the hold of that write is touched: a write no hold owns (Auto WB,
    /// the fps align button) answers with id 0 and changes nothing.
    pub fn answered(&mut self, key: &str, id: u64, answer: WriteAnswer, now: f64) {
        let hold_key = Row::ALL
            .iter()
            .find(|r| r.refused_keys().contains(&key))
            .map_or(key, |r| r.key());
        self.holds.answered(hold_key, id, answer, now);
    }
}

/// The −/+ gates and the position in the range of `row` at value `base`.
fn gates(row: Row, live: &Live, base: Option<f64>) -> (Gate, Gate, Option<f64>) {
    match row {
        Row::Aperture => aperture_gates(live, base),
        Row::Iso | Row::Shutter | Row::Fps => {
            let list = choices(row, live.caps);
            let empty = match row {
                Row::Iso => NO_ISO_STEPS,
                Row::Shutter => NO_SHUTTER_STEPS,
                _ => NO_FPS_STEPS,
            };
            if list.len() < 2 {
                return (Gate::off(empty), Gate::off(empty), None);
            }
            let Some(v) = base else {
                return (Gate::off(UNKNOWN_VALUE), Gate::off(UNKNOWN_VALUE), None);
            };
            let n = list.len();
            let on_grid = list.iter().position(|&c| (c - v).abs() < 1e-6);
            let pos = nearest_index(&list, v) as f64 / (n - 1) as f64;
            (
                Gate::end_if(on_grid == Some(0), AT_END),
                Gate::end_if(on_grid == Some(n - 1), AT_END),
                Some(pos),
            )
        }
        Row::Kelvin | Row::Tint => {
            let (lo, hi) = linear_range(row, live.caps);
            let Some(v) = base else {
                return (Gate::off(UNKNOWN_VALUE), Gate::off(UNKNOWN_VALUE), None);
            };
            let v = v.round() as i64;
            let pos = (v - lo) as f64 / (hi - lo).max(1) as f64;
            (
                Gate::end_if(v <= lo, AT_END),
                Gate::end_if(v >= hi, AT_END),
                Some(pos.clamp(0.0, 1.0)),
            )
        }
    }
}

/// Clona: disabled without the camera's f-number grid; at the grid's ends; and at the lens's own
/// ends from `irisRangePos` (shading#6 0.2.3: the grid is not the lens range on fw 8.6, and a zoom
/// moves the wide end).
fn aperture_gates(live: &Live, base: Option<f64>) -> (Gate, Gate, Option<f64>) {
    let list = choices(Row::Aperture, live.caps);
    let iris = live.params.and_then(|p| p.iris_range_pos);
    let pos = iris.or_else(|| live.params.and_then(|p| p.aperture_norm));
    if list.len() < 2 {
        return (
            Gate::off(NO_APERTURE_STEPS),
            Gate::off(NO_APERTURE_STEPS),
            pos,
        );
    }
    let n = list.len();
    let on_grid = base.and_then(|f| list.iter().position(|&c| (c - f).abs() < 1e-6));
    let wide = iris_at_end(iris, -1);
    let closed = iris_at_end(iris, 1);
    // shading#22: the camera's own lens position decides the ends when it gives one. The grid's
    // ends are not the lens's: without `lens/iris/description` the relay sends the default 2.8..22
    // until it has seen a wider stop (PP cam1 opens to f/1.75). Only without `irisRangePos` do the
    // grid's ends gate, as nothing better is known.
    let grid_gates = iris.is_none();
    let dec = if wide {
        Gate::at_end(AT_WIDE)
    } else {
        Gate::end_if(grid_gates && on_grid == Some(0), AT_END)
    };
    let inc = if closed {
        Gate::at_end(AT_CLOSED)
    } else {
        Gate::end_if(grid_gates && on_grid == Some(n - 1), AT_END)
    };
    (dec, inc, pos.map(|p| p.clamp(0.0, 1.0)))
}

#[cfg(test)]
#[path = "steppers_tests.rs"]
pub(crate) mod tests;
