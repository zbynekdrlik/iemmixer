//! Local-socket plumbing (program spec §2.3, I1; design note §3.6): names,
//! listeners, and connections shared between one reader thread and the
//! writing control or media thread. Linux (CI) uses Unix socket files; Windows
//! uses named pipes (S6 adds reject-remote and the current-user DACL).
//!
//! A connection is an `Arc<Stream>` (`&Stream` reads and writes); its reader
//! polls with a short receive timeout so that dropping the connection (the
//! `closed` flag) ends the reader and closes the socket for the peer.

use std::io::{self, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use iem_engine_proto::{FrameError, MediaHeader, read_frame, read_media};
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{
    Listener, ListenerNonblockingMode, ListenerOptions, Name, Stream,
};

/// How often a blocked reader looks at its `closed` flag.
pub const POLL: Duration = Duration::from_millis(50);
/// A peer that does not take a message within this long is dropped.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(1);

#[cfg(unix)]
fn name(s: String) -> io::Result<Name<'static>> {
    use interprocess::local_socket::GenericFilePath;
    s.to_fs_name::<GenericFilePath>()
}

#[cfg(not(unix))]
fn name(s: String) -> io::Result<Name<'static>> {
    use interprocess::local_socket::GenericNamespaced;
    s.to_ns_name::<GenericNamespaced>()
}

/// The control pipe: a socket path on Unix, a pipe name on Windows.
pub fn control_name(pipe: &str) -> io::Result<Name<'static>> {
    name(pipe.to_owned())
}

/// The media pipe beside it.
pub fn media_name(pipe: &str) -> io::Result<Name<'static>> {
    name(format!("{pipe}.media"))
}

/// A listener whose `accept` does not block (the acceptor polls a stop flag);
/// a leftover socket file of a previous run is replaced.
pub fn listen(name: Name<'_>) -> io::Result<Listener> {
    ListenerOptions::new()
        .name(name)
        .nonblocking(ListenerNonblockingMode::Accept)
        .try_overwrite(true)
        .create_sync()
}

/// One accepted connection.
#[derive(Debug, Clone)]
pub struct Conn {
    pub stream: Arc<Stream>,
    pub closed: Arc<AtomicBool>,
}

