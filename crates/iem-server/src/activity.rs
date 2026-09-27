//! The band-activity alarm while developing (program spec §4.2): peaks of the
//! stage inputs above −50 dBFS for at least 120 s within the last 300 s turn
//! it on; it shows a banner with "Back to REAPER" on engineer pages and
//! pushes one notice to the engineer's devices (never to an alarm
//! recipient). Pure: fed with instants and the loudest watched input peak of
//! each meter frame.

use std::collections::VecDeque;
use std::time::Instant;

use iem_core::ActivityConfig;

use crate::site_view::SiteView;

/// The inputs band activity watches, as indices into a meter frame's inputs
/// (topology order): the site's `[activity] inputs`, or every input of
/// category `mics` when that list is empty. The program input carries
/// signal while the band is silent (S1a), so only the stage counts. Listed
/// ids the topology does not have are returned second, to be reported, and
/// are left out.
pub fn watched_inputs(inputs: &[String], site: &SiteView) -> (Vec<usize>, Vec<String>) {
    let watched = site
        .inputs
        .iter()
        .enumerate()
        .filter(|(_, i)| {
            if inputs.is_empty() {
                i.category == "mics"
            } else {
                inputs.contains(&i.id.0)
            }
        })
        .map(|(k, _)| k)
        .collect();
    let unknown = inputs
        .iter()
        .filter(|id| site.input(id).is_none())
        .cloned()
        .collect();
    (watched, unknown)
}

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
    use crate::site_view::tests::test_view;
    use std::time::Duration;

    #[test]
    fn the_mics_are_watched_by_default() {
        let v = test_view();
        let (watched, unknown) = watched_inputs(&[], &v);
        let ids: Vec<&str> = watched.iter().map(|&k| v.inputs[k].id.0.as_str()).collect();
        assert_eq!(
            ids,
            [
                "mic1", "mic2", "mic3", "mic4", "mic5", "mic6", "mic7", "mic8", "mic9", "mic10",
                "keys"
            ]
        );
        assert!(unknown.is_empty());
    }

    #[test]
    fn a_listed_set_is_watched_in_topology_order_and_unknown_ids_are_left_out() {
        let v = test_view();
        let pos = |id: &str| v.inputs.iter().position(|i| i.id.0 == id).unwrap();
        let list = [
            "content".to_string(),
            "ghost".to_string(),
            "mic2".to_string(),
        ];
        let (watched, unknown) = watched_inputs(&list, &v);
        assert_eq!(watched, [pos("mic2"), pos("content")]);
        assert_eq!(unknown, ["ghost"]);
        let (none, unknown) = watched_inputs(&["ghost".to_string()], &v);
        assert!(none.is_empty(), "never the mics default instead");
        assert_eq!(unknown, ["ghost"]);
    }

    fn cfg() -> ActivityConfig {
        ActivityConfig {
            threshold_dbfs: -50.0,
            window_s: 300,
            sustain_s: 120,
            inputs: Vec::new(),
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
            inputs: Vec::new(),
        };
        let mut a = BandActivity::new(&small, t);
        let at_threshold = 10f64.powf(-50.0 / 20.0) as f32;
        assert_eq!(a.observe(at(t, 0), at_threshold), None);
        assert_eq!(a.observe(at(t, 1), at_threshold), None);
        assert_eq!(a.observe(at(t, 2), 0.0032), None);
        assert_eq!(a.observe(at(t, 3), 0.0032), Some(true));
    }
}
