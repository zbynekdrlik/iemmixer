//! Whether a WebSocket session's client is still there (#10, live run 3).
//! On the band's public path the runner lost its sockets and the server
//! logged no end of their sessions: the lost listen sessions streamed on for
//! at least a minute and a half, each holding a listen tap, and a lost mixer
//! session keeps its mix's solo from clearing. So the mixer and listen
//! sessions ping their client every [`PING_EVERY`] ([`Keepalive`]) and end
//! once they heard nothing from it (no command, no pong) for [`SILENT_FOR`],
//! with [`CLOSE_SILENT`]; and every send of theirs gets at most
//! [`SEND_WITHIN`] ([`send`]), so a client that reads nothing cannot hold a
//! session inside a send, where no ping is looked at. Every browser, Node's
//! `ws` and tungstenite answer a ping by themselves; the page's own watchdog
//! ends a socket silent for 30 s from its side.

use std::future::Future;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::ws::{CloseFrame, Message};

/// How often a session pings its client.
pub const PING_EVERY: Duration = Duration::from_secs(10);
/// A client silent this long (three ping periods) is gone.
pub const SILENT_FOR: Duration = Duration::from_secs(30);
/// The longest one send may take: a client that reads nothing that long is gone too.
pub const SEND_WITHIN: Duration = SILENT_FOR;
/// The longest the close to a silent client may take (it is likely gone).
pub const CLOSE_WITHIN: Duration = Duration::from_secs(1);
/// The close code of a session that ended because its client was silent
/// (a dropped connection reads 1006 at the client; the page reconnects on
/// either, it only reloads on `mixer_ws::CLOSE_RELOAD`).
pub const CLOSE_SILENT: u16 = 4002;

/// When a session last heard from its client, and how long a silence may last.
#[derive(Debug, Clone, Copy)]
pub struct Heard {
    at: Instant,
    limit: Duration,
}

impl Heard {
    /// A session that began at `now` (the upgrade was the client's last word),
    /// whose client is gone after `limit` of silence.
    pub fn new(now: Instant, limit: Duration) -> Self {
        Self { at: now, limit }
    }

    /// The client sent something at `now` (a command, a pong, a close).
    pub fn heard(&mut self, now: Instant) {
        self.at = now;
    }

    /// How long the client has been silent at `now` (none for a clock read before its last word).
    pub fn silent(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.at)
    }

    /// Whether the client is gone at `now`: silent for the limit or longer.
    pub fn gone(&self, now: Instant) -> bool {
        self.silent(now) >= self.limit
    }
}

/// What a session's ping clock asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// Ping the client.
    Ping,
    /// The client has been silent this long, the limit or more: end the session.
    Gone(Duration),
}

/// A session's ping clock and what it heard from its client.
#[derive(Debug)]
pub struct Keepalive {
    heard: Heard,
    ping: tokio::time::Interval,
}

impl Default for Keepalive {
    /// [`PING_EVERY`] and [`SILENT_FOR`].
    fn default() -> Self {
        Self::with(PING_EVERY, SILENT_FOR)
    }
}

impl Keepalive {
    /// A ping every `every` (the first one `every` after now; a late one does
    /// not bunch up), gone after `silent_for`.
    pub fn with(every: Duration, silent_for: Duration) -> Self {
        let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        Self {
            heard: Heard::new(Instant::now(), silent_for),
            ping,
        }
    }

    /// The client sent something now.
    pub fn heard(&mut self) {
        self.heard.heard(Instant::now());
    }

    /// Waits for the next ping time and says what it asks for. Cancel-safe
    /// (a session's `select!`): only the interval's tick is awaited.
    pub async fn due(&mut self) -> Due {
        self.ping.tick().await;
        let now = Instant::now();
        if self.heard.gone(now) {
            Due::Gone(self.heard.silent(now))
        } else {
            Due::Ping
        }
    }
}

/// How a bounded send ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    Done,
    Failed,
    /// It took longer than its limit (the client reads nothing).
    Stalled,
}

impl Sent {
    /// Whether the session goes on; a stalled send is logged.
    pub fn ok(self) -> bool {
        match self {
            Sent::Done => true,
            Sent::Failed => false,
            Sent::Stalled => {
                tracing::warn!("WebSocket send stalled: closing");
                false
            }
        }
    }
}

/// `send`, given at most `limit` to finish.
pub async fn within<E>(limit: Duration, send: impl Future<Output = Result<(), E>>) -> Sent {
    match tokio::time::timeout(limit, send).await {
        Ok(Ok(())) => Sent::Done,
        Ok(Err(_)) => Sent::Failed,
        Err(_) => Sent::Stalled,
    }
}

/// A session's send (`socket.send(…)`), given at most [`SEND_WITHIN`]:
/// whether the session goes on.
pub async fn send<E>(send: impl Future<Output = Result<(), E>>) -> bool {
    within(SEND_WITHIN, send).await.ok()
}

/// The ping a session sends.
pub fn ping() -> Message {
    Message::Ping(Bytes::new())
}

