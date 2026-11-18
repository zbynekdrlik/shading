//! ARRANGE in the page (shading#16; the rules are `behave::arrange_drag` / `arrange_edit`): a
//! block's cover with its number, the `⠿` grip and the eye; the drag under a finger or a mouse
//! (pointer events, captured by the block); and the end of the mode when the page is hidden.
//!
//! The drag moves only the carried block's transform and marks the target (`swap-target`, or the
//! yellow line at an edge) until the drop; the order changes once, at the drop, in the edit.

use leptos::ev;
use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::behave::arrange_drag::{drags, target_at, Note, Spot, Tone, NOT_SAVED_NOTE};
use crate::behave::arrange_edit::DropKind;
use crate::store::Store;

/// The drag under way: the pointer, the carried block, where it went down, the other blocks'
/// rects then, and the target under it now.
struct Carry {
    pointer: i32,
    id: String,
    x0: f64,
    y0: f64,
    spots: Vec<Spot>,
    carried: bool,
    target: Option<(String, DropKind)>,
}

/// The other blocks' rects (the drag reads them once, at the down).
fn spots_except(id: &str) -> Vec<Spot> {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return Vec::new();
    };
    let Ok(list) = doc.query_selector_all("#camera-grid > .cam") else {
        return Vec::new();
    };
    (0..list.length())
        .filter_map(|i| list.item(i)?.dyn_into::<web_sys::Element>().ok())
        .filter_map(|el| {
            let other = el.get_attribute("data-id")?;
            (other != id).then(|| {
                let r = el.get_bounding_client_rect();
                Spot {
                    id: other,
                    left: r.left(),
                    top: r.top(),
                    width: r.width(),
                    height: r.height(),
                }
            })
        })
        .collect()
}

fn element_of(id: &str) -> Option<web_sys::HtmlElement> {
    let doc = web_sys::window()?.document()?;
    doc.query_selector(&format!("#camera-grid > .cam[data-id=\"{id}\"]"))
        .ok()??
        .dyn_into::<web_sys::HtmlElement>()
        .ok()
}

fn drop_line() -> Option<web_sys::HtmlElement> {
    web_sys::window()?
        .document()?
        .get_element_by_id("drop-line")?
        .dyn_into::<web_sys::HtmlElement>()
        .ok()
}

fn clear_marks(spots: &[Spot]) {
    for s in spots {
        if let Some(el) = element_of(&s.id) {
            let _ = el.class_list().remove_1("swap-target");
        }
    }
    if let Some(line) = drop_line() {
        let _ = line.class_list().remove_1("on");
    }
}

/// Marks where a drop at the target would land: the target block (swap) or the line at its edge.
fn mark(target: Option<(&Spot, DropKind)>) {
    let Some((spot, kind)) = target else {
        return;
    };
    if kind == DropKind::Swap {
        if let Some(el) = element_of(&spot.id) {
            let _ = el.class_list().add_1("swap-target");
        }
        return;
    }
    let (Some(line), Some(grid)) = (
        drop_line(),
        web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id("camera-grid")),
    ) else {
        return;
    };
    let g = grid.get_bounding_client_rect();
    let x = if kind == DropKind::Before {
        spot.left - 5.0
    } else {
        spot.left + spot.width + 5.0
    };
    // The line is placed inside the grid, which scrolls aside in the 5 and 8-in-a-row layouts
    // (shading#24): its scroll offset moves the line's frame.
    let (sx, sy) = (f64::from(grid.scroll_left()), f64::from(grid.scroll_top()));
    let style = line.style();
    let _ = style.set_property("left", &format!("{}px", x - g.left() + sx));
    let _ = style.set_property("top", &format!("{}px", spot.top - g.top() + sy));
    let _ = style.set_property("height", &format!("{}px", spot.height));
    let _ = line.class_list().add_1("on");
}

const EYE_SVG: &str = r#"<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M2 12s3.6-6.5 10-6.5S22 12 22 12s-3.6 6.5-10 6.5S2 12 2 12z"/><circle cx="12" cy="12" r="3"/><path class="slash" d="M4 20L20 4"/></svg>"#;

