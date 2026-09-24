//! Application router

use iem_core::tunnel::SiteLinks;
use leptos::prelude::*;
use leptos_router::components::{Route, Router, Routes};
use leptos_router::path;
use wasm_bindgen_futures::spawn_local;

use crate::pages::{
    landing::LandingPage, login::LoginPage, mixer::MixerPage, not_found::NotFoundPage,
};

/// Main application component with routing
#[component]
pub fn App() -> impl IntoView {
    // Where the mixer is reachable (site config, GET /api/site); read by the
    // tunnel banner and the reconnect hint. Fetched once per page load, so it
    // is known before the connection can drop.
    let site_links = RwSignal::new(SiteLinks::default());
    provide_context(site_links);
    spawn_local(async move {
        match crate::api::get_site_links().await {
            Ok(links) => {
                let _ = site_links.try_set(links);
            }
            Err(e) => leptos::logging::log!("site links unavailable: {e}"),
        }
    });

    view! {
        <Router>
            <Routes fallback=|| view! { <NotFoundPage /> }>
                <Route path=path!("/") view=LandingPage />
                <Route path=path!("/login") view=LoginPage />
                <Route path=path!("/:member") view=MixerPage />
            </Routes>
        </Router>
    }
}
