//! The engineer's console in Settings (F29): what REAPER's own window used to
//! offer — each input's mute, trim and processing, every mix's limiter
//! counter with a reset, the mixer pages without a member (the translator),
//! and the login-failure counters; and the "Späť na REAPER" switch (§4.3)
//! where the site has one.

use iem_core::{ClientMsg, ConsoleInfo};
use leptos::prelude::*;
use wasm_bindgen::prelude::*;

/// The console is refreshed this often while it is shown (limiter seconds).
const REFRESH_MS: i32 = 2_000;
/// Trim range (dB); the buttons step it by whole decibels.
pub const TRIM_MIN_DB: f32 = -24.0;
pub const TRIM_MAX_DB: f32 = 24.0;

/// The trim `steps` decibels away from `current` (clamped to the range,
/// rounded to a whole decibel so repeated clicks never drift).
pub fn step_trim(current: f32, steps: f32) -> f32 {
    (current + steps).round().clamp(TRIM_MIN_DB, TRIM_MAX_DB)
}

/// A trim for display ("+3 dB", "0 dB", "-4.5 dB").
pub fn format_trim(db: f32) -> String {
    if db.abs() < 0.05 {
        "0 dB".to_string()
    } else if db.fract().abs() < 0.05 {
        format!("{:+.0} dB", db)
    } else {
        format!("{:+.1} dB", db)
    }
}

/// The login-failure line.
pub fn login_line(c: &ConsoleInfo) -> String {
    format!(
        "Failed logins: LAN {}, internet {}; engineer lockouts {}",
        c.login.lan, c.login.tunnel, c.login.engineer_budget_trips
    )
}

fn send(ws: ReadSignal<Option<web_sys::WebSocket>>, msg: &ClientMsg) {
    if let Some(Some(socket)) = ws.try_get_untracked()
        && socket.ready_state() == web_sys::WebSocket::OPEN
        && let Ok(json) = serde_json::to_string(msg)
    {
        let _ = socket.send_with_str(&json);
    }
}

