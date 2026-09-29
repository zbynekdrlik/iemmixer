//! CPU placement (S1c design note §6.1, §6.2 L5): logical-processor lists as
//! the tuning profile and the spike's flags write them ("14", "6-13",
//! "0,1,6-13") and their mapping to Windows CPU Set IDs. Portable and tested;
//! the Windows calls live in `os`.

/// One entry of the system's CPU Set information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuSet {
    /// The ID the CPU Set functions take.
    pub id: u32,
    pub group: u16,
    /// The logical processor's index within its group.
    pub lp: u8,
    pub core: u8,
    /// Reserved for real-time work (`ReservedCpuSets`, design note §6.5 X1).
    pub realtime: bool,
}

/// Parses a list of group-0 logical processors: comma-separated numbers and
/// `a-b` ranges, each processor at most once, 0..=63. Empty text is an empty
/// list. The result is ascending.
pub fn parse_lps(text: &str) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::new();
    for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (lo, hi) = part.split_once('-').unwrap_or((part, part));
        let num = |s: &str| {
            s.trim()
                .parse::<u8>()
                .ok()
                .filter(|&n| n < 64)
                .ok_or_else(|| format!("{s:?} in {text:?} is not a processor 0..63"))
        };
        let (lo, hi) = (num(lo)?, num(hi)?);
        if lo > hi {
            return Err(format!("range {part:?} in {text:?} runs backwards"));
        }
        for lp in lo..=hi {
            if out.contains(&lp) {
                return Err(format!("{text:?} names processor {lp} twice"));
            }
            out.push(lp);
        }
    }
    out.sort_unstable();
    Ok(out)
}

/// The CPU Set IDs of `lps` in group 0; every processor must exist.
pub fn ids_for(lps: &[u8], system: &[CpuSet]) -> Result<Vec<u32>, String> {
    lps.iter()
        .map(|&lp| {
            system
                .iter()
                .find(|c| c.group == 0 && c.lp == lp)
                .map(|c| c.id)
                .ok_or_else(|| format!("logical processor {lp} is not in group 0 of this machine"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system() -> Vec<CpuSet> {
        (0..16_u8)
            .map(|lp| CpuSet {
                id: 0x100 + u32::from(lp),
                group: 0,
                lp,
                core: lp / 2,
                realtime: false,
            })
            .chain([CpuSet {
                id: 0x200,
                group: 1,
                lp: 0,
                core: 0,
                realtime: false,
            }])
            .collect()
    }

    #[test]
    fn lists_and_ranges_parse_sorted() {
        assert_eq!(parse_lps("14"), Ok(vec![14]));
        assert_eq!(parse_lps(" 6-9 , 0,1 "), Ok(vec![0, 1, 6, 7, 8, 9]));
        assert_eq!(parse_lps("3-3"), Ok(vec![3]));
        assert_eq!(parse_lps("63"), Ok(vec![63]));
        assert_eq!(parse_lps(""), Ok(vec![]));
        assert_eq!(parse_lps(" , "), Ok(vec![]));
    }

    #[test]
    fn bad_lists_are_refused() {
        for bad in ["64", "-1", "a", "5-3", "1,1", "0-2,2", "1-", "1-2-3", "256"] {
            assert!(parse_lps(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn ids_come_from_group_zero_and_must_exist() {
        let s = system();
        assert_eq!(ids_for(&[0, 14], &s), Ok(vec![0x100, 0x10e]));
        assert_eq!(ids_for(&[], &s), Ok(vec![]));
        assert!(ids_for(&[16], &s).is_err());
        let only_group_one = [CpuSet {
            id: 0x200,
            group: 1,
            lp: 0,
            core: 0,
            realtime: false,
        }];
        assert!(ids_for(&[0], &only_group_one).is_err());
    }
}
