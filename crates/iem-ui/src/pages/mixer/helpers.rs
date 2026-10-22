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

/// Close code of a person's page after the band was switched or the roster
/// changed (#58; the server's `mixer_ws::CLOSE_LINEUP`). 4001 is
/// `handshake::CLOSE_RELOAD`, 4002 the server's close of a silent client.
pub(super) const CLOSE_LINEUP: u16 = 4003;

/// What the page does when its mixer socket closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OnClose {
    /// The server serves no protocol this page speaks (4001): the handshake
    /// decides on a reload; until one, a failed socket.
    Reload,
    /// The band changed (4003, #58): the page reads `/api/lineup` again,
    /// then connects again or shows the notice. Never a reload, never a
    /// failed socket, never "Reconnecting".
    Lineup,
    /// Any other close: a failed socket, which the reconnect tick retries.
    Failed,
}

impl OnClose {
    /// Whether the close counts as a failed socket (the backoff, and the
    /// token check after [`MAX_WS_FAILURES`]).
    pub(super) fn counts(self) -> bool {
        self != Self::Lineup
    }
}

/// [`OnClose`] for a close with `code`.
pub(super) fn on_close(code: u16) -> OnClose {
    match code {
        crate::handshake::CLOSE_RELOAD => OnClose::Reload,
        CLOSE_LINEUP => OnClose::Lineup,
        _ => OnClose::Failed,
    }
}

/// How long a page whose bearer does not play waits before it reads
/// `/api/lineup` again: it has no socket to hear a switch on (#58). A page
/// that does not know the band reads it again as often (#61 review C2).
pub(super) const LINEUP_RECHECK_MS: f64 = 5_000.0;

/// The page's reads of `/api/lineup` (#58): each gets a number, and when
/// the last one started (the tick's [`LineupView::recheck_due`]).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct LineupReads {
    /// The number of the read started last.
    pub issued: u64,
    /// The number of the answer the view took last.
    pub applied: u64,
    /// When the read started last (ms).
    pub last_at: f64,
}

impl LineupReads {
    /// A read starts at `now_ms`: its number.
    pub(super) fn start(&mut self, now_ms: f64) -> u64 {
        self.issued += 1;
        self.last_at = now_ms;
        self.issued
    }

    /// Whether the view takes the answer of read `seq`.
    pub(super) fn take(&mut self, seq: u64) -> bool {
        self.applied = seq;
        true
    }
}

/// What the mixer page knows of the band that plays (#58), from
/// `GET /api/lineup`: read before the page connects, again after the server
/// closed a person's page with [`CLOSE_LINEUP`] or told it of a new band.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum LineupView {
    /// The first read is under way: no socket yet.
    Reading,
    /// A site without a roster: the page as it was before #58 (no badge,
    /// no notice, no switch).
    Legacy,
    /// No answer could be read yet (no network, a server error, a refused
    /// token, a read that timed out): the page connects as before, and its
    /// own retries and token check take over; no badge. It reads again.
    Unknown,
    /// The band that plays and what the bearer may do.
    Roster(iem_core::LineupInfo),
    /// Read again after a [`CLOSE_LINEUP`] close: no socket until the
    /// answer; the band last known stays on the badge.
    Rereading(iem_core::LineupInfo),
}

impl LineupView {
    /// The page's view of `GET /api/lineup`'s answer (`Ok(None)`: no
    /// roster).
    pub(super) fn read(answer: Result<Option<iem_core::LineupInfo>, crate::api::ApiError>) -> Self {
        match answer {
            Ok(Some(info)) => Self::Roster(info),
            Ok(None) => Self::Legacy,
            Err(_) => Self::Unknown,
        }
    }

    /// The view after an answer of `GET /api/lineup`. A read that failed
    /// (#61 review C2: a phone waking, a Wi-Fi handover, a 502 while the
    /// server restarts) keeps the band the page knew, a page reading again
    /// falls back to it; with nothing known the page connects as before
    /// ([`LineupView::Unknown`]) and reads again ([`LineupView::recheck_due`]).
    pub(super) fn apply(
        &mut self,
        answer: Result<Option<iem_core::LineupInfo>, crate::api::ApiError>,
    ) {
        *self = match (std::mem::replace(self, Self::Reading), answer) {
            (_, Ok(known)) => Self::read(Ok(known)),
            (Self::Roster(info) | Self::Rereading(info), Err(_)) => Self::Roster(info),
            (Self::Legacy, Err(_)) => Self::Legacy,
            (_, Err(_)) => Self::Unknown,
        };
    }

