//! The header (mockup arrange-v1): title, version, connection, the hidden-cameras chip, the note,
//! `↺ podľa čísel` (in ARRANGE) and the `⇄` key.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::behave::arrange_drag::{Note, Tap, Tone, ARRANGE_NOTE, SAVING_NOTE, UNCHANGED_NOTE};
use crate::store::{net, Store};

/// The chip's text: how many cameras are hidden and how to bring them back.
pub fn hidden_chip_text(n: usize) -> String {
    match n {
        1 => "1 skrytá kamera · vrátiš ju cez ⇄".to_string(),
        2..=4 => format!("{n} skryté kamery · vrátiš ich cez ⇄"),
        _ => format!("{n} skrytých kamier · vrátiš ich cez ⇄"),
    }
}

/// A tap on `⇄`: start the mode, leave it, or send the save and end it with the answer.
pub fn tap_arrange(store: Store) {
    let cams = store
        .cameras
        .with_untracked(|c| c.iter().map(|c| c.id.clone()).collect::<Vec<String>>());
    if !store.layout_heard.get_untracked() {
        leptos::logging::log!("[arrange] tap before the service's layout arrived: ignored");
        return;
    }
    let doc = store.layout.get_untracked();
    let tap = store.arrange.try_update(|a| a.tap(&doc, &cams)).flatten();
    leptos::logging::log!("[arrange] tap at rev {}: {tap:?}", doc.rev);
    match tap {
        None => {}
        Some(Tap::Start) => store.note.set(None),
        Some(Tap::Leave) => store.show_note(Note {
            text: UNCHANGED_NOTE,
            tone: Tone::Plain,
        }),
        Some(Tap::Send(put)) => {
            store.show_sticky_note(Note {
                text: SAVING_NOTE,
                tone: Tone::Plain,
            });
            spawn_local(async move {
                let outcome = net::put_layout(&put).await;
                let answered = store.arrange.try_update(|a| a.answered(outcome)).flatten();
                let Some((note, saved)) = answered else {
                    return;
                };
                if let Some(doc) = saved {
                    // Shown at once; the /ws push of the same revision changes nothing.
                    if store.layout.with_untracked(|d| doc.rev > d.rev) {
                        store.layout.set(doc);
                    }
                }
                store.show_note(note);
            });
        }
    }
}

/// The header.
#[component]
pub fn Header() -> impl IntoView {
    let store = expect_context::<Store>();
    let arranging = move || store.arrange.with(|a| a.is_on());
    let hidden = move || store.hidden_count();
    let note = move || match store.note.get() {
        Some(n) => (n.text, n.tone.class()),
        None if arranging() => (ARRANGE_NOTE, Tone::Plain.class()),
        None => ("", Tone::Plain.class()),
    };
    let on_reset = move |_| {
        let cams = store.camera_ids();
        store.arrange.update(|a| {
            if let Some(edit) = a.edit_mut() {
                edit.reset_to_numbers(&cams);
            }
        });
    };

    view! {
        <header class="head">
            <span class="title">"SHADING"</span>
            <span class="ver" id="app-version" data-role="version" data-testid="version">
                {crate::version_text()}
            </span>
            <span class="conn" class:bad=move || !store.connected() data-role="conn" id="conn-status">
                {move || if store.connected() { "spojené" } else { "odpojené" }}
            </span>
            <span class="spacer"></span>
            <button
                type="button"
                class="hidden-chip"
                data-role="hidden-chip"
                title="Skryté kamery vrátiš cez ⇄"
                hidden=move || arranging() || hidden() == 0
                on:click=move |_| tap_arrange(store)
            >
                {move || hidden_chip_text(hidden())}
            </button>
            <span class=move || note().1 data-role="arrange-note">{move || note().0}</span>
            <button
                type="button"
                class="reset-num"
                data-role="reset-num"
                title="Vráti poradie podľa čísel kamier a ukáže všetky"
                hidden=move || !arranging()
                on:click=on_reset
            >
                "↺ podľa čísel"
            </button>
            <button
                type="button"
                class="arrange-key"
                class:selected=arranging
                data-role="arrange-key"
                aria-pressed=move || arranging().to_string()
                title="Usporiadať kamery"
                on:click=move |_| tap_arrange(store)
            >
                "⇄"
            </button>
        </header>
    }
}

#[cfg(test)]
mod tests {
    use super::hidden_chip_text;

    #[test]
    fn the_chip_counts_in_slovak() {
        assert_eq!(hidden_chip_text(1), "1 skrytá kamera · vrátiš ju cez ⇄");
        assert_eq!(hidden_chip_text(2), "2 skryté kamery · vrátiš ich cez ⇄");
        assert_eq!(hidden_chip_text(5), "5 skrytých kamier · vrátiš ich cez ⇄");
    }
}
