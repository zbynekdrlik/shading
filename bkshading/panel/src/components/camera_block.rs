//! One camera card (mockup theme-v4b, shading#24): the NDI preview on top, then the number, name and
//! online state on one centre, the handheld's one-line registration, the controls (the #14 warning
//! as an overlay over them, the EXP/SYS tablist, both panes in one grid cell), and the fps-vs-grab
//! line; in ARRANGE a cover with the grip and the eye over all of it. Every line is always present
//! and one line high, so no push, tally or pane switch moves anything (no layout shift).

use bkshading_proto::wire::CameraView;
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::behave::block::{
    fps_sync, is_online, number_text, online_text, registration_short, registration_title,
};
use crate::behave::steppers::{Pane, Row, RowView, Steppers};
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
                let now = net::now_ms();
                if let Some(v) = steppers.try_update_value(|s| s.view(&cam, now)) {
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
    // A new view of the camera and the clock tick (a hold that timed out shows the truth) redraw
    // the steppers;
    // `rows` changes only when a row does.
    Effect::new(move |_| {
        let now = store.now.get();
        if let Some(c) = cam.get() {
            if let Some(v) = steppers.try_update_value(|s| s.view(&c, now)) {
                set_rows(rows, v);
            }
        }
    });

    let press = move |row: Row, dir: i32| {
        let Some(c) = cam.try_get_untracked().flatten() else {
            return;
        };
        let now = net::now_ms();
        let Some(p) = steppers
            .try_update_value(|s| s.press(row, dir, &c, now))
            .flatten()
        else {
            return;
        };
        if let Some(v) = steppers.try_update_value(|s| s.view(&c, now)) {
            set_rows(rows, v);
        }
        send(store, steppers, rows, c.id.clone(), p.key, p.value);
    };

    // The slider of `row` released at `raw`: the write (held like a tap), or false when nothing
    // was written (the thumb goes back).
    let slide = move |row: Row, raw: f64| -> bool {
        let Some(c) = cam.try_get_untracked().flatten() else {
            return false;
        };
        let now = net::now_ms();
        let Some(p) = steppers
            .try_update_value(|s| s.slide(row, raw, &c, now))
            .flatten()
        else {
            leptos::logging::log!("[slider] {} release at {raw}: nothing to write", row.role());
            return false;
        };
        if let Some(v) = steppers.try_update_value(|s| s.view(&c, now)) {
            set_rows(rows, v);
        }
        leptos::logging::log!(
            "[slider] {} release at {raw}: {}={}",
            row.role(),
            p.key,
            p.value
        );
        send(store, steppers, rows, c.id.clone(), p.key, p.value);
        true
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
                .map(|r| (registration_short(r), registration_title(r), r.fresh))
        })
    };
    let sync = move || cam.with(|c| c.as_ref().and_then(fps_sync));
    let has_preview = move || cam.with(|c| c.as_ref().is_some_and(|c| c.has_preview));
    let live = Signal::derive(move || {
        cam.with(|c| c.as_ref().is_some_and(|c| c.has_preview && c.preview_live))
    });

    let row_view = move |i: usize| Signal::derive(move || rows.with(|r| r.get(i).cloned()));
    let pane = RwSignal::new(Pane::default());
    let stepper = move |i: usize, row: Row| {
        view! {
            <StepperRow
                row=row
                view=row_view(i)
                on_press=Callback::new(move |dir: i32| press(row, dir))
                on_slide=Callback::new(move |raw: f64| slide(row, raw))
            />
        }
    };
    let pane_rows = move |p: Pane| {
        Row::ALL
            .iter()
            .enumerate()
            .filter(|(_, r)| r.pane() == p)
            .map(|(i, &row)| stepper(i, row))
            .collect_view()
    };
    let tabs = Pane::ALL
        .iter()
        .map(|&p| {
            view! {
                <button
                    type="button"
                    role="tab"
                    data-pane=p.word()
                    data-role=format!("pane-{}", p.word())
                    aria-selected=move || (pane.get() == p).to_string()
                    title=p.title()
                    on:click=move |_| pane.set(p)
                >
                    {p.label()}
                </button>
            }
        })
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
            <div class="head">
                <span class="num">{move || number_text(&id.get_value())}</span>
                <span class="name" data-role="label">{label}</span>
                <span class="state" class:off=move || !online() data-role="online">
                    {move || cam.with(|c| c.as_ref().map_or("", online_text))}
                </span>
            </div>
            // Always one line, present in every card (no layout shift); selectable (the address
            // is copied); the full sentence is the tooltip.
            <div class="reg" title=move || reg().map(|r| r.1).unwrap_or_default()>
                <span
                    class="ago"
                    class:stale=move || reg().is_some_and(|r| !r.2)
                    data-role="registration"
                    hidden=move || reg().is_none()
                >
                    {move || reg().map(|r| r.0).unwrap_or_default()}
                </span>
                "\u{a0}"
            </div>
            <div class="rows">
                // shading#14: an overlay over the controls (never in their flow), never dimmed.
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
                <div class="pane-pill seg" role="tablist" aria-label=move || format!("{}: ovládače", label())>
                    {tabs}
                </div>
                // Both panes share one grid cell: the card is as tall as the taller pane, always.
                <div class="panes" data-show=move || pane.get().word() data-role="params">
                    <div class="pane" data-pane="exp" role="tabpanel">{pane_rows(Pane::Exp)}</div>
                    <div class="pane" data-pane="sys" role="tabpanel">
                        {pane_rows(Pane::Sys)}
                        <button
                            type="button"
                            class="wb"
                            data-role="auto-wb"
                            on:click=move |_| send(store, steppers, rows, id.get_value(), "autoWb", serde_json::json!(true))
                        >
                            "Auto WB"
                        </button>
                    </div>
                </div>
            </div>
            // The fps-vs-grab line: always present and one line high (no layout shift).
            <div class="foot">
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
            </div>
            <Cover id=id.get_value() block=root ghost=Signal::from(ghost) />
        </section>
    }
}
