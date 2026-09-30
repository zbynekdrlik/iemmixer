//! Local-socket plumbing (program spec §2.3, I1; design note §3.6): names,
//! listeners, and connections shared between one reader thread and the
//! writing control or media thread. Linux (CI) uses Unix socket files;
//! Windows uses named pipes (S6 design note §4): a protected DACL for the
//! logged-on user and SYSTEM ([`sddl_for`]), remote clients refused, and the
//! first-instance flag, so a second listener on a held name fails ("pipe name
//! taken").
//!
//! A connection is an `Arc<Stream>` (`&Stream` reads and writes); its reader
//! never waits long in a read ([`polled`]), so that dropping the connection
//! (the `closed` flag) ends the reader and closes the socket for the peer,
//! and its writers never wait long for the peer ([`Conn::writer`]): a peer
//! that takes nothing for [`SEND_TIMEOUT`] fails the write and is dropped.
//! The close never waits for the peer either: the peer reads what was
//! written before it, then the end of the stream, and one peer that does
//! not read holds up no other close ([`bounded`]).

use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use iem_engine_proto::{FrameError, MediaHeader, read_frame, read_media};
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{
    Listener, ListenerNonblockingMode, ListenerOptions, Name, Stream,
};

#[cfg(windows)]
mod win;

/// The receive timeout of a Unix stream: how long a read waits for data.
pub const POLL: Duration = Duration::from_millis(50);
/// How long a reader rests after a read found nothing, before it looks at
/// its `closed` flag and reads again (Windows pipes have no receive timeout:
/// their reads return at once, see [`polled`]).
pub const IDLE: Duration = Duration::from_millis(10);
/// A peer that does not take a message within this long is dropped: the
/// Unix send timeout, and the bound of a Windows pipe write ([`bounded`]).
pub const SEND_TIMEOUT: Duration = Duration::from_secs(1);
// The DACL read back is judged by iem-win, which the guard shares (its
// `Reply.engine.pipe_private` for HIL).
pub use iem_win::token::{SYSTEM, dacl_sids, sddl_is_private};

/// A socket path on Unix, a pipe name elsewhere.
fn name(s: String) -> io::Result<Name<'static>> {
    #[cfg(unix)]
    {
        use interprocess::local_socket::GenericFilePath;
        s.to_fs_name::<GenericFilePath>()
    }
    #[cfg(not(unix))]
    {
        use interprocess::local_socket::GenericNamespaced;
        s.to_ns_name::<GenericNamespaced>()
    }
}

/// The control pipe: a socket path on Unix, a pipe name on Windows.
pub fn control_name(pipe: &str) -> io::Result<Name<'static>> {
    name(pipe.to_owned())
}

/// The media pipe beside it.
pub fn media_name(pipe: &str) -> io::Result<Name<'static>> {
    name(format!("{pipe}.media"))
}

/// A listener whose `accept` does not block (the acceptor polls a stop flag).
/// Unix: a leftover socket file of a previous run is replaced. Windows: the
/// pipe admits only the logged-on user and SYSTEM ([`sddl_for`]) and no
/// remote client, and a name another listener holds fails at once with
/// `AddrInUse`, "pipe name taken" (the engine exits 1): waiting would not
/// free it. A gone listener's name is free as soon as its process has
/// ended, even while a client still holds its stream (`tests/pipes.rs`).
pub fn listen(name: Name<'_>) -> io::Result<Listener> {
    #[cfg(windows)]
    {
        win::listen(listener_options(name))
    }
    #[cfg(not(windows))]
    {
        listener_options(name).create_sync()
    }
}

