//! One camera block (mockup arrange-v1): the NDI preview on top, then the number, label and online
//! line, the #14 warning, the handheld's registration line, the six steppers, Auto WB and the
//! fps-vs-grab line; in ARRANGE a cover with the grip and the eye over all of it.

use bkshading_proto::wire::CameraView;
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::behave::block::{fps_sync, is_online, number_text, online_text, registration_text};
use crate::behave::steppers::{Row, RowView, Steppers};
use crate::components::arrange::Cover;
use crate::components::control_info::warning;
use crate::components::preview::Preview;
use crate::components::stepper_row::StepperRow;
use crate::store::{net, Store};

/// The kebab-case wire word of a camera's control status (`data-control`).
fn control_word(cam: &CameraView) -> String {
    serde_json::to_value(cam.control)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Sets the rows only when one changed (the clock ticks twice a second).
fn set_rows(rows: RwSignal<Vec<RowView>>, v: Vec<RowView>) {
    if rows.try_with_untracked(|r| *r != v).unwrap_or(false) {
        rows.set(v);
    }
}

/// Sends one write and drops its hold when it never reached the service.
fn send(
    store: Store,
    steppers: StoredValue<Steppers>,
    rows: RwSignal<Vec<RowView>>,
    id: String,
    key: &'static str,
    value: serde_json::Value,
) {
    spawn_local(async move {
        if net::put_param(&id, key, value).await.is_err() {
            steppers.update_value(|s| s.write_failed(key));
            if let Some(cam) = store.camera(&id) {
                let (seq, now) = (store.push_seq.get_untracked(), net::now_ms());
                if let Some(v) = steppers.try_update_value(|s| s.view(&cam, seq, now)) {
                    set_rows(rows, v);
                }
            }
        }
    });
}

/// A camera block, keyed by camera id.
#[component]
pub fn CameraBlock(id: String) -> impl IntoView {
    let store = expect_context::<Store>();
    let root = NodeRef::<html::Section>::new();
    let id = StoredValue::new(id);
    let cam = Memo::new(move |_| store.camera(&id.get_value()));
    let steppers = StoredValue::new(Steppers::default());
    let rows = RwSignal::new(Vec::<RowView>::new());
    let preview_busy = RwSignal::new(false);

    // Each push (and the clock, so a hold that timed out shows the truth) redraws the steppers.
    // Each push and the clock tick (a hold that timed out shows the truth) redraw the steppers;
    // `rows` changes only when a row does.
    Effect::new(move |_| {
        let now = store.now.get();
        let seq = store.push_seq.get();
        if let Some(c) = cam.get() {
            if let Some(v) = steppers.try_update_value(|s| s.view(&c, seq, now)) {
                set_rows(rows, v);
            }
        }
    });

    let press = move |row: Row, dir: i32| {
        let Some(c) = cam.get_untracked() else {
            return;
        };
        let now = net::now_ms();
        let Some(p) = steppers
            .try_update_value(|s| s.press(row, dir, &c, store.push_seq.get_untracked(), now))
            .flatten()
        else {
            return;
        };
        let seq = store.push_seq.get_untracked();
        if let Some(v) = steppers.try_update_value(|s| s.view(&c, seq, now)) {
            set_rows(rows, v);
        }
        send(store, steppers, rows, c.id.clone(), p.key, p.value);
    };

    // Whether this block is a hidden camera's dashed place in ARRANGE: only while the mode is on.
    let ghost = Memo::new(move |_| store.arrange.with(|a| a.hidden_in_edit(&id.get_value())));
    let online = move || cam.with(|c| c.as_ref().is_some_and(is_online));
    let control = move || cam.with(|c| c.as_ref().map(control_word).unwrap_or_default());
    let label = move || cam.with(|c| c.as_ref().map(|c| c.label.clone()).unwrap_or_default());
    let badge = move || cam.with(|c| c.as_ref().and_then(|c| warning(c.control)));
    let reg = move || {
        cam.with(|c| {
            c.as_ref()
                .and_then(|c| c.registration.as_ref())
                .map(|r| (registration_text(r), r.fresh))
        })
    };
    let sync = move || cam.with(|c| c.as_ref().and_then(fps_sync));
    let has_preview = move || cam.with(|c| c.as_ref().is_some_and(|c| c.has_preview));
    let live = Signal::derive(move || {
        cam.with(|c| c.as_ref().is_some_and(|c| c.has_preview && c.preview_live))
    });

    let row_view = move |i: usize| Signal::derive(move || rows.with(|r| r.get(i).cloned()));
    let steppers_view = Row::ALL
        .iter()
        .enumerate()
        .map(|(i, &row)| view! { <StepperRow row=row view=row_view(i) on_press=Callback::new(move |dir: i32| press(row, dir)) /> })
        .collect_view();

    view! {
        <section
            node_ref=root
            class="cam"
            class:ghost=move || ghost.get()
            class:disabled=move || !online()
            data-role="camera-block"
            data-id=move || id.get_value()
            data-control=control
            data-preview=move || if has_preview() { "1" } else { "0" }
            data-preview-live=move || if live.get() { "1" } else { "0" }
            data-preview-busy=move || if preview_busy.get() { "1" } else { "0" }
        >
            <Show
                when=has_preview
                fallback=|| view! { <div class="prev" data-role="no-preview"><div class="none">{crate::behave::preview::NO_PREVIEW_TEXT}</div></div> }
            >
                <Preview id=id.get_value() live=live busy=preview_busy />
            </Show>
            <div class="body">
                <div class="bhead">
                    <span class="num">{move || number_text(&id.get_value())}</span>
                    <span class="lbl" data-role="label">{label}</span>
                    <span class="on-dot" class:bad=move || !online() data-role="online">
                        {move || cam.with(|c| c.as_ref().map_or("", online_text))}
                    </span>
                </div>
                <button
                    type="button"
                    class="warn"
                    data-role="control-warn"
                    aria-haspopup="dialog"
                    title="Čo treba spraviť?"
                    hidden=move || badge().is_none()
                    on:click=move |_| store.info_for.set(Some(id.get_value()))
                >
                    {move || badge().unwrap_or_default()}
                </button>
                <div
                    class="reg"
                    class:stale=move || reg().is_some_and(|r| !r.1)
                    data-role="registration"
                    hidden=move || reg().is_none()
                >
                    {move || reg().map(|r| r.0).unwrap_or_default()}
                </div>
                <div class="ctrls" data-role="params">
                    {steppers_view}
                    <div class="extras">
                        <div class="fps-sync" data-role="fps-sync" hidden=move || sync().is_none()>
                            <span class="fps-grab" data-role="fps-grab">
                                {move || sync().map(|s| s.grab).unwrap_or_default()}
                            </span>
                            <span class="fps-desync" data-role="fps-desync" hidden=move || sync().and_then(|s| s.desync).is_none()>
                                {move || sync().and_then(|s| s.desync).unwrap_or_default()}
                            </span>
                            <span class="fps-warn" data-role="fps-warn" hidden=move || sync().and_then(|s| s.warn).is_none()>
                                {move || sync().and_then(|s| s.warn).unwrap_or_default()}
                            </span>
                            <button
                                type="button"
                                class="fps-set-grab"
                                data-role="fps-set-grab"
                                hidden=move || sync().and_then(|s| s.align).is_none()
                                on:click=move |_| {
                                    // issue 809: an explicit click only, never an automatic write.
                                    if let Some((_, grab)) = sync().and_then(|s| s.align) {
                                        send(store, steppers, rows, id.get_value(), "fps", serde_json::json!(grab));
                                    }
                                }
                            >
                                {move || sync().and_then(|s| s.align).map(|a| a.0).unwrap_or_default()}
                            </button>
                        </div>
                        <button
                            type="button"
                            class="autowb"
                            data-role="auto-wb"
                            on:click=move |_| send(store, steppers, rows, id.get_value(), "autoWb", serde_json::json!(true))
                        >
                            "Auto WB"
                        </button>
                    </div>
                </div>
            </div>
            <Cover id=id.get_value() block=root ghost=Signal::from(ghost) />
        </section>
    }
}
