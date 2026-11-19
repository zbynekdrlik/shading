//! The browser IO that feeds the [`Store`] (thin wrappers; the decisions are in `conn` and
//! `behave`): the `/ws` push with its reconnect, the HTTP fallback poll while the socket is down,
//! the clock tick, and the writes (`PUT /api/cameras/<id>/params`, `PUT /api/layout`). Writes go
//! over HTTP, never the socket (it works through the Cloudflare remote); the socket is push-only.
//!
//! Every path is relative to the service root (`/api/...`, `/ws`), wherever the panel is served.

use std::cell::RefCell;
use std::time::Duration;

use bkshading_proto::layout::{LayoutDoc, LayoutPut};
use bkshading_proto::wire::{Aggregate, ServerMsg};
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use super::{conn, Store};
use crate::behave::arrange_drag::{save_answer, SaveFail};

/// `Date.now()`: wall-clock ms (the connection age).
pub fn now_ms() -> f64 {
    js_sys::Date::now()
}

/// `performance.now()`: monotonic ms (a preview frame's age; immune to a clock step).
pub fn perf_now() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map_or_else(now_ms, |p| p.now())
}

/// Whether the page can be seen (`document.visibilityState` is not `hidden`). On strih the kiosk
/// app hides its view when OBS covers the window, since WebKitGTK alone reports an obscured
/// window as visible (shading#19).
pub fn page_visible() -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .is_none_or(|d| d.visibility_state() != web_sys::VisibilityState::Hidden)
}

/// Tells the service a visible panel watches the previews (`POST /api/preview/viewer`,
/// shading#19), so it keeps the NDI receivers connected. A hidden page says nothing.
fn announce_viewer() {
    if !page_visible() {
        return;
    }
    spawn_local(async {
        if let Err(e) = fetch("POST", "/api/preview/viewer", None, None).await {
            leptos::logging::log!("[preview] viewer call failed: {e}");
        }
    });
}

/// An answer of the service: its status and body text.
pub struct Answer {
    pub status: u16,
    pub text: String,
    /// A failed write's `x-bkshading-reason` header (shading#17), if any.
    pub reason: Option<String>,
}

/// One HTTP request (`no-store`); `Err` when it never got an answer (no network, aborted).
pub async fn fetch_text(
    method: &str,
    url: &str,
    json_body: Option<String>,
    signal: Option<&web_sys::AbortSignal>,
) -> Result<Answer, String> {
    let resp = fetch(method, url, json_body, signal).await?;
    let status = resp.status();
    let reason = resp
        .headers()
        .get(bkshading_proto::wire::NOT_APPLIED_REASON_HEADER)
        .ok()
        .flatten();
    let text = JsFuture::from(resp.text().map_err(js_err)?)
        .await
        .map_err(js_err)?
        .as_string()
        .unwrap_or_default();
    Ok(Answer {
        status,
        text,
        reason,
    })
}

/// One HTTP request; the raw response.
pub async fn fetch(
    method: &str,
    url: &str,
    json_body: Option<String>,
    signal: Option<&web_sys::AbortSignal>,
) -> Result<web_sys::Response, String> {
    let window = web_sys::window().ok_or("no window")?;
    let init = web_sys::RequestInit::new();
    init.set_method(method);
    init.set_cache(web_sys::RequestCache::NoStore);
    if let Some(body) = &json_body {
        init.set_body(&JsValue::from_str(body));
        let headers = web_sys::Headers::new().map_err(js_err)?;
        headers
            .set("Content-Type", "application/json")
            .map_err(js_err)?;
        init.set_headers(&headers);
    }
    init.set_signal(signal);
    let req = web_sys::Request::new_with_str_and_init(url, &init).map_err(js_err)?;
    let resp = JsFuture::from(window.fetch_with_request(&req))
        .await
        .map_err(js_err)?;
    resp.dyn_into::<web_sys::Response>().map_err(js_err)
}

fn js_err(e: JsValue) -> String {
    e.as_string()
        .or_else(|| {
            e.dyn_ref::<js_sys::Error>()
                .map(|e| String::from(e.message()))
        })
        .unwrap_or_else(|| format!("{e:?}"))
}

