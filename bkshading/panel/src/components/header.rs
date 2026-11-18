//! The top bar (mockups arrange-v1 and theme-v4d): title, version, connection, the hidden-cameras
//! chip, the note, `↺ podľa čísel` (in ARRANGE), the layout pill (4+4 | 5 | 8 v rade), the mode
//! pill (Svetlo | Tma) and the `⇄` key. On a phone (< 700 px, version D) only the title, the
//! connection and a `☰` stay in the bar; the rest opens in an overlay menu under it.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::behave::arrange_drag::{Note, Tap, Tone, ARRANGE_NOTE, SAVING_NOTE, UNCHANGED_NOTE};
use crate::behave::look::{CardLayout, ThemeMode, LAYOUT_KEY, MODE_KEY};
use crate::store::{look_io, net, Store};

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

    // shading#24: the layout and mode pills; a choice is remembered on this device at once.
    let layouts = CardLayout::ALL
        .iter()
        .map(|&l| {
            view! {
                <button
                    type="button"
                    data-role=format!("layout-{}", l.as_str())
                    aria-pressed=move || (store.card_layout.get() == l).to_string()
                    title=l.title()
                    on:click=move |_| {
                        store.card_layout.set(l);
                        look_io::save(LAYOUT_KEY, l.as_str());
                    }
                >
                    {l.label()}
                </button>
            }
        })
        .collect_view();
    let modes = ThemeMode::ALL
        .iter()
        .map(|&m| {
            view! {
                <button
                    type="button"
                    data-role=format!("mode-{}", m.as_str())
                    aria-pressed=move || (store.theme_mode.get() == m).to_string()
                    on:click=move |_| {
                        store.theme_mode.set(m);
                        look_io::save(MODE_KEY, m.as_str());
                    }
                >
                    {m.label()}
                </button>
            }
        })
        .collect_view();

    let menu_open = RwSignal::new(false);

    view! {
        <header class="bar">
            <h1 class="title">"Shading"</h1>
            <span class="ver" id="app-version" data-role="version" data-testid="version">
                {crate::version_text()}
            </span>
            <span class="conn" class:bad=move || !store.connected() data-role="conn" id="conn-status">
                {move || if store.connected() { "spojené" } else { "odpojené" }}
            </span>
            <span class="spacer"></span>
            <span class=move || note().1 data-role="arrange-note">{move || note().0}</span>
            // shading#24 version D: under 700 px the controls below fold into this menu (an overlay
            // under the bar, so the cards never move); above it the menu is invisible to layout.
            <button
                type="button"
                class="iconbtn burger"
                data-role="menu-key"
                aria-controls="top-menu"
                aria-expanded=move || menu_open.get().to_string()
                title="Menu"
                on:click=move |_| menu_open.update(|o| *o = !*o)
            >
                "☰"
            </button>
            <div class="menu" class:open=move || menu_open.get() id="top-menu" data-role="menu">
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
                <div class="seg" role="group" aria-label="Rozloženie" data-role="layout-pill">{layouts}</div>
                <div class="seg" role="group" aria-label="Režim" data-role="mode-pill">{modes}</div>
                <button
                    type="button"
                    class="arrange-key iconbtn"
                    class:selected=arranging
                    disabled=move || !store.layout_heard.get()
                    data-role="arrange-key"
                    aria-pressed=move || arranging().to_string()
                    title="Usporiadať kamery"
                    on:click=move |_| tap_arrange(store)
                >
                    "⇄"
                </button>
            </div>
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
