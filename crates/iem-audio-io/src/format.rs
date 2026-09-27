//! Sample formats of an ASIO card and their conversion to and from f64 (S1a
//! design note §3). Portable: the ASIO host (Windows) uses it, the tests run
//! everywhere. Integers are two's complement little-endian; a value outside
//! ±1.0 is clipped and a non-finite value becomes silence, in both directions.

use core::fmt;

/// The only sample rate iemmixer runs at (program spec I2).
pub const RATE: f64 = 96_000.0;

/// The little-endian ASIO sample types (ASIOSampleType 16–20 and 24–27; the
/// big-endian ones exist only on old Macs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    I16,
    I24,
    I32,
    F32,
    F64,
    /// A 32-bit container holding a sign-extended 16/18/20/24-bit value.
    I32In16,
    I32In18,
    I32In20,
    I32In24,
}

impl SampleFormat {
    pub fn from_asio(code: i32) -> Option<Self> {
        Some(match code {
            16 => Self::I16,
            17 => Self::I24,
            18 => Self::I32,
            19 => Self::F32,
            20 => Self::F64,
            24 => Self::I32In16,
            25 => Self::I32In18,
            26 => Self::I32In20,
            27 => Self::I32In24,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::I16 => "Int16LSB",
            Self::I24 => "Int24LSB",
            Self::I32 => "Int32LSB",
            Self::F32 => "Float32LSB",
            Self::F64 => "Float64LSB",
            Self::I32In16 => "Int32LSB16",
            Self::I32In18 => "Int32LSB18",
            Self::I32In20 => "Int32LSB20",
            Self::I32In24 => "Int32LSB24",
        }
    }

    /// Bytes per sample.
    pub fn bytes(self) -> usize {
        match self {
            Self::I16 => 2,
            Self::I24 => 3,
            Self::F64 => 8,
            Self::I32
            | Self::F32
            | Self::I32In16
            | Self::I32In18
            | Self::I32In20
            | Self::I32In24 => 4,
        }
    }

    /// 2^(bits − 1) of an integer format; `None` for floats.
    fn full_scale(self) -> Option<f64> {
        match self {
            Self::I16 | Self::I32In16 => Some(32_768.0),
            Self::I32In18 => Some(131_072.0),
            Self::I32In20 => Some(524_288.0),
            Self::I24 | Self::I32In24 => Some(8_388_608.0),
            Self::I32 => Some(2_147_483_648.0),
            Self::F32 | Self::F64 => None,
        }
    }

    /// Writes `src` into `dst` (whole samples only); returns the samples written.
    pub fn encode(self, src: &[f64], dst: &mut [u8]) -> usize {
        let n = src.len().min(dst.len() / self.bytes());
        for (out, &x) in dst.chunks_exact_mut(self.bytes()).zip(src) {
            let x = clean(x);
            match (self, self.full_scale()) {
                (Self::F32, _) => out.copy_from_slice(&(x as f32).to_le_bytes()),
                (Self::F64, _) => out.copy_from_slice(&x.to_le_bytes()),
                (Self::I16, Some(full)) => {
                    out.copy_from_slice(&(quantize(x, full) as i16).to_le_bytes())
                }
                (Self::I24, Some(full)) => {
                    let [b0, b1, b2, _] = (quantize(x, full) as i32).to_le_bytes();
                    out.copy_from_slice(&[b0, b1, b2]);
                }
                (_, Some(full)) => out.copy_from_slice(&(quantize(x, full) as i32).to_le_bytes()),
                (_, None) => out.fill(0),
            }
        }
        n
    }

    /// Reads whole samples of `src` into `dst`; returns the samples read.
    pub fn decode(self, src: &[u8], dst: &mut [f64]) -> usize {
        for (inp, out) in src.chunks_exact(self.bytes()).zip(dst.iter_mut()) {
            *out = self.read(inp);
        }
        dst.len().min(src.len() / self.bytes())
    }

    /// The largest |sample| of `src` (0 for no samples).
    pub fn peak(self, src: &[u8]) -> f64 {
        src.chunks_exact(self.bytes())
            .map(|s| self.read(s).abs())
            .fold(0.0, f64::max)
    }

    fn read(self, s: &[u8]) -> f64 {
        let x = match (self, self.full_scale()) {
            (Self::F32, _) => f64::from(f32::from_le_bytes(array(s))),
            (Self::F64, _) => f64::from_le_bytes(array(s)),
            (Self::I16, Some(full)) => f64::from(i16::from_le_bytes(array(s))) / full,
            (Self::I24, Some(full)) => {
                let [b0, b1, b2]: [u8; 3] = array(s);
                f64::from(i32::from_le_bytes([0, b0, b1, b2]) >> 8) / full
            }
            (_, Some(full)) => f64::from(i32::from_le_bytes(array(s))) / full,
            (_, None) => 0.0,
        };
        clean(x)
    }
}

impl fmt::Display for SampleFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

fn array<const N: usize>(s: &[u8]) -> [u8; N] {
    s.try_into().unwrap_or([0; N])
}

fn clean(x: f64) -> f64 {
    if x.is_finite() {
        x.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// −1.0 maps to −full, +1.0 to the largest code (full − 1).
fn quantize(x: f64, full: f64) -> i64 {
    let top = full as i64;
    ((x * full).round() as i64).clamp(-top, top - 1)
}

/// Why the host refuses to stream (I2, A1).
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    /// The card runs at another rate; the host never sets it.
    Rate(f64),
    /// The driver's preferred buffer is not the one the owner set.
    Buffer {
        preferred: i32,
        expected: i32,
    },
    NoChannels,
    /// A sample type this host does not convert.
    Format(i32),
    /// Channels of different sample types.
    MixedFormats,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rate(r) => write!(
                f,
                "the card runs at {r} Hz; only {RATE} Hz is accepted (I2)"
            ),
            Self::Buffer {
                preferred,
                expected,
            } => write!(
                f,
                "the driver's preferred buffer is {preferred} samples, expected {expected}"
            ),
            Self::NoChannels => f.write_str("the driver reports no channels"),
            Self::Format(code) => write!(f, "unsupported ASIO sample type {code}"),
            Self::MixedFormats => f.write_str("channels of different sample types"),
        }
    }
}

