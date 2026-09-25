//! Bottom toolbar component

use leptos::prelude::*;

use super::alert_button::AlertButton;
use super::audio_player::ListenButton;
use super::talk_button::{TalkButton, TalkState};

/// Bottom toolbar with Presets, History, and optional Mute All / Listen / Alert buttons
/// Note: Reset button removed - 0 dB = unity gain = dangerously loud for IEMs
/// Note: +Me button removed - was broken and unwanted by user
#[component]
pub fn Toolbar(
    /// Called when Presets button is clicked
    on_presets: Callback<()>,
    /// Called when History button is clicked
    on_history: Callback<()>,
    /// Whether the current user is an engineer (shows Listen button)
    #[prop(default = false)]
    is_engineer: bool,
    /// Whether engineer is on their own mixer page (shows Mute All)
    #[prop(default = false)]
    is_engineer_own_mixer: bool,
    /// Called when Mute All button is clicked (engineer on own mixer only)
    #[prop(optional)]
    on_mute_all: Option<Callback<()>>,
    /// Member ID for listen target (whose mix to listen to)
    #[prop(default = String::new())]
    member_id: String,
    /// WebSocket connection for alert button
    #[prop(optional)]
    ws: Option<ReadSignal<Option<web_sys::WebSocket>>>,
    /// Whether alert is currently active (for toggle button)
    #[prop(optional)]
    alert_active: Option<ReadSignal<bool>>,
    /// Talkback state (engineer only) (reaperiem#123)
    #[prop(optional)]
    talk_state: Option<ReadSignal<TalkState>>,
    /// Talkback state setter (engineer only) (reaperiem#123)
    #[prop(optional)]
    set_talk_state: Option<WriteSignal<TalkState>>,
) -> impl IntoView {
    view! {
        <div class="toolbar">
            {(is_engineer_own_mixer && on_mute_all.is_some()).then(|| {
                let cb = on_mute_all.unwrap();
                view! {
                    <button
                        class="toolbar-btn toolbar-btn-mute-all toolbar-btn-icon"
                        on:click=move |_| cb.run(())
                    >
                        "\u{1F507}"
                    </button>
                }
            })}
            {is_engineer.then(|| {
                let member_id = member_id.clone();
                view! {
                    <ListenButton member_id=member_id.clone() />
                }
            })}
            {(is_engineer_own_mixer && talk_state.is_some() && set_talk_state.is_some() && ws.is_some()).then(|| {
                let ts = talk_state.unwrap();
                let set_ts = set_talk_state.unwrap();
                let ws_sig = ws.unwrap();
                view! {
                    <TalkButton state=ts set_state=set_ts ws=ws_sig />
                }
            })}
            {(!is_engineer).then(|| {
                view! {
                    <button
                        class="toolbar-btn"
                        on:click=move |_| on_presets.run(())
                    >
                        "Presets"
                    </button>
                    <button
                        class="toolbar-btn"
                        on:click=move |_| on_history.run(())
                    >
                        "History"
                    </button>
                }
            })}
            {(!is_engineer && ws.is_some() && alert_active.is_some()).then(|| {
                let ws_sig = ws.unwrap();
                let active = alert_active.unwrap();
                view! {
                    <AlertButton ws=ws_sig active=active />
                }
            })}
        </div>
    }
}
