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

    /// The kind of a ReaEQ band type code (only the four the site uses).
    pub const fn from_code(code: i32) -> Option<Self> {
        match code {
            0 => Some(Self::LowShelf),
            1 => Some(Self::HighShelf),
            4 => Some(Self::HighPass),
            8 => Some(Self::Band),
            _ => None,
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

const MAGIC: &[u8; 4] = b"qeer";
const HEADER_LEN: usize = 60;
const SIZE_AT: usize = 48;
const STATE_VERSION: i32 = 33;
const BAND_LEN: usize = 33;

fn truncated(at: usize) -> RppError {
    RppError::Invalid(format!("ReaEQ chunk truncated at byte {at}"))
}

fn arr<const N: usize>(b: &[u8], at: usize) -> Result<[u8; N], RppError> {
    b.get(at..at + N)
        .and_then(|s| <[u8; N]>::try_from(s).ok())
        .ok_or_else(|| truncated(at))
}

fn put(b: &mut [u8], at: usize, v: &[u8]) -> Result<(), RppError> {
    b.get_mut(at..at + v.len())
        .ok_or_else(|| truncated(at))?
        .copy_from_slice(v);
    Ok(())
}

fn check_value(x: f64) -> Result<(), RppError> {
    if x.is_finite() && x >= 0.0 {
        Ok(())
    } else {
        Err(RppError::Invalid(format!("ReaEQ value {x}")))
    }
}

/// A ReaEQ chunk as read from a project (S4): the decoded EQ plus the exact
/// bytes and base64 line layout, so a patch changes only the values that differ.
#[derive(Debug, Clone, PartialEq)]
pub struct EqBlob {
    pub eq: ReaEq,
    bytes: Vec<u8>,
    line_lens: Vec<usize>,
}

impl EqBlob {
    /// Decodes the chunk's body lines (each line is its own base64 block).
    pub fn decode(lines: &[&str]) -> Result<Self, RppError> {
        let mut bytes = Vec::new();
        let mut line_lens = Vec::with_capacity(lines.len());
        for line in lines {
            let b = STANDARD
                .decode(line.trim())
                .map_err(|e| RppError::Invalid(format!("ReaEQ base64: {e}")))?;
            line_lens.push(b.len());
            bytes.extend_from_slice(&b);
        }
        if bytes.get(..4) != Some(&MAGIC[..]) {
            return Err(RppError::Invalid("not a ReaEQ chunk".into()));
        }
        let size = u32::from_le_bytes(arr(&bytes, SIZE_AT)?) as usize;
        let state = bytes
            .get(HEADER_LEN..HEADER_LEN + size)
            .ok_or_else(|| RppError::Invalid("ReaEQ state shorter than its header says".into()))?;
        let version = i32::from_le_bytes(arr(state, 0)?);
        if version != STATE_VERSION {
            return Err(RppError::Invalid(format!("ReaEQ state version {version}")));
        }
        let n = usize::try_from(i32::from_le_bytes(arr(state, 4)?))
            .map_err(|_| RppError::Invalid("negative ReaEQ band count".into()))?;
        let mut bands = Vec::with_capacity(n.min(64));
        for i in 0..n {
            let o = 8 + i * BAND_LEN;
            let code = i32::from_le_bytes(arr(state, o)?);
            let kind = BandKind::from_code(code).ok_or_else(|| {
                RppError::Invalid(format!("ReaEQ band {} has type {code}", i + 1))
            })?;
            bands.push(Band {
                kind,
                enabled: i32::from_le_bytes(arr(state, o + 4)?) != 0,
                freq_hz: f64::from_le_bytes(arr(state, o + 8)?),
                gain_lin: f64::from_le_bytes(arr(state, o + 16)?),
                bw_oct: f64::from_le_bytes(arr(state, o + 24)?),
            });
        }
        let global_gain = f64::from_le_bytes(arr(state, 8 + n * BAND_LEN + 8)?);
        Ok(Self {
            eq: ReaEq { bands, global_gain },
            bytes,
            line_lens,
        })
    }

    /// The chunk's body lines with `eq` written in: fields equal to the
    /// decoded ones keep their bytes, the base64 keeps the line layout.
    pub fn lines_for(&self, eq: &ReaEq) -> Result<Vec<String>, RppError> {
        if eq.bands.len() != self.eq.bands.len() {
            return Err(RppError::Invalid(format!(
                "ReaEQ has {} bands, the state {}",
                self.eq.bands.len(),
                eq.bands.len()
            )));
        }
        let mut b = self.bytes.clone();
        for (i, (new, old)) in eq.bands.iter().zip(&self.eq.bands).enumerate() {
            let o = HEADER_LEN + 8 + i * BAND_LEN;
            if new.kind != old.kind {
                put(&mut b, o, &new.kind.code().to_le_bytes())?;
            }
            if new.enabled != old.enabled {
                put(&mut b, o + 4, &i32::from(new.enabled).to_le_bytes())?;
            }
            for (at, x, was) in [
                (o + 8, new.freq_hz, old.freq_hz),
                (o + 16, new.gain_lin, old.gain_lin),
                (o + 24, new.bw_oct, old.bw_oct),
            ] {
                check_value(x)?;
                if x.to_bits() != was.to_bits() {
                    put(&mut b, at, &x.to_le_bytes())?;
                }
            }
        }
        check_value(eq.global_gain)?;
        if eq.global_gain.to_bits() != self.eq.global_gain.to_bits() {
            let at = HEADER_LEN + 8 + eq.bands.len() * BAND_LEN + 8;
            put(&mut b, at, &eq.global_gain.to_le_bytes())?;
        }
        let mut out = Vec::with_capacity(self.line_lens.len());
        let mut at = 0;
        for len in &self.line_lens {
            out.push(STANDARD.encode(b.get(at..at + len).ok_or_else(|| truncated(at))?));
            at += len;
        }
        Ok(out)
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

    fn body(c: &Chunk) -> Vec<String> {
        lines(c)
    }

    #[test]
    fn decodes_the_reaper_written_chunk() {
        let eq = ReaEq::single(Band::new(BandKind::HighPass, 100.41747866600939, 1.0, 2.0));
        let text = body(&eq.chunk().unwrap());
        let refs: Vec<&str> = text.iter().map(String::as_str).collect();
        let blob = EqBlob::decode(&refs).unwrap();
        assert_eq!(blob.eq, eq);
        assert_eq!(blob.lines_for(&eq).unwrap(), text);
        assert_eq!(BandKind::from_code(3), None);
        for kind in [
            BandKind::LowShelf,
            BandKind::HighShelf,
            BandKind::HighPass,
            BandKind::Band,
        ] {
            assert_eq!(BandKind::from_code(kind.code()), Some(kind));
        }
    }

    #[test]
    fn a_patch_changes_only_the_written_values() {
        let eq = ReaEq::standard_flat();
        let text = body(&eq.chunk().unwrap());
        let refs: Vec<&str> = text.iter().map(String::as_str).collect();
        let blob = EqBlob::decode(&refs).unwrap();
        let mut new = eq.clone();
        new.bands[2] = Band {
            kind: BandKind::HighShelf,
            enabled: true,
            freq_hz: 1234.5,
            gain_lin: 0.5,
            bw_oct: 0.7,
        };
        new.bands[4].enabled = true;
        new.global_gain = 2.0;
        let patched = blob.lines_for(&new).unwrap();
        assert_eq!(patched.len(), text.len());
        assert_eq!(patched[0], text[0], "header untouched");
        assert_eq!(patched.last(), text.last(), "program block untouched");
        let prefs: Vec<&str> = patched.iter().map(String::as_str).collect();
        assert_eq!(EqBlob::decode(&prefs).unwrap().eq, new);
        let mut fewer = eq.clone();
        fewer.bands.pop();
        assert!(blob.lines_for(&fewer).is_err());
        let mut bad = eq;
        bad.bands[0].gain_lin = f64::NAN;
        assert!(blob.lines_for(&bad).is_err());
        let mut bad_gain = ReaEq::standard_flat();
        bad_gain.global_gain = -1.0;
        assert!(blob.lines_for(&bad_gain).is_err());
    }

    #[test]
    fn broken_chunks_are_errors() {
        let text = body(&ReaEq::standard_flat().chunk().unwrap());
        assert!(EqBlob::decode(&["not base64!"]).is_err());
        assert!(EqBlob::decode(&["AAAA"]).is_err(), "no magic");
        let header_only = [text[0].as_str()];
        assert!(EqBlob::decode(&header_only).is_err(), "state missing");
        let mut state = ReaEq::standard_flat().state().unwrap();
        state[0] = 34;
        let mut bytes = STANDARD.decode(&text[0]).unwrap();
        bytes.extend_from_slice(&state);
        let wrong_version = STANDARD.encode(&bytes);
        assert!(EqBlob::decode(&[wrong_version.as_str()]).is_err());
        let mut state = ReaEq::standard_flat().state().unwrap();
        state[8] = 3;
        let mut bytes = STANDARD.decode(&text[0]).unwrap();
        bytes.extend_from_slice(&state);
        let bad_type = STANDARD.encode(&bytes);
        let err = EqBlob::decode(&[bad_type.as_str()]).unwrap_err();
        assert!(err.to_string().contains("type 3"), "{err}");
        let mut state = ReaEq::standard_flat().state().unwrap();
        state[4..8].copy_from_slice(&(-1i32).to_le_bytes());
        let mut bytes = STANDARD.decode(&text[0]).unwrap();
        bytes.extend_from_slice(&state);
        let negative = STANDARD.encode(&bytes);
        assert!(EqBlob::decode(&[negative.as_str()]).is_err());
        let mut state = ReaEq::standard_flat().state().unwrap();
        state[4..8].copy_from_slice(&6i32.to_le_bytes());
        let mut bytes = STANDARD.decode(&text[0]).unwrap();
        bytes.extend_from_slice(&state);
        let too_many = STANDARD.encode(&bytes);
        assert!(EqBlob::decode(&[too_many.as_str()]).is_err());
    }
}
