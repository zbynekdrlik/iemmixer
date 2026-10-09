//! Whether a WebSocket session's client is still there (#10, live run 3).
//! Behind the public tunnel a client can be gone while the server's side of
//! its connection stays open: the tunnel lost the runner's sockets and never
//! closed the server's, and those sessions streamed on for minutes, each
//! holding a listen tap and keeping its mix's solo from clearing. So the
//! mixer and listen sessions ping their client every [`PING_EVERY`] and end
//! once they heard nothing from it (no command, no pong) for [`SILENT_FOR`].
//! Every browser answers a ping by itself; the page's own watchdog ends a
//! socket silent for 30 s from its side. Pure (the caller reads the clock),
//! mutated.

use std::time::{Duration, Instant};

/// How often a session pings its client.
pub const PING_EVERY: Duration = Duration::from_secs(10);
/// A client silent this long (three ping periods) is gone.
pub const SILENT_FOR: Duration = Duration::from_secs(30);

/// When a session last heard from its client.
#[derive(Debug, Clone, Copy)]
pub struct Heard {
    at: Instant,
}

impl Heard {
    /// A session that began at `now` (the upgrade was the client's last word).
    pub fn new(now: Instant) -> Self {
        Self { at: now }
    }

    /// The client sent something at `now` (a command, a pong, a close).
    pub fn heard(&mut self, now: Instant) {
        self.at = now;
    }

    /// How long the client has been silent at `now` (none for a clock read before its last word).
    pub fn silent(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.at)
    }

    /// Whether the client is gone at `now`: silent for [`SILENT_FOR`] or longer.
    pub fn gone(&self, now: Instant) -> bool {
        self.silent(now) >= SILENT_FOR
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_silence_limit_is_three_ping_periods() {
        assert_eq!(PING_EVERY, Duration::from_secs(10));
        assert_eq!(SILENT_FOR, Duration::from_secs(30));
    }

    #[test]
    fn a_client_is_gone_once_silent_for_the_limit_and_not_a_moment_before() {
        let t0 = Instant::now();
        let h = Heard::new(t0);
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
        let mut h = Heard::new(t0);
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
        let h = Heard::new(t0 + Duration::from_secs(1));
        assert_eq!(h.silent(t0), Duration::ZERO);
        assert!(!h.gone(t0));
    }
}