impl Conn {
    /// Prepares an accepted stream: blocking reads with a poll timeout,
    /// bounded writes. Timeouts a platform refuses are left at their default.
    pub fn new(stream: Stream) -> Self {
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_recv_timeout(Some(POLL));
        let _ = stream.set_send_timeout(Some(SEND_TIMEOUT));
        Self {
            stream: Arc::new(stream),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

fn timed_out(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

/// Accumulates bytes from a reader that times out, and cuts them into
/// frames with a parser that fails with `UnexpectedEof`/`Closed` while a frame
/// is incomplete.
#[derive(Debug, Default)]
pub struct Framer {
    buf: Vec<u8>,
}

impl Framer {
    pub fn new() -> Self {
        Self::default()
    }

    fn next<T>(
        &mut self,
        parse: impl FnOnce(&mut &[u8]) -> Result<T, FrameError>,
    ) -> Result<Option<T>, FrameError> {
        let mut rest: &[u8] = &self.buf;
        match parse(&mut rest) {
            Ok(v) => {
                let used = self.buf.len() - rest.len();
                self.buf.drain(..used);
                Ok(Some(v))
            }
            Err(FrameError::Closed) => Ok(None),
            Err(FrameError::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The next complete control frame, if the buffer holds one.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        self.next(|r| {
            let mut out = Vec::new();
            read_frame(r, &mut out).map(|()| out)
        })
    }

    /// The next complete media frame, if the buffer holds one.
    pub fn next_media(&mut self) -> Result<Option<(MediaHeader, Vec<f32>)>, FrameError> {
        self.next(|r| {
            let mut out = Vec::new();
            read_media(r, &mut out).map(|h| (h, out))
        })
    }

    /// Reads what is there: `Ok(false)` when the read timed out, `Closed`
    /// when the peer is gone.
    pub fn fill(&mut self, mut r: impl Read) -> Result<bool, FrameError> {
        let mut chunk = [0u8; 16 * 1024];
        match r.read(&mut chunk) {
            Ok(0) => Err(FrameError::Closed),
            Ok(n) => {
                self.buf
                    .extend_from_slice(chunk.get(..n).unwrap_or_default());
                Ok(true)
            }
            Err(e) if timed_out(&e) => Ok(false),
            Err(e) => Err(FrameError::Io(e)),
        }
    }
}

/// Reads `conn` until it closes or its flag is set, handing each item that
/// `next` cuts to `deliver`; returns why it stopped.
pub fn read_loop<T>(
    conn: &Conn,
    mut next: impl FnMut(&mut Framer) -> Result<Option<T>, FrameError>,
    mut deliver: impl FnMut(T) -> bool,
) -> FrameError {
    let mut framer = Framer::new();
    loop {
        loop {
            match next(&mut framer) {
                Ok(Some(item)) => {
                    if !deliver(item) {
                        return FrameError::Closed;
                    }
                }
                Ok(None) => break,
                Err(e) => return e,
            }
        }
        if conn.is_closed() {
            return FrameError::Closed;
        }
        if let Err(e) = framer.fill(&*conn.stream) {
            return e;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iem_engine_proto::{Cmd, MAX_FRAME, media::stream, write_frame, write_media};

    #[test]
    fn the_framer_cuts_frames_across_reads() {
        let mut wire = Vec::new();
        write_frame(&mut wire, &Cmd::Ping).unwrap();
        write_frame(&mut wire, &Cmd::GetState).unwrap();
        let mut f = Framer::new();
        for chunk in wire.chunks(3) {
            assert!(f.fill(chunk).unwrap());
        }
        assert_eq!(f.next_frame().unwrap().unwrap(), br#"{"op":"ping"}"#);
        assert_eq!(f.next_frame().unwrap().unwrap(), br#"{"op":"get_state"}"#);
        assert!(f.next_frame().unwrap().is_none());
        assert!(f.buf.is_empty());
        // A partial header or body waits for more.
        f.fill(&wire[..2]).unwrap();
        assert!(f.next_frame().unwrap().is_none());
        f.fill(&wire[2..7]).unwrap();
        assert!(f.next_frame().unwrap().is_none());
        // An announced oversize frame is an error at once.
        let mut big = Framer::new();
        big.fill(&((MAX_FRAME + 1) as u32).to_le_bytes()[..])
            .unwrap();
        assert!(matches!(big.next_frame(), Err(FrameError::TooLarge(_))));
        assert!(matches!(
            Framer::new().fill(&b""[..]),
            Err(FrameError::Closed)
        ));
    }

    #[test]
    fn the_framer_cuts_media_frames() {
        let mut wire = Vec::new();
        let h = MediaHeader {
            stream: stream::TALKBACK,
            channels: 1,
            seq: 4,
            frames: 3,
        };
        write_media(&mut wire, &h, &[0.5, -0.5, 0.25]).unwrap();
        let mut f = Framer::new();
        f.fill(&wire[..10]).unwrap();
        assert!(f.next_media().unwrap().is_none());
        f.fill(&wire[10..]).unwrap();
        assert_eq!(f.next_media().unwrap().unwrap(), (h, vec![0.5, -0.5, 0.25]));
        f.fill(&b"XXXXXXXXXXXXXXXXXXXXXXXX"[..]).unwrap();
        assert!(f.next_media().is_err());
    }

    struct Timeouts(u8);

    impl Read for Timeouts {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            self.0 += 1;
            Err(match self.0 {
                1 => io::ErrorKind::WouldBlock.into(),
                2 => io::ErrorKind::TimedOut.into(),
                _ => io::Error::other("broken"),
            })
        }
    }

    #[test]
    fn timeouts_are_not_errors() {
        let mut f = Framer::new();
        let mut r = Timeouts(0);
        assert!(!f.fill(&mut r).unwrap());
        assert!(!f.fill(&mut r).unwrap());
        assert!(matches!(f.fill(&mut r), Err(FrameError::Io(_))));
    }

    #[test]
    fn names_follow_the_platform() {
        let c = control_name("/tmp/iem-test.sock").unwrap();
        let m = media_name("/tmp/iem-test.sock").unwrap();
        assert_ne!(c, m);
        assert_eq!(m, media_name("/tmp/iem-test.sock").unwrap());
    }
}
