//! The optimistic hold of a tap (issue 1337/1350, ported from the JS panel's `pending` map): a tap
//! shows its target at once and the panel keeps showing it against a stale pump snapshot until
//! the camera confirms it, refuses it (`notApplied`), or the hold times out.
//!
//! Two kinds:
//! - [`HoldKind::Value`]: the target is known (ISO 800, 5700 K). Ends when a push shows it.
//! - [`HoldKind::Step`]: an aperture +/- (shading#6): the relay walks the lens and the landing is
//!   unknown until it answers, so the panel shows a GUESS (the next grid stop, shading#29) and the
//!   hold ends when a push shows an f-stop DIFFERENT from the one at the tap (`from`); the value
//!   then settles on the camera's real stop.
//!
//! Only a view of the camera NEWER than the tap can confirm or refuse it (`seq`, the block's count
//! of the camera's different views): the view the tap was made against may still carry an older
//! refusal of the same key, and a push about another camera or the clock that redraws the block
//! may only time a hold out (the JS panel reconciled on pushes).
//!
//! shading#29 (PP operator Marcel, 18.11.2026): a hold is `pending` from the press until the
//! service answers the write. While a key is pending, or within [`DEBOUNCE_MS`] of its last
//! accepted press, a press of that row is ignored ([`Holds::accepts`]): one double click = one
//! step. A 2xx answer only ends the lock (the hold keeps its target against a stale snapshot,
//! shading#27); a refusal (a newer view with `notApplied`, or a write answer that is not 2xx /
//! never came) or the timeout of a still-unanswered hold reverts the value and flashes it red for
//! [`FLASH_MS`].
//!
//! Each hold carries the id of ITS write ([`Holds::hold`] returns it): an answer is applied only to
//! the hold whose write it answers, so a late answer of an older write never unlocks or ends a
//! newer press. A refusal that the view at the press ALREADY showed (a `notApplied` the relay
//! still carries from an earlier write) counts only once this write is answered: the service
//! pushes the write's own read-back before it answers.

use std::collections::HashMap;

/// How long a value hold waits for its confirmation (covers the ~2 s pump plus the camera's apply).
pub const VALUE_HOLD_MS: f64 = 4000.0;
/// How long an aperture step waits: the relay walks the lens (0.4-3 s, an older relay up to ~5 s).
pub const STEP_HOLD_MS: f64 = 8000.0;
/// A second press of the same row within this window of the last accepted one is ignored
/// (shading#29: the double-click guard).
pub const DEBOUNCE_MS: f64 = 300.0;
/// How long a refused or timed-out value flashes red (shading#29).
pub const FLASH_MS: f64 = 1500.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldKind {
    Value,
    Step,
}

/// One held tap: the text shown, the camera's text at the tap (a step's confirmation basis), the
/// value the next step builds on (none for a step: it builds on the camera's f-stop), the push it
/// was made against, when, whether that push already flagged the key refused, its write's id, and
/// whether the service already answered that write.
#[derive(Debug, Clone, PartialEq)]
pub struct Hold {
    pub kind: HoldKind,
    pub text: String,
    pub from: String,
    pub value: Option<f64>,
    pub seq: u64,
    pub at_ms: f64,
    pub was_refused: bool,
    pub id: u64,
    pub answered: bool,
}

/// What a press holds: the text shown at once, the camera's text at the press, the base of the
/// next step, and whether the view at the press already flagged the key refused.
#[derive(Debug, Clone, PartialEq)]
pub struct Tap {
    pub kind: HoldKind,
    pub text: String,
    pub from: String,
    pub value: Option<f64>,
    pub was_refused: bool,
}

/// What the service said to a write (`PUT /api/cameras/<id>/params`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteAnswer {
    /// 2xx: the relay applied it and read the camera back (its view is pushed before the answer).
    Accepted,
    /// Any other status (502: the relay failed), or no answer at all.
    Failed,
}

impl WriteAnswer {
    /// The answer of an HTTP status (`None` = the request never got one).
    pub fn of(status: Option<u16>) -> Self {
        match status {
            Some(s) if (200..300).contains(&s) => WriteAnswer::Accepted,
            _ => WriteAnswer::Failed,
        }
    }
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

/// The holds of one camera block, by wire key (`iso`, `kelvin`, `apertureStep`, ...), with each
/// key's last accepted press and red flash.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Holds {
    map: HashMap<&'static str, Hold>,
    last_press: HashMap<&'static str, f64>,
    flash_until: HashMap<&'static str, f64>,
    /// The last write id handed out (0 = none: a write no hold owns answers with id 0).
    last_id: u64,
}

