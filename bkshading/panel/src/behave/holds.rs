//! The optimistic hold of a tap (issue 1337/1350, ported from the JS panel's `pending` map): a tap
//! shows its target at once, marked pending, and the panel keeps showing it against a stale pump
//! snapshot until the server confirms it, refuses it (`notApplied`), or the hold times out.
//!
//! Two kinds:
//! - [`HoldKind::Value`]: the target is known (ISO 800, 5700 K). Ends when a push shows it.
//! - [`HoldKind::Step`]: an aperture +/- (shading#6): the relay walks the lens and the landing is
//!   unknown until it answers, so the target is the f-stop shown at the tap and the hold ends when
//!   a push shows a DIFFERENT one.

use std::collections::HashMap;

/// How long a value hold waits for its confirmation (covers the ~2 s pump plus the camera's apply).
pub const VALUE_HOLD_MS: f64 = 4000.0;
/// How long an aperture step waits: the relay walks the lens (0.4-3 s, an older relay up to ~5 s).
pub const STEP_HOLD_MS: f64 = 8000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldKind {
    Value,
    Step,
}

/// One held tap: the text shown, the value the next step builds on (none for a step), when.
#[derive(Debug, Clone, PartialEq)]
pub struct Hold {
    pub kind: HoldKind,
    pub text: String,
    pub value: Option<f64>,
    pub at_ms: f64,
}

/// The holds of one camera block, by wire key (`iso`, `kelvin`, `apertureStep`, ...).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Holds {
    map: HashMap<&'static str, Hold>,
}

impl Holds {
    /// A tap on `key` now holds `text` (and `value`, the base of the next step).
    pub fn hold(
        &mut self,
        key: &'static str,
        kind: HoldKind,
        text: String,
        value: Option<f64>,
        now: f64,
    ) {
        self.map.insert(
            key,
            Hold {
                kind,
                text,
                value,
                at_ms: now,
            },
        );
    }

    /// The write of `key` failed before it reached the service: let the next push show the truth.
    pub fn drop_key(&mut self, key: &str) {
        self.map.remove(key);
    }

    /// A push shows `pushed` for `key` (`refused`: the camera did not apply the key, issue 1343).
    /// Returns the hold that still stands, or `None` once it ended (confirmed, refused, timed out),
    /// in which case the push's own value is the truth.
    pub fn reconcile(&mut self, key: &str, pushed: &str, refused: bool, now: f64) -> Option<&Hold> {
        let h = self.map.get(key)?;
        let (limit, confirmed) = match h.kind {
            HoldKind::Value => (VALUE_HOLD_MS, pushed == h.text),
            HoldKind::Step => (STEP_HOLD_MS, pushed != h.text),
        };
        let ends = refused || confirmed || now - h.at_ms >= limit;
        if ends {
            self.map.remove(key);
            None
        } else {
            self.map.get(key)
        }
    }

    /// Whether `key` is held (without reconciling).
    pub fn is_held(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    /// The hold of `key` (without reconciling).
    pub fn get(&self, key: &str) -> Option<&Hold> {
        self.map.get(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_hold_survives_a_stale_push_and_ends_on_confirmation() {
        let mut h = Holds::default();
        h.hold(
            "kelvin",
            HoldKind::Value,
            "5700 K".into(),
            Some(5700.0),
            0.0,
        );
        assert_eq!(
            h.reconcile("kelvin", "5600 K", false, 100.0)
                .map(|x| x.text.clone()),
            Some("5700 K".into())
        );
        assert!(h.reconcile("kelvin", "5700 K", false, 200.0).is_none());
        assert!(!h.is_held("kelvin"));
    }

    #[test]
    fn a_refusal_or_the_timeout_ends_a_hold() {
        let mut h = Holds::default();
        h.hold("iso", HoldKind::Value, "800".into(), Some(800.0), 0.0);
        assert!(h.reconcile("iso", "400", true, 10.0).is_none(), "refused");
        h.hold("iso", HoldKind::Value, "800".into(), Some(800.0), 0.0);
        assert!(h
            .reconcile("iso", "400", false, VALUE_HOLD_MS - 1.0)
            .is_some());
        assert!(
            h.reconcile("iso", "400", false, VALUE_HOLD_MS).is_none(),
            "timed out"
        );
    }

    #[test]
    fn a_step_hold_ends_when_the_f_stop_changes() {
        let mut h = Holds::default();
        h.hold("apertureStep", HoldKind::Step, "f/5.2".into(), None, 0.0);
        assert!(h
            .reconcile("apertureStep", "f/5.2", false, 1000.0)
            .is_some());
        assert!(h
            .reconcile("apertureStep", "f/5.6", false, 1100.0)
            .is_none());
        h.hold("apertureStep", HoldKind::Step, "f/5.2".into(), None, 0.0);
        assert!(h
            .reconcile("apertureStep", "f/5.2", false, STEP_HOLD_MS - 1.0)
            .is_some());
        assert!(h
            .reconcile("apertureStep", "f/5.2", false, STEP_HOLD_MS)
            .is_none());
    }

    #[test]
    fn a_failed_write_drops_its_hold_and_an_unknown_key_holds_nothing() {
        let mut h = Holds::default();
        h.hold("tint", HoldKind::Value, "+1".into(), Some(1.0), 0.0);
        h.drop_key("tint");
        assert!(h.reconcile("tint", "0", false, 1.0).is_none());
        assert!(h.reconcile("nothing", "x", false, 1.0).is_none());
    }
}
