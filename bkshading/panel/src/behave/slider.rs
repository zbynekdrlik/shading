//! The slider behind a stepper's value (shading#24, mockup theme-v4d): hidden until the operator
//! taps the value (Marcel: an always-visible slider is a misclick risk on a live show), then a
//! range input under the row. Ported from the JS panel's sliders, which this release removes:
//!
//! - clona: an INDEX over the camera's f-number grid; its release writes `apertureNorm =
//!   idx/(n-1)`, the inverse of the relay's `norm_to_choice_index` (issue 1304). A release toward
//!   a lens end the iris has already reached (`irisRangePos`, shading#6/#22) writes nothing;
//! - ISO, uzávierka, FPS: an index over the listed (whole-rate) choices; the release writes the
//!   absolute choice;
//! - biely bod (50 K grain) and tint: the absolute value, clamped to the range.
//!
//! One write per RELEASE (`change`), never per drag tick: the issue-1229 one-write rule. The
//! write is held like a tap (a value hold under the row's key), so the value shows pending until a
//! push confirms it. A release on the value already shown writes nothing.

use bkshading_proto::wire::CameraView;

use super::holds::{HoldKind, Tap};
use super::steppers::{
    choices, fps_text, iris_at_end, linear_range, nearest_index, pushed, tint_text, Live, Press,
    Row, Steppers,
};

/// The Kelvin grain of the biely bod slider (the JS panel's `step="50"`).
pub const KELVIN_SLIDER_STEP: f64 = 50.0;

/// The range input of one row: its bounds, its grain and where its thumb sits.
#[derive(Debug, Clone, PartialEq)]
pub struct SliderSpec {
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub value: f64,
}

impl SliderSpec {
    /// Where the thumb sits in `[0, 100]` (the track's filled part, CSS `--p`).
    pub fn percent(&self) -> f64 {
        let span = self.max - self.min;
        if span <= 0.0 {
            return 0.0;
        }
        ((self.value - self.min) / span * 100.0).clamp(0.0, 100.0)
    }
}

/// The slider of `row` at value `base` (a held target or the pushed value), or none when it could
/// not write: no listed steps, or an unknown value.
pub(super) fn spec(row: Row, live: &Live, base: Option<f64>) -> Option<SliderSpec> {
    let base = base?;
    match row {
        Row::Aperture | Row::Iso | Row::Shutter | Row::Fps => {
            let list = choices(row, live.caps);
            if list.len() < 2 {
                return None;
            }
            Some(SliderSpec {
                min: 0.0,
                max: (list.len() - 1) as f64,
                step: 1.0,
                value: nearest_index(&list, base) as f64,
            })
        }
        Row::Kelvin | Row::Tint => {
            let (lo, hi) = linear_range(row, live.caps);
            Some(SliderSpec {
                min: lo as f64,
                max: hi as f64,
                step: if row == Row::Kelvin {
                    KELVIN_SLIDER_STEP
                } else {
                    1.0
                },
                value: base.round().clamp(lo as f64, hi as f64),
            })
        }
    }
}