    /// The view while `/api/lineup` is read again after a
    /// [`CLOSE_LINEUP`] close.
    pub(super) fn reread(&mut self) {
        *self = match std::mem::replace(self, Self::Reading) {
            Self::Roster(info) | Self::Rereading(info) => Self::Rereading(info),
            _ => Self::Reading,
        };
    }

    /// Whether the page opens its mixer socket: once the band is known,
    /// unless the bearer does not play in it (the server would refuse the
    /// socket; the page shows the notice instead).
    pub(super) fn connects(&self) -> bool {
        match self {
            Self::Reading | Self::Rereading(_) => false,
            Self::Legacy | Self::Unknown => true,
            Self::Roster(info) => info.playing,
        }
    }

    /// The band the notice names, when the bearer does not play in it.
    pub(super) fn notice(&self) -> Option<&iem_core::LineupInfo> {
        match self {
            Self::Roster(info) if !info.playing => Some(info),
            _ => None,
        }
    }

    /// The name on the header badge (a site with a roster only).
    pub(super) fn badge(&self) -> Option<&str> {
        match self {
            Self::Roster(info) | Self::Rereading(info) => Some(info.active.name.as_str()),
            _ => None,
        }
    }

    /// After the server's `LineupChanged`: `active` plays now.
    pub(super) fn now_plays(&mut self, active: iem_core::LineupRef) {
        if let Self::Roster(info) | Self::Rereading(info) = self {
            info.active = active;
        }
    }

    /// The band switch in the settings on `page`, for the bearer `bearer`
    /// (`engineer`: the engineer's token): a bearer who may switch and
    /// plays (one who does not has it on the notice), on their own page; the
    /// engineer on `/engineer` only. It stays while the band is read again
    /// after the switch it made (the 4003 close), so it keeps its answer.
    pub(super) fn switch_here(
        &self,
        page: &str,
        bearer: &str,
        engineer: bool,
    ) -> Option<&iem_core::LineupInfo> {
        let own = if engineer {
            page == "engineer"
        } else {
            page == bearer
        };
        match self {
            Self::Roster(info) | Self::Rereading(info)
                if info.can_switch && info.playing && own =>
            {
                Some(info)
            }
            _ => None,
        }
    }

