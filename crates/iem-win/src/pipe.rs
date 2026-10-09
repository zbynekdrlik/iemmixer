//! Named-pipe reads and writes that never wait long (S6 design note §4):
//! interprocess has no timeouts on Windows pipes, so the engine's reader
//! asks how many bytes wait before it reads ([`available`]) and looks at its
//! connection's `closed` flag in between instead of sitting in a read that
//! nothing ends, and the engine's and the guard's writers (the guard pipe's
//! replies, the supervisor connection's sends to the engine) give a peer a
//! bounded time to take a message ([`write_within`]) instead of waiting for
//! it forever. The guard also reads which process serves the engine's pipe
//! ([`server_pid`], S7 HIL v2). Windows only: the argument is a Windows
//! handle.

use std::io;
use std::os::windows::io::{AsRawHandle, BorrowedHandle};
use std::ptr;
use std::time::Duration;

use windows_sys::Win32::Foundation::{ERROR_IO_PENDING, ERROR_OPERATION_ABORTED};
use windows_sys::Win32::Storage::FileSystem::WriteFile;
use windows_sys::Win32::System::IO::{
    CancelIoEx, GetOverlappedResult, GetOverlappedResultEx, OVERLAPPED,
};
use windows_sys::Win32::System::Pipes::{GetNamedPipeServerProcessId, PeekNamedPipe};
use windows_sys::Win32::System::Threading::CreateEventW;

use crate::decide::wait_ms;
use crate::win::{check, owned};

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

/// The process id of the pipe's server end (`GetNamedPipeServerProcessId`),
/// read on either end: on a client's end, the process that created the pipe
/// instance it is connected to (S7 HIL v2: the guard reads it on its
/// supervisor connection, and HIL compares it with the engine's pid, which
/// proves the engine created the first instance and serves the guard's).
pub fn server_pid(pipe: BorrowedHandle<'_>) -> io::Result<u32> {
    let mut pid = 0u32;
    // SAFETY: a pipe handle valid for the call; only `pid` is written.
    check(unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) })?;
    Ok(pid)
}

