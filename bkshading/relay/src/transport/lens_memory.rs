//! What the relay learns about the lens's aperture steps while it walks them (shading#6, 0.2.5).
//!
//! Live on cam5 (Pocket 4K fw 9.8, zoom at 108 mm, 10.11.2026) one +/- press cost 4.2-5.9 s: the
//! lens has 1/3-stop steps and the relay finds the next one by probing 1/16 stop at a time. And at
//! the zoom's wide end (4.4) the camera's `normalised` reads 0.0387, measured against the
//! description min 4.1, so the end was probed again on every press. A lens's steps and its ends
//! depend on the focal length, so everything here is kept per `lens/zoom focalLength` (`None` = the
//! camera did not give one):
//! - **the ladder:** every f-stop seen at that focal length, and the pairs a probe walk proved
//!   ADJACENT (the walk never skips a step, so where it lands from `a` is the next step after `a`).
//!   A press whose next stop is such a proven neighbour, with no other known stop between them, is
//!   written directly: one PUT instead of a probe walk. A stop seen between two neighbours breaks
//!   their pair (the never-skip guarantee holds only for a pair nothing was ever seen inside);
//! - **the ends:** a press that found no further step that way records its start as that end. A
//!   later press toward it from there is refused without a PUT, and `irisRangePos` reads 0 (wide)
//!   or 1 (closed) there. A stop seen past a learned end drops that end.
//!
//! Pure state, no IO: the session reads and writes the camera and tells this what it saw. The
//! session clears it when the camera link drops (another camera or lens may be behind the link).

use std::collections::{BTreeSet, HashMap};

/// The focal length a memory belongs to, in whole mm; `None` = the camera gives no `lens/zoom`.
pub type FocalKey = Option<i64>;

/// An f-stop as the key of a step: hundredths (`4.4` -> 440). The camera reports its stops to 0.1
/// (0.05 on the 6K's 6.45), so two reads of one stop always give one key.
fn centi(f: f64) -> i64 {
    (f * 100.0).round() as i64
}

fn stop(c: i64) -> f64 {
    c as f64 / 100.0
}

/// What is known at one focal length.
#[derive(Debug, Default, Clone)]
struct Ladder {
    /// Every stop seen here (read, landed, or written and landed).
    stops: BTreeSet<i64>,
    /// `(lower, upper)` pairs a walk proved to be adjacent native steps.
    adjacent: BTreeSet<(i64, i64)>,
    /// The widest stop: a "-" from it found no further step.
    wide_end: Option<i64>,
    /// The most closed stop: a "+" from it found no further step.
    closed_end: Option<i64>,
}

/// The relay's lens memory, per focal length.
#[derive(Debug, Default, Clone)]
pub struct LensMemory {
    by_focal: HashMap<FocalKey, Ladder>,
}

impl LensMemory {
    /// Forgets everything (the camera link dropped).
    pub fn clear(&mut self) {
        self.by_focal.clear();
    }

    /// Forgets one focal length (its ladder turned out wrong).
    pub fn forget_focal(&mut self, focal: FocalKey) {
        self.by_focal.remove(&focal);
    }

    /// An f-stop the iris was read or landed at. A stop wider than the learned wide end (or more
    /// closed than the closed end) drops that end; a stop between two proven neighbours breaks
    /// their pair.
    pub fn note_stop(&mut self, focal: FocalKey, f: f64) {
        if !(f.is_finite() && f > 0.0) {
            return;
        }
        let c = centi(f);
        let l = self.by_focal.entry(focal).or_default();
        l.stops.insert(c);
        if l.wide_end.is_some_and(|w| c < w) {
            l.wide_end = None;
        }
        if l.closed_end.is_some_and(|x| c > x) {
            l.closed_end = None;
        }
        l.adjacent.retain(|(lo, hi)| !(c > *lo && c < *hi));
    }

    /// A walk from `from` landed on `to`: the next native step that way, so the two are adjacent.
    /// Any other pair claiming a different neighbour for either of them is dropped.
    pub fn note_adjacent(&mut self, focal: FocalKey, from: f64, to: f64) {
        self.note_stop(focal, from);
        self.note_stop(focal, to);
        let (a, b) = (centi(from), centi(to));
        if a == b {
            return;
        }
        let pair = (a.min(b), a.max(b));
        let l = self.by_focal.entry(focal).or_default();
        l.adjacent
            .retain(|p| *p == pair || (p.0 != pair.0 && p.1 != pair.1));
        l.adjacent.insert(pair);
    }

    /// A direct write from `from` to `to` did not land there: the pair is not trusted any more.
    pub fn forget_adjacent(&mut self, focal: FocalKey, from: f64, to: f64) {
        let (a, b) = (centi(from), centi(to));
        if let Some(l) = self.by_focal.get_mut(&focal) {
            l.adjacent.remove(&(a.min(b), a.max(b)));
        }
    }