impl std::error::Error for Refusal {}

/// Accepts a driver only at 96 kHz, at the expected preferred buffer, with one
/// supported sample type on every channel; returns that format.
pub fn admit(
    rate: f64,
    preferred: i32,
    expected: i32,
    types: &[i32],
) -> Result<SampleFormat, Refusal> {
    if rate != RATE {
        return Err(Refusal::Rate(rate));
    }
    if expected <= 0 || preferred != expected {
        return Err(Refusal::Buffer {
            preferred,
            expected,
        });
    }
    let first = *types.first().ok_or(Refusal::NoChannels)?;
    let format = SampleFormat::from_asio(first).ok_or(Refusal::Format(first))?;
    if types.iter().any(|&t| t != first) {
        return Err(Refusal::MixedFormats);
    }
    Ok(format)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [SampleFormat; 9] = [
        SampleFormat::I16,
        SampleFormat::I24,
        SampleFormat::I32,
        SampleFormat::F32,
        SampleFormat::F64,
        SampleFormat::I32In16,
        SampleFormat::I32In18,
        SampleFormat::I32In20,
        SampleFormat::I32In24,
    ];

    #[test]
    fn asio_codes_map_both_ways_and_unknown_codes_are_none() {
        let codes = [16, 17, 18, 19, 20, 24, 25, 26, 27];
        for (code, f) in codes.into_iter().zip(ALL) {
            assert_eq!(SampleFormat::from_asio(code), Some(f), "{code}");
        }
        for code in [-1, 0, 2, 15, 21, 23, 28, 32] {
            assert_eq!(SampleFormat::from_asio(code), None, "{code}");
        }
    }

    #[test]
    fn names_and_sizes() {
        let names: Vec<_> = ALL.iter().map(|f| f.to_string()).collect();
        assert_eq!(
            names,
            [
                "Int16LSB",
                "Int24LSB",
                "Int32LSB",
                "Float32LSB",
                "Float64LSB",
                "Int32LSB16",
                "Int32LSB18",
                "Int32LSB20",
                "Int32LSB24"
            ]
        );
        let sizes: Vec<_> = ALL.iter().map(|f| f.bytes()).collect();
        assert_eq!(sizes, [2, 3, 4, 4, 8, 4, 4, 4, 4]);
    }

    #[test]
    fn integer_codes_are_exact() {
        let mut b = [0u8; 4];
        SampleFormat::I32.encode(&[0.5], &mut b);
        assert_eq!(i32::from_le_bytes(b), 1 << 30);
        SampleFormat::I32.encode(&[1.0], &mut b);
        assert_eq!(i32::from_le_bytes(b), i32::MAX);
        SampleFormat::I32.encode(&[-1.0], &mut b);
        assert_eq!(i32::from_le_bytes(b), i32::MIN);
        SampleFormat::I32In24.encode(&[-0.5], &mut b);
        assert_eq!(i32::from_le_bytes(b), -(1 << 22));
        SampleFormat::I32In20.encode(&[1.0], &mut b);
        assert_eq!(i32::from_le_bytes(b), (1 << 19) - 1);
        SampleFormat::I32In18.encode(&[-1.0], &mut b);
        assert_eq!(i32::from_le_bytes(b), -(1 << 17));
        SampleFormat::I32In16.encode(&[0.25], &mut b);
        assert_eq!(i32::from_le_bytes(b), 1 << 13);
        let mut h = [0u8; 2];
        SampleFormat::I16.encode(&[-0.25], &mut h);
        assert_eq!(i16::from_le_bytes(h), -(1 << 13));
        let mut t = [0u8; 3];
        SampleFormat::I24.encode(&[-1.0 / 8_388_608.0], &mut t);
        assert_eq!(t, [0xff, 0xff, 0xff]);
        SampleFormat::I24.encode(&[0.5], &mut t);
        assert_eq!(t, [0x00, 0x00, 0x40]);
    }

    #[test]
    fn round_trips_are_within_one_code() {
        let xs = [0.0, 0.5, -0.5, 0.123_456_789, -0.987_654_321, 0.999_9, -1.0];
        for f in ALL {
            let mut bytes = vec![0u8; xs.len() * f.bytes()];
            assert_eq!(f.encode(&xs, &mut bytes), xs.len());
            let mut back = [9.0; 7];
            assert_eq!(f.decode(&bytes, &mut back), xs.len());
            let step = f.full_scale().map_or(1e-7, |full| 1.0 / full);
            for (a, b) in xs.iter().zip(back) {
                assert!((a - b).abs() <= step, "{f}: {a} -> {b}");
            }
        }
    }

    #[test]
    fn out_of_range_is_clipped_and_non_finite_is_silence() {
        for f in ALL {
            let xs = [2.0, -3.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY];
            let mut bytes = vec![0xAAu8; xs.len() * f.bytes()];
            f.encode(&xs, &mut bytes);
            let mut back = [9.0; 5];
            f.decode(&bytes, &mut back);
            let step = f.full_scale().map_or(0.0, |full| 1.0 / full);
            assert!((back[0] - 1.0).abs() <= step, "{f}: {}", back[0]);
            assert_eq!(back[1], -1.0, "{f}");
            assert_eq!(&back[2..], &[0.0; 3], "{f}");
        }
        let mut back = [9.0; 2];
        let nan = f32::NAN
            .to_le_bytes()
            .into_iter()
            .chain(2.5f32.to_le_bytes())
            .collect::<Vec<_>>();
        SampleFormat::F32.decode(&nan, &mut back);
        assert_eq!(back, [0.0, 1.0]);
    }

    #[test]
    fn only_whole_samples_are_converted() {
        let mut b = [0u8; 7];
        assert_eq!(SampleFormat::I32.encode(&[0.5, 0.5], &mut b), 1);
        assert_eq!(&b[4..], &[0, 0, 0]);
        let mut out = [9.0; 3];
        assert_eq!(
            SampleFormat::I16.decode(&[0, 0x40, 0, 0xC0, 7], &mut out),
            2
        );
        assert_eq!(out, [0.5, -0.5, 9.0]);
        assert_eq!(
            SampleFormat::I16.decode(&[0, 0x40, 0, 0xC0], &mut out[..1]),
            1
        );
    }

    #[test]
    fn peak_is_the_largest_magnitude() {
        let mut b = [0u8; 12];
        SampleFormat::I32.encode(&[0.25, -0.75, 0.5], &mut b);
        assert_eq!(SampleFormat::I32.peak(&b), 0.75);
        assert_eq!(SampleFormat::I32.peak(&[]), 0.0);
        assert_eq!(SampleFormat::I32.peak(&b[..3]), 0.0);
    }

    #[test]
    fn admit_accepts_only_96k_the_expected_buffer_and_one_known_format() {
        assert_eq!(
            admit(96_000.0, 32, 32, &[18, 18, 18]),
            Ok(SampleFormat::I32)
        );
        assert_eq!(admit(48_000.0, 32, 32, &[18]), Err(Refusal::Rate(48_000.0)));
        assert_eq!(
            admit(96_000.000_1, 32, 32, &[18]),
            Err(Refusal::Rate(96_000.000_1))
        );
        assert_eq!(
            admit(96_000.0, 64, 32, &[18]),
            Err(Refusal::Buffer {
                preferred: 64,
                expected: 32
            })
        );
        assert_eq!(
            admit(96_000.0, 0, 0, &[18]),
            Err(Refusal::Buffer {
                preferred: 0,
                expected: 0
            })
        );
        assert_eq!(
            admit(96_000.0, -1, -1, &[18]),
            Err(Refusal::Buffer {
                preferred: -1,
                expected: -1
            })
        );
        assert_eq!(admit(96_000.0, 32, 32, &[]), Err(Refusal::NoChannels));
        assert_eq!(admit(96_000.0, 32, 32, &[2]), Err(Refusal::Format(2)));
        assert_eq!(
            admit(96_000.0, 32, 32, &[18, 19]),
            Err(Refusal::MixedFormats)
        );
        assert_eq!(admit(96_000.0, 1, 1, &[19]), Ok(SampleFormat::F32));
    }

    #[test]
    fn refusals_explain_themselves() {
        assert!(Refusal::Rate(48_000.0).to_string().contains("48000 Hz"));
        assert!(
            Refusal::Buffer {
                preferred: 64,
                expected: 32
            }
            .to_string()
            .contains("64 samples, expected 32")
        );
        assert!(Refusal::Format(2).to_string().contains("type 2"));
        assert!(Refusal::NoChannels.to_string().contains("no channels"));
        assert!(Refusal::MixedFormats.to_string().contains("different"));
    }
}
