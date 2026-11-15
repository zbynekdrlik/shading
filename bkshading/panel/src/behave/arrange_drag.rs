//! ARRANGE, the panel's edit mode (shading#16, mirrored from fohmixer `behave::arrange_drag`; the
//! owner's approved mockup arrange-v1): the first tap on `⇄` starts it on the current revision with
//! every block under a cover; blocks are dragged (onto a block's middle = swap, onto its left or
//! right edge = move there) and hidden or shown with the eye; the second tap sends ONE save
//! (`PUT /api/layout`), or leaves at once with "Bez zmeny" when nothing changed. A newer layout
//! from another device, or a hide of the page, ends the mode unsaved. Nothing here touches a
//! camera: every control is under the cover.
//!
//! The drag: a block is carried once the finger moved past [`DRAG_SLOP`]; it lands on the block
//! whose centre is nearest the finger (the rects read once at the down), [`drop_kind`] by where on
//! that block. Unlike fohmixer's slot model there are no empty cells: the camera grid reflows with
//! the screen width, so an ordered list plus a hidden set is the whole model.

use bkshading_proto::layout::{effective_order, shown_order, LayoutDoc, LayoutPut, LayoutRefusal};

use super::arrange_edit::{DropKind, Edit};

/// How far (px) a finger moves before the block under it is carried (fohmixer's).
pub const DRAG_SLOP: f64 = 6.0;
/// The share of a block's width at each side that means "move before / after it" (mockup: 22 %).
pub const EDGE_SHARE: f64 = 0.22;
/// How long a refusal or a result note shows (design: 4 s).
pub const NOTE_MS: u32 = 4000;

/// The note while the mode is on (mockup arrange-v1).
pub const ARRANGE_NOTE: &str = "ťahaj ⠿ · oko skryje/ukáže · ⇄ = uložiť";
pub const SAVING_NOTE: &str = "Ukladám…";
pub const SAVED_NOTE: &str = "Uložené pre všetky zariadenia";
pub const UNCHANGED_NOTE: &str = "Bez zmeny";
/// The save was `stale`, or another device saved during the mode.
pub const STALE_NOTE: &str = "Neuložené: poradie sa medzitým zmenilo";
pub const INVALID_NOTE: &str = "Neuložené: poradie nie je platné";
pub const OFFLINE_NOTE: &str = "Neuložené: služba nie je pripojená";
pub const NOT_SAVED_NOTE: &str = "Neuložené";

/// The mode.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Arrange {
    #[default]
    Off,
    /// On since revision `rev`: `base` the edit it started from, `edit` the edit so far.
    On { rev: u64, base: Edit, edit: Edit },
    /// The save of `edit` on top of `rev` is on its way (the edit stays shown).
    Saving { rev: u64, edit: Edit },
}

/// What a tap on `⇄` does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tap {
    Start,
    Send(LayoutPut),
    /// Nothing changed: the mode ends.
    Leave,
}

/// Why a save did not land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveFail {
    Refused(LayoutRefusal),
    /// The request never reached the service (no network).
    Offline,
    /// Any other answer (a failed file write: 500).
    Other,
}

/// How a note looks: plain, a taken save (green), a save that did not land (orange).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Saved,
    Refused,
}

impl Tone {
    /// The note's CSS class.
    pub fn class(self) -> &'static str {
        match self {
            Tone::Plain => "note",
            Tone::Saved => "note saved",
            Tone::Refused => "note refused",
        }
    }
}

/// A note shown under the header, with its look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Note {
    pub text: &'static str,
    pub tone: Tone,
}

impl Arrange {
    /// A tap on `⇄` with the stored `doc` and the listed `cameras`. None while a save is on its way.
    pub fn tap(&mut self, doc: &LayoutDoc, cameras: &[String]) -> Option<Tap> {
        match self {
            Arrange::Off => {
                let base = Edit::of(doc, cameras);
                *self = Arrange::On {
                    rev: doc.rev,
                    edit: base.clone(),
                    base,
                };
                Some(Tap::Start)
            }
            Arrange::On { rev, base, edit } => {
                let rev = *rev;
                if edit.differs_from(base) {
                    let put = edit.put(rev);
                    *self = Arrange::Saving {
                        rev,
                        edit: std::mem::take(edit),
                    };
                    Some(Tap::Send(put))
                } else {
                    *self = Arrange::Off;
                    Some(Tap::Leave)
                }
            }
            Arrange::Saving { .. } => None,
        }
    }

    /// The edit, while the mode is on (never while saving).
    pub fn edit_mut(&mut self) -> Option<&mut Edit> {
        match self {
            Arrange::On { edit, .. } => Some(edit),
            _ => None,
        }
    }

    /// Whether the edit can change now (on, not saving): a drag or an eye tap may start.
    pub fn is_editing(&self) -> bool {
        matches!(self, Arrange::On { .. })
    }

