//! ARRANGE's edit (shading#16, reduced from fohmixer `behave::arrange_edit::Edit`): the order and
//! the hidden set being changed, nothing else (no rename, add or remove of layouts).
//!
//! The order is the FULL one ([`effective_order`]): it keeps ids the config does not list now (a
//! visiting camera, issue 1431), so a save never drops their place; the panel only draws the ids
//! the service lists.

use bkshading_proto::layout::{effective_order, number_order, same_layout, LayoutDoc, LayoutPut};

/// Where a carried block lands on another one (mockup arrange-v1): on its middle they swap; on
/// its left or right edge the carried block moves before or after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropKind {
    Swap,
    Before,
    After,
}

/// An edit in progress.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Edit {
    pub order: Vec<String>,
    pub hidden: Vec<String>,
}

impl Edit {
    /// The edit ARRANGE starts from: the stored layout with every listed camera in its place.
    pub fn of(doc: &LayoutDoc, cameras: &[String]) -> Edit {
        Edit {
            order: effective_order(doc, cameras),
            hidden: doc.hidden.clone(),
        }
    }

    /// The edit as a layout document at revision `rev` (for drawing it).
    pub fn as_doc(&self, rev: u64) -> LayoutDoc {
        LayoutDoc {
            rev,
            order: self.order.clone(),
            hidden: self.hidden.clone(),
        }
    }

    /// The save of this edit on top of revision `rev`.
    pub fn put(&self, rev: u64) -> LayoutPut {
        LayoutPut {
            rev,
            order: self.order.clone(),
            hidden: self.hidden.clone(),
        }
    }

    /// Whether this edit shows something else than `base` (the edit the mode started from).
    pub fn differs_from(&self, base: &Edit) -> bool {
        !same_layout(&self.order, &self.hidden, &base.as_doc(0))
    }

    /// The eye: hides a shown camera, shows a hidden one.
    pub fn toggle_hidden(&mut self, id: &str) {
        if let Some(i) = self.hidden.iter().position(|h| h == id) {
            self.hidden.remove(i);
        } else if self.order.iter().any(|o| o == id) {
            self.hidden.push(id.to_string());
        }
    }

    /// `↺ podľa čísel` (owner 15.11.2026: kept): camera-number order, every camera shown. The
    /// operator still confirms it with the save tap.
    pub fn reset_to_numbers(&mut self, cameras: &[String]) {
        let full = effective_order(&self.as_doc(0), cameras);
        self.order = number_order(&full);
        self.hidden.clear();
    }

    /// The block `id` carried onto the block `target` (`kind`): the new order kept. Whether the
    /// order changed (the same block, or an id no longer there, changes nothing).
    pub fn dropped(&mut self, id: &str, target: &str, kind: DropKind) -> bool {
        match dropped_order(&self.order, id, target, kind) {
            Some(order) if order != self.order => {
                self.order = order;
                true
            }
            _ => false,
        }
    }
}

/// `order` after the block `id` is dropped on `target` (`kind`), or none when either is not in it
/// or they are the same block.
pub fn dropped_order(
    order: &[String],
    id: &str,
    target: &str,
    kind: DropKind,
) -> Option<Vec<String>> {
    if id == target {
        return None;
    }
    let from = order.iter().position(|o| o == id)?;
    let to = order.iter().position(|o| o == target)?;
    let mut out = order.to_vec();
    match kind {
        DropKind::Swap => out.swap(from, to),
        DropKind::Before | DropKind::After => {
            let moved = out.remove(from);
            let at = out.iter().position(|o| o == target)?;
            let at = if kind == DropKind::After { at + 1 } else { at };
            out.insert(at, moved);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn swap_move_before_and_after() {
        let o = ids(&["cam1", "cam2", "cam3", "cam4"]);
        assert_eq!(
            dropped_order(&o, "cam1", "cam3", DropKind::Swap),
            Some(ids(&["cam3", "cam2", "cam1", "cam4"]))
        );
        assert_eq!(
            dropped_order(&o, "cam4", "cam2", DropKind::Before),
            Some(ids(&["cam1", "cam4", "cam2", "cam3"]))
        );
        assert_eq!(
            dropped_order(&o, "cam1", "cam3", DropKind::After),
            Some(ids(&["cam2", "cam3", "cam1", "cam4"]))
        );
        assert_eq!(
            dropped_order(&o, "cam1", "cam4", DropKind::After),
            Some(ids(&["cam2", "cam3", "cam4", "cam1"]))
        );
        assert_eq!(dropped_order(&o, "cam1", "cam1", DropKind::Swap), None);
        assert_eq!(dropped_order(&o, "cam9", "cam1", DropKind::Swap), None);
    }

    #[test]
    fn an_edit_starts_from_the_stored_layout_with_new_cameras_placed() {
        let doc = LayoutDoc {
            rev: 2,
            order: ids(&["cam3", "cam1"]),
            hidden: ids(&["cam1"]),
        };
        let e = Edit::of(&doc, &ids(&["cam1", "cam3", "cam2"]));
        assert_eq!(e.order, ids(&["cam3", "cam1", "cam2"]));
        assert_eq!(e.hidden, ids(&["cam1"]));
        assert_eq!(e.put(2).rev, 2);
    }

    #[test]
    fn the_eye_hides_and_shows() {
        let mut e = Edit {
            order: ids(&["cam1", "cam2"]),
            hidden: vec![],
        };
        e.toggle_hidden("cam2");
        assert_eq!(e.hidden, ids(&["cam2"]));
        e.toggle_hidden("cam2");
        assert!(e.hidden.is_empty());
        e.toggle_hidden("cam9");
        assert!(
            e.hidden.is_empty(),
            "an id not in the order is never hidden"
        );
    }

    #[test]
    fn reset_is_number_order_all_shown_keeping_unknown_ids() {
        let mut e = Edit {
            order: ids(&["cam5", "cam2", "cam3"]),
            hidden: ids(&["cam3"]),
        };
        // cam2 is not listed now (a visiting camera): it keeps a place in the saved order.
        e.reset_to_numbers(&ids(&["cam3", "cam5", "cam1"]));
        assert_eq!(e.order, ids(&["cam1", "cam2", "cam3", "cam5"]));
        assert!(e.hidden.is_empty());
    }

    #[test]
    fn differs_compares_order_and_hidden_set() {
        let base = Edit {
            order: ids(&["cam1", "cam2"]),
            hidden: ids(&["cam1", "cam2"]),
        };
        let mut e = base.clone();
        assert!(!e.differs_from(&base));
        e.hidden = ids(&["cam2", "cam1"]);
        assert!(!e.differs_from(&base), "the hidden SET");
        assert!(e.dropped("cam1", "cam2", DropKind::Swap));
        assert!(e.differs_from(&base));
        assert!(!e.dropped("cam1", "cam1", DropKind::Swap));
    }
}
