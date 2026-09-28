//! The driver's messages to the host (#9 2026-09-28), counted where the
//! driver sends them and logged by the ASIO backend's owner thread.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::selector::*;

    #[test]
    fn every_driver_message_but_a_question_has_a_topic() {
        let topics: Vec<Option<Topic>> = [
            RESET_REQUEST,
            BUFFER_SIZE_CHANGE,
            RESYNC_REQUEST,
            LATENCIES_CHANGED,
            OVERLOAD,
        ]
        .into_iter()
        .map(Topic::of)
        .collect();
        assert_eq!(
            topics,
            [
                Some(Topic::ResetRequest),
                Some(Topic::BufferSizeChange),
                Some(Topic::ResyncRequest),
                Some(Topic::LatenciesChanged),
                Some(Topic::Overload),
            ]
        );
        // The driver's questions are answered without a log line.
        for sel in [
            SELECTOR_SUPPORTED,
            ENGINE_VERSION,
            SUPPORTS_TIME_INFO,
            SUPPORTS_TIME_CODE,
        ] {
            assert_eq!(Topic::of(sel), None, "{sel}");
        }
        // Anything else is logged with its selector.
        for sel in [0, 9, 10, 14, 16, 99, -1] {
            assert_eq!(Topic::of(sel), Some(Topic::Other), "{sel}");
        }
    }

    #[test]
    fn the_topics_are_listed_once_in_order() {
        assert_eq!(TOPICS, 7);
        assert_eq!(Topic::ALL.len(), TOPICS);
        for (i, topic) in Topic::ALL.into_iter().enumerate() {
            assert_eq!(topic.index(), i, "{topic:?}");
        }
    }

    #[test]
    fn only_a_reset_a_size_or_a_rate_change_asks_for_a_reopen() {
        let asks: Vec<bool> = Topic::ALL.into_iter().map(Topic::asks_reopen).collect();
        assert_eq!(asks, [true, true, false, false, false, true, false]);
    }

    /// The handler counts (any thread); the owner thread logs what came
    /// since its last look, once per topic, with the last value and time.
    #[test]
    fn a_message_is_counted_under_its_topic_with_its_value_and_time() {
        let m = Messages::new();
        for topic in Topic::ALL {
            assert_eq!(m.count(topic), 0, "{topic:?}");
        }
        m.message(RESET_REQUEST, 0, 1_000);
        m.message(BUFFER_SIZE_CHANGE, 64, 2_000);
        m.message(BUFFER_SIZE_CHANGE, 32, 3_000);
        // A question: answered, not counted.
        m.message(SUPPORTS_TIME_INFO, 0, 4_000);
        // Another selector: its number is kept.
        m.message(42, 7, 5_000);
        m.rate_change(44_099.6, 6_000);
        let counts: Vec<u64> = Topic::ALL.into_iter().map(|t| m.count(t)).collect();
        assert_eq!(counts, [1, 2, 0, 0, 0, 1, 1]);

        let mut seen = [0; TOPICS];
        assert_eq!(
            m.since(&mut seen),
            [
                Arrived {
                    topic: Topic::ResetRequest,
                    new: 1,
                    total: 1,
                    value: 0,
                    at_ns: 1_000
                },
                Arrived {
                    topic: Topic::BufferSizeChange,
                    new: 2,
                    total: 2,
                    value: 32,
                    at_ns: 3_000
                },
                Arrived {
                    topic: Topic::RateChange,
                    new: 1,
                    total: 1,
                    value: 44_100,
                    at_ns: 6_000
                },
                Arrived {
                    topic: Topic::Other,
                    new: 1,
                    total: 1,
                    value: 42,
                    at_ns: 5_000
                },
            ]
        );
        assert_eq!(seen, [1, 2, 0, 0, 0, 1, 1]);
        // A second look finds nothing new.
        assert!(m.since(&mut seen).is_empty());

        // Only what came since the last look; the totals go on.
        m.message(RESET_REQUEST, 1, 7_000);
        m.message(RESET_REQUEST, 2, 8_000);
        m.message(OVERLOAD, 0, 9_000);
        assert_eq!(
            m.since(&mut seen),
            [
                Arrived {
                    topic: Topic::ResetRequest,
                    new: 2,
                    total: 3,
                    value: 2,
                    at_ns: 8_000
                },
                Arrived {
                    topic: Topic::Overload,
                    new: 1,
                    total: 1,
                    value: 0,
                    at_ns: 9_000
                },
            ]
        );
        assert_eq!(seen, [3, 2, 0, 0, 1, 1, 1]);
        // A new look from nothing sees every topic so far.
        let mut fresh = [0; TOPICS];
        assert_eq!(m.since(&mut fresh).len(), 5);
        assert_eq!(fresh, seen);
    }

    #[test]
    fn a_default_log_is_empty() {
        let m = Messages::default();
        let mut seen = [0; TOPICS];
        assert!(m.since(&mut seen).is_empty());
        m.record(Topic::Overload, 3, 10);
        assert_eq!(m.count(Topic::Overload), 1);
        assert_eq!(m.count(Topic::ResetRequest), 0);
    }

    #[test]
    fn what_arrived_reads_as_a_log_line() {
        let one = Arrived {
            topic: Topic::ResetRequest,
            new: 1,
            total: 1,
            value: 0,
            at_ns: 105_312_000,
        };
        assert_eq!(
            one.to_string(),
            "the driver sent 1 reset request (value 0, at t=0.105312 s); 1 so far"
        );
        let two = Arrived {
            topic: Topic::BufferSizeChange,
            new: 2,
            total: 5,
            value: 64,
            at_ns: 1_500_000_000,
        };
        assert_eq!(
            two.to_string(),
            "the driver sent 2 buffer size changes (the last: value 64, at t=1.500000 s); 5 so far"
        );
        let other = Arrived {
            topic: Topic::Other,
            new: 1,
            total: 3,
            value: 42,
            at_ns: 0,
        };
        assert_eq!(
            other.to_string(),
            "the driver sent 1 other message (selector 42, at t=0.000000 s); 3 so far"
        );
        let rates = Arrived {
            topic: Topic::RateChange,
            new: 3,
            total: 3,
            value: 48_000,
            at_ns: 2_000_001_000,
        };
        assert_eq!(
            rates.to_string(),
            "the driver sent 3 sample-rate changes (the last: value 48000, at t=2.000001 s); 3 so far"
        );
        let names: Vec<(&str, &str)> = Topic::ALL.into_iter().map(Topic::names).collect();
        assert_eq!(
            names,
            [
                ("reset request", "reset requests"),
                ("buffer size change", "buffer size changes"),
                ("resync request", "resync requests"),
                ("latency change", "latency changes"),
                ("overload", "overloads"),
                ("sample-rate change", "sample-rate changes"),
                ("other message", "other messages"),
            ]
        );
    }

    #[test]
    fn a_time_reads_in_seconds_to_the_microsecond() {
        assert_eq!(stamp(0), "t=0.000000 s");
        assert_eq!(stamp(1_000), "t=0.000001 s");
        assert_eq!(stamp(105_312_000), "t=0.105312 s");
        assert_eq!(stamp(3_600_000_000_000), "t=3600.000000 s");
    }
}
