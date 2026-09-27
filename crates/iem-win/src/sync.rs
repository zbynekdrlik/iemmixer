//! One guard across all sessions (S6 design note §5.1): the guard, and
//! `iemmode event --direct` without a guard, hold the named mutex
//! `Global\iemmixer-guard`, so a guard in session 1 and a run from an ssh
//! session (session 0) meet the same one.

use std::io;

/// A named mutex in the `Global\` namespace, held while this value lives.
/// Holding it means keeping it open: the first process to create it holds
/// it, and Windows closes it when that process ends, however it ends.
#[derive(Debug)]
pub struct GlobalMutex {
    name: String,
    // Never read: dropping it closes the handle and releases the name.
    #[allow(dead_code)]
    inner: imp::Held,
}

impl GlobalMutex {
    /// Takes `Global\<name>`; `None` when another holder (any session, this
    /// process included) has it. `name` is plain: not empty, no `\`.
    pub fn try_take(name: &str) -> io::Result<Option<GlobalMutex>> {
        let full = crate::decide::global_name(name)?;
        Ok(imp::try_take(&full)?.map(|inner| GlobalMutex { name: full, inner }))
    }

    /// The full name, `Global\<name>`.
    pub fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(not(windows))]
mod imp {
    use std::io;

    /// Never created off Windows.
    #[derive(Debug)]
    pub(super) enum Held {}

    pub(super) fn try_take(_full: &str) -> io::Result<Option<Held>> {
        crate::unsupported()
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use std::ptr;

    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, FALSE};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    use crate::win::wide;

    /// The open mutex; closing it releases the name.
    #[derive(Debug)]
    pub(super) struct Held(OwnedHandle);

    pub(super) fn try_take(full: &str) -> io::Result<Option<Held>> {
        let name = wide(full);
        // SAFETY: default security, not owned by a thread (holding it means
        // keeping it open), a NUL-terminated name.
        let raw = unsafe { CreateMutexW(ptr::null(), FALSE, name.as_ptr()) };
        // Read right after the call: a new mutex sets it to 0, an existing
        // one to ERROR_ALREADY_EXISTS.
        let last = io::Error::last_os_error();
        if raw.is_null() {
            // Another user's or a higher integrity level's mutex: held.
            if last.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
                return Ok(None);
            }
            return Err(last);
        }
        // SAFETY: a fresh handle that nothing else closes.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        if last.raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32) {
            // Someone holds it; our handle to it closes here.
            drop(handle);
            return Ok(None);
        }
        Ok(Some(Held(handle)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind;

    #[test]
    fn a_name_is_plain() {
        assert_eq!(
            kind(GlobalMutex::try_take("")),
            Some(io::ErrorKind::InvalidInput)
        );
        assert_eq!(
            kind(GlobalMutex::try_take("Local\\iemmixer-test")),
            Some(io::ErrorKind::InvalidInput)
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn the_mutex_is_unsupported_off_windows() {
        assert_eq!(
            kind(GlobalMutex::try_take("iemmixer-test")),
            Some(io::ErrorKind::Unsupported)
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_global_mutex_has_one_holder_until_it_is_dropped() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("iemmixer-test-{}-{nanos}", std::process::id());
        let first = GlobalMutex::try_take(&name).unwrap().expect("first take");
        assert_eq!(first.name(), format!("Global\\{name}"));
        assert!(GlobalMutex::try_take(&name).unwrap().is_none());
        assert!(GlobalMutex::try_take(&name).unwrap().is_none());
        drop(first);
        let again = GlobalMutex::try_take(&name).unwrap();
        assert!(again.is_some());
    }
}