impl Steppers {
    /// The operator released `row`'s slider at `raw` on camera `cam`: the write, or none (nothing to
    /// write: no slider, the value already shown, or a lens end already reached in that
    /// direction). The write is held like a tap.
    pub fn slide(&mut self, row: Row, raw: f64, cam: &CameraView, now: f64) -> Option<Press> {
        let current = self.view(cam, now).into_iter().find(|v| v.row == row)?;
        let spec = current.slider?;
        if !raw.is_finite() {
            return None;
        }
        let seq = self.seq;
        let live = Live::of(cam);
        let key = row.key();
        // shading#29: a release while the row's last write is unanswered writes nothing; the thumb
        // goes back. (The 300 ms double-press guard is for the buttons: a release is deliberate.)
        if self.holds.is_pending(key) {
            return None;
        }
        let from = pushed(row, &live).0;
        let base = self
            .holds
            .get(key)
            .and_then(|h| h.value)
            .or_else(|| pushed(row, &live).1)?;
        let (text, target, value) = match row {
            Row::Aperture | Row::Iso | Row::Shutter | Row::Fps => {
                let list = choices(row, live.caps);
                let n = list.len();
                let idx = (raw.round().max(0.0) as usize).min(n - 1);
                let target = list[idx];
                // The thumb's own place (an off-grid value sits on its nearest choice).
                if idx == nearest_index(&list, base) {
                    return None;
                }
                match row {
                    Row::Aperture => {
                        let dir = if target > base { 1 } else { -1 };
                        let iris = live.params.and_then(|p| p.iris_range_pos);
                        if iris_at_end(iris, dir) {
                            return None;
                        }
                        let norm = idx as f64 / (n - 1) as f64;
                        (format!("f/{target:.1}"), target, serde_json::json!(norm))
                    }
                    Row::Iso => (
                        format!("{}", target as i64),
                        target,
                        serde_json::json!(target as i64),
                    ),
                    Row::Shutter => (
                        format!("1/{}", target as i64),
                        target,
                        serde_json::json!(target as i64),
                    ),
                    _ => (fps_text(target), target, serde_json::json!(target as i64)),
                }
            }
            Row::Kelvin | Row::Tint => {
                // Onto the slider's grain (50 K / 1), inside the range.
                let v = ((raw / spec.step).round() * spec.step).clamp(spec.min, spec.max) as i64;
                if v == base.round() as i64 {
                    return None;
                }
                let text = if row == Row::Kelvin {
                    format!("{v} K")
                } else {
                    tint_text(v)
                };
                (text, v as f64, serde_json::json!(v))
            }
        };
        let tap = Tap {
            kind: HoldKind::Value,
            text,
            from,
            value: Some(target),
            was_refused: current.not_applied,
        };
        let id = self.holds.hold(key, tap, seq, now);
        Some(Press {
            key: if row == Row::Aperture {
                "apertureNorm"
            } else {
                key
            },
            value,
            id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behave::holds::WriteAnswer;
    use crate::behave::steppers::tests::cam;
    use crate::behave::steppers::RowView;
    use bkshading_proto::wire::ShadingParams;

    fn with(f: impl FnOnce(&mut ShadingParams)) -> CameraView {
        let mut c = cam();
        f(&mut c.state.as_mut().unwrap().params);
        c
    }

    fn row(views: &[RowView], r: Row) -> RowView {
        views.iter().find(|v| v.row == r).unwrap().clone()
    }

    #[test]
    fn each_row_has_its_slider_at_the_value() {
        let views = Steppers::default().view(&cam(), 0.0);
        let s = |r| row(&views, r).slider.unwrap();
        // f/5.2 on [2.8, 4.0, 5.2, 8.0]
        assert_eq!(
            s(Row::Aperture),
            SliderSpec {
                min: 0.0,
                max: 3.0,
                step: 1.0,
                value: 2.0
            }
        );
        assert_eq!(s(Row::Iso).value, 2.0, "ISO 400 on [100, 200, 400, 800]");
        assert_eq!(s(Row::Shutter).value, 0.0, "1/60 on [60, 100, 125]");
        let k = s(Row::Kelvin);
        assert_eq!(
            (k.min, k.max, k.step, k.value),
            (2500.0, 10000.0, 50.0, 5600.0)
        );
        let t = s(Row::Tint);
        assert_eq!((t.min, t.max, t.step, t.value), (-50.0, 50.0, 1.0, 0.0));
        // 60.00 fps on the whole listed rates [24, 25, 30, 50, 60]
        assert_eq!(s(Row::Fps).value, 4.0);
        assert!((s(Row::Iso).percent() - 66.666).abs() < 0.01);
    }

    #[test]
    fn no_slider_without_steps_or_a_value() {
        let mut no_caps = cam();
        no_caps.state.as_mut().unwrap().caps = None;
        let views = Steppers::default().view(&no_caps, 0.0);
        for r in [Row::Aperture, Row::Iso, Row::Shutter, Row::Fps] {
            assert_eq!(row(&views, r).slider, None, "{r:?}");
        }
        let unknown = with(|p| p.kelvin = None);
        let views = Steppers::default().view(&unknown, 0.0);
        assert_eq!(row(&views, Row::Kelvin).slider, None);
        let mut offline = cam();
        offline.reachable = false;
        assert!(Steppers::default()
            .view(&offline, 0.0)
            .iter()
            .all(|v| v.slider.is_none()));
    }

    #[test]
    fn a_release_writes_the_absolute_choice_and_holds_it() {
        let c = cam();
        let mut s = Steppers::default();
        let p = s.slide(Row::Iso, 3.0, &c, 0.0).unwrap();
        assert_eq!((p.key, p.value.clone()), ("iso", serde_json::json!(800)));
        let r = row(&s.view(&c, 10.0), Row::Iso);
        assert_eq!((r.text.as_str(), r.pending), ("800", true));
        assert_eq!(
            r.slider.unwrap().value,
            3.0,
            "the thumb follows the held target"
        );
        let p = s.slide(Row::Shutter, 2.0, &c, 0.0).unwrap();
        assert_eq!((p.key, p.value), ("shutter", serde_json::json!(125)));
        let p = s.slide(Row::Fps, 1.0, &c, 0.0).unwrap();
        assert_eq!((p.key, p.value), ("fps", serde_json::json!(25)));
        assert_eq!(row(&s.view(&c, 10.0), Row::Fps).text, "25.00");
    }

    #[test]
    fn a_release_on_the_shown_value_writes_nothing() {
        let c = cam();
        let mut s = Steppers::default();
        assert_eq!(s.slide(Row::Iso, 2.0, &c, 0.0), None);
        assert_eq!(
            s.slide(Row::Kelvin, 5610.0, &c, 0.0),
            None,
            "snaps onto 5600 (50 K grain)"
        );
        assert_eq!(s.slide(Row::Aperture, 2.0, &c, 0.0), None);
        assert_eq!(s.slide(Row::Tint, f64::NAN, &c, 0.0), None);
        assert!(!row(&s.view(&c, 1.0), Row::Iso).pending);
    }

    #[test]
    fn kelvin_and_tint_write_the_clamped_absolute_value() {
        let c = cam();
        let mut s = Steppers::default();
        let p = s.slide(Row::Kelvin, 6240.0, &c, 0.0).unwrap();
        assert_eq!((p.key, p.value), ("kelvin", serde_json::json!(6250)));
        assert_eq!(row(&s.view(&c, 1.0), Row::Kelvin).text, "6250 K");
        let p = s.slide(Row::Tint, -80.0, &c, 0.0).unwrap();
        assert_eq!((p.key, p.value), ("tint", serde_json::json!(-50)));
        assert_eq!(row(&s.view(&c, 1.0), Row::Tint).text, "-50");
        // The hold ends when the camera confirms it.
        let confirmed = with(|p| p.tint = Some(-50));
        assert!(!row(&s.view(&confirmed, 2.0), Row::Tint).pending);
    }

    #[test]
    fn the_aperture_release_writes_the_grid_norm_and_holds_the_f_stop() {
        let c = cam();
        let mut s = Steppers::default();
        let p = s.slide(Row::Aperture, 3.0, &c, 0.0).unwrap();
        assert_eq!(p.key, "apertureNorm");
        assert_eq!(p.value, serde_json::json!(1.0));
        let r = row(&s.view(&c, 1.0), Row::Aperture);
        assert_eq!((r.text.as_str(), r.pending), ("f/8.0", true));
        // shading#29: a second release waits for the first write's answer.
        assert_eq!(s.slide(Row::Aperture, 1.0, &c, 500.0), None);
        s.answered("apertureNorm", p.id, WriteAnswer::Accepted, 600.0);
        let p = s.slide(Row::Aperture, 1.0, &c, 610.0).unwrap();
        assert!((p.value.as_f64().unwrap() - 1.0 / 3.0).abs() < 1e-9);
    }

    /// The JS panel's guard (shading#6/#22): never write past a lens end the iris already reached.
    #[test]
    fn the_aperture_release_never_heads_past_a_reached_lens_end() {
        let wide = with(|p| {
            p.aperture_av = Some(4.0);
            p.iris_range_pos = Some(0.0);
        });
        let mut s = Steppers::default();
        assert_eq!(s.slide(Row::Aperture, 0.0, &wide, 0.0), None, "toward open");
        assert!(!row(&s.view(&wide, 1.0), Row::Aperture).pending);
        assert!(
            s.slide(Row::Aperture, 3.0, &wide, 0.0).is_some(),
            "toward closed"
        );
        let closed = with(|p| p.iris_range_pos = Some(1.0));
        let mut s = Steppers::default();
        assert_eq!(s.slide(Row::Aperture, 3.0, &closed, 0.0), None);
        assert!(s.slide(Row::Aperture, 0.0, &closed, 0.0).is_some());
    }

    #[test]
    fn the_track_fill_follows_the_thumb() {
        let s = SliderSpec {
            min: -50.0,
            max: 50.0,
            step: 1.0,
            value: 0.0,
        };
        assert_eq!(s.percent(), 50.0);
        let flat = SliderSpec {
            min: 1.0,
            max: 1.0,
            step: 1.0,
            value: 1.0,
        };
        assert_eq!(flat.percent(), 0.0);
    }
}
