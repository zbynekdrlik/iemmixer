//! Helpers shared by the `cfg(windows)` bodies: UTF-16 strings, owned
//! handles and `BOOL` results.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{FromRawHandle, OwnedHandle};

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::core::BOOL;

/// `s` as NUL-terminated UTF-16.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// The text before the first NUL of a fixed UTF-16 buffer.
pub(crate) fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(buf.get(..end).unwrap_or(buf))
}

/// A NUL-terminated UTF-16 string a Win32 call returned.
///
/// # Safety
///
/// `p` is non-null and points to a readable UTF-16 string ending in NUL.
pub(crate) unsafe fn from_wide_ptr(p: *const u16) -> String {
    let mut len = 0;
    // SAFETY: the caller guarantees a terminating NUL, so every unit up to
    // and including it is readable.
    while unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: the `len` units before the NUL were just read.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) })
}

/// A Win32 `BOOL` result: zero is failure, with the thread's last error.
pub(crate) fn check(ok: BOOL) -> io::Result<()> {
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Takes ownership of a handle a Win32 call returned; null or
/// `INVALID_HANDLE_VALUE` is that call's failure (with its last error).
pub(crate) fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a fresh handle the caller received and nothing else closes.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// Image, module and class names compare without ASCII case (Windows file
/// names and window classes are case-insensitive).
pub(crate) fn same_name(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}
