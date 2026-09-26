//! ReaEQ (Cockos) VST2 state chunk, as REAPER 7.65 writes it.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};

use crate::rpp::{Chunk, RppError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BandKind {
    LowShelf,
    HighShelf,
    HighPass,
    Band,
}

impl BandKind {
    pub const fn code(self) -> i32 {
        match self {
            Self::LowShelf => 0,
            Self::HighShelf => 1,
            Self::HighPass => 4,
            Self::Band => 8,
        }
    }

    /// Slot of this kind in the standard HP / LS / Band / Band / HS layout.
    const fn slot(self) -> usize {
        match self {
            Self::HighPass => 0,
            Self::LowShelf => 1,
            Self::Band => 2,
            Self::HighShelf => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Band {
    pub kind: BandKind,
    pub enabled: bool,
    pub freq_hz: f64,
    pub gain_lin: f64,
    pub bw_oct: f64,
}

impl Band {
    pub const fn new(kind: BandKind, freq_hz: f64, gain_lin: f64, bw_oct: f64) -> Self {
        Self {
            kind,
            enabled: true,
            freq_hz,
            gain_lin,
            bw_oct,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReaEq {
    pub bands: Vec<Band>,
    pub global_gain: f64,
}

/// Default band frequencies as REAPER stores them (bands 2-5 copied from a
/// REAPER-written chunk; the HPF default from its f32 normalised value).
pub const STANDARD_FREQS: [f64; 5] = [
    80.20834168547682,
    200.3077623397404,
    801.9398157380639,
    2996.2342070275295,
    8016.061124722856,
];
const STANDARD_KINDS: [BandKind; 5] = [
    BandKind::HighPass,
    BandKind::LowShelf,
    BandKind::Band,
    BandKind::Band,
    BandKind::HighShelf,
];
const STANDARD_BWS: [f64; 5] = [2.0, 2.0, 1.0, 1.0, 2.0];
const TAIL: [u8; 16] = [0, 0, 0, 0, 0xfd, 1, 0, 0, 0x5c, 1, 0, 0, 2, 0, 0, 0];
const PROGRAM: &[u8] = b"\0Program 1\0\x10\0\0\0";
pub const HEAD: &str =
    r#"VST "VST: ReaEQ (Cockos)" reaeq.dll 0 "" 1919247729<56535472656571726561657100000000> """#;

impl ReaEq {
    /// The standard layout, every band off and flat, global gain 1.
    pub fn standard_flat() -> Self {
        let bands = (0..5)
            .map(|i| Band {
                kind: STANDARD_KINDS[i],
                enabled: false,
                freq_hz: STANDARD_FREQS[i],
                gain_lin: 1.0,
                bw_oct: STANDARD_BWS[i],
            })
            .collect();
        Self {
            bands,
            global_gain: 1.0,
        }
    }

    /// The standard layout with one enabled band in its kind's slot.
    pub fn single(band: Band) -> Self {
        let mut eq = Self::standard_flat();
        eq.bands[band.kind.slot()] = band;
        eq
    }

    pub fn state(&self) -> Result<Vec<u8>, RppError> {
        let count = i32::try_from(self.bands.len())
            .map_err(|_| RppError::Invalid("too many bands".into()))?;
        let mut b = Vec::with_capacity(8 + 33 * self.bands.len() + 32);
        b.extend_from_slice(&33i32.to_le_bytes());
        b.extend_from_slice(&count.to_le_bytes());
        for band in &self.bands {
            for x in [band.freq_hz, band.gain_lin, band.bw_oct] {
                if !x.is_finite() || x < 0.0 {
                    return Err(RppError::Invalid(format!("ReaEQ band value {x}")));
                }
            }
            b.extend_from_slice(&band.kind.code().to_le_bytes());
            b.extend_from_slice(&i32::from(band.enabled).to_le_bytes());
            b.extend_from_slice(&band.freq_hz.to_le_bytes());
            b.extend_from_slice(&band.gain_lin.to_le_bytes());
            b.extend_from_slice(&band.bw_oct.to_le_bytes());
            b.push(1);
        }
        if !self.global_gain.is_finite() || self.global_gain < 0.0 {
            return Err(RppError::Invalid(format!(
                "ReaEQ global gain {}",
                self.global_gain
            )));
        }
        b.extend_from_slice(&1i32.to_le_bytes());
        b.extend_from_slice(&1i32.to_le_bytes());
        b.extend_from_slice(&self.global_gain.to_le_bytes());
        b.extend_from_slice(&TAIL);
        Ok(b)
    }

    pub fn chunk(&self) -> Result<Chunk, RppError> {
        let state = self.state()?;
        let len =
            u32::try_from(state.len()).map_err(|_| RppError::Invalid("state too long".into()))?;
        let mut header = Vec::with_capacity(60);
        header.extend_from_slice(b"qeer");
        header.extend_from_slice(&0xfeed_5eee_u32.to_le_bytes());
        for _ in 0..2 {
            header.extend_from_slice(&2u32.to_le_bytes());
            header.extend_from_slice(&1u64.to_le_bytes());
            header.extend_from_slice(&2u64.to_le_bytes());
        }
        header.extend_from_slice(&len.to_le_bytes());
        header.extend_from_slice(&1u32.to_le_bytes());
        header.extend_from_slice(&0x0010_0000_u32.to_le_bytes());
        let mut c = Chunk::new(HEAD);
        c.line(STANDARD.encode(&header));
        for part in state.chunks(96) {
            c.line(STANDARD.encode(part));
        }
        c.line(STANDARD.encode(PROGRAM));
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpp::Node;

    fn lines(c: &Chunk) -> Vec<String> {
        c.body
            .iter()
            .map(|n| match n {
                Node::Line(l) => l.clone(),
                Node::Chunk(_) => panic!("nested chunk"),
            })
            .collect()
    }

    #[test]
    fn matches_a_chunk_written_by_reaper_7_65() {
        let eq = ReaEq::single(Band::new(BandKind::HighPass, 100.41747866600939, 1.0, 2.0));
        assert_eq!(
            lines(&eq.chunk().unwrap()),
            vec![
                "cWVlcu5e7f4CAAAAAQAAAAAAAAACAAAAAAAAAAIAAAABAAAAAAAAAAIAAAAAAAAAzQAAAAEAAAAAABAA",
                "IQAAAAUAAAAEAAAAAQAAAG9ScPi3GllAAAAAAAAA8D8AAAAAAAAAQAEAAAAAAAAAAAQEaDDZCWlAAAAAAAAA8D8AAAAAAAAAQAEIAAAAAAAAAAEaHb6ED4lAAAAAAAAA",
                "8D8AAAAAAADwPwEIAAAAAAAAAHfH++l3aKdAAAAAAAAA8D8AAAAAAADwPwEBAAAAAAAAAKWt3qUPUL9AAAAAAAAA8D8AAAAAAAAAQAEBAAAAAQAAAAAAAAAAAPA/AAAA",
                "AP0BAABcAQAAAgAAAA==",
                "AFByb2dyYW0gMQAQAAAA",
            ]
        );
    }

    #[test]
    fn single_places_each_kind_in_its_slot() {
        for (kind, slot) in [
            (BandKind::HighPass, 0),
            (BandKind::LowShelf, 1),
            (BandKind::Band, 2),
            (BandKind::HighShelf, 4),
        ] {
            let eq = ReaEq::single(Band::new(kind, 1000.0, 2.0, 1.0));
            assert_eq!(eq.bands[slot].kind, kind);
            assert!(eq.bands[slot].enabled);
            assert_eq!(eq.bands.iter().filter(|b| b.enabled).count(), 1);
        }
    }

    #[test]
    fn state_length_is_in_the_header_and_bad_values_are_refused() {
        let eq = ReaEq::standard_flat();
        assert_eq!(eq.state().unwrap().len(), 205);
        let mut bad = ReaEq::standard_flat();
        bad.bands[2].gain_lin = f64::NAN;
        assert!(bad.chunk().is_err());
        let mut neg = ReaEq::standard_flat();
        neg.global_gain = -1.0;
        assert!(neg.chunk().is_err());
    }

    #[test]
    fn zero_gains_are_valid_and_written_as_is() {
        let mut eq = ReaEq::single(Band::new(BandKind::Band, 1000.0, 0.0, 1.0));
        eq.global_gain = 0.0;
        let state = eq.state().unwrap();
        let n = state.len();
        assert_eq!(state[n - 24..n - 16], 0.0f64.to_le_bytes());
        assert_eq!(state[n - 16..], TAIL);
    }

    #[test]
    fn kinds_serialise_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&BandKind::HighPass).unwrap(),
            "\"high_pass\""
        );
        let back: BandKind = serde_json::from_str("\"low_shelf\"").unwrap();
        assert_eq!(back, BandKind::LowShelf);
    }
}