#[component]
pub fn ConsoleSection(
    /// The console as the server last sent it
    console: ReadSignal<Option<ConsoleInfo>>,
    /// The mixer socket
    ws: ReadSignal<Option<web_sys::WebSocket>>,
) -> impl IntoView {
    // Ask now and every REFRESH_MS while shown.
    send(ws, &ClientMsg::GetConsole);
    let refresh =
        Closure::wrap(Box::new(move || send(ws, &ClientMsg::GetConsole)) as Box<dyn FnMut()>);
    let interval = web_sys::window().and_then(|w| {
        w.set_interval_with_callback_and_timeout_and_arguments_0(
            refresh.as_ref().unchecked_ref(),
            REFRESH_MS,
        )
        .ok()
    });
    crate::components::keep_while_mounted(refresh);
    on_cleanup(move || {
        if let (Some(w), Some(id)) = (web_sys::window(), interval) {
            w.clear_interval_with_handle(id);
        }
    });

    let set_input = move |input: String,
                          trim_db: Option<f32>,
                          muted: Option<bool>,
                          processing: Option<bool>| {
        send(
            ws,
            &ClientMsg::SetInput {
                input,
                trim_db,
                muted,
                processing,
            },
        );
    };

    let can_switch = move || console.with(|c| c.as_ref().is_some_and(|c| c.can_switch));

    view! {
        <div class="settings-section" data-testid="console-section">
            <div class="settings-section-title">"Console"</div>
            {move || match console.get() {
                None => view! { <div class="settings-desc">"Loading…"</div> }.into_any(),
                Some(c) => {
                    let login = login_line(&c);
                    view! {
                        <div class="console-inputs">
                            {c.inputs.into_iter().map(|i| {
                                let id_mute = i.id.clone();
                                let id_down = i.id.clone();
                                let id_up = i.id.clone();
                                let id_fx = i.id.clone();
                                let (muted, trim, processing) = (i.muted, i.trim_db, i.processing);
                                view! {
                                    <div class="settings-row console-input" data-input=i.id.clone()>
                                        <div class="settings-label">
                                            <div class="settings-name">{i.name.clone()}</div>
                                        </div>
                                        <button
                                            class=if muted { "mute-btn on" } else { "mute-btn off" }
                                            on:click=move |_| set_input(id_mute.clone(), None, Some(!muted), None)
                                        >
                                            "M"
                                        </button>
                                        <div class="listen-boost-stepper">
                                            <button
                                                class="boost-btn"
                                                aria-label="Trim down"
                                                on:click=move |_| set_input(id_down.clone(), Some(step_trim(trim, -1.0)), None, None)
                                            >
                                                "\u{2212}"
                                            </button>
                                            <span class="boost-value console-trim">{format_trim(trim)}</span>
                                            <button
                                                class="boost-btn"
                                                aria-label="Trim up"
                                                on:click=move |_| set_input(id_up.clone(), Some(step_trim(trim, 1.0)), None, None)
                                            >
                                                "+"
                                            </button>
                                        </div>
                                        <button
                                            class=if processing { "eq-band-toggle on" } else { "eq-band-toggle off" }
                                            title="Trim and EQ"
                                            on:click=move |_| set_input(id_fx.clone(), None, None, Some(!processing))
                                        >
                                            "FX"
                                        </button>
                                    </div>
                                }
                            }).collect_view()}
                        </div>
                        <div class="console-limiters">
                            {c.limiters.into_iter().map(|m| {
                                let mix = m.id.clone();
                                view! {
                                    <div class="settings-row console-limiter" data-mix=m.id.clone()>
                                        <div class="settings-label">
                                            <div class="settings-name">{m.name.clone()}</div>
                                            <div class="settings-desc">{crate::components::limiter_modal::format_active(m.active_seconds)}</div>
                                        </div>
                                        <button
                                            class="settings-action-btn"
                                            on:click=move |_| {
                                                send(ws, &ClientMsg::ResetLimiterStats { mix: mix.clone() });
                                                send(ws, &ClientMsg::GetConsole);
                                            }
                                        >
                                            "Reset"
                                        </button>
                                    </div>
                                }
                            }).collect_view()}
                        </div>
                        <div class="console-pages">
                            {c.pages.into_iter().map(|p| view! {
                                <a class="settings-action-btn console-page" href=format!("/{}", p.id)>{p.name}</a>
                            }).collect_view()}
                        </div>
                        <div class="settings-desc console-logins">{login}</div>
                    }.into_any()
                }
            }}
            // "Späť na REAPER" (§4.3) where the site has the switch. Outside the
            // refreshed block above, so the 2 s refresh never resets its PIN step.
            <Show when=can_switch fallback=|| ()>
                <crate::components::back_to_reaper::BackToReaper />
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_steps_are_whole_decibels_within_the_range() {
        assert_eq!(step_trim(0.0, 1.0), 1.0);
        assert_eq!(step_trim(0.0, -1.0), -1.0);
        assert_eq!(step_trim(2.4, 1.0), 3.0);
        assert_eq!(step_trim(23.5, 1.0), 24.0);
        assert_eq!(step_trim(24.0, 1.0), 24.0);
        assert_eq!(step_trim(-24.0, -1.0), -24.0);
    }

    #[test]
    fn trims_read_as_signed_decibels() {
        assert_eq!(format_trim(0.0), "0 dB");
        assert_eq!(format_trim(-0.01), "0 dB");
        assert_eq!(format_trim(3.0), "+3 dB");
        assert_eq!(format_trim(-12.0), "-12 dB");
        assert_eq!(format_trim(-4.5), "-4.5 dB");
        // 0.05 dB is no longer zero, and not a whole decibel either.
        assert_eq!(format_trim(0.05), "+0.1 dB");
        assert_eq!(format_trim(-0.05), "-0.1 dB");
    }

    #[test]
    fn the_login_line_names_every_counter() {
        let c = ConsoleInfo {
            login: iem_core::LoginFailures {
                lan: 2,
                tunnel: 5,
                engineer_budget_trips: 1,
            },
            ..Default::default()
        };
        assert_eq!(
            login_line(&c),
            "Failed logins: LAN 2, internet 5; engineer lockouts 1"
        );
    }
}
