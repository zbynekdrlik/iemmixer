//! Media-pipe frames (§2.3, X3–X5; design note §3.4): 20 ms of 48 kHz audio
//! as f32 little-endian after a 20-byte header — `IEMF`, version 1, stream,
//! channels, 0, sequence (`u64` LE), frames (`u16` LE), 0 0.

use std::io::{self, Read, Write};

use crate::frame::{FrameError, read_head};

pub const MEDIA_HEADER: usize = 20;
/// 20 ms at 48 kHz.
pub const FRAME_48K: usize = 960;
/// Largest `frames × channels` a frame may carry.
pub const MAX_SAMPLES: usize = 4 * FRAME_48K;
const MAGIC: [u8; 4] = *b"IEMF";
const VERSION: u8 = 1;

/// Stream ids.
pub mod stream {
    /// The engineer's listen tap (X3 slot 0), stereo.
    pub const ENGINEER_LISTEN: u8 = 0;
    /// One member's listen tap (X3 slot 1), stereo.
    pub const MEMBER_LISTEN: u8 = 1;
    /// Talkback from the server into the engine, mono.
    pub const TALKBACK: u8 = 16;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaHeader {
    pub stream: u8,
    pub channels: u8,
    pub seq: u64,
    pub frames: u16,
}

impl MediaHeader {
    fn samples(&self) -> usize {
        usize::from(self.frames) * usize::from(self.channels)
    }
}

/// Writes one frame; `samples` must hold `frames × channels` values.
pub fn write_media<W: Write>(w: &mut W, h: &MediaHeader, samples: &[f32]) -> io::Result<()> {
    if samples.len() != h.samples() || h.samples() > MAX_SAMPLES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "media frame size does not match its header",
        ));
    }
    let mut out = Vec::with_capacity(MEDIA_HEADER + 4 * samples.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&[VERSION, h.stream, h.channels, 0]);
    out.extend_from_slice(&h.seq.to_le_bytes());
    out.extend_from_slice(&h.frames.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    w.write_all(&out)?;
    w.flush()
}

fn bad(msg: &'static str) -> FrameError {
    FrameError::Io(io::Error::new(io::ErrorKind::InvalidData, msg))
}

/// Reads one frame's samples into `samples` (replacing its contents).
pub fn read_media<R: Read>(r: &mut R, samples: &mut Vec<f32>) -> Result<MediaHeader, FrameError> {
    let mut head = [0u8; MEDIA_HEADER];
    read_head(r, &mut head)?;
    let (magic, rest) = head.split_first_chunk::<4>().ok_or(bad("short header"))?;
    let (meta, rest) = rest.split_first_chunk::<4>().ok_or(bad("short header"))?;
    let (seq, rest) = rest.split_first_chunk::<8>().ok_or(bad("short header"))?;
    let (frames, _) = rest.split_first_chunk::<2>().ok_or(bad("short header"))?;
    if *magic != MAGIC {
        return Err(bad("not a media frame"));
    }
    let [version, stream, channels, _] = *meta;
    if version != VERSION {
        return Err(bad("unsupported media frame version"));
    }
    let h = MediaHeader {
        stream,
        channels,
        seq: u64::from_le_bytes(*seq),
        frames: u16::from_le_bytes(*frames),
    };
    let n = h.samples();
    if n > MAX_SAMPLES {
        return Err(FrameError::TooLarge(n));
    }
    let mut bytes = vec![0u8; 4 * n];
    r.read_exact(&mut bytes)?;
    samples.clear();
    let (chunks, _) = bytes.as_chunks::<4>();
    samples.extend(chunks.iter().map(|c| f32::from_le_bytes(*c)));
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(frames: u16, channels: u8) -> MediaHeader {
        MediaHeader {
            stream: stream::MEMBER_LISTEN,
            channels,
            seq: 0x0102_0304_0506_0708,
            frames,
        }
    }

    #[test]
    fn media_frames_round_trip() {
        let h = header(960, 2);
        let samples: Vec<f32> = (0..1920).map(|i| i as f32 / 1920.0 - 0.5).collect();
        let mut wire = Vec::new();
        write_media(&mut wire, &h, &samples).unwrap();
        assert_eq!(wire.len(), MEDIA_HEADER + 4 * 1920);
        assert_eq!(&wire[..8], b"IEMF\x01\x01\x02\x00");
        assert_eq!(&wire[8..16], &0x0102_0304_0506_0708u64.to_le_bytes());
        assert_eq!(&wire[16..20], &[0xC0, 0x03, 0, 0]);
        let mut out = vec![7.0];
        let got = read_media(&mut wire.as_slice(), &mut out).unwrap();
        assert_eq!(got, h);
        assert_eq!(out, samples);
    }

    #[test]
    fn bad_frames_fail() {
        let mut wire = Vec::new();
        write_media(&mut wire, &header(2, 1), &[0.5, -0.5]).unwrap();
        let mut out = Vec::new();
        let mut magic = wire.clone();
        magic[0] = b'X';
        assert!(
            matches!(read_media(&mut magic.as_slice(), &mut out), Err(FrameError::Io(e)) if e.kind() == io::ErrorKind::InvalidData)
        );
        let mut version = wire.clone();
        version[4] = 2;
        assert!(
            matches!(read_media(&mut version.as_slice(), &mut out), Err(FrameError::Io(e)) if e.kind() == io::ErrorKind::InvalidData)
        );
        let mut huge = wire.clone();
        huge[6] = 5;
        huge[16..18].copy_from_slice(&960u16.to_le_bytes());
        assert!(matches!(
            read_media(&mut huge.as_slice(), &mut out),
            Err(FrameError::TooLarge(4800))
        ));
        let short = &wire[..wire.len() - 1];
        assert!(
            matches!(read_media(&mut &short[..], &mut out), Err(FrameError::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof)
        );
        assert!(matches!(
            read_media(&mut &b""[..], &mut out),
            Err(FrameError::Closed)
        ));
        // The header must match the payload, and the payload must fit.
        assert!(write_media(&mut Vec::new(), &header(2, 1), &[0.5]).is_err());
        assert!(write_media(&mut Vec::new(), &header(960, 5), &vec![0.0; 4800]).is_err());
        // Exactly the maximum is fine.
        write_media(&mut Vec::new(), &header(960, 4), &vec![0.0; MAX_SAMPLES]).unwrap();
    }
}
