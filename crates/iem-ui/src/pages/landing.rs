//! Landing page with member grid

use leptos::prelude::*;
use leptos_router::hooks::use_navigate;
use wasm_bindgen::JsCast;

use crate::api::{MemberInfo, get_members_with_timeout};
use crate::auth::{get_auth, is_token_expired};

/// Load state for the landing page
#[derive(Clone)]
enum LoadState {
    Loading,
    Loaded(Vec<MemberInfo>),
    Error(String),
}

/// Landing page showing all band members
#[component]
pub fn LandingPage() -> impl IntoView {
    let navigate = use_navigate();

    // Auto-redirect: remember last member, get to mixer ASAP (reaperiem#84).
    // - Valid token → straight to mixer
    // - Expired token → PIN login for remembered member (skip member grid)
    // - No auth → show member grid (first-time user)
    // sessionStorage flag prevents re-redirect when user navigates back.
    Effect::new(move |_| {
        if let Some(auth) = get_auth() {
            let window = web_sys::window().unwrap();
            if let Ok(Some(storage)) = window.session_storage()
                && storage.get_item("iem_redirected").ok().flatten().is_none()
            {
                let _ = storage.set_item("iem_redirected", "1");
                if !is_token_expired() {
                    let target = format!("/{}", auth.member);
                    navigate(&target, Default::default());
                } else {
                    let target = format!("/login?member={m}&next=/{m}", m = auth.member);
                    navigate(&target, Default::default());
                }
            }
        }
    });

    // State signal for loading/loaded/error
    let (state, set_state) = signal(LoadState::Loading);
    // Retry trigger - incrementing this refetches
    let (retry_trigger, set_retry_trigger) = signal(0u32);

    // Effect that fetches members on mount and on retry
    Effect::new(move |_| {
        // Read retry trigger to track it
        let _ = retry_trigger.get();

        // Reset to loading state
        let _ = set_state.try_set(LoadState::Loading);

        // Spawn async fetch. try_update: the landing page can unmount
        // (user navigates) while get_members_with_timeout is awaiting. reaperiem#153
        wasm_bindgen_futures::spawn_local(async move {
            match get_members_with_timeout().await {
                Ok(members) => {
                    let _ = set_state.try_set(LoadState::Loaded(members));
                }
                Err(e) => {
                    let _ = set_state.try_set(LoadState::Error(e));
                }
            }
        });
    });

    let retry = move |_| {
        let _ = set_retry_trigger.try_update(|n| *n += 1);
    };

    view! {
        <div class="app">
            <header class="header">
                <h1>"IEM Mixer"</h1>
                <div class="header-version">
                    <span class="header-version-number" data-testid="version">{iem_core::version_label()}</span>
                    <span class="header-version-date">{iem_core::build_datetime()}</span>
                </div>
            </header>
            <main class="main">
                {move || {
                    match state.get() {
                        LoadState::Loading => view! {
                            <div class="loading">
                                <div class="spinner"></div>
                            </div>
                        }.into_any(),
                        LoadState::Loaded(members) => view! {
                            <MemberGrid members=members />
                        }.into_any(),
                        LoadState::Error(e) => view! {
                            <NetworkError error=e on_retry=retry />
                        }.into_any(),
                    }
                }}
            </main>
        </div>
    }
}

/// Network error component with retry button
#[component]
fn NetworkError(error: String, on_retry: impl Fn(()) + 'static) -> impl IntoView {
    let is_timeout = error == "NETWORK_TIMEOUT";

    view! {
        <div class="network-error">
            <div class="network-error-icon">"📡"</div>
            <h2>"Connection Failed"</h2>
            {if is_timeout {
                view! {
                    <p class="network-error-message">
                        "Unable to connect to the mixer server."
                        <br />
                        "Make sure your phone is on the "<strong>"band WiFi network"</strong>"."
                    </p>
                }.into_any()
            } else {
                view! {
                    <p class="network-error-message">
                        "Network error: " {error}
                    </p>
                }.into_any()
            }}
            <button class="retry-btn" on:click=move |_| on_retry(())>
                "Try Again"
            </button>
        </div>
    }
}