fn listener_options(name: Name<'_>) -> ListenerOptions<'_> {
    ListenerOptions::new()
        .name(name)
        .nonblocking(ListenerNonblockingMode::Accept)
        .try_overwrite(true)
}

/// The pipes' security descriptor (S3 hand-off, program spec §2.3): a
/// protected DACL that allows the user `user_sid` and SYSTEM, nobody else.
pub fn sddl_for(user_sid: &str) -> String {
    format!("D:P(A;;GA;;;{user_sid})(A;;GA;;;{SYSTEM})")
}

/// Windows' answer to a listener whose name another listener holds: the
/// first instance exists (`ERROR_ACCESS_DENIED` under
/// `FILE_FLAG_FIRST_PIPE_INSTANCE`) or every instance is busy
/// (`ERROR_PIPE_BUSY`). Other errors pass unchanged.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn name_taken(e: io::Error) -> io::Error {
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_PIPE_BUSY: i32 = 231;
    match e.raw_os_error() {
        Some(ERROR_ACCESS_DENIED | ERROR_PIPE_BUSY) => {
            io::Error::new(io::ErrorKind::AddrInUse, format!("pipe name taken ({e})"))
        }
        _ => e,
    }
}

/// Whether a pipe error means that the other end has gone.
#[cfg_attr(not(windows), allow(dead_code))]
fn gone(e: &io::Error) -> bool {
    const ERROR_BROKEN_PIPE: i32 = 109;
    const ERROR_NO_DATA: i32 = 232;
    const ERROR_PIPE_NOT_CONNECTED: i32 = 233;
    matches!(
        e.raw_os_error(),
        Some(ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED)
    )
}

/// One read of a Windows pipe that peeks first (`waiting` = the bytes the
/// pipe holds): nothing waiting is `WouldBlock`, as a Unix receive timeout
/// reports it; a peer that has gone is the end of the stream (`Ok(0)`);
/// only when bytes wait does `read` run, so it returns at once.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn polled_read(
    waiting: io::Result<u32>,
    read: impl FnOnce() -> io::Result<usize>,
) -> io::Result<usize> {
    match waiting {
        Ok(0) => Err(io::ErrorKind::WouldBlock.into()),
        Ok(_) => read(),
        Err(e) if gone(&e) => Ok(0),
        Err(e) => Err(e),
    }
}

/// A stream's reading side that never waits long: a Unix stream waits at
/// most its receive timeout ([`POLL`], set by [`Conn::new`]) and then
/// reports `WouldBlock`; a Windows pipe has no timeouts, so it is read only
/// after a peek shows waiting bytes, else `WouldBlock` at once
/// (`polled_read`). [`Framer::fill`] turns `WouldBlock` into "nothing yet".
pub fn polled(stream: &Stream) -> impl Read + '_ {
    #[cfg(windows)]
    {
        win::Polled(stream)
    }
    #[cfg(not(windows))]
    {
        stream
    }
}

/// A stream's writing side that never waits long for the peer: a Unix
/// stream waits at most its send timeout ([`SEND_TIMEOUT`], set by
/// [`Conn::new`]); a Windows pipe has no timeouts, so each write is issued
/// overlapped and cancelled when the peer has not taken it within
/// [`SEND_TIMEOUT`] (`iem_win::pipe::write_within`). Either way the write
/// then fails, and the caller drops the peer. A Windows write through it
/// also leaves the stream out of interprocess's flush on drop, whose one
/// thread per process waits for each peer to read everything before it
/// closes the next stream: the engine's end closes as it is dropped.
pub fn bounded(stream: &Stream) -> impl Write + '_ {
    #[cfg(windows)]
    {
        win::Bounded(stream)
    }
    #[cfg(not(windows))]
    {
        stream
    }
}

/// One accepted connection.
#[derive(Debug, Clone)]
pub struct Conn {
    pub stream: Arc<Stream>,
    pub closed: Arc<AtomicBool>,
}

impl Conn {
    /// Prepares a stream: blocking reads with a poll timeout, bounded writes
    /// (Unix). Windows pipes refuse both timeouts, which stay at their
    /// default: their reads go through [`polled`], their writes through
    /// [`bounded`].
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

    /// The connection's writing side ([`bounded`]): always write through it,
    /// never through `&*stream`, whose Windows writes wait for the peer
    /// without a bound and leave the stream to interprocess's flush on drop.
    pub fn writer(&self) -> impl Write + '_ {
        bounded(&self.stream)
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

