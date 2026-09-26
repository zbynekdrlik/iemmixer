//! A minimal WAV codec for the `Offline` backend: reads PCM 16/24/32-bit and
//! IEEE float 32/64-bit (plain or `WAVE_FORMAT_EXTENSIBLE`), writes 64-bit
//! float `WAVE_FORMAT_EXTENSIBLE`, so renders keep the engine's f64 exactly.

use std::io;
use std::path::Path;

use crate::Planar;

const PCM: u16 = 1;
const FLOAT: u16 = 3;
const EXTENSIBLE: u16 = 0xFFFE;
/// `KSDATAFORMAT_SUBTYPE_IEEE_FLOAT` after its first two bytes (the format code).
const GUID_TAIL: [u8; 14] = [
    0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
];

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_owned())
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(*b.get(at..)?.first_chunk::<2>()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(*b.get(at..)?.first_chunk::<4>()?))
}

#[derive(Debug, Clone, Copy)]
struct Format {
    code: u16,
    channels: usize,
    rate: u32,
    bits: u16,
}

fn parse_fmt(b: &[u8]) -> io::Result<Format> {
    let field = |v: Option<u16>| v.ok_or_else(|| invalid("short fmt chunk"));
    let mut code = field(u16_at(b, 0))?;
    let channels = usize::from(field(u16_at(b, 2))?);
    let rate = u32_at(b, 4).ok_or_else(|| invalid("short fmt chunk"))?;
    let bits = field(u16_at(b, 14))?;
    if code == EXTENSIBLE {
        code = field(u16_at(b, 24))?;
    }
    if channels == 0 {
        return Err(invalid("zero channels"));
    }
    Ok(Format {
        code,
        channels,
        rate,
        bits,
    })
}

fn sample(fmt: Format, s: &[u8]) -> Option<f64> {
    Some(match (fmt.code, fmt.bits) {
        (PCM, 16) => f64::from(i16::from_le_bytes(*s.first_chunk::<2>()?)) / 32_768.0,
        (PCM, 24) => {
            let [a, b, c] = *s.first_chunk::<3>()?;
            f64::from(i32::from_le_bytes([0, a, b, c]) >> 8) / 8_388_608.0
        }
        (PCM, 32) => f64::from(i32::from_le_bytes(*s.first_chunk::<4>()?)) / 2_147_483_648.0,
        (FLOAT, 32) => f64::from(f32::from_le_bytes(*s.first_chunk::<4>()?)),
        (FLOAT, 64) => f64::from_le_bytes(*s.first_chunk::<8>()?),
        _ => return None,
    })
}

/// Decodes a WAV file; returns the sample rate and the audio.
pub fn read(bytes: &[u8]) -> io::Result<(u32, Planar)> {
    if bytes.get(..4) != Some(&b"RIFF"[..]) || bytes.get(8..12) != Some(&b"WAVE"[..]) {
        return Err(invalid("not a RIFF/WAVE file"));
    }
    let mut at = 12usize;
    let mut fmt = None;
    while let (Some(id), Some(len)) = (
        bytes.get(at..at.saturating_add(4)),
        u32_at(bytes, at.saturating_add(4)),
    ) {
        let len = len as usize;
        let body = at.saturating_add(8);
        let chunk = bytes
            .get(body..body.saturating_add(len))
            .unwrap_or_else(|| bytes.get(body..).unwrap_or_default());
        match id {
            b"fmt " => fmt = Some(parse_fmt(chunk)?),
            b"data" => {
                let f = fmt.ok_or_else(|| invalid("data before fmt"))?;
                let width = usize::from(f.bits / 8);
                if sample(f, &[0; 8]).is_none() || width == 0 {
                    return Err(invalid("unsupported sample format"));
                }
                let frames = chunk.len() / (width * f.channels);
                let mut audio = Planar::new(f.channels, frames);
                for (i, frame) in chunk
                    .chunks_exact(width * f.channels)
                    .take(frames)
                    .enumerate()
                {
                    for (ch, s) in frame.chunks_exact(width).enumerate() {
                        if let Some(x) = audio.channel_mut(ch).get_mut(i) {
                            *x = sample(f, s).unwrap_or(0.0);
                        }
                    }
                }
                return Ok((f.rate, audio));
            }
            _ => {}
        }
        at = body.saturating_add(len).saturating_add(len & 1);
    }
    Err(invalid("no data chunk"))
}

/// Encodes 64-bit float `WAVE_FORMAT_EXTENSIBLE`.
pub fn write(rate: u32, audio: &Planar) -> io::Result<Vec<u8>> {
    let too_big = || io::Error::new(io::ErrorKind::InvalidInput, "audio too large for WAV");
    let channels = u16::try_from(audio.channels()).map_err(|_| too_big())?;
    let data_len = audio
        .channels()
        .checked_mul(audio.frames())
        .and_then(|n| n.checked_mul(8))
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(too_big)?;
    let riff_len = data_len.checked_add(4 + 8 + 40 + 8).ok_or_else(too_big)?;
    let block_align = channels.checked_mul(8).ok_or_else(too_big)?;
    let byte_rate = rate
        .checked_mul(u32::from(block_align))
        .ok_or_else(too_big)?;
    let mut out = Vec::with_capacity(data_len as usize + 68);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_len.to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&EXTENSIBLE.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&64u16.to_le_bytes());
    out.extend_from_slice(&22u16.to_le_bytes());
    out.extend_from_slice(&64u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&FLOAT.to_le_bytes());
    out.extend_from_slice(&GUID_TAIL);
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..audio.frames() {
        for ch in 0..audio.channels() {
            let x = audio.channel(ch).get(i).copied().unwrap_or(0.0);
            out.extend_from_slice(&x.to_le_bytes());
        }
    }
    Ok(out)
}

