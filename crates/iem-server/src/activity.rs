//! The band-activity alarm while developing (program spec §4.2): input peaks
//! above −50 dBFS for at least 120 s within the last 300 s turn it on; it
//! shows a banner with "Back to REAPER" on engineer pages and pushes one
//! alarm. Pure: fed with instants and the loudest input peak of each meter
//! frame.

use std::collections::VecDeque;
use std::time::Instant;

use iem_core::ActivityConfig;

#[derive(Debug, Clone)]
pub struct BandActivity {
    threshold: f32,
    window: u64,
    sustain: usize,
    start: Instant,
    /// Seconds since `start` with activity, oldest first.
    seconds: VecDeque<u64>,
    active: bool,
}

impl BandActivity {
    pub fn new(cfg: &ActivityConfig, start: Instant) -> Self {
        Self {
            threshold: 10f64.powf(cfg.threshold_dbfs / 20.0) as f32,
            window: cfg.window_s,
            sustain: usize::try_from(cfg.sustain_s).unwrap_or(usize::MAX),
            start,
            seconds: VecDeque::new(),
            active: false,
        }
    }

    /// One frame's loudest input peak at `now`; `Some` when the alarm
    /// turned on (`true`) or off (`false`).
    pub fn observe(&mut self, now: Instant, peak: f32) -> Option<bool> {
        let sec = now.saturating_duration_since(self.start).as_secs();
        if peak > self.threshold && self.seconds.back() != Some(&sec) {
            self.seconds.push_back(sec);
        }
        while self
            .seconds
            .front()
            .is_some_and(|s| sec.saturating_sub(*s) >= self.window)
        {
            self.seconds.pop_front();
        }
        let on = self.seconds.len() >= self.sustain;
        if on == self.active {
            None
        } else {
            self.active = on;
            Some(on)
        }
    }

    pub fn active(&self) -> bool {
        self.active
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn cfg() -> ActivityConfig {
        ActivityConfig {
            threshold_dbfs: -50.0,
            window_s: 300,
            sustain_s: 120,
        }
    }

    const LOUD: f32 = 0.01; // −40 dBFS

    fn at(t: Instant, s: u64) -> Instant {
        t + Duration::from_secs(s)
    }

    #[test]
    fn two_minutes_of_playing_within_five_turn_the_alarm_on_once() {
        let t = Instant::now();
        let mut a = BandActivity::new(&cfg(), t);
        for s in 0..119 {
            // Several frames per second count once.
            assert_eq!(a.observe(at(t, s), LOUD), None);
            assert_eq!(a.observe(at(t, s) + Duration::from_millis(500), LOUD), None);
        }
        assert!(!a.active());
        assert_eq!(a.observe(at(t, 119), LOUD), Some(true));
        assert!(a.active());
        assert_eq!(a.observe(at(t, 120), LOUD), None);
    }

    #[test]
    fn scattered_activity_counts_within_the_window_and_decays() {
        let t = Instant::now();
        let mut a = BandActivity::new(&cfg(), t);
        // 120 active seconds spread over 240 s.
        let mut turned = None;
        for s in 0..240 {
            let peak = if s % 2 == 0 { LOUD } else { 0.0 };
            if let Some(v) = a.observe(at(t, s), peak) {
                turned = Some((s, v));
            }
        }
        assert_eq!(turned, Some((238, true)));
        // Silence: the oldest active second leaves the window at 300 s.
        assert_eq!(a.observe(at(t, 299), 0.0), None);
        assert_eq!(a.observe(at(t, 300), 0.0), Some(false));
        assert!(!a.active());
    }

    #[test]
    fn peaks_at_the_threshold_do_not_count() {
        let t = Instant::now();
        let small = ActivityConfig {
            threshold_dbfs: -50.0,
            window_s: 10,
            sustain_s: 2,
        };
        let mut a = BandActivity::new(&small, t);
        let at_threshold = 10f64.powf(-50.0 / 20.0) as f32;
        assert_eq!(a.observe(at(t, 0), at_threshold), None);
        assert_eq!(a.observe(at(t, 1), at_threshold), None);
        assert_eq!(a.observe(at(t, 2), 0.0032), None);
        assert_eq!(a.observe(at(t, 3), 0.0032), Some(true));
    }
}
