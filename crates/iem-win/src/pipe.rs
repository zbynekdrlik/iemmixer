//! Named-pipe reads that never wait (S6 design note §4): interprocess has no
//! timeouts on Windows pipes, so the engine's reader asks how many bytes
//! wait before it reads, and looks at its connection's `closed` flag in
//! between instead of sitting in a read that nothing ends. Windows only:
//! the argument is a Windows handle.

use std::io;
use std::os::windows::io::{AsRawHandle, BorrowedHandle};
use std::ptr;

use windows_sys::Win32::System::Pipes::PeekNamedPipe;

use crate::win::check;

/// The bytes waiting to be read from a pipe (`PeekNamedPipe`, nothing is
/// taken). Once the other end is gone it fails (`ERROR_BROKEN_PIPE` and
/// kin), after the last waiting byte was read.
pub fn available(pipe: BorrowedHandle<'_>) -> io::Result<u32> {
    let mut waiting = 0u32;
    // SAFETY: a pipe handle valid for the call; no buffer (size 0) and only
    // the total count is written.
    check(unsafe {
        PeekNamedPipe(
            pipe.as_raw_handle(),
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            &mut waiting,
            ptr::null_mut(),
        )
    })?;
    Ok(waiting)
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::windows::io::AsHandle;

    use super::*;

    /// `ERROR_BROKEN_PIPE`.
    const BROKEN: i32 = 109;

    #[test]
    fn available_counts_the_waiting_bytes_until_the_writer_is_gone() {
        let (mut r, mut w) = std::io::pipe().unwrap();
        assert_eq!(available(r.as_handle()).unwrap(), 0);
        w.write_all(b"abc").unwrap();
        assert_eq!(available(r.as_handle()).unwrap(), 3);
        assert_eq!(available(r.as_handle()).unwrap(), 3, "a peek takes nothing");
        let mut buf = [0u8; 8];
        assert_eq!(r.read(&mut buf).unwrap(), 3);
        assert_eq!(&buf[..3], b"abc");
        assert_eq!(available(r.as_handle()).unwrap(), 0);
        drop(w);
        let gone = available(r.as_handle()).unwrap_err();
        assert_eq!(gone.raw_os_error(), Some(BROKEN), "{gone}");
    }
}
