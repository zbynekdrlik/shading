//! The root component: the header (version, connection, layout and mode pills, ARRANGE), the
//! connection banner, the camera cards and the #14 popup (mockups arrange-v1 and theme-v4d).

use leptos::prelude::*;

use crate::components::camera_block::CameraBlock;
use crate::components::control_info::ControlInfo;
use crate::components::header::Header;
use crate::store::{conn, look_io, net, Store};

/// The app.
#[component]
pub fn App() -> impl IntoView {
    let store = Store::new(net::now_ms());
    provide_context(store);
    net::start(store);
    crate::components::arrange::end_on_page_hide(store);

    // shading#24: the per-device layout and colour mode, read once, put on <html> on every change.
    let (layout, mode) = look_io::load();
    store.card_layout.set(layout);
    store.theme_mode.set(mode);
    Effect::new(move |_| look_io::apply(store.card_layout.get(), store.theme_mode.get()));

    let banner = move || {
        conn::banner_text(
            store.ws_open.get(),
            store.last_contact.get(),
            store.now.get(),
        )
    };
    let empty = move || store.heard.get() && store.cameras.with(Vec::is_empty);
    let ids = move || {
        store
            .blocks()
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<String>>()
    };

    view! {
        <div class="app" data-testid="app" data-arrange=move || store.arrange.with(|a| a.is_on()).to_string()>
            <Header />
            <div
                id="conn-banner"
                class="banner"
                data-role="conn-banner"
                role="alert"
                aria-live="assertive"
                hidden=move || banner().is_none()
            >
                {move || banner().unwrap_or_default()}
            </div>
            <main id="camera-grid" class="grid" aria-live="polite">
                <For each=ids key=|id: &String| id.clone() let:id>
                    <CameraBlock id=id />
                </For>
                <div class="drop-line" id="drop-line"></div>
            </main>
            <p class="empty-note" data-role="empty-note" hidden=move || !empty()>
                "Žiadne kamery v konfigurácii."
            </p>
            <ControlInfo />
        </div>
    }
}
