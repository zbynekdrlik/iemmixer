//! The calling process's user (S6 design note §4: the pipes' DACL names the
//! user and SYSTEM), and a pipe's DACL read back as SDDL, so a test or HIL
//! can prove that only those two may open it.

use std::io;

/// The SID of the calling process's user as text (`S-1-5-21-…`).
pub fn current_user_sid() -> io::Result<String> {
    imp::current_user_sid()
}

/// The DACL of the local named pipe `name` (`\\.\pipe\<name>`) as SDDL, the
/// way Windows writes it: generic rights mapped (`GA` reads back as `FA`),
/// well-known accounts by their alias (`SY`). Reading it opens the pipe
/// with `READ_CONTROL` only, as a client for that moment; a pipe with no
/// free instance answers `ERROR_PIPE_BUSY`.
pub fn pipe_sddl(name: &str) -> io::Result<String> {
    imp::pipe_sddl(name)
}

/// How SDDL writes the account `sid`: its alias where Windows has one
/// (`S-1-5-18` is `SY`; this computer's built-in administrator is `LA`),
/// else the SID itself. Compare it with the accounts of a [`pipe_sddl`]
/// text, which Windows writes the same way.
pub fn sddl_sid(sid: &str) -> io::Result<String> {
    imp::sddl_sid(sid)
}

#[cfg(not(windows))]
mod imp {
    use std::io;

    pub(super) fn current_user_sid() -> io::Result<String> {
        crate::unsupported()
    }

    pub(super) fn pipe_sddl(_name: &str) -> io::Result<String> {
        crate::unsupported()
    }

    pub(super) fn sddl_sid(_sid: &str) -> io::Result<String> {
        crate::unsupported()
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::ptr;

    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
        ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
        SE_KERNEL_OBJECT,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GetTokenInformation, PSECURITY_DESCRIPTOR, TOKEN_QUERY,
        TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows_sys::core::PWSTR;

    use crate::win::{check, from_wide_ptr, owned, wide};

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

    pub(super) fn pipe_sddl(name: &str) -> io::Result<String> {
        let path = wide(&format!(r"\\.\pipe\{name}"));
        // SAFETY: a NUL-terminated path; no security attributes and no
        // template. READ_CONTROL reads the descriptor and no data.
        let raw = unsafe {
            CreateFileW(
                path.as_ptr(),
                READ_CONTROL,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null(),
                OPEN_EXISTING,
                0,
                ptr::null_mut(),
            )
        };
        let pipe = owned(raw)?;
        let mut dacl: *mut ACL = ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `pipe` is open with READ_CONTROL; `sd` receives a
        // LocalAlloc'd descriptor (freed below) that `dacl` points into.
        let rc = unsafe {
            GetSecurityInfo(
                pipe.as_raw_handle(),
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut sd,
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
        let text = dacl_text(sd);
        // SAFETY: allocated by GetSecurityInfo; neither it nor `dacl`, which
        // points into it, is used after this.
        unsafe { LocalFree(sd) };
        text
    }

    pub(super) fn sddl_sid(sid: &str) -> io::Result<String> {
        let probe = wide(&format!("D:(A;;GA;;;{sid})"));
        let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: a NUL-terminated SDDL string; `sd` receives a LocalAlloc'd
        // descriptor, freed below.
        check(unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                probe.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                ptr::null_mut(),
            )
        })?;
        let text = dacl_text(sd);
        // SAFETY: allocated by the call and not used after this.
        unsafe { LocalFree(sd) };
        let text = text?;
        // `D:(A;;GA;;;<account>)`: the account is the ACE's last field.
        text.strip_suffix(')')
            .and_then(|ace| ace.rsplit_once(';'))
            .map(|(_, account)| account.to_owned())
            .ok_or_else(|| io::Error::other(format!("unexpected SDDL {text}")))
    }

    /// The DACL of a security descriptor as SDDL.
    fn dacl_text(sd: PSECURITY_DESCRIPTOR) -> io::Result<String> {
        let mut text: PWSTR = ptr::null_mut();
        // SAFETY: a valid descriptor; `text` receives a LocalAlloc'd string.
        check(unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                sd,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                ptr::null_mut(),
            )
        })?;
        // SAFETY: the call returned a NUL-terminated string.
        let out = unsafe { from_wide_ptr(text) };
        // SAFETY: allocated by the call and not used after this.
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
        assert_eq!(
            crate::kind(pipe_sddl("iem-test")),
            Some(io::ErrorKind::Unsupported)
        );
        assert_eq!(
            crate::kind(sddl_sid("S-1-5-18")),
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

    #[cfg(windows)]
    #[test]
    fn sddl_writes_well_known_accounts_by_their_alias() {
        assert_eq!(sddl_sid("S-1-5-18").unwrap(), "SY");
        assert_eq!(sddl_sid("S-1-1-0").unwrap(), "WD");
        let user = current_user_sid().unwrap();
        let written = sddl_sid(&user).unwrap();
        let alias = written.len() == 2 && written.bytes().all(|b| b.is_ascii_uppercase());
        assert!(written == user || alias, "{user} is written {written}");
        assert!(sddl_sid("not a sid").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn a_missing_pipe_has_no_dacl_to_read() {
        let e = pipe_sddl("iemmixer-test-no-such-pipe").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound, "{e}");
    }
}
