use leptos::prelude::*;
use wasm_bindgen::prelude::*;

/// Post-release guard duration in milliseconds.
/// With server-side echo suppression, this only needs to cover WebSocket round-trip (~10-20ms).
pub(super) const POST_RELEASE_GUARD_MS: i32 = 100;

/// Minimum interval between WebSocket sends per track (ms).
/// Limits to ~20 commands/sec to avoid overwhelming the server.
pub(super) const THROTTLE_INTERVAL_MS: f64 = 50.0;

/// A channel strip as shown on the active tab.
/// Note: level_db, pan, muted are read via derived signals from channels
#[derive(Debug, Clone, PartialEq)]
pub(super) struct DisplayChannel {
    /// The engine id (input or heard mix).
    pub id: String,
    pub display_name: String,
    /// The page member's own channel (the "more me" strip, first on Main).
    pub is_my_input: bool,
    /// The viewer may open this channel's EQ.
    pub eq: bool,
}

/// The strips of the active tab: Main = the own channel and the pinned ones
/// (own first); Hidden = the hidden ones; a category tab = its channels
/// without the hidden ones (stems: CLICK, then GUIDE, then the rest).
pub(super) fn display_channels(
    chs: &[iem_core::Channel],
    active: crate::components::category_tabs::Category,
    pinned: &[String],
    hidden: &[String],
) -> Vec<DisplayChannel> {
    use crate::components::category_tabs::Category;
    let mut result: Vec<DisplayChannel> = chs
        .iter()
        .filter(|ch| {
            let is_pinned = pinned.contains(&ch.id);
            let is_hidden = hidden.contains(&ch.id);
            match active {
                Category::Hidden => is_hidden,
                // Hidden does NOT remove pinned channels from Main — only from category tabs
                Category::Main => ch.own || is_pinned,
                cat => cat.matches(&ch.category) && !is_hidden,
            }
        })
        .map(|ch| DisplayChannel {
            id: ch.id.clone(),
            display_name: ch.name.clone(),
            is_my_input: ch.own,
            eq: ch.eq,
        })
        .collect();
    match active {
        Category::Stems => result.sort_by_key(|ch| match ch.display_name.to_uppercase().as_str() {
            "CLICK" => 0,
            "GUIDE" => 1,
            _ => 2,
        }),
        Category::Main => result.sort_by_key(|ch| !ch.is_my_input),
        _ => {}
    }
    result
}

/// What a mute click does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MuteClick {
    /// Show and send the new mute.
    Toggle(bool),
    /// The channel is silenced by a solo on another channel: its own mute
    /// changes (sent, and restored when the solo ends) but it stays silent
    /// until then (S5 design note §9).
    Masked(bool),
}

/// The mute click on channel `id` showing `shown_muted`, given the page's solo
/// and the mutes the solo started from.
pub(super) fn mute_click(
    id: &str,
    shown_muted: bool,
    soloed: &std::collections::HashSet<String>,
    pre_solo_mutes: &std::collections::HashMap<String, bool>,
) -> MuteClick {
    if !soloed.is_empty() && !soloed.contains(id) {
        MuteClick::Masked(!pre_solo_mutes.get(id).copied().unwrap_or(false))
    } else {
        MuteClick::Toggle(!shown_muted)
    }
}

/// The `&'static` copy of a channel id, so strip callbacks can capture it by
/// copy. Ids are the site's (a few dozen), each interned once for the page's
/// life.
pub(super) fn intern(id: &str) -> &'static str {
    thread_local! {
        static IDS: std::cell::RefCell<std::collections::HashSet<&'static str>> =
            std::cell::RefCell::new(std::collections::HashSet::new());
    }
    IDS.with(|ids| {
        let mut ids = ids.borrow_mut();
        if let Some(known) = ids.get(id) {
            return *known;
        }
        let leaked: &'static str = Box::leak(id.to_string().into_boxed_str());
        ids.insert(leaked);
        leaked
    })
}

/// Send a command via WebSocket (synchronous, non-blocking)
pub(super) fn ws_send(ws: ReadSignal<Option<web_sys::WebSocket>>, cmd: &iem_core::ClientMsg) {
    if let Some(ws) = ws.get_untracked()
        && ws.ready_state() == web_sys::WebSocket::OPEN
        && let Ok(json) = serde_json::to_string(cmd)
    {
        let _ = ws.send_with_str(&json);
    }
}

/// Storage for WebSocket closures to prevent memory leaks on reconnect.
/// Dropping a Closure that was passed to JS via `as_ref().unchecked_ref()` properly
/// releases the WASM-side allocation. Without this, `Closure::forget()` leaks on every reconnect.
/// Uses Rc<RefCell<>> because wasm_bindgen::Closure is !Send (WASM is single-threaded).
pub(super) type WsClosures = (
    Closure<dyn FnMut(web_sys::MessageEvent)>,
    Closure<dyn FnMut(web_sys::CloseEvent)>,
    Closure<dyn FnMut()>,
);
pub(super) type WsClosureStore = std::rc::Rc<std::cell::RefCell<Option<WsClosures>>>;

