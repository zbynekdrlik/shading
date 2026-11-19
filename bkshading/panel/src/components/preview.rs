//! A block's live NDI preview (issue 808, ported from the JS panel's loader; the decisions are in
//! `behave::preview`). Each tick ([`PREVIEW_FPS`] a second) a block whose feed the service reports
//! live, while the panel has contact, fetches ONE frame: `fetch(no-store)` -> blob -> object URL,
//! one request in flight, aborted after [`FETCH_TIMEOUT_MS`] or when the block drops to its
//! placeholder. A 204 (no fresh frame), a failed fetch, or a push that turns the feed off shows the
//! placeholder with the `<img>` source REMOVED (no load/error event, no frozen frame) and the
//! object URL revoked. A page nobody sees (`document.visibilityState` hidden) fetches nothing and
//! shows the placeholder (shading#19).

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use crate::behave::preview::{
    keeps_frame, placeholder_text, tick_action, Tick, FETCH_TIMEOUT_MS, PREVIEW_FPS,
};
use crate::store::{net, Store};

/// The preview's state.
#[derive(Clone, Copy)]
struct Frame {
    url: RwSignal<Option<String>>,
    ready: RwSignal<bool>,
    seen: RwSignal<bool>,
    /// When the frame on screen was painted (`performance.now()`), none without one.
    shown_at: StoredValue<Option<f64>>,
    abort: StoredValue<Option<web_sys::AbortController>, LocalStorage>,
}

// Every access below is a `try_` one: a fetch can finish after its block was removed (an ARRANGE
// hide, a push without the camera), when the block's signals are already disposed; a plain access
// would panic the whole app there. A disposed frame just does nothing.
impl Frame {
    /// Back to the placeholder: abort a fetch in flight, drop and release the frame.
    fn placeholder(self) {
        if let Some(ctrl) = self.abort.try_update_value(Option::take).flatten() {
            ctrl.abort();
        }
        if self.ready.try_get_untracked() == Some(true) {
            self.ready.try_set(false);
        }
        if let Some(old) = self.url.try_get_untracked().flatten() {
            self.url.try_set(None);
            let _ = web_sys::Url::revoke_object_url(&old);
        }
        self.shown_at.try_set_value(None);
    }

    /// A new frame's object URL replaces the shown one (released at once); a disposed frame
    /// releases the new one instead.
    fn show(self, url: String) {
        let Some(old) = self.url.try_get_untracked() else {
            let _ = web_sys::Url::revoke_object_url(&url);
            return;
        };
        self.url.try_set(Some(url));
        if let Some(old) = old {
            let _ = web_sys::Url::revoke_object_url(&old);
        }
    }

    /// The block is going away: abort a fetch in flight and release the object URL, WITHOUT
    /// writing a signal. A write here would schedule this view's closures to re-run after the
    /// block's signals are disposed, and they would panic reading them (PP 18.11.2026, shading#16).
    fn release(self, img: NodeRef<html::Img>) {
        // No source on the removed element: an image still decoding would fire `load` on a
        // listener Leptos already dropped.
        if let Some(Some(el)) = img.try_get_untracked() {
            let _ = el.remove_attribute("src");
        }
        if let Some(ctrl) = self.abort.try_update_value(Option::take).flatten() {
            ctrl.abort();
        }
        if let Some(Some(old)) = self.url.try_get_untracked() {
            let _ = web_sys::Url::revoke_object_url(&old);
        }
    }

    /// The `<img>` painted a frame. Its `load` event can land after the block was removed (an
    /// ARRANGE save hid the camera while a frame was decoding: PP 18.11.2026, shading#16), so this
    /// is `try_` too.
    fn loaded(self) {
        self.ready.try_set(true);
        self.seen.try_set(true);
        self.shown_at.try_set_value(Some(net::perf_now()));
    }
}

