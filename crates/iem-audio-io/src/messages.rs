//! The driver's messages to the host (#9 2026-09-28), counted where the
//! driver sends them and logged by the ASIO backend's owner thread.
//!
//! The driver calls `asioMessage` and `sampleRateDidChange` on a thread of
//! its own choosing: azo 0.2.1 hands the host's `Callbacks` to
//! `createBuffers` unchanged, so a message may come on the audio callback's
//! thread, on a thread of the driver, or on the owner thread inside
//! `createBuffers`. The handler therefore only counts, in atomics (no
//! allocation, lock or log: I7), also while no stream exists, when the
//! per-stream [`crate::telemetry`] cannot count; the owner thread, which may
//! log, reads what came since its last look ([`Messages::since`]). Portable
//! and mutation-tested; `asio.rs` holds the process's one [`Messages`].

use core::fmt;
use core::sync::atomic::{
    AtomicI64, AtomicU64,
    Ordering::{Acquire, Relaxed, Release},
};

use crate::telemetry::selector;

/// The number of [`Topic`]s.
pub const TOPICS: usize = 7;

/// A driver message the owner thread logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    /// `kAsioResetRequest`: the driver asks the host to reopen it.
    ResetRequest,
    /// `kAsioBufferSizeChange` (answered 0: the driver then asks for a reset).
    BufferSizeChange,
    /// `kAsioResyncRequest`.
    ResyncRequest,
    /// `kAsioLatenciesChanged`.
    LatenciesChanged,
    /// `kAsioOverload`.
    Overload,
    /// `sampleRateDidChange` (the host never sets a rate).
    RateChange,
    /// Any other selector but the driver's questions; its value is the
    /// selector.
    Other,
}

impl Topic {
    pub const ALL: [Topic; TOPICS] = [
        Topic::ResetRequest,
        Topic::BufferSizeChange,
        Topic::ResyncRequest,
        Topic::LatenciesChanged,
        Topic::Overload,
        Topic::RateChange,
        Topic::Other,
    ];

    /// The topic of an `asioMessage` selector; `None` for the driver's
    /// questions (selector supported, engine version, time info and time
    /// code), which it asks while `createBuffers` runs and the host answers
    /// without a log line.
    pub fn of(sel: i32) -> Option<Topic> {
        match sel {
            selector::RESET_REQUEST => Some(Topic::ResetRequest),
            selector::BUFFER_SIZE_CHANGE => Some(Topic::BufferSizeChange),
            selector::RESYNC_REQUEST => Some(Topic::ResyncRequest),
            selector::LATENCIES_CHANGED => Some(Topic::LatenciesChanged),
            selector::OVERLOAD => Some(Topic::Overload),
            selector::SELECTOR_SUPPORTED
            | selector::ENGINE_VERSION
            | selector::SUPPORTS_TIME_INFO
            | selector::SUPPORTS_TIME_CODE => None,
            _ => Some(Topic::Other),
        }
    }

    /// Singular and plural, for the log.
    pub fn names(self) -> (&'static str, &'static str) {
        match self {
            Topic::ResetRequest => ("reset request", "reset requests"),
            Topic::BufferSizeChange => ("buffer size change", "buffer size changes"),
            Topic::ResyncRequest => ("resync request", "resync requests"),
            Topic::LatenciesChanged => ("latency change", "latency changes"),
            Topic::Overload => ("overload", "overloads"),
            Topic::RateChange => ("sample-rate change", "sample-rate changes"),
            Topic::Other => ("other message", "other messages"),
        }
    }

    /// The owner thread asks its reopen budget for it: a reset request, a
    /// buffer size change or a sample-rate change (logged as a warning).
    pub fn asks_reopen(self) -> bool {
        matches!(
            self,
            Topic::ResetRequest | Topic::BufferSizeChange | Topic::RateChange
        )
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// Per topic: how many came, and the last one's value and time.
pub struct Messages {
    count: [AtomicU64; TOPICS],
    value: [AtomicI64; TOPICS],
    at_ns: [AtomicU64; TOPICS],
}

impl Default for Messages {
    fn default() -> Self {
        Self::new()
    }
}

impl Messages {
    pub const fn new() -> Self {
        Self {
            count: [const { AtomicU64::new(0) }; TOPICS],
            value: [const { AtomicI64::new(0) }; TOPICS],
            at_ns: [const { AtomicU64::new(0) }; TOPICS],
        }
    }

    /// One message, on any thread (the audio callback's too): atomics only.
    /// `value` is the message's (the selector for [`Topic::Other`], the rate
    /// in Hz for [`Topic::RateChange`]); `at_ns` its time on the owner
    /// thread's clock.
    pub fn record(&self, topic: Topic, value: i64, at_ns: u64) {
        let i = topic.index();
        if let (Some(count), Some(last), Some(at)) =
            (self.count.get(i), self.value.get(i), self.at_ns.get(i))
        {
            last.store(value, Relaxed);
            at.store(at_ns, Relaxed);
            // A reader that sees the new count sees its value and time.
            count.fetch_add(1, Release);
        }
    }

    /// An `asioMessage`: counted under its topic, none for the driver's
    /// questions; [`Topic::Other`] keeps the selector.
    pub fn message(&self, sel: i32, value: i32, at_ns: u64) {
        if let Some(topic) = Topic::of(sel) {
            let kept = if topic == Topic::Other { sel } else { value };
            self.record(topic, i64::from(kept), at_ns);
        }
    }

    /// `sampleRateDidChange`: the new rate, in whole Hz.
    pub fn rate_change(&self, rate: f64, at_ns: u64) {
        self.record(Topic::RateChange, rate.round() as i64, at_ns);
    }

    /// The messages of `topic` so far.
    pub fn count(&self, topic: Topic) -> u64 {
        self.count.get(topic.index()).map_or(0, |c| c.load(Acquire))
    }

    /// What came since the owner thread's last look `seen` (one entry per
    /// topic with new messages, in [`Topic::ALL`] order); `seen` moves up.
    pub fn since(&self, seen: &mut [u64; TOPICS]) -> Vec<Arrived> {
        let mut out = Vec::new();
        for (topic, last) in Topic::ALL.into_iter().zip(seen.iter_mut()) {
            let total = self.count(topic);
            if total > *last {
                let i = topic.index();
                out.push(Arrived {
                    topic,
                    new: total - *last,
                    total,
                    value: self.value.get(i).map_or(0, |v| v.load(Relaxed)),
                    at_ns: self.at_ns.get(i).map_or(0, |t| t.load(Relaxed)),
                });
                *last = total;
            }
        }
        out
    }
}

/// What came of one topic since the owner thread's last look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arrived {
    pub topic: Topic,
    /// Since the last look.
    pub new: u64,
    /// Since the process started.
    pub total: u64,
    /// The last one's value (see [`Messages::record`]).
    pub value: i64,
    /// When the last one came, on the owner thread's clock.
    pub at_ns: u64,
}

impl fmt::Display for Arrived {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (one, many) = self.topic.names();
        let (name, last) = if self.new == 1 {
            (one, "")
        } else {
            (many, "the last: ")
        };
        let what = if self.topic == Topic::Other {
            "selector"
        } else {
            "value"
        };
        write!(
            f,
            "the driver sent {} {name} ({last}{what} {}, at {}); {} so far",
            self.new,
            self.value,
            stamp(self.at_ns),
            self.total
        )
    }
}

/// A time on the owner thread's clock, for the log: seconds to the
/// microsecond.
pub fn stamp(ns: u64) -> String {
    format!("t={:.6} s", ns as f64 / 1e9)
}

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