impl Holds {
    /// A tap on `key`, made against push `seq` at `now`, holds `tap`. It is also the key's last
    /// accepted press. Returns the id of the tap's write (never 0), which its answer carries back.
    pub fn hold(&mut self, key: &'static str, tap: Tap, seq: u64, now: f64) -> u64 {
        self.last_id += 1;
        let id = self.last_id;
        self.map.insert(
            key,
            Hold {
                kind: tap.kind,
                text: tap.text,
                from: tap.from,
                value: tap.value,
                seq,
                at_ms: now,
                was_refused: tap.was_refused,
                id,
                answered: false,
            },
        );
        self.last_press.insert(key, now);
        self.flash_until.remove(key);
        id
    }

    /// Whether a press of `key` at `now` goes through: no write of the key is still unanswered
    /// (the lock) and the last accepted press is at least [`DEBOUNCE_MS`] away (a clock stepped
    /// back reads as away, never as a lock that cannot end).
    pub fn accepts(&self, key: &str, now: f64) -> bool {
        let locked = self.is_pending(key);
        let bounced = self
            .last_press
            .get(key)
            .is_some_and(|&t| now >= t && now - t < DEBOUNCE_MS);
        !locked && !bounced
    }

    /// The service answered write `id` of `key`: a 2xx ends the lock (the hold keeps its target
    /// until a view confirms it), anything else ends the hold and flashes the value red. An answer
    /// of another write (an older press, a write no hold owns) changes nothing.
    pub fn answered(&mut self, key: &str, id: u64, answer: WriteAnswer, now: f64) {
        let Some((&k, h)) = self.map.get_key_value(key) else {
            return;
        };
        if h.answered || h.id != id {
            return;
        }
        match answer {
            WriteAnswer::Accepted => {
                if let Some(h) = self.map.get_mut(k) {
                    h.answered = true;
                }
            }
            WriteAnswer::Failed => {
                self.map.remove(k);
                self.flash_until.insert(k, now + FLASH_MS);
            }
        }
    }

    /// The block is drawn for `seen` (`refused`: the camera did not apply the key, issue 1343).
    /// Returns the hold that still stands, or `None` once it ended (confirmed or refused by a newer
    /// push, or timed out), in which case the push's own value is the truth. A refusal, and the
    /// timeout of a write the service never answered, flash the value red.
    pub fn reconcile(&mut self, key: &str, seen: Seen) -> Option<&Hold> {
        let (&k, h) = self.map.get_key_value(key)?;
        let (limit, confirmed) = match h.kind {
            HoldKind::Value => (VALUE_HOLD_MS, seen.pushed == h.text),
            HoldKind::Step => (STEP_HOLD_MS, seen.pushed != h.from),
        };
        let newer = seen.seq > h.seq;
        // A refusal the press's own view already showed is an earlier write's until this write's
        // read-back (pushed before the answer) is in.
        let refused = newer && seen.refused && (!h.was_refused || h.answered);
        let timed_out = seen.now - h.at_ms >= limit;
        let ends = refused || (newer && confirmed) || timed_out;
        if !ends {
            return self.map.get(key);
        }
        let flash = refused || (timed_out && !(newer && confirmed) && !h.answered);
        self.map.remove(k);
        if flash {
            self.flash_until.insert(k, seen.now + FLASH_MS);
        }
        None
    }