/// Failed sockets in a row: counted at each close, reset when a socket
/// opens. Shared across connect_websocket calls via Rc<Cell<>>.
pub(super) type WsFailCounter = std::rc::Rc<std::cell::Cell<u32>>;

/// Failed sockets in a row after which the page asks the server whether its
/// token still holds (an invalid one goes to the login).
pub(super) const MAX_WS_FAILURES: u32 = 3;

/// What the page's reconnect tick does with a closed socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReconnectStep {
    /// The backoff delay since the last attempt has not passed yet.
    Wait,
    /// Open a new socket.
    Connect,
    /// Open a new socket and ask the server whether the token still holds:
    /// an invalid one goes to the login, a valid one keeps retrying.
    ConnectAndCheckToken,
}

/// The reconnect tick's step at `now_ms` for a closed socket, given the
/// last attempt (`0.0`: none yet), the backoff attempt and the failed
/// sockets in a row. The page never stops retrying while its token holds:
/// the failure count only decides whether the token is checked.
pub(super) fn reconnect_step(
    now_ms: f64,
    last_attempt_ms: f64,
    attempt: u32,
    failures: u32,
) -> ReconnectStep {
    let delay_ms = f64::from(crate::lifecycle::backoff_delay_ms(attempt));
    if last_attempt_ms > 0.0 && now_ms - last_attempt_ms < delay_ms {
        ReconnectStep::Wait
    } else if failures >= MAX_WS_FAILURES {
        ReconnectStep::ConnectAndCheckToken
    } else {
        ReconnectStep::Connect
    }
}

/// What a mixer page that is left does with its socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LeaveClose {
    /// Open: close it at once (the server ends this page's session).
    Now,
    /// Still connecting: close it when it opens. Closing it now makes
    /// Chrome log "WebSocket is closed before the connection is
    /// established", which the E2E console guard rejects.
    WhenOpen,
    /// Closing or closed already.
    Nothing,
}

/// The socket's close when its page is left, by its `ready_state`.
pub(super) fn leave_close(ready_state: u16) -> LeaveClose {
    match ready_state {
        web_sys::WebSocket::OPEN => LeaveClose::Now,
        web_sys::WebSocket::CONNECTING => LeaveClose::WhenOpen,
        _ => LeaveClose::Nothing,
    }
}

/// Parse track name into main and type parts
pub(super) fn parse_track_name(name: &str) -> (String, String) {
    let parts: Vec<&str> = name.split_whitespace().collect();
    if parts.len() >= 2 {
        (parts[0].to_string(), parts[1..].join(" "))
    } else {
        (name.to_string(), String::new())
    }
}

