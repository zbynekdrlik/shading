//! The bkshading operator panel (shading#16): a Leptos 0.7 CSR app built to WASM by Trunk and
//! served by the service (embedded with rust-embed), on fohmixer-ui's patterns — the house app
//! stack (airuleset#1333).
//!
//! - `behave`: the panel's behaviour as pure state machines and rules, unit-tested natively
//!   (`cargo test -p bkshading-panel`): steppers, holds, ARRANGE, popup texts, preview decisions;
//! - `store`: the live state (signals), the connection rules (`store::conn`) and the browser IO
//!   (`store::net`: the `/ws` push, the HTTP fallback poll, the PUTs);
//! - `components` + `app`: the view.
//!
//! The panel speaks the service's own wire types (`bkshading-proto`), so there is no hand-written
//! mirror of them.

pub mod app;
pub mod behave;
pub mod components;
pub mod store;

use wasm_bindgen::prelude::*;

/// The version the panel shows: `v` + the workspace version it was built from. Panel and service
/// are built from the same tree, so this is the service's `/api/version` too (version-on-dashboard).
pub fn version_text() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

/// WASM entry point.
#[wasm_bindgen(start)]
pub fn main() {
    // A panic shows in the console (the E2E's zero-console gate catches it), before the mount so a
    // panic during mount is captured.
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(app::App);
    // Remove the pre-WASM loading shell now that the app is mounted.
    if let Some(shell) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("app-shell"))
    {
        shell.remove();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_version_label_is_the_workspace_version() {
        assert_eq!(
            super::version_text(),
            format!("v{}", env!("CARGO_PKG_VERSION"))
        );
    }
}