    /// Whether `key` is held (without reconciling).
    pub fn is_held(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    /// Whether `key`'s write is still unanswered (the row is locked).
    pub fn is_pending(&self, key: &str) -> bool {
        self.map.get(key).is_some_and(|h| !h.answered)
    }

    /// Whether `key`'s value flashes red at `now` (a refusal or a timed-out write).
    pub fn flashing(&self, key: &str, now: f64) -> bool {
        self.flash_until.get(key).is_some_and(|&t| now < t)
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

    fn tap(kind: HoldKind, text: &str, from: &str, value: Option<f64>) -> Tap {
        Tap {
            kind,
            text: text.into(),
            from: from.into(),
            value,
            was_refused: false,
        }
    }

    fn value(h: &mut Holds, key: &'static str, text: &str, v: f64, seq: u64, now: f64) -> u64 {
        h.hold(key, tap(HoldKind::Value, text, "old", Some(v)), seq, now)
    }

    fn step(h: &mut Holds, guess: &str, from: &str, seq: u64, now: f64) -> u64 {
        h.hold(
            "apertureStep",
            tap(HoldKind::Step, guess, from, None),
            seq,
            now,
        )
    }

    #[test]
    fn a_value_hold_survives_a_stale_push_and_ends_on_confirmation() {
        let mut h = Holds::default();
        value(&mut h, "kelvin", "5700 K", 5700.0, 1, 0.0);
        assert_eq!(
            h.reconcile("kelvin", seen("5600 K", false, 2, 100.0))
                .map(|x| x.text.clone()),
            Some("5700 K".into())
        );
        assert!(h
            .reconcile("kelvin", seen("5700 K", false, 3, 200.0))
            .is_none());
        assert!(!h.is_held("kelvin"));
        assert!(!h.flashing("kelvin", 200.0), "a confirmation is quiet");
    }

    #[test]
    fn a_refusal_or_the_timeout_ends_a_hold_and_flashes() {
        let mut h = Holds::default();
        value(&mut h, "iso", "800", 800.0, 1, 0.0);
        assert!(
            h.reconcile("iso", seen("400", true, 2, 10.0)).is_none(),
            "refused"
        );
        assert!(h.flashing("iso", 10.0) && h.flashing("iso", 10.0 + FLASH_MS - 1.0));
        assert!(!h.flashing("iso", 10.0 + FLASH_MS), "the flash ends");
        value(&mut h, "iso", "800", 800.0, 1, 0.0);
        assert!(!h.flashing("iso", 1.0), "a new press clears an old flash");
        assert!(h
            .reconcile("iso", seen("400", false, 1, VALUE_HOLD_MS - 1.0))
            .is_some());
        assert!(
            h.reconcile("iso", seen("400", false, 1, VALUE_HOLD_MS))
                .is_none(),
            "timed out"
        );
        assert!(
            h.flashing("iso", VALUE_HOLD_MS),
            "an unanswered timeout flashes"
        );
    }

    #[test]
    fn an_answered_hold_that_times_out_settles_quietly() {
        let mut h = Holds::default();
        let id = value(&mut h, "iso", "800", 800.0, 1, 0.0);
        h.answered("iso", id, WriteAnswer::Accepted, 50.0);
        assert!(h.reconcile("iso", seen("400", false, 2, 100.0)).is_some());
        assert!(h
            .reconcile("iso", seen("400", false, 2, VALUE_HOLD_MS))
            .is_none());
        assert!(!h.flashing("iso", VALUE_HOLD_MS));
    }

    #[test]
    fn only_a_newer_push_confirms_or_refuses() {
        let mut h = Holds::default();
        // The push the tap was made against still carries an older refusal of the key.
        value(&mut h, "kelvin", "5700 K", 5700.0, 4, 0.0);
        assert!(h
            .reconcile("kelvin", seen("5600 K", true, 4, 10.0))
            .is_some());
        assert!(h
            .reconcile("kelvin", seen("5600 K", true, 5, 20.0))
            .is_none());
    }

    #[test]
    fn a_step_hold_shows_its_guess_and_ends_when_the_f_stop_changes() {
        let mut h = Holds::default();
        step(&mut h, "f/8.0", "f/5.2", 1, 0.0);
        assert_eq!(
            h.reconcile("apertureStep", seen("f/5.2", false, 2, 1000.0))
                .map(|x| x.text.clone()),
            Some("f/8.0".into()),
            "a stale push keeps the guess"
        );
        assert!(
            h.reconcile("apertureStep", seen("f/5.6", false, 3, 1100.0))
                .is_none(),
            "the lens landed elsewhere than the guess: settles on the camera"
        );
        assert!(!h.flashing("apertureStep", 1100.0));
        step(&mut h, "f/8.0", "f/5.2", 1, 0.0);
        assert!(h
            .reconcile("apertureStep", seen("f/5.2", false, 2, STEP_HOLD_MS - 1.0))
            .is_some());
        assert!(h
            .reconcile("apertureStep", seen("f/5.2", false, 2, STEP_HOLD_MS))
            .is_none());
        assert!(h.flashing("apertureStep", STEP_HOLD_MS));
    }

    #[test]
    fn the_row_is_locked_until_the_answer_and_debounced_for_300_ms() {
        let mut h = Holds::default();
        assert!(h.accepts("tint", 0.0), "a free key accepts");
        let id = value(&mut h, "tint", "+1", 1.0, 1, 1000.0);
        assert!(h.is_pending("tint"));
        assert!(!h.accepts("tint", 1010.0), "a double press");
        assert!(!h.accepts("tint", 5000.0), "still unanswered: locked");
        assert!(h.accepts("iso", 1010.0), "only the pressed row");
        h.answered("tint", id, WriteAnswer::Accepted, 1100.0);
        assert!(!h.is_pending("tint") && h.is_held("tint"));
        assert!(
            !h.accepts("tint", 1000.0 + DEBOUNCE_MS - 1.0),
            "a fast answer still debounces"
        );
        assert!(h.accepts("tint", 1000.0 + DEBOUNCE_MS));
        assert!(h.accepts("tint", 10.0), "a clock stepped back never locks");
    }

    #[test]
    fn a_failed_write_drops_its_hold_and_flashes() {
        let mut h = Holds::default();
        let id = value(&mut h, "tint", "+1", 1.0, 1, 0.0);
        h.answered("tint", id, WriteAnswer::Failed, 20.0);
        assert!(!h.is_held("tint") && h.flashing("tint", 20.0));
        assert!(h.accepts("tint", 20.0 + DEBOUNCE_MS));
        assert!(h.reconcile("tint", seen("0", false, 2, 30.0)).is_none());
        assert!(h.reconcile("nothing", seen("x", false, 2, 1.0)).is_none());
        // An answer for a hold that already ended, or a second answer, changes nothing.
        h.answered("nothing", 1, WriteAnswer::Failed, 40.0);
        assert!(!h.flashing("nothing", 40.0));
        let id = value(&mut h, "iso", "800", 800.0, 1, 0.0);
        h.answered("iso", id, WriteAnswer::Accepted, 10.0);
        h.answered("iso", id, WriteAnswer::Failed, 20.0);
        assert!(h.is_held("iso") && !h.flashing("iso", 20.0));
    }

    /// Review finding: a late answer of an older write must never unlock or end a newer press.
    #[test]
    fn an_answer_reaches_only_the_hold_of_its_own_write() {
        let mut h = Holds::default();
        let first = value(&mut h, "iso", "800", 800.0, 1, 0.0);
        // The first hold timed out unanswered; the operator pressed again.
        assert!(h
            .reconcile("iso", seen("400", false, 1, VALUE_HOLD_MS))
            .is_none());
        let second = value(&mut h, "iso", "800", 800.0, 1, VALUE_HOLD_MS + 10.0);
        assert_ne!(first, second);
        h.answered("iso", first, WriteAnswer::Accepted, VALUE_HOLD_MS + 20.0);
        assert!(
            h.is_pending("iso"),
            "the old answer does not unlock the new press"
        );
        h.answered("iso", first, WriteAnswer::Failed, VALUE_HOLD_MS + 30.0);
        assert!(h.is_held("iso"), "nor end it");
        h.answered("iso", 0, WriteAnswer::Failed, VALUE_HOLD_MS + 40.0);
        assert!(
            h.is_pending("iso"),
            "a write no hold owns (id 0) changes nothing"
        );
        h.answered("iso", second, WriteAnswer::Failed, VALUE_HOLD_MS + 50.0);
        assert!(!h.is_held("iso") && h.flashing("iso", VALUE_HOLD_MS + 50.0));
    }

    /// Review finding: a `notApplied` an EARLIER write left in the relay's state, still in the
    /// view the press was made against, must not refuse the new press from an unrelated newer
    /// push (a tally change); it counts once the write's own answer is in.
    #[test]
    fn an_earlier_refusal_counts_only_after_this_writes_answer() {
        let mut h = Holds::default();
        let mut t = tap(HoldKind::Value, "5700 K", "5600 K", Some(5700.0));
        t.was_refused = true;
        let id = h.hold("kelvin", t, 1, 0.0);
        assert!(
            h.reconcile("kelvin", seen("5600 K", true, 2, 10.0))
                .is_some(),
            "a newer push about the tally still carries the old refusal"
        );
        h.answered("kelvin", id, WriteAnswer::Accepted, 20.0);
        assert!(h
            .reconcile("kelvin", seen("5600 K", true, 2, 30.0))
            .is_none());
        assert!(
            h.flashing("kelvin", 30.0),
            "refused again: the write's own read-back"
        );
        // Without an earlier refusal a newer refusing view ends the hold at once.
        value(&mut h, "kelvin", "5700 K", 5700.0, 1, 100.0);
        assert!(h
            .reconcile("kelvin", seen("5600 K", true, 2, 110.0))
            .is_none());
    }

    #[test]
    fn only_a_2xx_status_is_accepted() {
        assert_eq!(WriteAnswer::of(Some(200)), WriteAnswer::Accepted);
        assert_eq!(WriteAnswer::of(Some(204)), WriteAnswer::Accepted);
        assert_eq!(WriteAnswer::of(Some(502)), WriteAnswer::Failed);
        assert_eq!(WriteAnswer::of(Some(404)), WriteAnswer::Failed);
        assert_eq!(WriteAnswer::of(None), WriteAnswer::Failed);
    }
}
