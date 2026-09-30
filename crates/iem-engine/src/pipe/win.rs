//! The Windows side of the pipes (S6 design note §4): the listener's
//! security descriptor, a reader that peeks before it reads and a writer
//! that gives the peer [`SEND_TIMEOUT`] to take a message and leaves the
//! stream to be closed at once when it is dropped. Not compiled on
//! Linux, so it stays outside mutation testing (`.cargo/mutants.toml`); its
//! decisions are the parent module's portable `sddl_for`, `name_taken` and
//! `polled_read`, and `tests/pipes.rs` runs on it in the `windows` job.

use std::io::{self, Read, Write};
use std::os::windows::io::AsHandle;

use interprocess::local_socket::{Listener, ListenerOptions, Stream};
use interprocess::os::windows::local_socket::ListenerOptionsExt;
use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use widestring::U16CString;

use super::{SEND_TIMEOUT, name_taken, polled_read, sddl_for};

/// Creates the pipe's first instance (interprocess sets
/// `FILE_FLAG_FIRST_PIPE_INSTANCE` on it and `PIPE_REJECT_REMOTE_CLIENTS` on
/// every instance) with a DACL for the logged-on user and SYSTEM.
pub(super) fn listen(options: ListenerOptions<'_>) -> io::Result<Listener> {
    let user = iem_win::token::current_user_sid()?;
    let sddl = U16CString::from_str(sddl_for(&user))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    let descriptor = SecurityDescriptor::deserialize(&sddl)?;
    options
        .security_descriptor(descriptor)
        .create_sync()
        .map_err(name_taken)
}

/// A pipe stream read only after a peek shows waiting bytes, so a read
/// never waits.
pub(super) struct Polled<'a>(pub(super) &'a Stream);

impl Read for Polled<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let stream = self.0;
        let Stream::NamedPipe(pipe) = stream;
        let waiting = iem_win::pipe::available(pipe.inner().as_handle());
        polled_read(waiting, || {
            let mut reader = stream;
            reader.read(buf)
        })
    }
}

/// A pipe stream whose writes the peer must take within [`SEND_TIMEOUT`]
/// (`iem_win::pipe::write_within`): a write still pending then is
/// cancelled and fails with `TimedOut`.
///
/// Its writes leave the stream clean, unlike interprocess's own, which mark
/// it for the flush on drop (limbo): a dirty stream is dropped onto the
/// process's one linger thread, whose `FlushFileBuffers` waits until the
/// peer has read everything, so one peer that stops reading would keep that
/// end open, and every written stream dropped after it in the process would
/// wait behind it (Windows CI run 36373563262). A clean stream's handle is
/// closed as it is dropped; what was written stays in the pipe, and the
/// peer reads it before the end of the stream (`tests/pipes.rs`
/// `a_peer_that_does_not_read_holds_up_no_close`).
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
