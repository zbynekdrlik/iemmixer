//! The period the driver really delivers, measured from the first callbacks'
//! sample positions (S6 design note §3). The driver re-reads its preference at
//! open, so the configured 32 is a request, not a fact: `Status.frames` carries
//! this measurement and anything but the expected size refuses the stream.
//!
//! The owner thread (never the callback) collects the positions and decides,
//! so this module may allocate.

/// Sample positions of consecutive callbacks → the frames per callback, once
/// the last `need` deltas agree. `None` while undecided or inconsistent: too
/// few positions, a zero delta (a position that did not advance or went
/// back), deltas that differ, or a delta beyond `u32`.
pub fn measured(positions: &[u64], need: usize) -> Option<u32> {
    let deltas: Vec<u64> = positions
        .iter()
        .zip(positions.iter().skip(1))
        .map(|(a, b)| b.saturating_sub(*a))
        .collect();
    let tail = deltas
        .len()
        .checked_sub(need)
        .and_then(|s| deltas.get(s..))?;
    let first = *tail.first()?;
    if first > 0 && tail.iter().all(|d| *d == first) {
        u32::try_from(first).ok()
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodVerdict {
    /// Not enough agreeing callbacks yet.
    Undecided,
    Ok(u32),
    /// The driver delivers another period than the one required: the open
    /// fails (exit 3).
    Wrong {
        expected: u32,
        measured: u32,
    },
}

/// The verdict on the measured period against the `expected` one.
pub fn verdict(positions: &[u64], need: usize, expected: u32) -> PeriodVerdict {
    match measured(positions, need) {
        None => PeriodVerdict::Undecided,
        Some(m) if m == expected => PeriodVerdict::Ok(m),
        Some(m) => PeriodVerdict::Wrong {
            expected,
            measured: m,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Positions starting at `start`, advanced by each delta in turn.
    fn positions(start: u64, deltas: &[u64]) -> Vec<u64> {
        let mut at = start;
        let mut out = vec![at];
        for d in deltas {
            at += d;
            out.push(at);
        }
        out
    }

    #[test]
    fn steady_32_sample_steps_are_ok() {
        let p = positions(1_000, &[32; 15]);
        assert_eq!(measured(&p, 8), Some(32));
        assert_eq!(verdict(&p, 8, 32), PeriodVerdict::Ok(32));
    }

    #[test]
    fn steady_64_is_wrong() {
        let p = positions(0, &[64; 15]);
        assert_eq!(measured(&p, 8), Some(64));
        assert_eq!(
            verdict(&p, 8, 32),
            PeriodVerdict::Wrong {
                expected: 32,
                measured: 64
            }
        );
        // One sample short of the expected period is wrong too.
        assert_eq!(
            verdict(&positions(0, &[31; 8]), 8, 32),
            PeriodVerdict::Wrong {
                expected: 32,
                measured: 31
            }
        );
    }

    #[test]
    fn a_jittered_start_is_decided_only_on_the_settled_tail() {
        let jitter: [u64; 4] = [40, 24, 35, 29];
        let settled: Vec<u64> = jitter.iter().copied().chain([32; 8]).collect();
        assert_eq!(
            verdict(&positions(0, &settled), 8, 32),
            PeriodVerdict::Ok(32)
        );
        // Seven settled deltas: the eighth from the end is still jitter.
        let early: Vec<u64> = jitter.iter().copied().chain([32; 7]).collect();
        assert_eq!(measured(&positions(0, &early), 8), None);
        assert_eq!(
            verdict(&positions(0, &early), 8, 32),
            PeriodVerdict::Undecided
        );
        // A late glitch undoes the decision.
        assert_eq!(measured(&positions(0, &[32, 32, 32, 32, 33]), 4), None);
    }

    #[test]
    fn too_few_positions_are_undecided() {
        // Eight deltas need nine positions.
        assert_eq!(
            verdict(&positions(0, &[32; 7]), 8, 32),
            PeriodVerdict::Undecided
        );
        assert_eq!(
            verdict(&positions(0, &[32; 8]), 8, 32),
            PeriodVerdict::Ok(32)
        );
        assert_eq!(verdict(&[7], 1, 32), PeriodVerdict::Undecided);
        assert_eq!(verdict(&[], 1, 32), PeriodVerdict::Undecided);
        // Needing no delta at all decides nothing.
        assert_eq!(measured(&positions(0, &[32; 8]), 0), None);
    }

    #[test]
    fn a_zero_delta_is_undecided() {
        assert_eq!(measured(&[500; 12], 8), None);
        assert_eq!(verdict(&[500; 12], 8, 32), PeriodVerdict::Undecided);
        // A position that goes back counts as no advance.
        let back: [u64; 4] = [640, 320, 352, 384];
        assert_eq!(measured(&back, 3), None);
        assert_eq!(measured(&back, 2), Some(32));
    }

    #[test]
    fn a_delta_beyond_u32_is_undecided() {
        let huge = 1u64 << 32;
        assert_eq!(measured(&positions(0, &[huge; 4]), 4), None);
        let max = u64::from(u32::MAX);
        assert_eq!(measured(&positions(0, &[max; 4]), 4), Some(u32::MAX));
    }
}