/// A block's ARRANGE cover (shown only in the mode, by `arrange.css`).
#[component]
pub fn Cover(id: String, block: NodeRef<html::Section>, ghost: Signal<bool>) -> impl IntoView {
    let store = expect_context::<Store>();
    let carry = StoredValue::new_local(None::<Carry>);
    let id = StoredValue::new(id);

    let on_down = move |e: ev::PointerEvent| {
        if !store.arrange.with_untracked(|a| a.is_editing()) || e.button() > 0 {
            return;
        }
        e.prevent_default();
        // The cover captures the pointer: its moves and its up keep arriving here off the block.
        if let Some(cover) = e
            .current_target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        {
            let _ = cover.set_pointer_capture(e.pointer_id());
        }
        carry.set_value(Some(Carry {
            pointer: e.pointer_id(),
            id: id.get_value(),
            x0: f64::from(e.client_x()),
            y0: f64::from(e.client_y()),
            spots: spots_except(&id.get_value()),
            carried: false,
            target: None,
        }));
    };

    let on_move = move |e: ev::PointerEvent| {
        carry.update_value(|c| {
            let Some(c) = c.as_mut().filter(|c| c.pointer == e.pointer_id()) else {
                return;
            };
            let (x, y) = (f64::from(e.client_x()), f64::from(e.client_y()));
            if !c.carried && !drags(c.x0, c.y0, x, y) {
                return;
            }
            c.carried = true;
            if let Some(el) = block.get_untracked() {
                let _ = el.class_list().add_1("carried");
                let _ = el.style().set_property(
                    "transform",
                    &format!("translate({}px, {}px)", x - c.x0, y - c.y0),
                );
            }
            clear_marks(&c.spots);
            let target = target_at(&c.spots, x, y);
            mark(target);
            c.target = target.map(|(s, k)| (s.id.clone(), k));
        });
    };

    let end = move |e: ev::PointerEvent, drop: bool| {
        let Some(c) = carry
            .try_update_value(|c| c.take_if(|c| c.pointer == e.pointer_id()))
            .flatten()
        else {
            return;
        };
        clear_marks(&c.spots);
        if let Some(el) = block.get_untracked() {
            let _ = el.class_list().remove_1("carried");
            let _ = el.style().remove_property("transform");
        }
        if !(drop && c.carried) {
            return;
        }
        if let Some((target, kind)) = c.target {
            store.arrange.update(|a| {
                if let Some(edit) = a.edit_mut() {
                    let moved = edit.dropped(&c.id, &target, kind);
                    leptos::logging::log!(
                        "[arrange] {} onto {target} ({kind:?}): moved {moved}",
                        c.id
                    );
                }
            });
        }
    };

    let on_eye = move |_| {
        store.arrange.update(|a| {
            if let Some(edit) = a.edit_mut() {
                edit.toggle_hidden(&id.get_value());
            }
        });
    };

    view! {
        <div
            class="cover"
            data-role="cover"
            on:pointerdown=on_down
            on:pointermove=on_move
            on:pointerup=move |e| end(e, true)
            on:pointercancel=move |e| end(e, false)
            on:lostpointercapture=move |e| end(e, false)
        >
            <span class="cnum">{move || crate::behave::block::number_text(&id.get_value())}</span>
            <div class="grip">"⠿"<small>"ŤAHAJ"</small></div>
            <button
                type="button"
                class="eye"
                data-role="eye"
                title=move || if ghost.get() { "Ukázať kameru" } else { "Skryť kameru" }
                aria-pressed=move || ghost.get().to_string()
                aria-label=move || format!("{} {}", if ghost.get() { "Ukázať" } else { "Skryť" }, id.get_value())
                on:pointerdown=|e: ev::PointerEvent| e.stop_propagation()
                on:click=on_eye
                inner_html=EYE_SVG
            ></button>
            <span class="hid-tag">"SKRYTÁ · ťukni na oko"</span>
        </div>
    }
}

/// A hide of the page ends ARRANGE unsaved (fohmixer's rule): an edit is never left half-done
/// behind another app.
pub fn end_on_page_hide(store: Store) {
    let handle = window_event_listener_untyped("visibilitychange", move |_| {
        let hidden = web_sys::window()
            .and_then(|w| w.document())
            .is_some_and(|d| d.visibility_state() == web_sys::VisibilityState::Hidden);
        if hidden
            && store
                .arrange
                .try_update(|a| a.hidden_page())
                .unwrap_or(false)
        {
            leptos::logging::log!("[arrange] page hidden: the edit ends unsaved");
            store.show_note(Note {
                text: NOT_SAVED_NOTE,
                tone: Tone::Refused,
            });
        }
    });
    on_cleanup(move || handle.remove());
}