pub fn read_file(path: &Path) -> io::Result<(u32, Planar)> {
    read(&std::fs::read(path)?)
}

pub fn write_file(path: &Path, rate: u32, audio: &Planar) -> io::Result<()> {
    std::fs::write(path, write(rate, audio)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn riff(fmt: &[u8], extra: &[u8], data: &[u8]) -> Vec<u8> {
        let mut v = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        v.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
        v.extend_from_slice(fmt);
        v.extend_from_slice(extra);
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(data);
        v
    }

    fn fmt(code: u16, channels: u16, bits: u16) -> Vec<u8> {
        let align = channels * bits / 8;
        let mut f = Vec::new();
        f.extend_from_slice(&code.to_le_bytes());
        f.extend_from_slice(&channels.to_le_bytes());
        f.extend_from_slice(&48_000u32.to_le_bytes());
        f.extend_from_slice(&(48_000 * u32::from(align)).to_le_bytes());
        f.extend_from_slice(&align.to_le_bytes());
        f.extend_from_slice(&bits.to_le_bytes());
        f
    }

    #[test]
    fn wav_round_trips_float64() {
        let mut a = Planar::new(3, 5);
        for ch in 0..3 {
            for (i, x) in a.channel_mut(ch).iter_mut().enumerate() {
                *x = (ch as f64 + 1.0) * (i as f64 - 2.0) / 3.0 + 1e-17;
            }
        }
        let bytes = write(96_000, &a).unwrap();
        assert_eq!(bytes.len(), 68 + 3 * 5 * 8);
        assert_eq!(u32_at(&bytes, 4), Some(bytes.len() as u32 - 8));
        let (rate, b) = read(&bytes).unwrap();
        assert_eq!(rate, 96_000);
        assert_eq!(b, a);
    }

    #[test]
    fn pcm_and_float32_are_read() {
        let pcm16 = riff(
            &fmt(PCM, 2, 16),
            &[],
            &[0x00, 0x40, 0x00, 0x80, 0xFF, 0x7F, 0x01, 0x00],
        );
        let (rate, a) = read(&pcm16).unwrap();
        assert_eq!(rate, 48_000);
        assert_eq!(a.channel(0), &[0.5, 32_767.0 / 32_768.0]);
        assert_eq!(a.channel(1), &[-1.0, 1.0 / 32_768.0]);
        let pcm24 = riff(&fmt(PCM, 1, 24), &[], &[0x00, 0x00, 0xC0, 0x01, 0x00, 0x00]);
        assert_eq!(
            read(&pcm24).unwrap().1.channel(0),
            &[-0.5, 1.0 / 8_388_608.0]
        );
        let pcm32 = riff(&fmt(PCM, 1, 32), &[], &(-1_073_741_824i32).to_le_bytes());
        assert_eq!(read(&pcm32).unwrap().1.channel(0), &[-0.5]);
        let f32s = riff(&fmt(FLOAT, 1, 32), &[], &0.25f32.to_le_bytes());
        assert_eq!(read(&f32s).unwrap().1.channel(0), &[0.25]);
    }

    #[test]
    fn odd_chunks_are_skipped_with_their_pad_byte() {
        let mut extra = b"LIST".to_vec();
        extra.extend_from_slice(&3u32.to_le_bytes());
        extra.extend_from_slice(b"abc\0");
        let bytes = riff(&fmt(FLOAT, 1, 64), &extra, &0.75f64.to_le_bytes());
        assert_eq!(read(&bytes).unwrap().1.channel(0), &[0.75]);
    }

    #[test]
    fn bad_files_are_invalid_data() {
        let bad = [
            b"RIFX\0\0\0\0WAVE".to_vec(),
            riff(&fmt(PCM, 1, 8), &[], &[0]),
            riff(&fmt(PCM, 0, 16), &[], &[0, 0]),
            riff(&[1, 0], &[], &[]),
            b"RIFF\0\0\0\0WAVEdata\x02\0\0\0\0\0".to_vec(),
            b"RIFF\0\0\0\0WAVE".to_vec(),
        ];
        for b in bad {
            assert_eq!(read(&b).unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
        // Valid chunks do not rescue a wrong RIFF or WAVE tag.
        let good = riff(&fmt(FLOAT, 1, 64), &[], &0.5f64.to_le_bytes());
        assert_eq!(read(&good).unwrap().1.channel(0), &[0.5]);
        for (at, tag) in [(0, b"RIFX"), (8, b"WAVX")] {
            let mut wrong = good.clone();
            wrong[at..at + 4].copy_from_slice(tag);
            assert_eq!(
                read(&wrong).unwrap_err().to_string(),
                "not a RIFF/WAVE file"
            );
        }
        // A trailing partial frame is ignored.
        let partial = riff(&fmt(PCM, 2, 16), &[], &[0, 0x40, 0, 0x40, 0]);
        assert_eq!(read(&partial).unwrap().1.frames(), 1);
    }

    #[test]
    fn files_round_trip() {
        let dir = std::env::temp_dir().join(format!("iem-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.wav");
        let mut a = Planar::new(1, 2);
        a.channel_mut(0).copy_from_slice(&[0.5, -0.5]);
        write_file(&path, 96_000, &a).unwrap();
        assert_eq!(read_file(&path).unwrap(), (96_000, a));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