    /// Whether the mode is on (saving included): the blocks are under their covers.
    pub fn is_on(&self) -> bool {
        !matches!(self, Arrange::Off)
    }

    /// The service answered the save: the layout it stored (`Ok`: its new revision) ends the mode
    /// with the saved note and comes back so the panel shows it at once; a failure ends it with its
    /// note. Ignored unless a save is on its way.
    pub fn answered(
        &mut self,
        outcome: Result<u64, SaveFail>,
    ) -> Option<(Note, Option<LayoutDoc>)> {
        let Arrange::Saving { edit, .. } = std::mem::take(self) else {
            return None;
        };
        Some(match outcome {
            Ok(rev) => (
                Note {
                    text: SAVED_NOTE,
                    tone: Tone::Saved,
                },
                Some(edit.as_doc(rev)),
            ),
            Err(fail) => (
                Note {
                    text: fail_note(fail),
                    tone: Tone::Refused,
                },
                None,
            ),
        })
    }

    /// The service pushed the layout of revision `rev`: while the mode is on, another revision ends
    /// it unsaved (a save from another device), with its note. While saving, nothing: the save's
    /// own answer ends it (the push of the new layout may arrive before that answer, and a save
    /// that another device overtook is answered `stale`).
    pub fn doc_arrived(&mut self, rev: u64) -> Option<Note> {
        match self {
            Arrange::On { rev: since, .. } if *since != rev => {
                *self = Arrange::Off;
                Some(Note {
                    text: STALE_NOTE,
                    tone: Tone::Refused,
                })
            }
            _ => None,
        }
    }

    /// The page was hidden: the mode ends unsaved. Whether it was on.
    pub fn hidden_page(&mut self) -> bool {
        let was = matches!(self, Arrange::On { .. });
        if was {
            *self = Arrange::Off;
        }
        was
    }

    /// The blocks as the panel draws them, each with whether it is a hidden camera's dashed place:
    /// outside the mode the stored layout's shown cameras; in it every listed camera in the edit's
    /// order, the hidden ones as ghosts.
    pub fn blocks(&self, doc: &LayoutDoc, cameras: &[String]) -> Vec<(String, bool)> {
        let edit = match self {
            Arrange::Off => {
                return shown_order(doc, cameras)
                    .into_iter()
                    .map(|id| (id, false))
                    .collect()
            }
            Arrange::On { edit, .. } | Arrange::Saving { edit, .. } => edit,
        };
        effective_order(&edit.as_doc(0), cameras)
            .into_iter()
            .filter(|id| cameras.contains(id))
            .map(|id| {
                let ghost = edit.hidden.contains(&id);
                (id, ghost)
            })
            .collect()
    }
}

/// The note of a failed save.
pub fn fail_note(fail: SaveFail) -> &'static str {
    match fail {
        SaveFail::Refused(LayoutRefusal::Stale) => STALE_NOTE,
        SaveFail::Refused(LayoutRefusal::Invalid) => INVALID_NOTE,
        SaveFail::Offline => OFFLINE_NOTE,
        SaveFail::Other => NOT_SAVED_NOTE,
    }
}

/// A note's showings, numbered: a note's timer clears it only while it is still the one shown
/// (fohmixer's `NoteTimer`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoteTimer {
    shown: u64,
}

impl NoteTimer {
    pub fn show(&mut self) -> u64 {
        self.shown += 1;
        self.shown
    }
    pub fn expires(&self, mine: u64) -> bool {
        self.shown == mine
    }
}

/// A block's rect on screen, read once at the down.
#[derive(Debug, Clone, PartialEq)]
pub struct Spot {
    pub id: String,
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

/// Where on a block a finger at relative x (`0` left edge, `1` right edge) drops.
pub fn drop_kind(rel_x: f64) -> DropKind {
    if rel_x < EDGE_SHARE {
        DropKind::Before
    } else if rel_x > 1.0 - EDGE_SHARE {
        DropKind::After
    } else {
        DropKind::Swap
    }
}

/// The block (of `spots`, the other blocks) whose centre is nearest `(x, y)`, and where on it.
pub fn target_at(spots: &[Spot], x: f64, y: f64) -> Option<(&Spot, DropKind)> {
    let dist = |s: &Spot| (x - (s.left + s.width / 2.0)).hypot(y - (s.top + s.height / 2.0));
    let best = spots.iter().min_by(|a, b| dist(a).total_cmp(&dist(b)))?;
    let rel = (x - best.left) / best.width.max(1.0);
    Some((best, drop_kind(rel)))
}

/// Whether a finger moved from `(x0, y0)` to `(x, y)` carries the block.
pub fn drags(x0: f64, y0: f64, x: f64, y: f64) -> bool {
    (x - x0).hypot(y - y0) > DRAG_SLOP
}

#[cfg(test)]
#[path = "arrange_drag_tests.rs"]
mod tests;
