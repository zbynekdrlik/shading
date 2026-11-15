//! shading#14's popup: ONE `<dialog>` for every block, filled from `behave::control_info` for the
//! block whose warning was tapped. An open popup follows the push for its camera: refilled with
//! the new cause, closed once the camera can be controlled again.

use bkshading_proto::wire::ControlStatus;
use leptos::html;
use leptos::prelude::*;

use crate::behave::control_info::{cause_text, info};
use crate::store::Store;

/// The popup.
#[component]
pub fn ControlInfo() -> impl IntoView {
    let store = expect_context::<Store>();
    let dialog = NodeRef::<html::Dialog>::new();

    // What the popup shows: the camera's label and its status's texts, or none to close it.
    let shown = Memo::new(move |_| {
        let id = store.info_for.get()?;
        let cam = store.camera(&id)?;
        let i = info(cam.control)?;
        let version = cam.state.as_ref().map(|s| s.version.clone());
        Some((
            cam.label.clone(),
            i.title,
            cause_text(&i, version.as_deref()),
            i.step,
        ))
    });

    Effect::new(move |_| {
        let Some(d) = dialog.get() else {
            return;
        };
        match shown.get() {
            Some(_) if !d.open() => {
                if let Err(e) = d.show_modal() {
                    leptos::logging::warn!("[popup] cannot open: {e:?}");
                }
            }
            None if d.open() => d.close(),
            _ => {}
        }
    });

    let text = move |i: usize| {
        move || {
            shown.with(|s| {
                s.as_ref().map_or(String::new(), |s| match i {
                    0 => s.0.clone(),
                    1 => s.1.to_string(),
                    2 => s.2.clone(),
                    _ => s.3.to_string(),
                })
            })
        }
    };

    view! {
        <dialog
            node_ref=dialog
            id="control-info"
            class="info"
            data-role="control-info"
            aria-labelledby="control-info-title"
            on:close=move |_| store.info_for.set(None)
        >
            <h2 id="control-info-title" data-role="control-info-title">{text(1)}</h2>
            <p class="cam-n" data-role="control-info-camera">{text(0)}</p>
            <p data-role="control-info-cause">{text(2)}</p>
            <p class="step-t" data-role="control-info-step">{text(3)}</p>
            <form method="dialog">
                <button type="submit" data-role="control-info-close">"Zavrieť"</button>
            </form>
        </dialog>
    }
}

/// The status a block's warning shows, or none while the camera can be controlled.
pub fn warning(status: ControlStatus) -> Option<&'static str> {
    info(status).map(|i| i.badge)
}