/// Format dB value for display with unit suffix
pub(super) fn format_db(db: f32) -> String {
    if db <= -60.0 {
        "-\u{221E}dB".to_string()
    } else if db >= 0.0 {
        format!("+{:.1}dB", db)
    } else {
        format!("{:.1}dB", db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::components::category_tabs::Category;
    use std::collections::{HashMap, HashSet};

    fn ch(id: &str, name: &str, category: &str, own: bool) -> iem_core::Channel {
        iem_core::Channel {
            id: id.into(),
            name: name.into(),
            level_db: 0.0,
            pan: 0.5,
            muted: false,
            category: category.into(),
            eq: own,
            own,
        }
    }

    fn ids(list: &[DisplayChannel]) -> Vec<&str> {
        list.iter().map(|c| c.id.as_str()).collect()
    }

    fn site() -> Vec<iem_core::Channel> {
        vec![
            ch("mic1", "MEMBER1 mic", "mics", false),
            ch("mic3", "MEMBER3 mic", "mics", true),
            ch("keys", "KEYS", "stems", false),
            ch("guide", "GUIDE", "stems", false),
            ch("click", "CLICK", "stems", false),
            ch("hand1", "HAND1 mic", "tech", false),
            ch("member2", "Member2", "mixes", false),
        ]
    }

    #[test]
    fn main_shows_the_own_channel_first_then_the_pinned_ones() {
        let chs = site();
        let pinned = vec!["keys".to_string(), "mic1".to_string()];
        let hidden = vec!["keys".to_string()];
        let main = display_channels(&chs, Category::Main, &pinned, &hidden);
        assert_eq!(
            ids(&main),
            ["mic3", "mic1", "keys"],
            "hidden stays on Main when pinned"
        );
        assert!(main[0].is_my_input && main[0].eq);
        assert!(!main[1].is_my_input && !main[1].eq);
    }

    #[test]
    fn category_tabs_filter_by_category_and_leave_out_hidden_channels() {
        let chs = site();
        let hidden = vec!["mic1".to_string()];
        assert_eq!(
            ids(&display_channels(&chs, Category::Mics, &[], &hidden)),
            ["mic3"]
        );
        assert_eq!(
            ids(&display_channels(&chs, Category::Tech, &[], &[])),
            ["hand1"]
        );
        assert_eq!(
            ids(&display_channels(&chs, Category::Mixes, &[], &[])),
            ["member2"]
        );
        assert_eq!(
            ids(&display_channels(&chs, Category::Hidden, &[], &hidden)),
            ["mic1"]
        );
        assert!(display_channels(&chs, Category::Hidden, &[], &[]).is_empty());
    }

    #[test]
    fn stems_start_with_click_then_guide() {
        let chs = site();
        assert_eq!(
            ids(&display_channels(&chs, Category::Stems, &[], &[])),
            ["click", "guide", "keys"]
        );
    }

    #[test]
    fn an_id_is_interned_once() {
        let a = intern("mic7");
        let b = intern(&String::from("mic7"));
        assert_eq!(a, "mic7");
        assert!(std::ptr::eq(a, b));
        assert_ne!(intern("mic8"), a);
    }

    #[test]
    fn a_mute_click_inside_a_solo_changes_the_hidden_mute_only() {
        let none = HashSet::new();
        let pre = HashMap::from([("mic1".to_string(), true)]);
        assert_eq!(
            mute_click("mic1", false, &none, &pre),
            MuteClick::Toggle(true)
        );
        assert_eq!(
            mute_click("mic1", true, &none, &pre),
            MuteClick::Toggle(false)
        );
        let solo = HashSet::from(["mic3".to_string()]);
        assert_eq!(
            mute_click("mic3", false, &solo, &pre),
            MuteClick::Toggle(true)
        );
        assert_eq!(
            mute_click("mic1", true, &solo, &pre),
            MuteClick::Masked(false)
        );
        assert_eq!(
            mute_click("keys", true, &solo, &pre),
            MuteClick::Masked(true)
        );
    }

    #[test]
    fn the_reconnect_tick_waits_out_the_backoff_delay() {
        // No attempt yet: the first tick after the drop reconnects.
        assert_eq!(reconnect_step(1_000.0, 0.0, 1, 1), ReconnectStep::Connect);
        // Second attempt: 8 s after the first.
        assert_eq!(
            reconnect_step(17_999.0, 10_000.0, 2, 2),
            ReconnectStep::Wait
        );
        assert_eq!(
            reconnect_step(18_000.0, 10_000.0, 2, 2),
            ReconnectStep::Connect
        );
        // Third: 15 s.
        assert_eq!(
            reconnect_step(24_999.0, 10_000.0, 3, 2),
            ReconnectStep::Wait
        );
        assert_eq!(
            reconnect_step(25_000.0, 10_000.0, 3, 2),
            ReconnectStep::Connect
        );
    }

    #[test]
    fn a_page_keeps_retrying_after_max_failures_and_checks_its_token() {
        // The third failed socket in a row: the next attempt still opens a
        // socket, and asks whether the token holds (an invalid one goes to
        // the login). A server outage longer than the backoff's first steps
        // (a restart, a mode switch) never leaves the page stuck.
        assert_eq!(
            reconnect_step(25_000.0, 10_000.0, 3, MAX_WS_FAILURES),
            ReconnectStep::ConnectAndCheckToken
        );
        // Every 30 s from then on, however long the outage.
        assert_eq!(
            reconnect_step(39_999.0, 10_000.0, 9, 9),
            ReconnectStep::Wait
        );
        assert_eq!(
            reconnect_step(40_000.0, 10_000.0, 9, 9),
            ReconnectStep::ConnectAndCheckToken
        );
        assert_eq!(
            reconnect_step(1_000.0, 0.0, 40, 40),
            ReconnectStep::ConnectAndCheckToken
        );
        // Below the limit no token check.
        assert_eq!(
            reconnect_step(40_000.0, 10_000.0, 9, MAX_WS_FAILURES - 1),
            ReconnectStep::Connect
        );
    }

    #[test]
    fn a_page_that_is_left_closes_an_open_socket_and_a_connecting_one_when_it_opens() {
        assert_eq!(leave_close(web_sys::WebSocket::OPEN), LeaveClose::Now);
        assert_eq!(
            leave_close(web_sys::WebSocket::CONNECTING),
            LeaveClose::WhenOpen
        );
        assert_eq!(
            leave_close(web_sys::WebSocket::CLOSING),
            LeaveClose::Nothing
        );
        assert_eq!(leave_close(web_sys::WebSocket::CLOSED), LeaveClose::Nothing);
    }

    #[test]
    fn test_format_db_uses_proper_notation() {
        assert!(format_db(0.0).ends_with("dB"), "Must use 'dB' not 'db'");
        assert!(format_db(-6.0).ends_with("dB"));
        assert!(format_db(-60.0).ends_with("dB")); // -inf case
    }

    #[test]
    fn test_format_db_max_length() {
        let cases = [0.0, 6.0, 12.0, -6.0, -12.5, -59.9, -60.0, -100.0];
        for db in cases {
            let s = format_db(db);
            assert!(
                s.chars().count() <= 7,
                "format_db({db}) = \"{s}\" exceeds 7 chars"
            );
        }
    }
}
