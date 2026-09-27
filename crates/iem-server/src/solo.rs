//! Solo clean-up (X2, the server's half; S3 hand-off): a mix's solo is
//! cleared 10 s after the last page connection on that mix closed; a
//! reconnect within the grace keeps it. The engine clears every solo itself
//! 10 s after the controller (the server) left.

use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const SOLO_GRACE: Duration = Duration::from_secs(10);

#[derive(Debug, Default)]
pub struct SoloJanitor {
    open: HashMap<String, usize>,
    left: HashMap<String, Instant>,
}

impl SoloJanitor {
    /// A page connection on `mix` opened.
    pub fn connected(&mut self, mix: &str) {
        *self.open.entry(mix.to_string()).or_default() += 1;
        self.left.remove(mix);
    }

    /// A page connection on `mix` closed at `now`.
    pub fn disconnected(&mut self, mix: &str, now: Instant) {
        let Some(n) = self.open.get_mut(mix) else {
            return;
        };
        *n = n.saturating_sub(1);
        if *n == 0 {
            self.open.remove(mix);
            self.left.insert(mix.to_string(), now);
        }
    }

    /// Mixes whose solo must be cleared now (each once).
    pub fn due(&mut self, now: Instant) -> Vec<String> {
        let mut due: Vec<String> = self
            .left
            .iter()
            .filter(|(_, t)| now.saturating_duration_since(**t) >= SOLO_GRACE)
            .map(|(m, _)| m.clone())
            .collect();
        due.sort();
        for m in &due {
            self.left.remove(m);
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_solo_is_cleared_ten_seconds_after_the_last_connection_closed() {
        let t0 = Instant::now();
        let mut j = SoloJanitor::default();
        j.connected("member1");
        j.connected("member1");
        j.disconnected("member1", t0);
        assert!(
            j.due(t0 + Duration::from_secs(60)).is_empty(),
            "a tab is still open"
        );
        j.disconnected("member1", t0);
        assert!(j.due(t0 + Duration::from_millis(9_999)).is_empty());
        assert_eq!(j.due(t0 + SOLO_GRACE), ["member1"]);
        assert!(j.due(t0 + Duration::from_secs(60)).is_empty(), "only once");
    }

    #[test]
    fn a_reconnect_within_the_grace_keeps_the_solo() {
        let t0 = Instant::now();
        let mut j = SoloJanitor::default();
        j.connected("member2");
        j.disconnected("member2", t0);
        j.connected("member2");
        assert!(j.due(t0 + Duration::from_secs(30)).is_empty());
        // An unknown close changes nothing.
        j.disconnected("ghost", t0);
        assert!(j.due(t0 + Duration::from_secs(30)).is_empty());
        j.disconnected("member2", t0);
        j.connected("member3");
        j.disconnected("member3", t0);
        assert_eq!(j.due(t0 + SOLO_GRACE), ["member2", "member3"]);
    }
}
