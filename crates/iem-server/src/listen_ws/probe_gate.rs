//! The listen probe's gate for one `&hil=1` session (S7 design note §6, #10):
//! probe frames go out as they come; the slot's own (silent) frames wait
//! until no probe frame came for [`PROBE_HOLD`], so the player gets one
//! stream. The first probe frame opens a burst and the first slot frame
//! after the hold leaves it: the session tells the socket `probe` and
//! `listening` there. Pure (the caller reads the clock), mutated.

use std::time::{Duration, Instant};

/// How long the slot's own frames stay dropped after the last probe frame:
/// five 20 ms frames, so a probe frame a few frames late never lets a
/// silent one in between.
pub const PROBE_HOLD: Duration = Duration::from_millis(100);

/// What happens to one of the slot's own frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    /// Inside a burst: the probe frames stand in for it.
    Drop,
    /// Sent, as without a probe.
    Send,
    /// Sent, and it ends the burst: the socket is told `listening` first.
    SendLeaving,
}

#[derive(Debug, Default)]
pub struct ProbeGate {
    last_probe: Option<Instant>,
    in_burst: bool,
}

impl ProbeGate {
    /// A probe frame at `now`: always sent; `true` when it opens a burst (the
    /// socket is told `probe` first).
    pub fn probe(&mut self, now: Instant) -> bool {
        self.last_probe = Some(now);
        !std::mem::replace(&mut self.in_burst, true)
    }

    /// One of the slot's own frames at `now`: dropped while the last probe
    /// frame came less than [`PROBE_HOLD`] before; the first one after a
    /// burst leaves it.
    pub fn listen(&mut self, now: Instant) -> Pass {
        let held = self
            .last_probe
            .is_some_and(|t| now.saturating_duration_since(t) < PROBE_HOLD);
        if held {
            Pass::Drop
        } else if std::mem::take(&mut self.in_burst) {
            Pass::SendLeaving
        } else {
            Pass::Send
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    #[test]
    fn a_listen_frame_passes_while_no_probe_came() {
        let t0 = Instant::now();
        let mut g = ProbeGate::default();
        assert_eq!(g.listen(t0), Pass::Send);
        assert_eq!(g.listen(at(t0, 20)), Pass::Send, "and goes on passing");
    }

    #[test]
    fn after_a_probe_frame_listen_frames_wait_probe_hold() {
        assert_eq!(PROBE_HOLD, Duration::from_millis(100));
        let t0 = Instant::now();
        let mut g = ProbeGate::default();
        g.probe(t0);
        assert_eq!(g.listen(t0), Pass::Drop);
        assert_eq!(
            g.listen(at(t0, 99)),
            Pass::Drop,
            "99 ms after the probe frame"
        );
        assert_eq!(
            g.listen(at(t0, 100)),
            Pass::SendLeaving,
            "PROBE_HOLD after it"
        );
        // Every probe frame starts the hold again.
        g.probe(at(t0, 200));
        g.probe(at(t0, 250));
        assert_eq!(
            g.listen(at(t0, 349)),
            Pass::Drop,
            "99 ms after the last one"
        );
        assert_eq!(g.listen(at(t0, 350)), Pass::SendLeaving);
        // A frame whose time was read before the probe frame's waits too.
        g.probe(at(t0, 500));
        assert_eq!(g.listen(at(t0, 490)), Pass::Drop);
    }

    #[test]
    fn the_first_probe_frame_enters_a_burst_and_the_first_listen_frame_after_it_leaves_it() {
        let t0 = Instant::now();
        let mut g = ProbeGate::default();
        assert!(g.probe(t0), "the first probe frame opens a burst");
        assert!(!g.probe(at(t0, 20)), "the next ones are inside it");
        assert_eq!(
            g.listen(at(t0, 200)),
            Pass::SendLeaving,
            "the first frame of the slot's own after the hold leaves it"
        );
        assert_eq!(g.listen(at(t0, 220)), Pass::Send, "then as before");
        assert!(g.probe(at(t0, 300)), "a new burst");
    }
}
