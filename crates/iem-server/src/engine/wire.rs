//! The engine's pipes from the server's side (program spec §2.3; S3 design
//! note §3.6): names, and async framing — control frames are a `u32` LE length
//! and that much JSON (≤ `MAX_FRAME`), media frames a 20-byte header and f32
//! LE samples (`iem_engine_proto::media`).

use std::io;

use iem_engine_proto::media::MAX_SAMPLES;
use iem_engine_proto::{MAX_FRAME, MEDIA_HEADER, MediaHeader, read_media, write_media};
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::client::{Reader, Writer};

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("pipe i/o: {0}")]
    Io(#[from] io::Error),
    #[error("frame of {0} bytes exceeds the limit")]
    TooLarge(usize),
    #[error("the peer closed the pipe")]
    Closed,
    #[error("malformed frame: {0}")]
    Malformed(String),
}

/// The media pipe beside the control pipe `pipe`.
pub fn media_pipe(pipe: &str) -> String {
    format!("{pipe}.media")
}

/// Connects to the engine's pipe `pipe`: a Unix socket path, or on Windows the
/// name of a named pipe (`\\.\pipe\<pipe>`, the engine's namespaced name).
pub async fn connect(pipe: &str) -> io::Result<(Reader, Writer)> {
    #[cfg(unix)]
    {
        let s = tokio::net::UnixStream::connect(pipe).await?;
        let (r, w) = s.into_split();
        Ok((Box::new(r), Box::new(w)))
    }
    #[cfg(windows)]
    {
        let path = format!(r"\\.\pipe\{pipe}");
        let c = tokio::net::windows::named_pipe::ClientOptions::new().open(&path)?;
        let (r, w) = tokio::io::split(c);
        Ok((Box::new(r), Box::new(w)))
    }
}

/// Fills `buf`; a clean end before the first byte is `Closed`.
async fn read_exact_or_closed<R: AsyncRead + Unpin>(
    r: &mut R,
    buf: &mut [u8],
) -> Result<(), WireError> {
    let mut got = 0;
    while got < buf.len() {
        let slot = buf.get_mut(got..).unwrap_or_default();
        match r.read(slot).await? {
            0 if got == 0 => return Err(WireError::Closed),
            0 => return Err(WireError::Io(io::ErrorKind::UnexpectedEof.into())),
            n => got += n,
        }
    }
    Ok(())
}

/// One control frame's body.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Vec<u8>, WireError> {
    let mut head = [0u8; 4];
    read_exact_or_closed(r, &mut head).await?;
    let len = u32::from_le_bytes(head) as usize;
    if len > MAX_FRAME {
        return Err(WireError::TooLarge(len));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    Ok(body)
}

/// Serialises `msg` into one control frame and writes it whole.
pub async fn write_msg<W: AsyncWrite + Unpin, T: Serialize>(
    w: &mut W,
    msg: &T,
) -> Result<(), WireError> {
    let mut out = Vec::new();
    iem_engine_proto::write_frame(&mut out, msg).map_err(|e| match e {
        iem_engine_proto::FrameError::TooLarge(n) => WireError::TooLarge(n),
        other => WireError::Malformed(other.to_string()),
    })?;
    w.write_all(&out).await?;
    w.flush().await?;
    Ok(())
}

/// One media frame (header and samples).
pub async fn read_media_frame<R: AsyncRead + Unpin>(
    r: &mut R,
) -> Result<(MediaHeader, Vec<f32>), WireError> {
    let mut frame = vec![0u8; MEDIA_HEADER];
    read_exact_or_closed(r, &mut frame).await?;
    let channels = usize::from(*frame.get(6).unwrap_or(&0));
    let frames = frame
        .get(16..18)
        .map_or(0, |b| usize::from(u16::from_le_bytes([b[0], b[1]])));
    let n = channels * frames;
    if n > MAX_SAMPLES {
        return Err(WireError::TooLarge(n));
    }
    frame.resize(MEDIA_HEADER + 4 * n, 0);
    r.read_exact(frame.get_mut(MEDIA_HEADER..).unwrap_or_default())
        .await?;
    let mut samples = Vec::with_capacity(n);
    let header = read_media(&mut frame.as_slice(), &mut samples)
        .map_err(|e| WireError::Malformed(e.to_string()))?;
    Ok((header, samples))
}

