//! The browser side of the per-device look (shading#24; the rules are `behave::look`): the layout
//! and the colour mode are read from and written to `localStorage`, and put on `<html>` as
//! `data-layout` / `data-mode` (the CSS keys on them). Storage can be absent or throw (a private
//! window, blocked site data, a WebView): then the defaults apply and nothing is remembered, with a
//! log line, never a console error or a panic.

use crate::behave::look::{CardLayout, ThemeMode, LAYOUT_KEY, MODE_KEY};

fn storage() -> Option<web_sys::Storage> {
    match web_sys::window()?.local_storage() {
        Ok(s) => s,
        Err(e) => {
            leptos::logging::log!("[look] localStorage unavailable: {e:?}");
            None
        }
    }
}

fn read(key: &str) -> Option<String> {
    match storage()?.get_item(key) {
        Ok(v) => v,
        Err(e) => {
            leptos::logging::log!("[look] cannot read {key}: {e:?}");
            None
        }
    }
}

/// The stored layout and mode (defaults when missing or unreadable).
pub fn load() -> (CardLayout, ThemeMode) {
    let layout = CardLayout::parse(read(LAYOUT_KEY).as_deref());
    let mode = ThemeMode::parse(read(MODE_KEY).as_deref());
    leptos::logging::log!("[look] layout {} mode {}", layout.as_str(), mode.as_str());
    (layout, mode)
}

/// Remembers `word` under `key` on this device.
pub fn save(key: &str, word: &str) {
    let Some(s) = storage() else {
        return;
    };
    if let Err(e) = s.set_item(key, word) {
        leptos::logging::log!("[look] cannot store {key}={word}: {e:?}");
    }
}

/// Puts the layout and the mode on `<html>`.
pub fn apply(layout: CardLayout, mode: ThemeMode) {
    let Some(root) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.document_element())
    else {
        return;
    };
    let _ = root.set_attribute("data-layout", layout.as_str());
    let _ = root.set_attribute("data-mode", mode.as_str());
    if let Ok(el) = wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlElement>(root) {
        let _ = el.style().set_property("color-scheme", mode.color_scheme());
    }
}
