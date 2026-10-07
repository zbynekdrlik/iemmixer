//! The counts of one run and the summary they make (S7 design note §4): the
//! socket threads count into a [`Tally`], and [`Tally::summary`] turns it
//! into the [`Summary`] written every `write_every` and at the end. Pure:
//! every time is passed in.

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
    fn the_run_clock_starts_at_the_first_open_and_reopens_count() {
        let t0 = Instant::now();
        let mut tally = Tally::default();
        tally.opened(at(t0, 100), false);
        // The other socket's first open, then a reopen.
        tally.opened(at(t0, 200), false);
        tally.opened(at(t0, 900), true);
        assert_eq!(tally.ends_at(3), Some(at(t0, 3_100)));
        let s = tally.summary(at(t0, 2_600), false);
        assert_eq!(s.seconds, 2.5);
        assert_eq!(s.reconnects, 1);
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
        tally.opened(t0, false);
        tally.listen_started(at(t0, 10));
        // A reopen's ListenStart does not move the first one.
        tally.listen_started(at(t0, 30));
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
        tally.fail(Reason::ServerGone);
        tally.fail(Reason::LoginRefused);
        assert_eq!(tally.error(), Some(Reason::ServerGone));
        let s = tally.summary(Instant::now(), false);
        assert_eq!(s.error, Some(Reason::ServerGone));
    }
}
