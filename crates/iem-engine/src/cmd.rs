//! Commands to the RT thread (I7): `Copy` messages with indices and linear
//! values, stamped with the sample time they apply at (0 = as soon as
//! possible). A group of commands is written as one ring chunk and the RT
//! thread applies it whole (a batch lands at one sample).

use iem_dsp::eq::EqParams;

use crate::params::InputParams;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum RtOp {
    #[default]
    Nop,
    Input {
        i: u16,
        p: InputParams,
    },
    InputEq {
        i: u16,
        eq: EqParams,
    },
    /// A mix's volume and mute (its output stage).
    MixOut {
        m: u16,
        volume: f64,
        muted: bool,
    },
    MixEq {
        m: u16,
        eq: EqParams,
    },
    Limiter {
        m: u16,
        enabled: bool,
        limit_db: f64,
    },
    ResetLimiter {
        m: u16,
    },
    /// Level slot `k` of mix `m`; `muted` is the effective mute (the
    /// level's own or a solo's).
    Level {
        m: u16,
        k: u16,
        gain: f64,
        pan: f64,
        muted: bool,
    },
    /// Group `g`'s strip in mix `m`.
    Group {
        m: u16,
        g: u16,
        gain: f64,
        muted: bool,
    },
    GroupEq {
        m: u16,
        g: u16,
        eq: EqParams,
    },
    Listen {
        slot: u8,
        mix: Option<u16>,
    },
    TestSignal {
        i: u16,
        hz: f64,
        amp: f64,
        ttl: u64,
    },
    StopTestSignal,
    FadeOut,
    /// Fault injection (the `--fault-injection` launch flag only).
    Panic,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RtCmd {
    /// Sample time to apply at; commands due earlier apply at once.
    pub at: u64,
    /// On the first command of a group: the group's length; 0 on the rest.
    pub group: u16,
    pub op: RtOp,
}

/// Writes `ops` as one group stamped `at`; false (nothing written) when the
/// ring lacks room or the group is empty.
pub fn push_group(p: &mut rtrb::Producer<RtCmd>, at: u64, ops: &[RtOp]) -> bool {
    let Ok(len) = u16::try_from(ops.len()) else {
        return false;
    };
    if len == 0 {
        return false;
    }
    let Ok(mut chunk) = p.write_chunk(ops.len()) else {
        return false;
    };
    let (a, b) = chunk.as_mut_slices();
    for (k, (slot, op)) in a.iter_mut().chain(b.iter_mut()).zip(ops).enumerate() {
        *slot = RtCmd {
            at,
            group: if k == 0 { len } else { 0 },
            op: *op,
        };
    }
    chunk.commit_all();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_group_is_one_chunk_with_its_length_on_the_head() {
        let (mut p, mut c) = rtrb::RingBuffer::new(5);
        assert!(!push_group(&mut p, 0, &[]));
        assert!(push_group(
            &mut p,
            7,
            &[RtOp::FadeOut, RtOp::Panic, RtOp::StopTestSignal]
        ));
        assert!(!push_group(&mut p, 9, &[RtOp::Nop; 3]), "only 2 slots left");
        assert_eq!(p.slots(), 2);
        let got: Vec<RtCmd> = std::iter::from_fn(|| c.pop().ok()).collect();
        assert_eq!(
            got,
            vec![
                RtCmd {
                    at: 7,
                    group: 3,
                    op: RtOp::FadeOut
                },
                RtCmd {
                    at: 7,
                    group: 0,
                    op: RtOp::Panic
                },
                RtCmd {
                    at: 7,
                    group: 0,
                    op: RtOp::StopTestSignal
                },
            ]
        );
        // Wrapping around the ring end still writes one whole group.
        assert!(push_group(
            &mut p,
            1,
            &[RtOp::Nop, RtOp::FadeOut, RtOp::Panic, RtOp::Nop]
        ));
        let got: Vec<RtCmd> = std::iter::from_fn(|| c.pop().ok()).collect();
        assert_eq!(got.len(), 4);
        assert_eq!(got[0].group, 4);
        assert!(got[1..].iter().all(|c| c.group == 0 && c.at == 1));
        assert_eq!(got[2].op, RtOp::Panic);
        let many = vec![RtOp::Nop; usize::from(u16::MAX) + 1];
        assert!(!push_group(&mut p, 0, &many));
    }
}