/// Writes `buf` to a pipe opened for overlapped I/O (interprocess opens
/// both ends that way) and waits at most `limit` for the peer to take it:
/// a blocking-mode pipe completes a write only once its data fits the
/// pipe's buffer or the peer has read it. A write still pending after
/// `limit` is cancelled, and this returns only after the cancellation has
/// landed, so nothing of it reaches the peer later; it then fails with
/// `TimedOut`. Other failures (the peer has gone: `ERROR_NO_DATA` and kin)
/// pass unchanged. Returns the bytes written.
///
/// It writes on the handle itself, so an interprocess stream written only
/// through it is never marked for interprocess's flush on drop (limbo):
/// dropping the stream closes its handle at once, and the peer still reads
/// what was written, then the end of the stream.
pub fn write_within(pipe: BorrowedHandle<'_>, buf: &[u8], limit: Duration) -> io::Result<usize> {
    let handle = pipe.as_raw_handle();
    // SAFETY: no attributes and no name: a new unnamed manual-reset event,
    // not signalled, owned below.
    let event = owned(unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) })?;
    let mut overlapped = OVERLAPPED {
        hEvent: event.as_raw_handle(),
        ..Default::default()
    };
    // Only this pointer is used from here on: the system writes the
    // structure while the write is pending.
    let ov: *mut OVERLAPPED = &mut overlapped;
    let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    // SAFETY: `buf` and `overlapped` outlive the write: every path below
    // that returns after the write started waits for its completion or its
    // cancellation first. The count is null, as for any overlapped call.
    let started = unsafe { WriteFile(handle, buf.as_ptr(), len, ptr::null_mut(), ov) };
    if started == 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
            // The write never started.
            return Err(e);
        }
    }
    let mut done = 0u32;
    // SAFETY: the write above; waits on its event at most `limit`, not
    // alertable.
    if unsafe { GetOverlappedResultEx(handle, ov, &mut done, wait_ms(limit), 0) } != 0 {
        return Ok(done as usize);
    }
    let waited = io::Error::last_os_error();
    // Not done in time (or done with an error): cancel what may still be
    // pending (a finished write has nothing to cancel), then wait until the
    // write is over for good.
    // SAFETY: the same handle and structure; the result is not needed.
    unsafe { CancelIoEx(handle, ov) };
    // SAFETY: waits until the write has completed or been cancelled.
    if unsafe { GetOverlappedResult(handle, ov, &mut done, 1) } != 0 {
        // It completed between the wait and the cancellation.
        return Ok(done as usize);
    }
    let failed = io::Error::last_os_error();
    if failed.raw_os_error() == Some(ERROR_OPERATION_ABORTED as i32) {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("the peer took nothing for {limit:?} ({waited})"),
        ))
    } else {
        Err(failed)
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Write};
    use std::os::windows::io::{AsHandle, OwnedHandle};
    use std::time::Instant;

    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
    };

    use super::*;
    use crate::win::wide;

    /// `ERROR_BROKEN_PIPE`.
    const BROKEN: i32 = 109;
    /// `ERROR_NO_DATA`: the pipe is being closed.
    const CLOSING: i32 = 232;
    /// `ERROR_PIPE_NOT_CONNECTED`.
    const NOT_CONNECTED: i32 = 233;
    const LIMIT: Duration = Duration::from_millis(500);

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

    /// A server end opened like interprocess opens it (overlapped, blocking
    /// mode, 512-byte buffers) and a synchronous client connected to it.
    fn pair(name: &str) -> (OwnedHandle, File) {
        let path = format!(r"\\.\pipe\{name}");
        let wide_path = wide(&path);
        // SAFETY: a NUL-terminated name; no security attributes. The handle
        // is owned at once.
        let server = owned(unsafe {
            CreateNamedPipeW(
                wide_path.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                512,
                512,
                0,
                ptr::null(),
            )
        })
        .unwrap();
        let client = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        (server, client)
    }

    /// HIL v2's pipe owner (S7, #10): the process serving a pipe, read on a
    /// client's end, is the one that created it (with the first-instance
    /// flag, as interprocess creates the engine's). Server and client are
    /// one process here, so this proves the call and its id, not that the
    /// id is the server's rather than the caller's: HIL v2's `pipe-owner`
    /// check proves that on the PC, where the engine serves and the guard
    /// reads.
    #[test]
    fn server_pid_names_the_listening_process() {
        let (server, client) = pair(&format!("iem-win-server-pid-{}", std::process::id()));
        assert_eq!(server_pid(client.as_handle()).unwrap(), std::process::id());
        // The server's own end names it too.
        assert_eq!(server_pid(server.as_handle()).unwrap(), std::process::id());
    }

    #[test]
    fn a_write_the_peer_does_not_take_is_cancelled_after_the_limit() {
        let (server, mut client) = pair(&format!("iem-win-write-{}", std::process::id()));
        let mut got = [0u8; 5];
        assert_eq!(
            write_within(server.as_handle(), b"hello", LIMIT).unwrap(),
            5,
            "it fits the pipe's buffer: done at once"
        );
        client.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"hello");
        // 64 KiB with nobody reading: the write waits for the limit, then
        // it is cancelled.
        let big = vec![7u8; 64 * 1024];
        let start = Instant::now();
        let late = write_within(server.as_handle(), &big, LIMIT).unwrap_err();
        let took = start.elapsed();
        assert_eq!(late.kind(), io::ErrorKind::TimedOut, "{late}");
        assert!(
            late.to_string()
                .starts_with("the peer took nothing for 500ms ("),
            "{late}"
        );
        // The wait can return a hair early: `WaitForSingleObject`'s timer and
        // `Instant` are different clocks, and a 500 ms wait was once measured
        // at 499.6 ms (CI run 36473998179). Allow one timer tick (16 ms) of
        // slack on the lower bound; the upper bound stays tight.
        let slack = Duration::from_millis(16);
        assert!(
            took + slack >= LIMIT && took < LIMIT + Duration::from_secs(2),
            "{took:?}"
        );
        // Nothing of the cancelled write is left for the peer.
        assert_eq!(
            write_within(server.as_handle(), b"after", LIMIT).unwrap(),
            5
        );
        client.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"after");
        // A peer that has gone fails a write at once.
        drop(client);
        let start = Instant::now();
        let gone = write_within(server.as_handle(), b"x", LIMIT).unwrap_err();
        assert!(start.elapsed() < LIMIT, "{:?}", start.elapsed());
        assert!(
            matches!(gone.raw_os_error(), Some(BROKEN | CLOSING | NOT_CONNECTED)),
            "{gone}"
        );
    }
}