/// The close a session sends its silent client.
pub fn silent_close() -> Message {
    Message::Close(Some(CloseFrame {
        code: CLOSE_SILENT,
        reason: "silent".into(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_silence_limit_is_three_ping_periods_and_a_send_gets_as_long() {
        assert_eq!(PING_EVERY, Duration::from_secs(10));
        assert_eq!(SILENT_FOR, PING_EVERY * 3);
        assert_eq!(SEND_WITHIN, SILENT_FOR);
        assert_eq!(CLOSE_WITHIN, Duration::from_secs(1));
    }

    #[test]
    fn a_client_is_gone_once_silent_for_the_limit_and_not_a_moment_before() {
        let t0 = Instant::now();
        let h = Heard::new(t0, SILENT_FOR);
        assert!(!h.gone(t0));
        assert!(!h.gone(t0 + SILENT_FOR - Duration::from_millis(1)));
        assert!(h.gone(t0 + SILENT_FOR));
        assert!(h.gone(t0 + SILENT_FOR + Duration::from_secs(5)));
        assert_eq!(
            h.silent(t0 + Duration::from_secs(7)),
            Duration::from_secs(7)
        );
    }

    #[test]
    fn anything_heard_starts_the_silence_again() {
        let t0 = Instant::now();
        let mut h = Heard::new(t0, SILENT_FOR);
        h.heard(t0 + Duration::from_secs(25));
        assert_eq!(
            h.silent(t0 + Duration::from_secs(26)),
            Duration::from_secs(1)
        );
        assert!(!h.gone(t0 + SILENT_FOR));
        assert!(!h.gone(t0 + Duration::from_secs(54)));
        assert!(h.gone(t0 + Duration::from_secs(55)));
    }

    #[test]
    fn a_clock_read_before_the_last_word_is_no_silence() {
        let t0 = Instant::now();
        let h = Heard::new(t0 + Duration::from_secs(1), SILENT_FOR);
        assert_eq!(h.silent(t0), Duration::ZERO);
        assert!(!h.gone(t0));
    }

    #[tokio::test]
    async fn the_clock_asks_for_a_ping_while_the_client_answers() {
        let t0 = tokio::time::Instant::now();
        let mut k = Keepalive::with(Duration::from_millis(20), Duration::from_secs(10));
        assert_eq!(k.due().await, Due::Ping);
        assert!(
            t0.elapsed() >= Duration::from_millis(20),
            "the first ping comes a period after the start"
        );
        assert_eq!(k.due().await, Due::Ping);
    }

    #[tokio::test]
    async fn the_clock_names_a_silent_client_gone_with_its_silence() {
        let mut k = Keepalive::with(Duration::from_millis(20), Duration::from_millis(100));
        tokio::time::sleep(Duration::from_millis(150)).await;
        match k.due().await {
            Due::Gone(silent) => assert!(silent >= Duration::from_millis(150), "silent {silent:?}"),
            Due::Ping => panic!("a client silent past the limit is gone"),
        }
    }

    #[tokio::test]
    async fn a_word_from_the_client_keeps_it_there() {
        let mut k = Keepalive::with(Duration::from_millis(20), Duration::from_millis(300));
        tokio::time::sleep(Duration::from_millis(350)).await;
        k.heard();
        assert_eq!(k.due().await, Due::Ping);
    }

    #[tokio::test]
    async fn the_default_clock_is_the_sessions_one() {
        let k = Keepalive::default();
        assert_eq!(k.ping.period(), PING_EVERY);
        assert_eq!(k.heard.limit, SILENT_FOR);
    }

    #[tokio::test]
    async fn a_bounded_send_is_done_failed_or_stalled() {
        let ms = Duration::from_millis(20);
        assert_eq!(within(ms, async { Ok::<(), ()>(()) }).await, Sent::Done);
        assert_eq!(within(ms, async { Err::<(), ()>(()) }).await, Sent::Failed);
        assert_eq!(
            within(ms, std::future::pending::<Result<(), ()>>()).await,
            Sent::Stalled
        );
        assert!(Sent::Done.ok());
        assert!(!Sent::Failed.ok());
        assert!(!Sent::Stalled.ok());
    }

    #[tokio::test]
    async fn a_session_send_goes_on_only_when_it_went_out() {
        assert!(send(async { Ok::<(), ()>(()) }).await);
        assert!(!send(async { Err::<(), ()>(()) }).await);
    }

    #[test]
    fn the_session_pings_empty_and_closes_a_silent_client_with_its_own_code() {
        assert!(matches!(ping(), Message::Ping(b) if b.is_empty()));
        match silent_close() {
            Message::Close(Some(frame)) => {
                assert_eq!(frame.code, CLOSE_SILENT);
                assert_eq!(frame.code, 4002);
                assert_eq!(frame.reason.as_str(), "silent");
            }
            other => panic!("not a close: {other:?}"),
        }
        assert_ne!(CLOSE_SILENT, crate::mixer_ws::CLOSE_RELOAD);
    }
}