/// The tiles in their order (#58): the people who play in the band that
/// plays (the engineer among them) first, then the others; each part in the
/// server's order. A site without a roster: everyone plays, no others.
fn tiles(members: Vec<MemberInfo>) -> (Vec<MemberInfo>, Vec<MemberInfo>) {
    members.into_iter().partition(|m| m.playing)
}

/// A tile's class: dimmed for a person who plays in another band.
fn card_class(member: &MemberInfo) -> &'static str {
    if member.playing {
        "member-card"
    } else {
        "member-card not-playing"
    }
}

/// Grid of member cards: the band that plays, then "Iná kapela" and the
/// others (#58).
#[component]
fn MemberGrid(members: Vec<MemberInfo>) -> impl IntoView {
    if members.is_empty() {
        view! {
            <div class="empty-state">
                <div class="empty-icon">"🎧"</div>
                <h2>"No Members Configured"</h2>
                <p>"Add band members ([[members]]) to the site file to get started."</p>
                <p class="hint">"The server reads the site file named by IEMMIXER_CONFIG (default: iemmixer.toml in its working directory)."</p>
            </div>
        }
        .into_any()
    } else {
        let (playing, others) = tiles(members);
        let divider = (!others.is_empty()).then(|| {
            view! { <div class="member-divider" data-testid="other-band">"Iná kapela"</div> }
        });
        view! {
            <div class="member-grid">
                {playing.into_iter().map(|member| view! { <MemberCard member=member /> }).collect::<Vec<_>>()}
                {divider}
                {others.into_iter().map(|member| view! { <MemberCard member=member /> }).collect::<Vec<_>>()}
            </div>
        }
        .into_any()
    }
}

/// One member's tile: their photo (or initial) and name, a link to their page.
#[component]
fn MemberCard(member: MemberInfo) -> impl IntoView {
    let class = card_class(&member);
    let initial = member
        .name
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string();
    let href = format!("/{}", member.id);
    let has_photo = member.has_photo;
    let photo_url = format!("/api/members/{}/photo", member.id);
    let initial_fallback = initial.clone();
    view! {
        <a href=href class=class>
            <div class="avatar">
                {if has_photo {
                    let fb = initial_fallback.clone();
                    view! {
                        <img
                            class="avatar-photo"
                            src=photo_url
                            alt=fb.clone()
                            on:error=move |e| {
                                // Fallback: hide broken img, show initial text
                                if let Some(el) = e.target() {
                                    let el: web_sys::HtmlElement = el.unchecked_into();
                                    el.set_hidden(true);
                                    if let Some(parent) = el.parent_element() {
                                        parent.set_text_content(Some(&fb));
                                    }
                                }
                            }
                        />
                    }.into_any()
                } else {
                    view! { {initial} }.into_any()
                }}
            </div>
            <div class="name">{member.name}</div>
        </a>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: &str, playing: bool) -> MemberInfo {
        MemberInfo {
            id: id.into(),
            name: id.into(),
            has_photo: false,
            playing,
        }
    }

    fn ids(list: &[MemberInfo]) -> Vec<&str> {
        list.iter().map(|m| m.id.as_str()).collect()
    }

    #[test]
    fn playing_people_come_first() {
        // As the server lists them while youth plays: its people, the
        // others, then the engineer (who always plays).
        let (playing, others) = tiles(vec![
            member("member10", true),
            member("member3", true),
            member("member1", false),
            member("member2", false),
            member("member11", true),
            member("engineer", true),
        ]);
        assert_eq!(
            ids(&playing),
            ["member10", "member3", "member11", "engineer"]
        );
        assert_eq!(ids(&others), ["member1", "member2"]);
        assert_eq!(card_class(&playing[0]), "member-card");
        assert_eq!(card_class(&others[0]), "member-card not-playing");
        // A site without a roster: everyone plays, so no "Iná kapela".
        let (all, none) = tiles(vec![member("member1", true), member("engineer", true)]);
        assert_eq!(ids(&all), ["member1", "engineer"]);
        assert!(none.is_empty());
    }
}
