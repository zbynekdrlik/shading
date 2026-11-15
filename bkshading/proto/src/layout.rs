//! The venue's panel layout (shading#16): the order of the camera blocks and which of them the
//! panel hides. One document per venue (one service per strih PC), stored by the service, edited in
//! the panel's ARRANGE mode and pushed to every panel at the venue.
//!
//! Pure and IO-free: the service (store + API) and the panel (display + ARRANGE) both use these
//! rules, so a camera new to the config lands at the same place on every device.
//!
//! The document keeps ids the current config does not list (a visiting camera of a joint event,
//! issue 1431): the panel skips them, and they are back in their place at the next joint event.
//! Hiding is panel-only: a hidden camera is still polled and keeps its settings (owner 15.11.2026).

use serde::{Deserialize, Serialize};

/// The stored layout of one venue. `rev` counts saves (0 = never saved: number order, nothing
/// hidden). `order` lists camera ids, first block first; `hidden` lists ids the panel leaves out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutDoc {
    pub rev: u64,
    #[serde(default)]
    pub order: Vec<String>,
    #[serde(default)]
    pub hidden: Vec<String>,
}

/// A panel's save: the new order and hidden set, made on top of revision `rev`
/// (`PUT /api/layout`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutPut {
    pub rev: u64,
    pub order: Vec<String>,
    pub hidden: Vec<String>,
}

/// Why the service refused a save. The wire word is [`LayoutRefusal::text`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutRefusal {
    /// `rev` is not the current revision: another device saved first.
    Stale,
    /// An id repeats in `order` or `hidden`, an id is empty, or a hidden id is not in `order`.
    Invalid,
}

impl LayoutRefusal {
    /// The refusal word the service answers with (`{"error": "<word>"}`).
    pub fn text(self) -> &'static str {
        match self {
            LayoutRefusal::Stale => "stale",
            LayoutRefusal::Invalid => "invalid",
        }
    }

    /// The refusal for a wire word; `None` for any other word.
    pub fn from_text(word: &str) -> Option<Self> {
        match word {
            "stale" => Some(LayoutRefusal::Stale),
            "invalid" => Some(LayoutRefusal::Invalid),
            _ => None,
        }
    }
}

/// The camera number of an id: its trailing digits (`cam7` -> 7, `cam10` -> 10), the order the
/// operators know (shading#13). `None` for an id without trailing digits.
pub fn camera_number(id: &str) -> Option<u64> {
    let digits = id.len() - id.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    id[id.len() - digits..].parse().ok()
}

/// The sort key of an id in number order: numbered ids by number, then ids without a number.
fn number_key(id: &str) -> (bool, u64) {
    match camera_number(id) {
        Some(n) => (false, n),
        None => (true, 0),
    }
}

/// `ids` in camera-number order (shading#13): by number, an id without a number last; equal keys
/// keep their order in `ids` (a stable sort).
pub fn number_order(ids: &[String]) -> Vec<String> {
    let mut out = ids.to_vec();
    out.sort_by_key(|id| number_key(id));
    out
}

/// Whether `a` stands before `b` in number order.
fn lower_number(a: &str, b: &str) -> bool {
    number_key(a) < number_key(b)
}

/// The full order the panel works with: the document's order, then every camera of `cameras` (the
/// ids the service lists, in config order) it does not hold yet, each placed at its number slot,
/// right after the last id already placed whose number is lower (first, when there is none).
/// Unknown ids of the document stay where they are (the panel skips them when it draws). A fresh
/// install (an empty order) gives plain number order. A repeated id keeps its first place.
pub fn effective_order(doc: &LayoutDoc, cameras: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(doc.order.len() + cameras.len());
    for id in &doc.order {
        if !out.contains(id) {
            out.push(id.clone());
        }
    }
    for id in number_order(cameras) {
        if out.contains(&id) {
            continue;
        }
        let at = out
            .iter()
            .rposition(|placed| lower_number(placed, &id))
            .map_or(0, |i| i + 1);
        out.insert(at, id);
    }
    out
}

/// The blocks the panel shows outside ARRANGE: [`effective_order`] limited to `cameras`, without
/// the hidden ones.
pub fn shown_order(doc: &LayoutDoc, cameras: &[String]) -> Vec<String> {
    effective_order(doc, cameras)
        .into_iter()
        .filter(|id| cameras.contains(id) && !doc.hidden.contains(id))
        .collect()
}

/// How many of `cameras` the document hides (the header chip; an unknown hidden id is not counted).
pub fn hidden_count(doc: &LayoutDoc, cameras: &[String]) -> usize {
    cameras.iter().filter(|id| doc.hidden.contains(id)).count()
}

/// Whether a save's lists are valid: no empty id, no id twice in `order` or in `hidden`, and every
/// hidden id in `order`.
pub fn validate(order: &[String], hidden: &[String]) -> Result<(), LayoutRefusal> {
    let unique = |ids: &[String]| ids.iter().enumerate().all(|(i, id)| !ids[..i].contains(id));
    let filled = order.iter().chain(hidden).all(|id| !id.trim().is_empty());
    if filled && unique(order) && unique(hidden) && hidden.iter().all(|id| order.contains(id)) {
        Ok(())
    } else {
        Err(LayoutRefusal::Invalid)
    }
}

/// The document after a save `put` on top of `current`: the next revision, or the refusal. The
/// revision check comes first, so a device that lost a race is told so even with a bad edit.
pub fn apply_put(current: &LayoutDoc, put: &LayoutPut) -> Result<LayoutDoc, LayoutRefusal> {
    if put.rev != current.rev {
        return Err(LayoutRefusal::Stale);
    }
    validate(&put.order, &put.hidden)?;
    Ok(LayoutDoc {
        rev: current.rev + 1,
        order: put.order.clone(),
        hidden: put.hidden.clone(),
    })
}

/// Whether two layouts show the same thing: the same order and the same hidden SET.
pub fn same_layout(a_order: &[String], a_hidden: &[String], b: &LayoutDoc) -> bool {
    let mut x = a_hidden.to_vec();
    let mut y = b.hidden.clone();
    x.sort();
    y.sort();
    a_order == b.order.as_slice() && x == y
}
