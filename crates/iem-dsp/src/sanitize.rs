//! Sanitiser (X1): a node whose block holds a non-finite sample or one above
//! 1e6 in magnitude is silenced for that block. The caller counts the trip,
//! resets the node and raises the alarm (S3).

/// Largest magnitude a node may output.
pub const LIMIT: f64 = 1e6;

/// True when any channel of the block tripped; all channels are then zeroed.
pub fn sanitize<const CH: usize>(chans: [&mut [f64]; CH]) -> bool {
    let bad = chans
        .iter()
        .any(|ch| ch.iter().any(|x| x.is_nan() || x.abs() > LIMIT));
    if bad {
        for ch in chans {
            ch.fill(0.0);
        }
    }
    bad
}

/// Trip counter of one node (X1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Trips {
    count: u64,
}

impl Trips {
    /// Sanitises the block and counts a trip; true when it tripped.
    pub fn check<const CH: usize>(&mut self, chans: [&mut [f64]; CH]) -> bool {
        let tripped = sanitize(chans);
        if tripped {
            self.count += 1;
        }
        tripped
    }

    pub const fn count(&self) -> u64 {
        self.count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_blocks_pass_untouched() {
        let (mut l, mut r) = ([0.5, -LIMIT], [LIMIT, 0.0]);
        assert!(!sanitize([&mut l, &mut r]));
        assert_eq!((l, r), ([0.5, -LIMIT], [LIMIT, 0.0]));
    }

    #[test]
    fn nan_inf_and_huge_values_silence_every_channel() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1.000001e6, -2e6] {
            let (mut l, mut r) = ([0.5, 0.25], [0.1, bad]);
            assert!(sanitize([&mut l, &mut r]), "{bad}");
            assert_eq!((l, r), ([0.0, 0.0], [0.0, 0.0]));
        }
    }

    #[test]
    fn trips_are_counted() {
        let mut t = Trips::default();
        let mut x = [f64::NAN];
        assert!(t.check([&mut x]));
        assert!(!t.check([&mut x]));
        let mut y = [3e6];
        assert!(t.check([&mut y]));
        assert_eq!(t.count(), 2);
    }
}