/// A tap's write: `PUT /api/cameras/<id>/params` with one key. The HTTP status of the answer and
/// its `x-bkshading-reason` header (shading#17), or `Err` when the request never reached the
/// service; the caller hands the status to `Steppers::answered` (a 2xx unlocks the row, anything
/// else reverts it, shading#29) and shows the reason.
pub async fn put_param(
    id: &str,
    key: &str,
    value: serde_json::Value,
) -> Result<(u16, Option<String>), String> {
    let url = format!("/api/cameras/{}/params", js_sys::encode_uri_component(id));
    let body = serde_json::json!({ key: value }).to_string();
    let started = now_ms();
    match fetch_text("PUT", &url, Some(body.clone()), None).await {
        Ok(a) => {
            leptos::logging::log!(
                "[write] {id} {body} -> {} in {:.0} ms{}",
                a.status,
                now_ms() - started,
                a.reason
                    .as_deref()
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default()
            );
            Ok((a.status, a.reason))
        }
        Err(e) => {
            leptos::logging::log!("[write] {id} {body} failed: {e}");
            Err(e)
        }
    }
}

/// How long ARRANGE's save may take before it counts as not having reached the service (a hung
/// request would otherwise keep the mode saving, its covers up, until a reload).
pub const SAVE_TIMEOUT_MS: u64 = 10_000;

/// ARRANGE's save: `PUT /api/layout`, aborted after [`SAVE_TIMEOUT_MS`]. The new revision, or why
/// not.
pub async fn put_layout(put: &LayoutPut) -> Result<u64, SaveFail> {
    let body = serde_json::to_string(put).map_err(|_| SaveFail::Other)?;
    let ctrl = web_sys::AbortController::new().map_err(|_| SaveFail::Other)?;
    let timer = {
        let ctrl = ctrl.clone();
        set_timeout_with_handle(move || ctrl.abort(), Duration::from_millis(SAVE_TIMEOUT_MS)).ok()
    };
    let sent = fetch_text("PUT", "/api/layout", Some(body), Some(&ctrl.signal())).await;
    if let Some(t) = timer {
        t.clear();
    }
    let answer = sent.map_err(|e| {
        leptos::logging::log!("[layout] save did not reach the service: {e}");
        SaveFail::Offline
    })?;
    leptos::logging::log!(
        "[layout] save rev {} -> {} {}",
        put.rev,
        answer.status,
        answer.text
    );
    save_answer(answer.status, &answer.text)
}

/// The open socket and its handlers (dropped with it).
struct Socket {
    ws: web_sys::WebSocket,
    _on_open: Closure<dyn FnMut()>,
    _on_message: Closure<dyn FnMut(web_sys::MessageEvent)>,
    _on_close: Closure<dyn FnMut()>,
    _on_error: Closure<dyn FnMut()>,
}

thread_local! {
    static SOCKET: RefCell<Option<Socket>> = const { RefCell::new(None) };
    static ATTEMPT: RefCell<u32> = const { RefCell::new(0) };
}

/// A message text from the service: applied, or logged (a bad message never stops the panel).
pub fn apply_text(store: Store, text: &str) {
    match serde_json::from_str::<ServerMsg>(text) {
        Ok(msg) => store.apply(msg, now_ms()),
        Err(e) => leptos::logging::warn!("[ws] not a service message ({e}): {text:.200}"),
    }
}

/// Opens the push socket; on close it reconnects after [`conn::reconnect_delay`].
fn connect(store: Store) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let loc = window.location();
    let url = conn::ws_url(
        &loc.protocol().unwrap_or_default(),
        &loc.host().unwrap_or_default(),
    );
    let ws = match web_sys::WebSocket::new(&url) {
        Ok(ws) => ws,
        Err(e) => {
            leptos::logging::log!("[ws] cannot open {url}: {}", js_err(e));
            schedule_reconnect(store);
            return;
        }
    };
    let on_open = Closure::<dyn FnMut()>::new(move || {
        ATTEMPT.with(|a| *a.borrow_mut() = 0);
        store.ws_open.set(true);
        store.contact(now_ms());
        leptos::logging::log!("[ws] open");
    });
    let on_message = Closure::<dyn FnMut(_)>::new(move |ev: web_sys::MessageEvent| {
        if let Some(text) = ev.data().as_string() {
            apply_text(store, &text);
        }
    });
    let on_close = Closure::<dyn FnMut()>::new(move || {
        store.ws_open.set(false);
        leptos::logging::log!("[ws] closed");
        schedule_reconnect(store);
    });
    // An error is always followed by close, which reconnects. No console warning: a socket error
    // while the service is unreachable is the expected offline path (the banner is the signal).
    let on_error = Closure::<dyn FnMut()>::new(|| {});
    ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
    ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    let fresh = Socket {
        ws,
        _on_open: on_open,
        _on_message: on_message,
        _on_close: on_close,
        _on_error: on_error,
    };
    // The previous socket is closed by now (a reconnect follows its close): detach and drop it.
    if let Some(old) = SOCKET.with(|s| s.borrow_mut().replace(fresh)) {
        old.ws.set_onopen(None);
        old.ws.set_onmessage(None);
        old.ws.set_onclose(None);
        old.ws.set_onerror(None);
    }
}

