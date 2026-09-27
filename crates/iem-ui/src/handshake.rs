//! The mixer socket's protocol handshake (program spec §5.3, S5 design note
//! §5): the server's first message is `Hello{proto, build, min_client_proto}`;
//! a page whose protocol the server no longer serves, or that is older than
//! the server's, reloads to fetch the current UI. A server that never says
//! hello (the predecessor, or a stale proxy) gets one reload. Reloads are at
//! most one per [`RELOAD_GAP_MS`], so a mismatch that a reload cannot cure
//! never becomes a reload loop.

/// The UI protocol of this build.
pub const OUR_PROTO: u16 = iem_core::UI_PROTO;
/// No hello this long after the socket opened: one reload.
pub const HELLO_TIMEOUT_MS: f64 = 3_000.0;
/// At most one handshake reload per this interval.
pub const RELOAD_GAP_MS: f64 = 60_000.0;
/// The WS close code of a server that refuses a page without `proto`.
pub const CLOSE_RELOAD: u16 = 4001;
/// Where the last handshake reload time is kept (per browser).
const STORAGE_KEY: &str = "iem_proto_reload_at";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Keep,
    Reload,
}

/// Whether a reload is allowed at `now` after the last one at `last_reload`.
fn may_reload(now: f64, last_reload: Option<f64>) -> bool {
    last_reload.is_none_or(|t| now - t >= RELOAD_GAP_MS || now < t)
}

/// The decision on the server's hello.
pub fn on_hello(
    ours: u16,
    server_proto: u16,
    min_client_proto: u16,
    now: f64,
    last_reload: Option<f64>,
) -> Decision {
    let served = (min_client_proto..=server_proto).contains(&ours);
    if served || !may_reload(now, last_reload) {
        Decision::Keep
    } else {
        Decision::Reload
    }
}

/// The decision when the socket has been open [`HELLO_TIMEOUT_MS`] without a
/// hello, or the server closed it with [`CLOSE_RELOAD`].
pub fn on_missing_hello(now: f64, last_reload: Option<f64>) -> Decision {
    if may_reload(now, last_reload) {
        Decision::Reload
    } else {
        Decision::Keep
    }
}

/// The WS URL of the mixer page `page` (`proto` tells the server which UI
/// protocol this page speaks).
pub fn mixer_ws_url(scheme: &str, host: &str, page: &str, token: &str) -> String {
    format!("{scheme}://{host}/ws/{page}?token={token}&proto={OUR_PROTO}")
}

/// The last handshake reload time from local storage.
pub fn last_reload() -> Option<f64> {
    let storage = web_sys::window()?.local_storage().ok()??;
    storage.get_item(STORAGE_KEY).ok()??.parse().ok()
}

/// Records the reload time and reloads the page.
pub fn reload(now: f64, why: &str) {
    web_sys::console::warn_1(&format!("protocol handshake: {why}; reloading").into());
    if let Some(window) = web_sys::window() {
        if let Ok(Some(storage)) = window.local_storage() {
            let _ = storage.set_item(STORAGE_KEY, &now.to_string());
        }
        let _ = window.location().reload();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_served_protocol_keeps_the_page() {
        assert_eq!(on_hello(2, 2, 2, 0.0, None), Decision::Keep);
        assert_eq!(on_hello(2, 3, 2, 0.0, None), Decision::Keep);
        assert_eq!(on_hello(2, 3, 1, 0.0, None), Decision::Keep);
    }

    #[test]
    fn a_protocol_outside_the_servers_range_reloads() {
        // The server no longer serves us (we are too old).
        assert_eq!(on_hello(2, 4, 3, 1e6, None), Decision::Reload);
        // The server is older than us (a stale page from a newer build).
        assert_eq!(on_hello(3, 2, 2, 1e6, None), Decision::Reload);
    }

    #[test]
    fn reloads_are_at_most_one_per_minute() {
        let t = 1_000_000.0;
        assert_eq!(on_hello(3, 2, 2, t, Some(t - 59_999.0)), Decision::Keep);
        assert_eq!(on_hello(3, 2, 2, t, Some(t - 60_000.0)), Decision::Reload);
        assert_eq!(on_missing_hello(t, Some(t - 1_000.0)), Decision::Keep);
        assert_eq!(
            on_missing_hello(t, Some(t - RELOAD_GAP_MS)),
            Decision::Reload
        );
        assert_eq!(on_missing_hello(t, None), Decision::Reload);
        assert_eq!(
            on_missing_hello(t, Some(t)),
            Decision::Keep,
            "not twice in the same moment"
        );
        // A clock that went backwards does not block reloads forever.
        assert_eq!(on_missing_hello(t, Some(t + 5_000.0)), Decision::Reload);
    }

    #[test]
    fn the_mixer_url_carries_the_protocol() {
        assert_eq!(
            mixer_ws_url("wss", "mixer.example.org", "member3", "tok"),
            "wss://mixer.example.org/ws/member3?token=tok&proto=2"
        );
        assert_eq!(OUR_PROTO, 2);
        assert_eq!(HELLO_TIMEOUT_MS, 3_000.0);
    }
}
