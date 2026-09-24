//! Internet access (Cloudflare tunnel) status UI.
//!
//! The server pushes `ServerMsg::TunnelStatus` on connect and on every change.
//! Texts come from `iem_core::tunnel` (unit tested there); the LAN URL and the
//! public host come from the site config (`GET /api/site`, provided as
//! context by `router::App`).

use iem_core::tunnel::{SiteLinks, member_banner_text, needs_lan_hint, reconnect_lan_hint};
use iem_core::{TunnelState, TunnelStatusInfo};
use leptos::prelude::*;

fn site_links() -> RwSignal<SiteLinks> {
    use_context::<RwSignal<SiteLinks>>().unwrap_or_else(|| RwSignal::new(SiteLinks::default()))
}

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
    let links = site_links();
    let broken = move || status.get().is_some_and(|s| s.state.is_broken());
    let text = move || links.with(|l| member_banner_text(l.lan_url.as_deref()));
    view! {
        <Show when=broken fallback=|| ()>
            <div class="tunnel-banner" data-testid="tunnel-banner" role="alert">
                {text}
            </div>
        </Show>
    }
}

/// Extra line in the "Reconnecting" banner for pages opened via the public
/// host: while the tunnel is down their WebSocket cannot reconnect at all.
#[component]
pub fn LanHint() -> impl IntoView {
    let links = site_links();
    let hostname = web_sys::window()
        .and_then(|w| w.location().hostname().ok())
        .unwrap_or_default();
    move || {
        links
            .with(|l| {
                match (
                    &l.lan_url,
                    needs_lan_hint(&hostname, l.public_host.as_deref()),
                ) {
                    (Some(lan_url), true) => Some(reconnect_lan_hint(lan_url)),
                    _ => None,
                }
            })
            .map(|hint| view! { <div class="lan-hint" data-testid="lan-hint">{hint}</div> })
    }
}
