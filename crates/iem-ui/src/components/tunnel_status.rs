//! Internet access (Cloudflare tunnel) status UI (reaperiem#202).
//!
//! The server pushes `ServerMsg::TunnelStatus` on connect and on every change.
//! Texts and CSS classes come from `iem_core::TunnelState` so they are unit
//! tested on the server side.

use iem_core::{TunnelState, TunnelStatusInfo};
use leptos::prelude::*;

/// Engineer indicator row under the header: „Vonkajší prístup: OK / nefunguje".
/// Hidden until the first status message arrives.
#[component]
pub fn TunnelIndicator(status: ReadSignal<Option<TunnelStatusInfo>>) -> impl IntoView {
    let state = move || status.get().map(|s| s.state);
    let has_status = move || state().is_some();
    let class = move || {
        let modifier = state().map(TunnelState::css_class).unwrap_or("ok");
        format!("tunnel-status {modifier}")
    };
    let label = move || status.get().map(|s| s.engineer_label()).unwrap_or_default();
    view! {
        <Show when=has_status fallback=|| ()>
            <div class=class data-testid="tunnel-status" role="status">
                {label}
            </div>
        </Show>
    }
}

/// Band-member banner shown while the tunnel is Down/Restarting: tells people
/// on the venue network to open the local address instead.
#[component]
pub fn TunnelBanner(status: ReadSignal<Option<TunnelStatusInfo>>) -> impl IntoView {
    let broken = move || status.get().is_some_and(|s| s.state.is_broken());
    view! {
        <Show when=broken fallback=|| ()>
            <div class="tunnel-banner" data-testid="tunnel-banner" role="alert">
                {iem_core::tunnel::MEMBER_BANNER_TEXT}
            </div>
        </Show>
    }
}

/// Extra line in the "Reconnecting" banner for pages opened via the public
/// URL: while the tunnel is down their WebSocket cannot reconnect at all, so
/// people on the venue network are pointed at the local address.
#[component]
pub fn LanHint() -> impl IntoView {
    let hostname = web_sys::window()
        .and_then(|w| w.location().hostname().ok())
        .unwrap_or_default();
    iem_core::tunnel::needs_lan_hint(&hostname).then(|| {
        view! {
            <div class="lan-hint" data-testid="lan-hint">
                {iem_core::tunnel::RECONNECT_LAN_HINT}
            </div>
        }
    })
}
