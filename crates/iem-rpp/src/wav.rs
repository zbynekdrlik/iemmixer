//! Minimal IEEE-float WAV writer (format tag 3) for the stimuli.

use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum WavError {
    #[error("no channels")]
    NoChannels,
    #[error("channels differ in length")]
    Ragged,
    #[error("unsupported bit depth {0} (32 or 64)")]
    Bits(u16),
    #[error("sample is not finite")]
    NotFinite,
    #[error("too large for a RIFF file")]
    TooLarge,
}

/// Byte offset of the first sample.
pub const DATA_OFFSET: usize = 58;

pub fn float_wav(rate: u32, bits: u16, channels: &[Vec<f64>]) -> Result<Vec<u8>, WavError> {
    let first = channels.first().ok_or(WavError::NoChannels)?;
    let frames = first.len();
    if channels.iter().any(|c| c.len() != frames) {
        return Err(WavError::Ragged);
    }
    if bits != 32 && bits != 64 {
        return Err(WavError::Bits(bits));
    }
    let too_large = |_| WavError::TooLarge;
    let n_ch = u16::try_from(channels.len()).map_err(too_large)?;
    let block_align = n_ch.checked_mul(bits / 8).ok_or(WavError::TooLarge)?;
    let byte_rate = rate
        .checked_mul(u32::from(block_align))
        .ok_or(WavError::TooLarge)?;
    let data_len = frames
        .checked_mul(usize::from(block_align))
        .ok_or(WavError::TooLarge)?;
    let data_len32 = u32::try_from(data_len).map_err(too_large)?;
    let riff_len = u32::try_from(DATA_OFFSET - 8 + data_len).map_err(too_large)?;
    let frames32 = u32::try_from(frames).map_err(too_large)?;

    let mut out = Vec::with_capacity(DATA_OFFSET + data_len);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_len.to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&18u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes());
    out.extend_from_slice(&n_ch.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(b"fact");
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&frames32.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len32.to_le_bytes());
    for i in 0..frames {
        for ch in channels {
            let x = ch[i];
            if !x.is_finite() {
                return Err(WavError::NotFinite);
            }
            if bits == 64 {
                out.extend_from_slice(&x.to_le_bytes());
            } else {
                #[allow(clippy::cast_possible_truncation)]
                out.extend_from_slice(&(x as f32).to_le_bytes());
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_at(b: &[u8], i: usize) -> u16 {
        u16::from_le_bytes([b[i], b[i + 1]])
    }
    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
    }

    #[test]
    fn writes_a_64_bit_float_header_and_interleaved_frames() {
        let b = float_wav(96_000, 64, &[vec![0.5, -1.0], vec![0.25, 0.0]]).unwrap();
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(u32_at(&b, 4) as usize, b.len() - 8);
        assert_eq!(&b[8..16], b"WAVEfmt ");
        assert_eq!(u32_at(&b, 16), 18);
        assert_eq!(u16_at(&b, 20), 3);
        assert_eq!(u16_at(&b, 22), 2);
        assert_eq!(u32_at(&b, 24), 96_000);
        assert_eq!(u32_at(&b, 28), 96_000 * 16);
        assert_eq!(u16_at(&b, 32), 16);
        assert_eq!(u16_at(&b, 34), 64);
        assert_eq!(&b[38..42], b"fact");
        assert_eq!(u32_at(&b, 46), 2);
        assert_eq!(&b[50..54], b"data");
        assert_eq!(u32_at(&b, 54), 32);
        let frame0: Vec<f64> = b[DATA_OFFSET..DATA_OFFSET + 16]
            .chunks(8)
            .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
            .collect();
        assert_eq!(frame0, vec![0.5, 0.25]);
        assert_eq!(b.len(), DATA_OFFSET + 32);
    }

    #[test]
    fn writes_32_bit_samples_as_f32() {
        let b = float_wav(48_000, 32, &[vec![0.5]]).unwrap();
        assert_eq!(u16_at(&b, 34), 32);
        assert_eq!(
            f32::from_le_bytes(b[DATA_OFFSET..DATA_OFFSET + 4].try_into().unwrap()),
            0.5
        );
    }

    #[test]
    fn refuses_bad_input() {
        assert_eq!(float_wav(48_000, 64, &[]), Err(WavError::NoChannels));
        assert_eq!(
            float_wav(48_000, 64, &[vec![0.0], vec![]]),
            Err(WavError::Ragged)
        );
        assert_eq!(float_wav(48_000, 24, &[vec![0.0]]), Err(WavError::Bits(24)));
        assert_eq!(
            float_wav(48_000, 64, &[vec![f64::NAN]]),
            Err(WavError::NotFinite)
        );
    }
}
