//! The calling process's user (S6 design note §4: the pipes' DACL names the
//! user and SYSTEM).

use std::io;

/// The SID of the calling process's user as text (`S-1-5-21-…`).
pub fn current_user_sid() -> io::Result<String> {
    imp::current_user_sid()
}

#[cfg(not(windows))]
mod imp {
    use std::io;

    pub(super) fn current_user_sid() -> io::Result<String> {
        crate::unsupported()
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::ptr;

    use windows_sys::Win32::Foundation::{HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows_sys::core::PWSTR;

    use crate::win::{check, from_wide_ptr, owned};

    pub(super) fn current_user_sid() -> io::Result<String> {
        let mut raw: HANDLE = ptr::null_mut();
        // SAFETY: the current-process pseudo handle; `raw` receives a new
        // token handle, owned below.
        check(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) })?;
        let token = owned(raw)?;
        let mut len = 0u32;
        // SAFETY: a size query (null buffer of length 0): it fails with
        // ERROR_INSUFFICIENT_BUFFER and sets `len`.
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                ptr::null_mut(),
                0,
                &mut len,
            )
        };
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        // u64 storage: TOKEN_USER holds a pointer and needs its alignment.
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        // SAFETY: `buf` holds at least `len` bytes, 8-byte aligned.
        check(unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buf.as_mut_ptr().cast(),
                len,
                &mut len,
            )
        })?;
        // SAFETY: the call wrote a TOKEN_USER at the start of `buf`; its SID
        // points into `buf`, which lives until the end of this function.
        let sid = unsafe { buf.as_ptr().cast::<TOKEN_USER>().read() }.User.Sid;
        let mut text: PWSTR = ptr::null_mut();
        // SAFETY: a valid SID; `text` receives a LocalAlloc'd string.
        check(unsafe { ConvertSidToStringSidW(sid, &mut text) })?;
        // SAFETY: the call returned a NUL-terminated string.
        let out = unsafe { from_wide_ptr(text) };
        // SAFETY: `text` was allocated by the call with LocalAlloc and is not
        // used after this.
        unsafe { LocalFree(text.cast()) };
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn the_user_sid_is_unsupported_off_windows() {
        assert_eq!(
            crate::kind(current_user_sid()),
            Some(io::ErrorKind::Unsupported)
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_user_sid_is_a_local_or_domain_account() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-5-21-"), "{sid}");
        assert_eq!(current_user_sid().unwrap(), sid);
    }
}