/// Fetches one frame of camera `id`.
async fn load(id: String, frame: Frame, live: Signal<bool>, busy: RwSignal<bool>, store: Store) {
    let Ok(ctrl) = web_sys::AbortController::new() else {
        return;
    };
    let signal = ctrl.signal();
    frame.abort.try_set_value(Some(ctrl.clone()));
    busy.try_set(true);
    let timed_out = Rc::new(Cell::new(false));
    let timer = {
        let timed_out = timed_out.clone();
        let ctrl = ctrl.clone();
        set_timeout_with_handle(
            move || {
                timed_out.set(true);
                ctrl.abort();
            },
            Duration::from_millis(FETCH_TIMEOUT_MS as u64),
        )
        .ok()
    };
    let url = format!(
        "/api/cameras/{}/preview.jpg",
        js_sys::encode_uri_component(&id)
    );
    let outcome: Result<Option<web_sys::Blob>, String> = async {
        let resp = net::fetch("GET", &url, None, Some(&signal)).await?;
        if resp.status() != 200 {
            return Ok(None); // 204: no fresh frame
        }
        let blob = JsFuture::from(resp.blob().map_err(|e| format!("{e:?}"))?)
            .await
            .map_err(|e| format!("{e:?}"))?;
        Ok(blob.dyn_into::<web_sys::Blob>().ok())
    }
    .await;
    // After the await the block may be gone (see `impl Frame`): from here on only `try_` reads.
    let live_now = live.try_get_untracked().unwrap_or(false);
    match outcome {
        Ok(Some(blob)) if !signal.aborted() && live_now && store.connected_untracked() => {
            if let Ok(obj) = web_sys::Url::create_object_url_with_blob(&blob) {
                frame.show(obj);
            }
        }
        Ok(_) => frame.placeholder(),
        Err(_) => {
            let keep = keeps_frame(
                timed_out.get(),
                frame.shown_at.try_get_value().flatten(),
                net::perf_now(),
                live_now,
                store.connected_untracked(),
            );
            if !keep {
                frame.placeholder();
            }
        }
    }
    if let Some(t) = timer {
        t.clear();
    }
    // This fetch is over: free the slot (unless a newer one took it meanwhile).
    let mine = frame
        .abort
        .try_with_value(|a| a.as_ref().is_some_and(|c| c == &ctrl))
        .unwrap_or(false);
    if mine {
        frame.abort.try_set_value(None);
    }
    busy.try_set(false);
}

/// A block's preview area.
#[component]
pub fn Preview(id: String, live: Signal<bool>, busy: RwSignal<bool>) -> impl IntoView {
    let store = expect_context::<Store>();
    let img = NodeRef::<html::Img>::new();
    let frame = Frame {
        url: RwSignal::new(None),
        ready: RwSignal::new(false),
        seen: RwSignal::new(false),
        shown_at: StoredValue::new(None),
        abort: StoredValue::new_local(None),
    };

    // A push that turns the feed off drops the frame at once.
    Effect::new(move |_| {
        if !live.get() {
            frame.placeholder();
        }
    });

    let tick_id = id.clone();
    let tick = move || {
        // An interval tick can still run while the block is being removed: `try_` reads.
        match tick_action(
            live.try_get_untracked().unwrap_or(false),
            store.connected_untracked(),
            net::page_visible(),
            busy.try_get_untracked().unwrap_or(true),
        ) {
            Tick::Fetch => spawn_local(load(tick_id.clone(), frame, live, busy, store)),
            Tick::Wait => {}
            Tick::Placeholder => frame.placeholder(),
        }
    };
    if let Ok(handle) =
        set_interval_with_handle(tick, Duration::from_millis(u64::from(1000 / PREVIEW_FPS)))
    {
        on_cleanup(move || {
            handle.clear();
            frame.release(img);
        });
    }

    let tag = format!("NDI · {}", id.to_uppercase());
    view! {
        <div class="prev" data-role="preview">
            <img
                node_ref=img
                class="preview-img"
                class:ready=move || frame.ready.try_get().unwrap_or(false)
                data-role="preview-img"
                alt="Živý NDI preview kamery"
                src=move || frame.url.try_get().flatten()
                on:load=move |_| frame.loaded()
                on:error=move |_| frame.placeholder()
            />
            // The view's closures read with `try_`: one can still run while the block is removed.
            <div class="none" data-role="preview-placeholder" hidden=move || frame.ready.try_get().unwrap_or(false)>
                {move || placeholder_text(frame.seen.try_get().unwrap_or(false))}
            </div>
            <span class="tag" hidden=move || !frame.ready.try_get().unwrap_or(false)>{tag}</span>
        </div>
    }
}
