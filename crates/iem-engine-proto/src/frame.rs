//! Control-pipe frames (§2.3, design note §3.6): a little-endian `u32` length
//! and that many bytes of JSON, at most [`MAX_FRAME`]. A peer that announces a
//! larger frame is refused before its body is read.

use core::fmt;
use std::io::{self, Read, Write};

use serde::Serialize;

/// Largest frame body in bytes.
pub const MAX_FRAME: usize = 1 << 20;

#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    /// The announced or produced body exceeds [`MAX_FRAME`].
    TooLarge(usize),
    /// The peer closed the stream between frames.
    Closed,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "pipe i/o: {e}"),
            Self::TooLarge(n) => write!(f, "frame of {n} bytes exceeds {MAX_FRAME}"),
            Self::Closed => f.write_str("pipe closed"),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<io::Error> for FrameError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Fills `head` completely; a clean end of stream before the first byte is
/// [`FrameError::Closed`], one inside the header is `UnexpectedEof`.
pub(crate) fn read_head<R: Read>(r: &mut R, head: &mut [u8]) -> Result<(), FrameError> {
    let mut got = 0;
    while let Some(rest) = head.get_mut(got..) {
        if rest.is_empty() {
            break;
        }
        match r.read(rest) {
            Ok(0) if got == 0 => return Err(FrameError::Closed),
            Ok(0) => return Err(FrameError::Io(io::ErrorKind::UnexpectedEof.into())),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(FrameError::Io(e)),
        }
    }
    Ok(())
}

/// Writes one frame (length and body in one write).
pub fn write_frame<W: Write, T: Serialize>(w: &mut W, msg: &T) -> Result<(), FrameError> {
    let body = serde_json::to_vec(msg).map_err(|e| FrameError::Io(io::Error::other(e)))?;
    let len = u32::try_from(body.len())
        .ok()
        .filter(|_| body.len() <= MAX_FRAME)
        .ok_or(FrameError::TooLarge(body.len()))?;
    let mut out = Vec::with_capacity(body.len() + 4);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&body);
    w.write_all(&out)?;
    w.flush()?;
    Ok(())
}

/// Reads one frame body into `buf` (replacing its contents).
pub fn read_frame<R: Read>(r: &mut R, buf: &mut Vec<u8>) -> Result<(), FrameError> {
    let mut head = [0u8; 4];
    read_head(r, &mut head)?;
    let len = u32::from_le_bytes(head) as usize;
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    buf.clear();
    buf.resize(len, 0);
    r.read_exact(buf)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::{ClientMsg, Cmd};

    #[test]
    fn frames_round_trip() {
        let msg = ClientMsg::Request {
            id: 3,
            origin: None,
            cmd: Cmd::Ping,
        };
        let mut wire = Vec::new();
        write_frame(&mut wire, &msg).unwrap();
        write_frame(&mut wire, &msg).unwrap();
        let body = serde_json::to_vec(&msg).unwrap();
        assert_eq!(&wire[..4], &(body.len() as u32).to_le_bytes());
        let mut r = wire.as_slice();
        let mut buf = vec![9; 3];
        read_frame(&mut r, &mut buf).unwrap();
        assert_eq!(serde_json::from_slice::<ClientMsg>(&buf).unwrap(), msg);
        read_frame(&mut r, &mut buf).unwrap();
        assert_eq!(buf, body);
        assert!(matches!(
            read_frame(&mut r, &mut buf),
            Err(FrameError::Closed)
        ));
    }

    #[test]
    fn oversize_is_refused_before_the_body() {
        let mut wire = ((MAX_FRAME + 1) as u32).to_le_bytes().to_vec();
        wire.extend_from_slice(b"{}");
        let mut buf = Vec::new();
        match read_frame(&mut wire.as_slice(), &mut buf) {
            Err(FrameError::TooLarge(n)) => assert_eq!(n, MAX_FRAME + 1),
            other => panic!("{other:?}"),
        }
        assert!(buf.is_empty());
        // A body of exactly MAX_FRAME bytes is allowed.
        let mut exact = (MAX_FRAME as u32).to_le_bytes().to_vec();
        exact.resize(4 + MAX_FRAME, b' ');
        read_frame(&mut exact.as_slice(), &mut buf).unwrap();
        assert_eq!(buf.len(), MAX_FRAME);
        let big = "x".repeat(MAX_FRAME);
        assert!(matches!(
            write_frame(&mut Vec::new(), &big),
            Err(FrameError::TooLarge(n)) if n == MAX_FRAME + 2
        ));
    }

    #[test]
    fn truncation_is_an_unexpected_eof() {
        let mut buf = Vec::new();
        for wire in [&[5u8, 0][..], &[5, 0, 0, 0, b'{'][..]] {
            match read_frame(&mut &wire[..], &mut buf) {
                Err(FrameError::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof),
                other => panic!("{other:?}"),
            }
        }
    }

    /// A reader that returns one byte per call and an interruption in between.
    struct Trickle<'a> {
        data: &'a [u8],
        interrupt: bool,
    }

    impl Read for Trickle<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.interrupt = !self.interrupt;
            if self.interrupt {
                return Err(io::ErrorKind::Interrupted.into());
            }
            match (self.data.split_first(), out.first_mut()) {
                (Some((b, rest)), Some(o)) => {
                    *o = *b;
                    self.data = rest;
                    Ok(1)
                }
                _ => Ok(0),
            }
        }
    }

    #[test]
    fn short_reads_and_interruptions_are_retried() {
        let mut wire = Vec::new();
        write_frame(&mut wire, &Cmd::Ping).unwrap();
        let mut r = Trickle {
            data: &wire,
            interrupt: false,
        };
        let mut buf = Vec::new();
        read_frame(&mut r, &mut buf).unwrap();
        assert_eq!(buf, br#"{"op":"ping"}"#);
        assert!(matches!(
            read_frame(&mut r, &mut buf),
            Err(FrameError::Closed)
        ));
    }

    #[test]
    fn errors_display() {
        assert_eq!(FrameError::Closed.to_string(), "pipe closed");
        assert_eq!(
            FrameError::TooLarge(7).to_string(),
            format!("frame of 7 bytes exceeds {MAX_FRAME}")
        );
        let io: FrameError = io::Error::other("boom").into();
        assert_eq!(io.to_string(), "pipe i/o: boom");
    }
}