/// Writes one media frame whole.
pub async fn write_media_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    header: &MediaHeader,
    samples: &[f32],
) -> Result<(), WireError> {
    let mut out = Vec::with_capacity(MEDIA_HEADER + 4 * samples.len());
    write_media(&mut out, header, samples)?;
    w.write_all(&out).await?;
    w.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use iem_engine_proto::media::stream;
    use iem_engine_proto::{ClientMsg, Cmd};

    #[tokio::test]
    async fn control_frames_round_trip_and_close_cleanly() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        let msg = ClientMsg::Request {
            id: 7,
            origin: Some(3),
            cmd: Cmd::Ping,
        };
        write_msg(&mut a, &msg).await.unwrap();
        write_msg(&mut a, &Cmd::GetState).await.unwrap();
        drop(a);
        let body = read_frame(&mut b).await.unwrap();
        assert_eq!(serde_json::from_slice::<ClientMsg>(&body).unwrap(), msg);
        assert_eq!(read_frame(&mut b).await.unwrap(), br#"{"op":"get_state"}"#);
        assert!(matches!(read_frame(&mut b).await, Err(WireError::Closed)));
    }

    #[tokio::test]
    async fn an_oversize_frame_is_refused_before_its_body() {
        let wire = ((MAX_FRAME + 1) as u32).to_le_bytes();
        let mut r = &wire[..];
        assert!(matches!(
            read_frame(&mut r).await,
            Err(WireError::TooLarge(n)) if n == MAX_FRAME + 1
        ));
    }

    #[tokio::test]
    async fn a_control_frame_of_the_largest_size_passes() {
        let mut wire = (MAX_FRAME as u32).to_le_bytes().to_vec();
        wire.resize(4 + MAX_FRAME, b' ');
        let mut r = &wire[..];
        assert_eq!(read_frame(&mut r).await.unwrap().len(), MAX_FRAME);
    }

    #[tokio::test]
    async fn a_frame_cut_inside_is_an_unexpected_eof() {
        let mut wire = Vec::new();
        iem_engine_proto::write_frame(&mut wire, &Cmd::Ping).unwrap();
        let mut head_only = &wire[..2];
        assert!(matches!(
            read_frame(&mut head_only).await,
            Err(WireError::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof
        ));
        let mut body_cut = &wire[..wire.len() - 1];
        assert!(matches!(
            read_frame(&mut body_cut).await,
            Err(WireError::Io(_))
        ));
    }

    #[tokio::test]
    async fn media_frames_round_trip() {
        let (mut a, mut b) = tokio::io::duplex(1 << 16);
        let h = MediaHeader {
            stream: stream::TALKBACK,
            channels: 1,
            seq: 9,
            frames: 3,
        };
        write_media_frame(&mut a, &h, &[0.5, -0.5, 0.25])
            .await
            .unwrap();
        let stereo = MediaHeader {
            stream: stream::ENGINEER_LISTEN,
            channels: 2,
            seq: 1,
            frames: 960,
        };
        let samples: Vec<f32> = (0..1920).map(|i| i as f32 / 1920.0).collect();
        write_media_frame(&mut a, &stereo, &samples).await.unwrap();
        drop(a);
        assert_eq!(
            read_media_frame(&mut b).await.unwrap(),
            (h, vec![0.5, -0.5, 0.25])
        );
        assert_eq!(read_media_frame(&mut b).await.unwrap(), (stereo, samples));
        assert!(matches!(
            read_media_frame(&mut b).await,
            Err(WireError::Closed)
        ));
        // A header announcing more samples than a frame may hold is refused.
        let mut huge = Vec::new();
        write_media(&mut huge, &h, &[0.0, 0.0, 0.0]).unwrap();
        huge[6] = 5;
        huge[16..18].copy_from_slice(&960u16.to_le_bytes());
        assert!(matches!(
            read_media_frame(&mut &huge[..]).await,
            Err(WireError::TooLarge(4800))
        ));
        // A wrong magic is malformed.
        let mut bad = Vec::new();
        write_media(&mut bad, &h, &[0.0, 0.0, 0.0]).unwrap();
        bad[0] = b'X';
        assert!(matches!(
            read_media_frame(&mut &bad[..]).await,
            Err(WireError::Malformed(_))
        ));
        // A mismatched header and payload cannot be written.
        assert!(
            write_media_frame(&mut Vec::new(), &h, &[0.0])
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_media_frame_of_the_largest_size_passes() {
        let (mut a, mut b) = tokio::io::duplex(1 << 20);
        let h = MediaHeader {
            stream: stream::ENGINEER_LISTEN,
            channels: 4,
            seq: 2,
            frames: 960,
        };
        let samples: Vec<f32> = (0..MAX_SAMPLES).map(|i| i as f32).collect();
        assert_eq!(samples.len(), 4 * 960);
        write_media_frame(&mut a, &h, &samples).await.unwrap();
        assert_eq!(read_media_frame(&mut b).await.unwrap(), (h, samples));
    }

    #[test]
    fn the_media_pipe_is_beside_the_control_pipe() {
        assert_eq!(media_pipe("/run/iem.sock"), "/run/iem.sock.media");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn connecting_to_a_missing_pipe_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("none.sock");
        assert!(connect(&path.to_string_lossy()).await.is_err());
    }
}
