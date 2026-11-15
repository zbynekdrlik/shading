//! The optimistic hold of a tap (issue 1337/1350, ported from the JS panel's `pending` map): a tap
//! shows its target at once, marked pending, and the panel keeps showing it against a stale pump
//! snapshot until the server confirms it, refuses it (`notApplied`), or the hold times out.
//!
//! Two kinds:
//! - [`HoldKind::Value`]: the target is known (ISO 800, 5700 K). Ends when a push shows it.
//! - [`HoldKind::Step`]: an aperture +/- (shading#6): the relay walks the lens and the landing is
//!   unknown until it answers, so the target is the f-stop shown at the tap and the hold ends when
//!   a push shows a DIFFERENT one.
//!
//! Only a push NEWER than the tap can confirm or refuse it (`seq`, the push counter): the push the
//! tap was made against may still carry an older refusal of the same key, and the clock that
//! redraws the block between pushes may only time a hold out (the JS panel reconciled on pushes).

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

/// One held tap: the text shown, the value the next step builds on (none for a step), the push it
/// was made against, when.
#[derive(Debug, Clone, PartialEq)]
pub struct Hold {
    pub kind: HoldKind,
    pub text: String,
    pub value: Option<f64>,
    pub seq: u64,
    pub at_ms: f64,
}

/// What a hold is reconciled against: the push's text for the key, whether the camera refused the
/// key, the push's number, and the time.
#[derive(Debug, Clone, Copy)]
pub struct Seen<'a> {
    pub pushed: &'a str,
    pub refused: bool,
    pub seq: u64,
    pub now: f64,
}

/// The holds of one camera block, by wire key (`iso`, `kelvin`, `apertureStep`, ...).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Holds {
    map: HashMap<&'static str, Hold>,
}

impl Holds {
    /// A tap on `key`, made against push `seq` at `now`, holds `text` (and `value`, the base of the
    /// next step).
    pub fn hold(
        &mut self,
        key: &'static str,
        kind: HoldKind,
        text: String,
        value: Option<f64>,
        seq: u64,
        now: f64,
    ) {
        self.map.insert(
            key,
            Hold {
                kind,
                text,
                value,
                seq,
                at_ms: now,
            },
        );
    }

    /// The write of `key` failed before it reached the service: let the next push show the truth.
    pub fn drop_key(&mut self, key: &str) {
        self.map.remove(key);
    }

    /// The block is drawn for `seen` (`refused`: the camera did not apply the key, issue 1343).
    /// Returns the hold that still stands, or `None` once it ended (confirmed or refused by a newer
    /// push, or timed out), in which case the push's own value is the truth.
    pub fn reconcile(&mut self, key: &str, seen: Seen) -> Option<&Hold> {
        let h = self.map.get(key)?;
        let (limit, confirmed) = match h.kind {
            HoldKind::Value => (VALUE_HOLD_MS, seen.pushed == h.text),
            HoldKind::Step => (STEP_HOLD_MS, seen.pushed != h.text),
        };
        let newer = seen.seq > h.seq;
        let ends = (newer && (seen.refused || confirmed)) || seen.now - h.at_ms >= limit;
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

    fn seen(pushed: &str, refused: bool, seq: u64, now: f64) -> Seen<'_> {
        Seen {
            pushed,
            refused,
            seq,
            now,
        }
    }

    #[test]
    fn a_value_hold_survives_a_stale_push_and_ends_on_confirmation() {
        let mut h = Holds::default();
        h.hold(
            "kelvin",
            HoldKind::Value,
            "5700 K".into(),
            Some(5700.0),
            1,
            0.0,
        );
        assert_eq!(
            h.reconcile("kelvin", seen("5600 K", false, 2, 100.0))
                .map(|x| x.text.clone()),
            Some("5700 K".into())
        );
        assert!(h
            .reconcile("kelvin", seen("5700 K", false, 3, 200.0))
            .is_none());
        assert!(!h.is_held("kelvin"));
    }

    #[test]
    fn a_refusal_or_the_timeout_ends_a_hold() {
        let mut h = Holds::default();
        h.hold("iso", HoldKind::Value, "800".into(), Some(800.0), 1, 0.0);
        assert!(
            h.reconcile("iso", seen("400", true, 2, 10.0)).is_none(),
            "refused"
        );
        h.hold("iso", HoldKind::Value, "800".into(), Some(800.0), 1, 0.0);
        assert!(h
            .reconcile("iso", seen("400", false, 1, VALUE_HOLD_MS - 1.0))
            .is_some());
        assert!(
            h.reconcile("iso", seen("400", false, 1, VALUE_HOLD_MS))
                .is_none(),
            "timed out"
        );
    }

    #[test]
    fn only_a_newer_push_confirms_or_refuses() {
        let mut h = Holds::default();
        // The push the tap was made against still carries an older refusal of the key.
        h.hold(
            "kelvin",
            HoldKind::Value,
            "5700 K".into(),
            Some(5700.0),
            4,
            0.0,
        );
        assert!(h
            .reconcile("kelvin", seen("5600 K", true, 4, 10.0))
            .is_some());
        assert!(h
            .reconcile("kelvin", seen("5600 K", true, 5, 20.0))
            .is_none());
    }

    #[test]
    fn a_step_hold_ends_when_the_f_stop_changes() {
        let mut h = Holds::default();
        h.hold("apertureStep", HoldKind::Step, "f/5.2".into(), None, 1, 0.0);
        assert!(h
            .reconcile("apertureStep", seen("f/5.2", false, 2, 1000.0))
            .is_some());
        assert!(h
            .reconcile("apertureStep", seen("f/5.6", false, 3, 1100.0))
            .is_none());
        h.hold("apertureStep", HoldKind::Step, "f/5.2".into(), None, 1, 0.0);
        assert!(h
            .reconcile("apertureStep", seen("f/5.2", false, 2, STEP_HOLD_MS - 1.0))
            .is_some());
        assert!(h
            .reconcile("apertureStep", seen("f/5.2", false, 2, STEP_HOLD_MS))
            .is_none());
    }

    #[test]
    fn a_failed_write_drops_its_hold_and_an_unknown_key_holds_nothing() {
        let mut h = Holds::default();
        h.hold("tint", HoldKind::Value, "+1".into(), Some(1.0), 1, 0.0);
        h.drop_key("tint");
        assert!(h.reconcile("tint", seen("0", false, 2, 1.0)).is_none());
        assert!(h.reconcile("nothing", seen("x", false, 2, 1.0)).is_none());
    }
}
