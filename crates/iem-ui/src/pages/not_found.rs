//! 404 Not Found page

use leptos::prelude::*;

/// 404 Not Found page
#[component]
pub fn NotFoundPage() -> impl IntoView {
    view! {
        <div class="app">
            <main class="main">
                <div class="login-container">
                    <div class="login-box">
                        <h2>"Page Not Found"</h2>
                        <p class="subtitle">"The page you're looking for doesn't exist."</p>
                        <a href="/" class="btn">"Go Home"</a>
                    </div>
                </div>
            </main>
        </div>
    }
}
