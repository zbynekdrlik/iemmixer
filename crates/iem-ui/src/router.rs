//! Application router

use leptos::prelude::*;
use leptos_router::components::{Route, Router, Routes};
use leptos_router::path;

use crate::pages::{
    landing::LandingPage, login::LoginPage, mixer::MixerPage, not_found::NotFoundPage,
};

/// Main application component with routing
#[component]
pub fn App() -> impl IntoView {
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
