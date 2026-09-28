//! The Windows side of the guard's pipe writes: a writer that gives the
//! peer a bounded time to take a message and leaves the stream to be closed
//! at once when it is dropped. The guard pipe's replies and updates
//! (`pipe::bounded`, [`SEND_TIMEOUT`](super::SEND_TIMEOUT)) and the
//! supervisor connection's sends to the engine (`win::engine`) go through
//! it. Not compiled on Linux, so it stays outside mutation testing
//! (`.cargo/mutants.toml`); the `windows` job runs the guard's tests over
//! it (`pipe::tests` `a_client_that_does_not_read_holds_up_no_close` and
//! `a_subscriber_that_does_not_read_is_dropped_within_a_bound`,
//! `win::engine::tests`
//! `a_send_to_an_engine_that_does_not_read_fails_within_a_bound`).

use std::io::{self, Write};
use std::os::windows::io::AsHandle;
use std::time::Duration;

use interprocess::local_socket::Stream;

/// A pipe stream whose writes the peer must take within `limit`
/// (`iem_win::pipe::write_within`, the engine's writer too): a write still
/// pending then is cancelled and fails with `TimedOut`.
///
/// Its writes leave the stream clean, unlike interprocess's own, which mark
/// it for the flush on drop (limbo): a dirty stream is dropped onto the
/// process's one linger thread, whose `FlushFileBuffers` waits until the
/// peer has read everything, so one peer that stops reading would keep
/// that end open, and every stream the guard dropped after it would wait
/// behind it (the engine's pipes met it in Windows CI run 36373563262). A
/// clean stream's handle is closed as it is dropped; what was written stays
/// in the pipe, and the peer reads it before the end of the stream.
pub(crate) struct Bounded<'a> {
    stream: &'a Stream,
    limit: Duration,
}

impl<'a> Bounded<'a> {
    /// Writes to `stream` that the peer must take within `limit`.
    pub(crate) fn new(stream: &'a Stream, limit: Duration) -> Self {
        Self { stream, limit }
    }
}

impl Write for Bounded<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let Stream::NamedPipe(pipe) = self.stream;
        iem_win::pipe::write_within(pipe.inner().as_handle(), buf, self.limit)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
