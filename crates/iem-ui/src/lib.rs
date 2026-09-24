//! IEM Mixer WASM Frontend
//!
//! Leptos CSR application for controlling in-ear monitor mixes.

pub mod api;
pub mod auth;
pub mod components;
pub mod lifecycle;
pub mod pages;
pub mod router;

use wasm_bindgen::prelude::*;

/// Main entry point for WASM
#[wasm_bindgen(start)]
pub fn main() {
    // Install panic hook with overlay + server reporting (reaperiem#153)
    lifecycle::install_panic_hook();

    // Mount the app
    leptos::mount::mount_to_body(router::App);

    // Remove pre-WASM loading shell now that Leptos has mounted
    if let Some(shell) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("app-shell"))
    {
        shell.remove();
    }
}