fn schedule_reconnect(store: Store) {
    let attempt = ATTEMPT.with(|a| {
        let n = *a.borrow();
        *a.borrow_mut() = n.saturating_add(1);
        n
    });
    let wait = conn::reconnect_delay(attempt);
    set_timeout(
        move || connect(store),
        Duration::from_millis(u64::from(wait)),
    );
}

/// The fallback poll (only while the socket is down): the aggregate and the layout over HTTP. An
/// answer that arrives once the socket is open again is dropped: the socket's pushes are newer.
async fn poll(store: Store) {
    let cams = fetch_text("GET", "/api/cameras", None, None).await;
    let agg = cams
        .ok()
        .filter(|a| a.status == 200)
        .and_then(|a| serde_json::from_str::<Aggregate>(&a.text).ok());
    let Some(agg) = agg else {
        return; // no contact: the banner shows once the last contact is stale
    };
    if store.ws_open.get_untracked() {
        return;
    }
    store.apply(ServerMsg::State(agg), now_ms());
    if let Ok(a) = fetch_text("GET", "/api/layout", None, None).await {
        if let Ok(doc) = serde_json::from_str::<LayoutDoc>(&a.text) {
            if !store.ws_open.get_untracked() {
                store.apply_layout(doc);
            }
        }
    }
}

/// Exposes `window.shadingPush(msg)`: feeds one service message (`{type: "state", ...}` or
/// `{type: "layout", ...}`) to the panel exactly as a `/ws` push would. The E2E drives fixtures
/// through it; a person can replay a captured push from the console.
fn install_push_hook(store: Store) {
    let hook =
        Closure::<dyn Fn(JsValue)>::new(move |msg: JsValue| {
            match js_sys::JSON::stringify(&msg)
                .ok()
                .and_then(|s| s.as_string())
            {
                Some(text) => apply_text(store, &text),
                None => leptos::logging::warn!("[hook] shadingPush needs a JSON object"),
            }
        });
    if let Some(window) = web_sys::window() {
        let _ = js_sys::Reflect::set(&window, &"shadingPush".into(), hook.as_ref());
    }
    hook.forget(); // one per page, lives as long as the page
}

/// Starts the connection: a first poll for an immediate paint, the socket, the fallback poll, the
/// clock tick, and the test hook.
pub fn start(store: Store) {
    install_push_hook(store);
    spawn_local(poll(store));
    connect(store);
    set_interval(
        move || {
            if !store.ws_open.get_untracked() {
                spawn_local(poll(store));
            }
        },
        Duration::from_millis(u64::from(conn::POLL_MS)),
    );
    set_interval(
        move || store.now.set(now_ms()),
        Duration::from_millis(u64::from(conn::TICK_MS)),
    );
    // shading#19: the preview receivers run only while a visible panel says so; a page that
    // becomes visible says it at once (the first picture follows the connect).
    announce_viewer();
    set_interval(
        announce_viewer,
        Duration::from_millis(u64::from(crate::behave::preview::VIEWER_MS)),
    );
    if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
        let on_change = Closure::<dyn Fn()>::new(announce_viewer);
        let _ = doc.add_event_listener_with_callback(
            "visibilitychange",
            on_change.as_ref().unchecked_ref(),
        );
        on_change.forget(); // one per page, lives as long as the page
    }
}
