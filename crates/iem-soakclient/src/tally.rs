//! The counts of one run and the summary they make (S7 design note §4): the
//! socket threads count into a [`Tally`], and [`Tally::summary`] turns it
//! into the [`Summary`] written every `write_every` and at the end. Pure:
//! every time is passed in.

use std::time::{Duration, Instant};

use crate::{Event, GapCount, Gaps, Reason, Summary, ms};

/// Samples per channel in one listen frame: 20 ms at 48 kHz, the engine's
/// frame, which the server encodes (pinned to `FRAME_48K` in the tests).
pub const SAMPLES: usize = 960;
/// The `AudioStatus` of a listen with nothing to hear.
const NO_SOURCE: &str = "no_source";

/// The counts of one run so far.
#[derive(Debug, Clone, Default)]
pub struct Tally {
    /// The first open of either socket: the run's clock starts there.
    opened: Option<Instant>,
    /// The first `ListenStart`.
    listen_started: Option<Instant>,
    first_frame_ms: Option<u64>,
    gaps: Gaps,
    frames: u64,
    decode_errors: u64,
    meter_frames: u64,
    no_source: u64,
    error: Option<Reason>,
}

impl Tally {
    /// A socket opened at `now` (each is opened once).
    pub fn opened(&mut self, now: Instant) {
        self.opened.get_or_insert(now);
    }

    /// The listen socket sent `ListenStart` at `now`.
    pub fn listen_started(&mut self, now: Instant) {
        self.listen_started.get_or_insert(now);
    }

    /// A listen frame arrived at `now` and decoded to `samples` per channel
    /// (`None`: Opus refused it). Only a whole frame is a frame; anything
    /// else is a decode error and, for the gap clock, no frame.
    pub fn frame(&mut self, now: Instant, samples: Option<usize>) {
        if samples != Some(SAMPLES) {
            self.decode_errors += 1;
            return;
        }
        self.frames += 1;
        self.gaps.frame(now);
        if self.frames == 1 {
            self.first_frame_ms = self
                .listen_started
                .map(|start| ms(now.saturating_duration_since(start)));
        }
    }

    /// A text frame from the server.
    pub fn text(&mut self, event: &Event) {
        match event {
            Event::Meters => self.meter_frames += 1,
            Event::AudioStatus(status) if status == NO_SOURCE => self.no_source += 1,
            Event::AudioStatus(_) | Event::Other => {}
        }
    }

    /// The run ends early for `reason`; the first reason stays.
    pub fn fail(&mut self, reason: Reason) {
        self.error.get_or_insert(reason);
    }

    pub fn error(&self) -> Option<Reason> {
        self.error
    }

    /// When a run of `seconds` ends: that long after the first open.
    pub fn ends_at(&self, seconds: u64) -> Option<Instant> {
        self.opened
            .map(|opened| opened + Duration::from_secs(seconds))
    }

    /// The summary if the run ended at `now`.
    pub fn summary(&self, now: Instant, complete: bool) -> Summary {
        let (gaps, seconds) = match self.opened {
            Some(opened) => (
                self.gaps.end(opened, now),
                now.saturating_duration_since(opened).as_secs_f64(),
            ),
            None => (GapCount::default(), 0.0),
        };
        Summary {
            complete,
            seconds,
            frames: self.frames,
            expected_frames: self.gaps.expected(now),
            decode_errors: self.decode_errors,
            gaps: gaps.gaps,
            max_gap_ms: gaps.max_gap_ms,
            first_frame_ms: self.first_frame_ms,
            meter_frames: self.meter_frames,
            no_source: self.no_source,
            error: self.error,
            ..Summary::default()
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
    fn samples_is_the_engines_listen_frame() {
        // The server encodes the engine's 20 ms frames (X4).
        assert_eq!(SAMPLES, iem_engine_proto::FRAME_48K);
    }

    #[test]
    fn a_tally_before_any_open_is_the_default_summary() {
        let t0 = Instant::now();
        let tally = Tally::default();
        assert_eq!(tally.summary(at(t0, 5_000), false), Summary::default());
        assert_eq!(tally.ends_at(3), None);
        let complete = Summary {
            complete: true,
            ..Summary::default()
        };
        assert_eq!(tally.summary(t0, true), complete);
    }

    #[test]
    fn the_run_clock_starts_at_the_first_open() {
        let t0 = Instant::now();
        let mut tally = Tally::default();
        tally.opened(at(t0, 100));
        // The other socket's open does not move the clock.
        tally.opened(at(t0, 200));
        assert_eq!(tally.ends_at(3), Some(at(t0, 3_100)));
        let s = tally.summary(at(t0, 2_600), false);
        assert_eq!(s.seconds, 2.5);
        // Nothing reopens a socket (#10): the summary's reconnects stay 0.
        assert_eq!(s.reconnects, 0);
        assert!(!s.complete);
        // No frame since the first open: one gap as long as the run.
        assert_eq!((s.gaps, s.max_gap_ms), (1, 2_500));
        assert_eq!(
            (s.frames, s.expected_frames, s.first_frame_ms),
            (0, 0, None)
        );
    }

    #[test]
    fn frames_of_960_samples_count_and_anything_else_is_a_decode_error() {
        let t0 = Instant::now();
        let mut tally = Tally::default();
        tally.opened(t0);
        tally.listen_started(at(t0, 10));
        for bad in [None, Some(SAMPLES - 1), Some(SAMPLES + 1), Some(0)] {
            tally.frame(at(t0, 20), bad);
        }
        tally.frame(at(t0, 50), Some(SAMPLES));
        tally.frame(at(t0, 70), Some(SAMPLES));
        let s = tally.summary(at(t0, 110), false);
        assert_eq!(s.decode_errors, 4);
        assert_eq!(s.frames, 2);
        assert_eq!(s.first_frame_ms, Some(40));
        // One per 20 ms from the first good frame: 50, 70, 90 and 110 ms.
        assert_eq!(s.expected_frames, 4);
        // Waits of 20 and 40 ms (to the end): no gap.
        assert_eq!((s.gaps, s.max_gap_ms), (0, 40));
    }

    #[test]
    fn meters_and_no_source_are_counted_and_nothing_else() {
        let mut tally = Tally::default();
        let events = [
            Event::Meters,
            Event::Meters,
            Event::AudioStatus("no_source".to_owned()),
            Event::AudioStatus("listening".to_owned()),
            Event::AudioStatus("stopped".to_owned()),
            Event::Other,
        ];
        for event in &events {
            tally.text(event);
        }
        let counted = Summary {
            meter_frames: 2,
            no_source: 1,
            ..Summary::default()
        };
        assert_eq!(tally.summary(Instant::now(), false), counted);
    }

    #[test]
    fn the_first_reason_stays() {
        let mut tally = Tally::default();
        assert_eq!(tally.error(), None);
        tally.fail(Reason::ConnectionLost);
        tally.fail(Reason::ServerGone);
        assert_eq!(tally.error(), Some(Reason::ConnectionLost));
        let s = tally.summary(Instant::now(), false);
        assert_eq!(s.error, Some(Reason::ConnectionLost));
    }
}
