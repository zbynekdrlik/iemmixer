//! The stream's distributions (S7 design note §3): the callback interval and
//! the callback's own time, in 1 µs buckets below two periods and one overflow
//! bucket at two periods or more, counted since the stream opened. The RT
//! thread records with one relaxed increment into an array allocated before
//! the stream starts (I7); the control thread reads a sparse snapshot once a
//! second for `Status`.

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// One bucket: 1 µs.
pub const BUCKET_NS: u64 = 1_000;
/// The longest range below the overflow bucket: 1 ms. Two periods are 667 µs
/// at 32 samples, 96 kHz; a larger NullRt block is capped here, so `Status`
/// and the guard's reply stay bounded (at most 1001 buckets each).
pub const MAX_RANGE_NS: u64 = 1_000_000;

/// One distribution: whole µs below two periods, then the overflow bucket.
pub struct PeriodHist {
    /// Two periods (capped at [`MAX_RANGE_NS`]): the overflow's lower edge.
    limit_ns: u64,
    /// The overflow bucket's index.
    top: usize,
    /// `top + 1` counters, allocated here, before the stream starts.
    counts: Box<[AtomicU64]>,
}

impl PeriodHist {
    pub fn new(period_ns: u64) -> Self {
        let limit_ns = period_ns.max(1).saturating_mul(2).min(MAX_RANGE_NS);
        let top = usize::try_from(limit_ns.div_ceil(BUCKET_NS)).unwrap_or(0);
        Self {
            limit_ns,
            top,
            counts: (0..=top).map(|_| AtomicU64::new(0)).collect(),
        }
    }

    /// The overflow bucket's index: two periods in µs, rounded up.
    pub fn top(&self) -> u32 {
        u32::try_from(self.top).unwrap_or(u32::MAX)
    }

    /// The bucket of `ns`: whole µs below two periods, else the overflow.
    pub fn index(&self, ns: u64) -> usize {
        if ns >= self.limit_ns {
            self.top
        } else {
            usize::try_from(ns / BUCKET_NS).unwrap_or(self.top)
        }
    }

    /// RT thread: one relaxed increment (I7).
    #[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]
    pub fn record(&self, ns: u64) {
        if let Some(c) = self.counts.get(self.index(ns)) {
            c.fetch_add(1, Relaxed);
        }
    }

    /// The non-empty buckets, ascending (control thread; allocates).
    pub fn sparse(&self) -> Vec<(u32, u64)> {
        self.counts
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let n = c.load(Relaxed);
                (n > 0).then(|| (u32::try_from(i).unwrap_or(u32::MAX), n))
            })
            .collect()
    }
}

/// Both histograms of one stream, shared with its RT thread.
pub struct StreamHists {
    /// The interval between two callbacks' entries (after telemetry's
    /// warm-up on the card).
    pub interval: PeriodHist,
    /// The callback's own time (the span of `StreamStats::max_process_ns`).
    pub process: PeriodHist,
}

impl StreamHists {
    pub fn new(period_ns: u64) -> Self {
        Self {
            interval: PeriodHist::new(period_ns),
            process: PeriodHist::new(period_ns),
        }
    }

    pub fn snapshot(&self) -> HistSnapshot {
        HistSnapshot {
            top_us: self.interval.top(),
            interval: self.interval.sparse(),
            process: self.process.sparse(),
        }
    }
}

/// Both histograms read at one moment, sparse: `(bucket, count)` pairs,
/// ascending, empty buckets left out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistSnapshot {
    /// The overflow bucket's index (both histograms share the period).
    pub top_us: u32,
    pub interval: Vec<(u32, u64)>,
    pub process: Vec<(u32, u64)>,
}

/// The upper edge (µs) of the bucket holding the `per_mille` quantile (rank
/// ⌈per_mille·n/1000⌉, at least 1): the soak verdict's rule
/// (scripts/iem-pc/soak_verdict.py `quantile_us`). `None` when empty.
pub fn quantile_us(sparse: &[(u32, u64)], per_mille: u64) -> Option<u32> {
    let total: u64 = sparse.iter().map(|e| e.1).sum();
    let rank = per_mille.saturating_mul(total).div_ceil(1000).max(1);
    let mut seen = 0u64;
    // Empty (or all zero): `seen` never reaches the rank of at least 1.
    sparse.iter().find_map(|&(b, n)| {
        seen = seen.saturating_add(n);
        (seen >= rank).then(|| b.saturating_add(1))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::{Gap, classify, period_ns};

    fn at_32_samples() -> PeriodHist {
        PeriodHist::new(period_ns(32, 96_000.0))
    }

    #[test]
    fn two_periods_at_32_samples_end_at_bucket_667() {
        let h = at_32_samples();
        assert_eq!(h.top(), 667);
        for (ns, bucket) in [
            (0, 0),
            (999, 0),
            (1_000, 1),
            (346_999, 346),
            (347_000, 347),
            (666_665, 666),
            (666_666, 667),
            (u64::MAX, 667),
        ] {
            assert_eq!(h.index(ns), bucket, "{ns} ns");
        }
    }

    #[test]
    fn the_overflow_bucket_holds_exactly_what_telemetry_counts_missed() {
        let h = at_32_samples();
        for dt in [
            499_999,
            500_000,
            666_665,
            666_666,
            666_667,
            1_000_000,
            u64::MAX,
        ] {
            assert_eq!(
                h.index(dt) == 667,
                classify(dt, 333_333) == Gap::Missed,
                "{dt} ns"
            );
        }
    }

    #[test]
    fn a_larger_period_is_capped_at_one_millisecond() {
        let h = PeriodHist::new(1_000_000);
        assert_eq!(h.top(), 1000);
        assert_eq!((h.index(999_999), h.index(1_000_000)), (999, 1000));
    }

    #[test]
    fn a_zero_period_has_one_bucket_and_the_overflow() {
        let h = PeriodHist::new(0);
        assert_eq!(h.top(), 1);
        assert_eq!((h.index(1), h.index(2)), (0, 1));
    }

    #[test]
    fn record_and_sparse_keep_ascending_non_empty_buckets() {
        let s = StreamHists::new(period_ns(32, 96_000.0));
        for ns in [0, 0, 346_999, 347_000, u64::MAX] {
            s.interval.record(ns);
        }
        let snap = s.snapshot();
        assert_eq!(snap.interval, vec![(0, 2), (346, 1), (347, 1), (667, 1)]);
        assert!(snap.process.is_empty(), "{snap:?}");
        assert_eq!(snap.top_us, 667);
    }

    #[test]
    fn the_quantile_is_the_upper_edge_at_rank_ceil_per_mille_n() {
        assert_eq!(quantile_us(&[(10, 998), (82, 1), (83, 1)], 999), Some(83));
        assert_eq!(quantile_us(&[(10, 997), (83, 3)], 999), Some(84));
        assert_eq!(quantile_us(&[(5, 1)], 1), Some(6));
        assert_eq!(quantile_us(&[(5, 1), (9, 1)], 1000), Some(10));
        // Rank ⌈1.5⌉ = 2: the second bucket, never the first.
        assert_eq!(quantile_us(&[(1, 1), (2, 1), (3, 1)], 500), Some(3));
        assert_eq!(quantile_us(&[], 999), None);
    }
}
