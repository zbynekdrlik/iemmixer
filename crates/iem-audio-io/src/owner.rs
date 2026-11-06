//! How a stream ended, what a backend's stop reports to the engine.

/// How a stream ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// Stopped, disposed and released.
    Released,
    /// A callback stayed in the stream past the stop wait: the stream is left
    /// allocated (never freed under a callback) and the supervisor alarms.
    Parked,
}