    /// Reads what is there: `Ok(false)` when nothing came (the read timed
    /// out, or a peek found nothing waiting: [`polled`]), `Closed` when the
    /// peer is gone.
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
        match framer.fill(polled(&conn.stream)) {
            Ok(true) => {}
            Ok(false) => std::thread::sleep(IDLE),
            Err(e) => return e,
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

    #[test]
    fn one_fill_takes_up_to_16_kib() {
        let big = vec![7u8; 20_000];
        let mut f = Framer::new();
        let mut r = big.as_slice();
        assert!(f.fill(&mut r).unwrap());
        assert_eq!(f.buf.len(), 16 * 1024);
        assert!(f.fill(&mut r).unwrap());
        assert_eq!(f.buf.len(), 20_000);
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

    const USER: &str = "S-1-5-21-1-2-3-1001";

    #[test]
    fn sddl_for_allows_the_user_and_system_in_a_protected_dacl() {
        assert_eq!(
            sddl_for(USER),
            "D:P(A;;GA;;;S-1-5-21-1-2-3-1001)(A;;GA;;;SY)"
        );
    }

    /// iem-win decides whether a DACL read back is private
    /// (`iem_win::token::sddl_is_private`, tested there); this pipe's own
    /// DACL passes it.
    #[test]
    fn sddl_the_engines_own_dacl_is_private() {
        assert_eq!(dacl_sids(&sddl_for(USER)), Some(vec![USER, SYSTEM]));
        assert!(sddl_is_private(&sddl_for(USER), USER));
        assert!(!sddl_is_private(&sddl_for(USER), "S-1-5-21-1-2-3-1002"));
    }

    #[test]
    fn a_held_pipe_name_is_reported_as_taken() {
        for code in [5, 231] {
            let e = name_taken(io::Error::from_raw_os_error(code));
            assert_eq!(e.kind(), io::ErrorKind::AddrInUse, "{code}");
            assert_eq!(e.raw_os_error(), None, "{code}");
            assert!(e.to_string().starts_with("pipe name taken ("), "{e}");
        }
        for code in [2, 4, 6, 230, 232] {
            let e = name_taken(io::Error::from_raw_os_error(code));
            assert_eq!(e.raw_os_error(), Some(code));
        }
        let other = name_taken(io::Error::other("no code"));
        assert_eq!(
            (other.kind(), other.raw_os_error()),
            (io::ErrorKind::Other, None)
        );
    }

    #[test]
    fn a_polled_read_reads_only_what_waits() {
        let never = || -> io::Result<usize> { panic!("nothing waits: no read") };
        assert_eq!(
            polled_read(Ok(0), never).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(polled_read(Ok(1), || Ok(7)).unwrap(), 7);
        assert_eq!(polled_read(Ok(u32::MAX), || Ok(3)).unwrap(), 3);
        let failed = polled_read(Ok(4), || Err(io::Error::from_raw_os_error(6)));
        assert_eq!(failed.unwrap_err().raw_os_error(), Some(6));
        for code in [109, 232, 233] {
            let r = polled_read(Err(io::Error::from_raw_os_error(code)), never);
            assert_eq!(r.unwrap(), 0, "the peer has gone: {code}");
        }
        for code in [5, 108, 110, 231, 234] {
            let r = polled_read(Err(io::Error::from_raw_os_error(code)), never);
            assert_eq!(r.unwrap_err().raw_os_error(), Some(code));
        }
        let r = polled_read(Err(io::Error::other("no code")), never);
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::Other);
    }

    #[test]
    fn nothing_waiting_is_not_an_error_for_the_framer() {
        // A peek that finds nothing reads as a timeout; a gone peer as the
        // end of the stream.
        let mut f = Framer::new();
        let empty = Probe(Some(Ok(0)));
        assert!(!f.fill(empty).unwrap());
        let peer_gone = Probe(Some(Err(io::Error::from_raw_os_error(109))));
        assert!(matches!(f.fill(peer_gone), Err(FrameError::Closed)));
    }

    /// A reader that answers one read like a polled Windows pipe.
    struct Probe(Option<io::Result<u32>>);

    impl Read for Probe {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let waiting = self.0.take().expect("one read");
            polled_read(waiting, || Ok(buf.len()))
        }
    }
}