    /// Whether the page reads the band again at `now_ms`, after the last
    /// read at `last_ms` (#58): a page whose bearer does not play, so the
    /// notice follows a switch back to a band the bearer plays in; and one
    /// that does not know the band (the first read, one after a 4003 close,
    /// an answer it could not read), so a read that hung or failed never
    /// leaves it so (#61 review C2).
    pub(super) fn recheck_due(&self, now_ms: f64, last_ms: f64) -> bool {
        let unsure = matches!(self, Self::Reading | Self::Rereading(_) | Self::Unknown);
        (unsure || self.notice().is_some()) && now_ms - last_ms >= LINEUP_RECHECK_MS
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
    fn a_lineup_close_is_neither_a_failure_nor_a_reload() {
        assert_eq!(CLOSE_LINEUP, 4003);
        let lineup = on_close(CLOSE_LINEUP);
        assert_eq!(lineup, OnClose::Lineup);
        assert_ne!(lineup, OnClose::Reload, "never a reload");
        assert!(
            !lineup.counts(),
            "never a failed socket: no backoff, no token check"
        );
        // The others as before: 4001 asks for a reload (as the handshake
        // allows) and counts until one; a silent client's close (4002), a
        // dropped connection (1006) and a plain close count.
        assert_eq!(on_close(crate::handshake::CLOSE_RELOAD), OnClose::Reload);
        assert!(OnClose::Reload.counts());
        for code in [1000, 1006, 4002] {
            assert_eq!(on_close(code), OnClose::Failed, "{code}");
            assert!(on_close(code).counts(), "{code}");
        }
    }

    fn lineup(id: &str) -> iem_core::LineupRef {
        iem_core::LineupRef {
            id: id.into(),
            name: format!("{id} band"),
        }
    }

    fn info(active: &str, playing: bool, can_switch: bool) -> iem_core::LineupInfo {
        iem_core::LineupInfo {
            active: lineup(active),
            lineups: vec![lineup("main"), lineup("youth")],
            playing,
            leader: false,
            can_switch,
        }
    }

    #[test]
    fn a_site_without_a_roster_keeps_the_page_as_it_was() {
        let legacy = LineupView::read(Ok(None));
        assert_eq!(legacy, LineupView::Legacy);
        assert!(legacy.connects());
        assert_eq!(legacy.badge(), None);
        assert_eq!(legacy.notice(), None);
        assert_eq!(legacy.switch_here("engineer", "engineer", true), None);
        assert!(!legacy.recheck_due(1e12, 0.0));
        // A band change never reaches it; if one did, nothing changes.
        let mut told = legacy.clone();
        told.now_plays(lineup("youth"));
        assert_eq!(told, LineupView::Legacy);
        // An answer that could not be read: the page as before too.
        let unknown = LineupView::read(Err(crate::api::ApiError {
            code: "NETWORK".into(),
            message: "offline".into(),
        }));
        assert_eq!(unknown, LineupView::Unknown);
        assert!(unknown.connects());
        assert_eq!(unknown.badge(), None);
        assert_eq!(unknown.notice(), None);
    }

    #[test]
    fn the_page_connects_once_it_knows_the_bearer_plays() {
        assert!(!LineupView::Reading.connects(), "not before the answer");
        assert_eq!(LineupView::Reading.badge(), None);
        let playing = LineupView::read(Ok(Some(info("youth", true, false))));
        assert_eq!(playing, LineupView::Roster(info("youth", true, false)));
        assert!(playing.connects());
        assert_eq!(playing.badge(), Some("youth band"));
        assert_eq!(playing.notice(), None);
        // Not in the band: the notice, never a socket the server refuses.
        let away = LineupView::read(Ok(Some(info("youth", false, true))));
        assert!(!away.connects());
        assert_eq!(away.notice(), Some(&info("youth", false, true)));
        assert_eq!(away.badge(), Some("youth band"));
    }

    #[test]
    fn a_band_change_is_read_again_with_the_last_band_on_the_badge() {
        let mut v = LineupView::read(Ok(Some(info("main", true, false))));
        v.now_plays(lineup("youth"));
        assert_eq!(v, LineupView::Roster(info("youth", true, false)));
        assert_eq!(v.badge(), Some("youth band"));
        // The 4003 close: no socket and no notice until the answer.
        v.reread();
        assert_eq!(v, LineupView::Rereading(info("youth", true, false)));
        assert!(!v.connects());
        assert_eq!(v.notice(), None);
        assert_eq!(v.badge(), Some("youth band"));
        v.now_plays(lineup("main"));
        assert_eq!(v.badge(), Some("main band"));
        v.reread();
        assert_eq!(v, LineupView::Rereading(info("main", true, false)));
        let mut first = LineupView::Reading;
        first.reread();
        assert_eq!(first, LineupView::Reading);
        let mut unknown = LineupView::Unknown;
        unknown.reread();
        assert_eq!(unknown, LineupView::Reading);
    }

    #[test]
    fn the_switch_is_in_the_settings_of_a_playing_leaders_page_and_of_engineer() {
        let leader = LineupView::Roster(info("main", true, true));
        assert_eq!(
            leader.switch_here("member3", "member3", false),
            Some(&info("main", true, true))
        );
        assert_eq!(leader.switch_here("member1", "member3", false), None);
        // The engineer: on /engineer, not on a person's page they opened.
        assert!(leader.switch_here("engineer", "engineer", true).is_some());
        assert_eq!(leader.switch_here("member3", "engineer", true), None);
        let no_right = LineupView::Roster(info("main", true, false));
        assert_eq!(no_right.switch_here("member4", "member4", false), None);
        // A leader who does not play has it on the notice instead.
        let away = LineupView::Roster(info("youth", false, true));
        assert_eq!(away.switch_here("member1", "member1", false), None);
        // Read again after the 4003 close its own switch caused: it stays,
        // with its answer.
        let rereading = LineupView::Rereading(info("youth", true, true));
        assert_eq!(
            rereading.switch_here("member3", "member3", false),
            Some(&info("youth", true, true))
        );
        assert_eq!(rereading.switch_here("member1", "member3", false), None);
        assert_eq!(
            LineupView::Reading.switch_here("engineer", "engineer", true),
            None
        );
    }

    #[test]
    fn a_page_that_does_not_play_reads_the_band_again_every_five_seconds() {
        let away = LineupView::Roster(info("youth", false, false));
        assert!(!away.recheck_due(14_999.0, 10_000.0));
        assert!(away.recheck_due(15_000.0, 10_000.0));
        let playing = LineupView::Roster(info("youth", true, false));
        assert!(!playing.recheck_due(1e12, 0.0), "its socket hears a switch");
        assert!(
            !LineupView::Legacy.recheck_due(1e12, 0.0),
            "no band to read"
        );
    }

    /// #61 review C2: a page that does not know the band (the first read,
    /// one after a 4003 close, an answer it could not read) reads it again
    /// every five seconds: a read that hangs or fails never leaves it so.
    #[test]
    fn a_page_that_does_not_know_the_band_reads_it_again_every_five_seconds() {
        for view in [
            LineupView::Reading,
            LineupView::Rereading(info("youth", true, false)),
            LineupView::Unknown,
        ] {
            assert!(!view.recheck_due(14_999.0, 10_000.0), "{view:?}");
            assert!(view.recheck_due(15_000.0, 10_000.0), "{view:?}");
        }
    }

    /// #61 review M3: the page's reads are numbered, so an older answer
    /// (a slow read started before a switch) never overwrites a newer one.
    #[test]
    fn an_older_answer_never_overwrites_a_newer_one() {
        let mut reads = LineupReads::default();
        let first = reads.start(1_000.0);
        let second = reads.start(2_000.0);
        assert_eq!(reads.last_at, 2_000.0, "the tick counts from the last read");
        assert!(second > first);
        assert!(reads.take(second), "the newest answer is taken");
        assert!(!reads.take(first), "an older one that comes later is not");
        // In order, each is taken.
        let third = reads.start(3_000.0);
        let fourth = reads.start(4_000.0);
        assert!(reads.take(third));
        assert!(reads.take(fourth));
        assert!(!reads.take(third));
        assert_eq!(reads.applied, fourth);
    }

    fn failed() -> Result<Option<iem_core::LineupInfo>, crate::api::ApiError> {
        Err(crate::api::ApiError {
            code: "HTTP_502".into(),
            message: "Server error: 502".into(),
        })
    }

    /// #61 review C2: a read that fails (a phone waking, a Wi-Fi handover, a
    /// 502 while the server restarts) keeps the band the page knew: a notice
    /// page stays the notice with no socket, a page reading again after a
    /// 4003 close falls back to the band it knew.
    #[test]
    fn a_failed_read_keeps_the_band_the_page_knew() {
        let mut away = LineupView::Roster(info("youth", false, false));
        away.apply(failed());
        assert_eq!(away, LineupView::Roster(info("youth", false, false)));
        assert!(!away.connects(), "no socket the server would refuse");
        assert!(away.notice().is_some());
        let mut rereading = LineupView::Rereading(info("youth", true, true));
        rereading.apply(failed());
        assert_eq!(rereading, LineupView::Roster(info("youth", true, true)));
        let mut legacy = LineupView::Legacy;
        legacy.apply(failed());
        assert_eq!(legacy, LineupView::Legacy);
        // Nothing known yet: the page connects as before and reads again.
        let mut first = LineupView::Reading;
        first.apply(failed());
        assert_eq!(first, LineupView::Unknown);
        let mut unknown = LineupView::Unknown;
        unknown.apply(failed());
        assert_eq!(unknown, LineupView::Unknown);
        // An answer replaces whatever the page knew.
        let mut known = LineupView::Unknown;
        known.apply(Ok(Some(info("main", true, false))));
        assert_eq!(known, LineupView::Roster(info("main", true, false)));
        known.apply(Ok(None));
        assert_eq!(known, LineupView::Legacy);
        let mut back = LineupView::Rereading(info("main", true, false));
        back.apply(Ok(Some(info("youth", false, true))));
        assert_eq!(back, LineupView::Roster(info("youth", false, true)));
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
