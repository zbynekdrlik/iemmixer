//! The pre-emption token (S6 design note §5.2).
//!
//! "ide event" pre-empts a running switch: every waiting step sleeps through
//! [`Cancel::sleep`] in slices of at most [`Cancel::SLICE`], so it gives up
//! well within 1 s; a mutating step finishes its mutation first.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// A wait ended by a pre-emption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preempted;

/// Shared by the switch runner and the request loop; clones share the flag.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    /// The longest a waiting step sleeps between two looks at the token.
    pub const SLICE: Duration = Duration::from_millis(100);

    pub fn preempt(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn preempted(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub fn clear(&self) {
        self.0.store(false, Ordering::SeqCst);
    }

    /// Sleeps `d` unless pre-empted first; a token already pre-empted ends
    /// the wait at once.
    pub fn sleep(&self, d: Duration) -> Result<(), Preempted> {
        let start = Instant::now();
        loop {
            if self.preempted() {
                return Err(Preempted);
            }
            let left = d.saturating_sub(start.elapsed());
            if left.is_zero() {
                return Ok(());
            }
            thread::sleep(left.min(Self::SLICE));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_is_shared_and_can_be_cleared() {
        let c = Cancel::default();
        let other = c.clone();
        assert!(!c.preempted());
        other.preempt();
        assert!(c.preempted());
        c.clear();
        assert!(!other.preempted());
    }

    #[test]
    fn an_undisturbed_sleep_lasts_its_duration() {
        let c = Cancel::default();
        let t = Instant::now();
        assert_eq!(c.sleep(Duration::from_millis(250)), Ok(()));
        let took = t.elapsed();
        assert!(took >= Duration::from_millis(250), "{took:?}");
        assert!(took < Duration::from_secs(2), "{took:?}");
        assert_eq!(c.sleep(Duration::ZERO), Ok(()));
    }

    #[test]
    fn a_preempted_token_ends_the_wait_at_once() {
        let c = Cancel::default();
        c.preempt();
        let t = Instant::now();
        assert_eq!(c.sleep(Duration::from_secs(3)), Err(Preempted));
        assert_eq!(c.sleep(Duration::ZERO), Err(Preempted));
        assert!(
            t.elapsed() < Duration::from_millis(200),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn cancel_sleep_returns_within_a_slice() {
        let c = Cancel::default();
        let other = c.clone();
        let fired = thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            other.preempt();
            Instant::now()
        });
        let t = Instant::now();
        assert_eq!(c.sleep(Duration::from_secs(4)), Err(Preempted));
        let back = Instant::now();
        let at = fired.join().unwrap();
        assert!(back.duration_since(t) >= Duration::from_millis(150));
        // Well inside the 1 s promise; a slice is 100 ms.
        assert!(
            back.duration_since(at) < Duration::from_millis(600),
            "{:?}",
            back.duration_since(at)
        );
    }
}
