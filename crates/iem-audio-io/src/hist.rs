//! The stream's distributions (S7 design note §3): the callback interval and
//! the callback's own time, in 1 µs buckets below two periods and one overflow
//! bucket at two periods or more, counted since the stream opened. The RT
//! thread records with one relaxed increment into an array allocated before
//! the stream starts (I7); the control thread reads a sparse snapshot once a
//! second for `Status`.

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
