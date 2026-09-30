//! Topology card channels (numbered from 1) → card buffer indices (S6 design
//! note §3). Built once per stream from `Topology::rx`/`tx` and the driver's
//! channel counts; a channel the card lacks refuses the stream.

use core::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelMap {
    rx: Vec<usize>,
    tx: Vec<usize>,
    /// Card input indices of the D5(b) loopback return (S6 test 5): the spare
    /// card inputs the HIL spare outputs loop back to. Empty unless the engine
    /// opened them (`--test-signal`). They sit in the input buffer after the
    /// topology's `rx`, in this order.
    hil_rx: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapError {
    Zero {
        side: &'static str,
    },
    Missing {
        side: &'static str,
        channel: u16,
        card: usize,
    },
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero { side } => write!(f, "{side} channel 0: card channels count from 1"),
            Self::Missing {
                side,
                channel,
                card,
            } => {
                write!(
                    f,
                    "{side} channel {channel} is not on the card ({card} channels)"
                )
            }
        }
    }
}

impl std::error::Error for MapError {}

fn indices(side: &'static str, list: &[u16], card: usize) -> Result<Vec<usize>, MapError> {
    list.iter()
        .map(|&c| {
            let i = usize::from(c)
                .checked_sub(1)
                .ok_or(MapError::Zero { side })?;
            if i < card {
                Ok(i)
            } else {
                Err(MapError::Missing {
                    side,
                    channel: c,
                    card,
                })
            }
        })
        .collect()
}

impl ChannelMap {
    /// `rx`/`tx` in topology order; `card_in`/`card_out` are the driver's
    /// input and output channel counts.
    pub fn new(rx: &[u16], tx: &[u16], card_in: usize, card_out: usize) -> Result<Self, MapError> {
        Ok(Self {
            rx: indices("rx", rx, card_in)?,
            tx: indices("tx", tx, card_out)?,
            hil_rx: Vec::new(),
        })
    }

    /// Opens the D5(b) loopback return (S6 test 5): `hil_rx` are the spare card
    /// input channels (numbered from 1, on the card), added after the
    /// topology's `rx`. A channel the card lacks refuses the stream, exactly
    /// like `rx`.
    pub fn with_hil_return(mut self, hil_rx: &[u16], card_in: usize) -> Result<Self, MapError> {
        self.hil_rx = indices("hil-return rx", hil_rx, card_in)?;
        Ok(self)
    }

    /// Card input index of engine input slot `k`, in `Topology::rx` order.
    pub fn rx(&self) -> &[usize] {
        &self.rx
    }

    /// Card input indices of the loopback return, after the topology's `rx`.
    pub fn hil_rx(&self) -> &[usize] {
        &self.hil_rx
    }

    /// Every card input the stream reads: the topology's `rx` then the
    /// loopback return, in the input buffer's order.
    pub fn all_rx(&self) -> Vec<usize> {
        let mut all = self.rx.clone();
        all.extend_from_slice(&self.hil_rx);
        all
    }

    /// Card output index of engine output slot `k`: `Topology::tx` order,
    /// then HIL's spare outputs (S6).
    pub fn tx(&self) -> &[usize] {
        &self.tx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic card: 132 inputs, 93 outputs (the test site's RX 101–132 and
    // TX 71–93 end exactly on the last channel).
    const IN: usize = 132;
    const OUT: usize = 93;

    #[test]
    fn the_loopback_return_follows_the_topology_rx() {
        let m = ChannelMap::new(&[101, 102], &[71], IN, OUT)
            .unwrap()
            .with_hil_return(&[110, 111], IN)
            .unwrap();
        // Return card inputs come after the topology's rx, in order.
        assert_eq!(m.rx(), &[100, 101]);
        assert_eq!(m.hil_rx(), &[109, 110]);
        assert_eq!(m.all_rx(), vec![100, 101, 109, 110]);
        // A return channel the card lacks refuses the stream, like rx.
        assert_eq!(
            ChannelMap::new(&[101], &[71], IN, OUT)
                .unwrap()
                .with_hil_return(&[133], IN),
            Err(MapError::Missing {
                side: "hil-return rx",
                channel: 133,
                card: IN,
            })
        );
        // No return by default.
        assert!(
            ChannelMap::new(&[101], &[71], IN, OUT)
                .unwrap()
                .hil_rx()
                .is_empty()
        );
    }

    #[test]
    fn card_numbers_count_from_one() {
        let rx: Vec<u16> = (101..=132).collect();
        let tx: Vec<u16> = (71..=93).collect();
        let m = ChannelMap::new(&rx, &tx, IN, OUT).unwrap();
        let want_rx: Vec<usize> = (100..=131).collect();
        let want_tx: Vec<usize> = (70..=92).collect();
        assert_eq!((m.rx(), m.tx()), (&want_rx[..], &want_tx[..]));
        // Topology order, not card order.
        let m = ChannelMap::new(&[1, 132, 101], &[93, 71], IN, OUT).unwrap();
        assert_eq!(
            (m.rx(), m.tx()),
            (&[0usize, 131, 100][..], &[92usize, 70][..])
        );
        let empty = ChannelMap::new(&[], &[], 0, 0).unwrap();
        assert!(empty.rx().is_empty() && empty.tx().is_empty());
    }

    #[test]
    fn channels_the_card_lacks_refuse() {
        assert_eq!(
            ChannelMap::new(&[0], &[], IN, OUT),
            Err(MapError::Zero { side: "rx" })
        );
        assert_eq!(
            ChannelMap::new(&[], &[71, 0], IN, OUT),
            Err(MapError::Zero { side: "tx" })
        );
        assert_eq!(
            ChannelMap::new(&[101, 133], &[], IN, OUT),
            Err(MapError::Missing {
                side: "rx",
                channel: 133,
                card: IN
            })
        );
        assert_eq!(
            ChannelMap::new(&[], &[94], IN, OUT),
            Err(MapError::Missing {
                side: "tx",
                channel: 94,
                card: OUT
            })
        );
        assert_eq!(
            ChannelMap::new(&[1], &[], 0, OUT),
            Err(MapError::Missing {
                side: "rx",
                channel: 1,
                card: 0
            })
        );
        // The last channel of each side is on the card.
        assert!(ChannelMap::new(&[132], &[93], IN, OUT).is_ok());
    }

    #[test]
    fn each_side_is_checked_against_its_own_count() {
        assert_eq!(
            ChannelMap::new(&[93], &[132], IN, OUT),
            Err(MapError::Missing {
                side: "tx",
                channel: 132,
                card: OUT
            })
        );
        // The inputs are checked first.
        assert_eq!(
            ChannelMap::new(&[0], &[0], IN, OUT),
            Err(MapError::Zero { side: "rx" })
        );
    }

    #[test]
    fn errors_name_the_side_and_the_channel() {
        assert_eq!(
            MapError::Zero { side: "tx" }.to_string(),
            "tx channel 0: card channels count from 1"
        );
        assert_eq!(
            MapError::Missing {
                side: "rx",
                channel: 133,
                card: IN
            }
            .to_string(),
            "rx channel 133 is not on the card (132 channels)"
        );
    }
}
