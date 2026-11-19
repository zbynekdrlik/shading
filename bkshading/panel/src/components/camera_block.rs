//! One camera card (mockup docs/mockups/theme-v4d.html, shading#24): the NDI preview on top, then
//! the number, name and online state on one centre, the handheld's one-line registration, the
//! controls (the #14 warning as an overlay over them, the EXP/SYS tablist, both panes in one grid
//! cell), and the fps-vs-grab line; in ARRANGE a cover with the grip and the eye over all of it.
//! Every line is always present and one line high, so no push, tally or pane switch moves anything
//! (no layout shift).

use bkshading_proto::wire::{CameraView, Tally};
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::behave::arrange_drag::{Note, Tone};
use crate::behave::block::{
    fps_sync, has_tally, is_online, number_text, online_text, status_line, status_title,
    tally_badge,
};
use crate::behave::holds::WriteAnswer;
use crate::behave::refusal::{header_reason, new_refusal_note, reason_text};
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

/// Sets the rows only when one changed (the clock ticks twice a second). A refusal flash that
/// starts here shows its reason in the note (shading#17).
fn set_rows(store: Store, rows: RwSignal<Vec<RowView>>, v: Vec<RowView>) {
    let note = rows
        .try_with_untracked(|r| (*r != v).then(|| new_refusal_note(r, &v)))
        .flatten();
    if let Some(note) = note {
        if let Some(text) = note {
            store.show_note(Note {
                text,
                tone: Tone::Refused,
            });
        }
        rows.set(v);
    }
}

/// Sends write `write` (the id of its hold, 0 for a write no hold owns) and hands the service's
/// answer to the steppers (shading#29: a 2xx unlocks the row, anything else or no answer reverts
/// it with a red flash), then redraws the block.
fn send(
    store: Store,
    steppers: StoredValue<Steppers>,
    rows: RwSignal<Vec<RowView>>,
    id: String,
    key: &'static str,
    value: serde_json::Value,
    write: u64,
) {
    spawn_local(async move {
        let got = net::put_param(&id, key, value).await.ok();
        // shading#17: a failed write that names its reason shows it in the note.
        if let Some((status, reason)) = &got {
            if !(200..300).contains(status) {
                if let Some(r) = header_reason(reason.as_deref()) {
                    store.show_note(Note {
                        text: reason_text(Some(r)),
                        tone: Tone::Refused,
                    });
                }
            }
        }
        let answer = WriteAnswer::of(got.map(|(status, _)| status));
        let now = net::now_ms();
        // The block may be gone by now (hidden in ARRANGE): every access is fallible.
        if steppers
            .try_update_value(|s| s.answered(key, write, answer, now))
            .is_none()
        {
            return;
        }
        if let Some(cam) = store.camera(&id) {
            if let Some(v) = steppers.try_update_value(|s| s.view(&cam, now)) {
                set_rows(store, rows, v);
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
                set_rows(store, rows, v);
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
            set_rows(store, rows, v);
        }
        send(store, steppers, rows, c.id.clone(), p.key, p.value, p.id);
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
            set_rows(store, rows, v);
        }
        leptos::logging::log!(
            "[slider] {} release at {raw}: {}={}",
            row.role(),
            p.key,
            p.value
        );
        send(store, steppers, rows, c.id.clone(), p.key, p.value, p.id);
        true
    };

    // Whether this block is a hidden camera's dashed place in ARRANGE: only while the mode is on.
    let ghost = Memo::new(move |_| store.arrange.with(|a| a.hidden_in_edit(&id.get_value())));
    let online = move || cam.with(|c| c.as_ref().is_some_and(is_online));
    let control = move || cam.with(|c| c.as_ref().map(control_word).unwrap_or_default());
    let label = move || cam.with(|c| c.as_ref().map(|c| c.label.clone()).unwrap_or_default());
    let badge = move || cam.with(|c| c.as_ref().and_then(|c| warning(c.control)));
    // shading#28: (line, tooltip, stale) — the line is never empty.
    let reg = move || {
        cam.with(|c| {
            c.as_ref().map(|c| {
                let stale = c.registration.as_ref().is_some_and(|r| !r.fresh);
                (status_line(c), status_title(c), stale)
            })
        })
    };
    let sync = move || cam.with(|c| c.as_ref().and_then(fps_sync));
    // shading#25: (class, badge text) of the camera's PGM/PVW tally.
    let tally = move || cam.with(|c| c.as_ref().and_then(tally_badge));
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
            class:pgm=move || cam.with(|c| c.as_ref().is_some_and(|c| has_tally(c, Tally::Pgm)))
            class:pvw=move || cam.with(|c| c.as_ref().is_some_and(|c| has_tally(c, Tally::Pvw)))
            data-tally=move || tally().map(|t| t.0).unwrap_or("")
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
                {move || tally().map(|(class, text)| view! {
                    <span class=format!("tally {class}") data-role="tally">{text}</span>
                })}
                <span class="state" class:off=move || !online() data-role="online">
                    {move || cam.with(|c| c.as_ref().map_or("", online_text))}
                </span>
            </div>
            // Always one line of text, present in every card (shading#28: an offline card keeps it,
            // no layout shift); selectable (the address is copied); the full sentence is the tooltip.
            <div class="reg" title=move || reg().map(|r| r.1).unwrap_or_default()>
                <span class="ago" class:stale=move || reg().is_some_and(|r| r.2) data-role="registration">
                    {move || reg().map(|r| r.0).unwrap_or_default()}
                </span>
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
                            on:click=move |_| send(store, steppers, rows, id.get_value(), "autoWb", serde_json::json!(true), 0)
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
                                send(store, steppers, rows, id.get_value(), "fps", serde_json::json!(grab), 0);
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
