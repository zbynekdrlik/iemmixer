//! The Windows side of the guard pipe's writes: a writer that gives the
//! client [`SEND_TIMEOUT`] to take a message and leaves the stream to be
//! closed at once when it is dropped. Not compiled on Linux, so it stays
//! outside mutation testing (`.cargo/mutants.toml`); the `windows` job runs
//! the guard's pipe tests over it (`pipe::tests`
//! `a_client_that_does_not_read_holds_up_no_close` and
//! `a_subscriber_that_does_not_read_is_dropped_within_a_bound`).

use std::io::{self, Write};
use std::os::windows::io::AsHandle;

use interprocess::local_socket::Stream;

use super::SEND_TIMEOUT;

/// A pipe stream whose writes the client must take within [`SEND_TIMEOUT`]
/// (`iem_win::pipe::write_within`, the engine's writer too): a write still
/// pending then is cancelled and fails with `TimedOut`.
///
/// Its writes leave the stream clean, unlike interprocess's own, which mark
/// it for the flush on drop (limbo): a dirty stream is dropped onto the
/// process's one linger thread, whose `FlushFileBuffers` waits until the
/// client has read everything, so one client that stops reading would keep
/// that end open, and every connection the guard dropped after it would
/// wait behind it (the engine's pipes met it in Windows CI run
/// 36373563262). A clean stream's handle is closed as it is dropped; what
/// was written stays in the pipe, and the client reads it before the end of
/// the stream.
pub(super) struct Bounded<'a>(pub(super) &'a Stream);

impl Write for Bounded<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let Stream::NamedPipe(pipe) = self.0;
        iem_win::pipe::write_within(pipe.inner().as_handle(), buf, SEND_TIMEOUT)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