    /// The proven next stop from `from` in direction `dir` (sign), only when no other known stop
    /// lies between the two (the never-skip guarantee). `None` = probe.
    pub fn neighbour(&self, focal: FocalKey, from: f64, dir: i64) -> Option<f64> {
        let l = self.by_focal.get(&focal)?;
        let c = centi(from);
        let n = if dir > 0 {
            l.adjacent.iter().find(|p| p.0 == c).map(|p| p.1)
        } else if dir < 0 {
            l.adjacent.iter().find(|p| p.1 == c).map(|p| p.0)
        } else {
            None
        }?;
        let (lo, hi) = (c.min(n), c.max(n));
        if l.stops.range(lo + 1..hi).next().is_some() {
            return None;
        }
        Some(stop(n))
    }

    /// A press from `at` in direction `dir` found no further step: `at` is that end of the lens.
    pub fn note_end(&mut self, focal: FocalKey, at: f64, dir: i64) {
        self.note_stop(focal, at);
        let c = centi(at);
        let l = self.by_focal.entry(focal).or_default();
        if dir < 0 {
            l.wide_end = Some(c);
        } else if dir > 0 {
            l.closed_end = Some(c);
        }
    }

    /// Whether `at` is the learned end a press in direction `dir` heads for.
    pub fn is_end(&self, focal: FocalKey, at: f64, dir: i64) -> bool {
        let Some(l) = self.by_focal.get(&focal) else {
            return false;
        };
        let c = centi(at);
        (dir < 0 && l.wide_end == Some(c)) || (dir > 0 && l.closed_end == Some(c))
    }

    /// `irisRangePos` for an iris at a learned end: `0.0` at the wide end, `1.0` at the closed end,
    /// `None` elsewhere (the camera's own `normalised` stands).
    pub fn end_position(&self, focal: FocalKey, at: f64) -> Option<f64> {
        let l = self.by_focal.get(&focal)?;
        let c = centi(at);
        if l.wide_end == Some(c) {
            Some(0.0)
        } else if l.closed_end == Some(c) {
            Some(1.0)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const Z: FocalKey = Some(108);

    #[test]
    fn a_proven_pair_is_a_neighbour_both_ways() {
        let mut m = LensMemory::default();
        m.note_adjacent(Z, 6.7, 6.2);
        assert_eq!(m.neighbour(Z, 6.2, 1), Some(6.7));
        assert_eq!(m.neighbour(Z, 6.7, -1), Some(6.2));
        assert_eq!(m.neighbour(Z, 6.7, 1), None);
        assert_eq!(m.neighbour(Some(24), 6.2, 1), None, "another focal length");
        assert_eq!(m.neighbour(None, 6.2, 1), None);
    }

    #[test]
    fn a_stop_seen_between_two_neighbours_breaks_the_pair() {
        let mut m = LensMemory::default();
        m.note_adjacent(Z, 4.4, 5.2);
        m.note_stop(Z, 4.8);
        assert_eq!(m.neighbour(Z, 4.4, 1), None);
        assert_eq!(m.neighbour(Z, 5.2, -1), None);
    }

    #[test]
    fn a_new_neighbour_replaces_an_old_claim() {
        let mut m = LensMemory::default();
        m.note_adjacent(Z, 4.4, 5.2);
        m.note_adjacent(Z, 4.4, 4.8);
        assert_eq!(m.neighbour(Z, 4.4, 1), Some(4.8));
        assert_eq!(m.neighbour(Z, 5.2, -1), None);
        m.forget_adjacent(Z, 4.8, 4.4);
        assert_eq!(m.neighbour(Z, 4.4, 1), None);
    }

    #[test]
    fn ends_are_per_focal_length_and_dropped_by_a_stop_past_them() {
        let mut m = LensMemory::default();
        m.note_end(Z, 4.4, -1);
        m.note_end(Z, 22.0, 1);
        assert!(m.is_end(Z, 4.4, -1));
        assert!(!m.is_end(Z, 4.4, 1));
        assert!(!m.is_end(Some(70), 4.4, -1));
        assert_eq!(m.end_position(Z, 4.4), Some(0.0));
        assert_eq!(m.end_position(Z, 22.0), Some(1.0));
        assert_eq!(m.end_position(Z, 5.2), None);
        m.note_stop(Z, 4.0);
        assert!(!m.is_end(Z, 4.4, -1), "4.0 was seen: 4.4 is not the end");
        m.forget_focal(Z);
        assert!(!m.is_end(Z, 22.0, 1));
        m.note_end(Z, 4.4, -1);
        m.clear();
        assert!(!m.is_end(Z, 4.4, -1));
    }
}
