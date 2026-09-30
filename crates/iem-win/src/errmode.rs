//! Crashes release the card first (S6 design note §3): the engine turns off
//! Windows' crash and critical-error dialogs at start, so a crash never
//! leaves a dialog in session 1 that keeps the process, and with it the
//! driver, alive.

use std::io;

/// Turns off the calling process's error dialogs: the critical-error box and
/// the fault box (`SetErrorMode` with `SEM_FAILCRITICALERRORS` and
/// `SEM_NOGPFAULTERRORBOX`, added to the flags already set), and the Windows
/// Error Reporting UI (`WerSetFlags(WER_FAULT_REPORTING_NO_UI)`).
pub fn quiet_crashes() -> io::Result<()> {
    imp::quiet_crashes()
}

#[cfg(not(windows))]
mod imp {
    use std::io;

    pub(super) fn quiet_crashes() -> io::Result<()> {
        crate::unsupported()
    }
}

#[cfg(windows)]
mod imp {
    use std::io;

    use windows_sys::Win32::Foundation::S_OK;
    use windows_sys::Win32::System::Diagnostics::Debug::{
        GetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode,
    };
    use windows_sys::Win32::System::ErrorReporting::{WER_FAULT_REPORTING_NO_UI, WerSetFlags};

    pub(super) fn quiet_crashes() -> io::Result<()> {
        // SAFETY: plain calls on the process's own error mode; the old flags
        // are kept.
        unsafe {
            SetErrorMode(GetErrorMode() | SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
        }
        // SAFETY: a plain call with a documented flag.
        let hr = unsafe { WerSetFlags(WER_FAULT_REPORTING_NO_UI) };
        if hr == S_OK {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(hr))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn quiet_crashes_is_unsupported_off_windows() {
        assert_eq!(
            crate::kind(quiet_crashes()),
            Some(io::ErrorKind::Unsupported)
        );
    }

    #[cfg(windows)]
    #[test]
    fn no_error_dialog_after_quiet_crashes() {
        use windows_sys::Win32::System::Diagnostics::Debug::{
            GetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX,
        };
        use windows_sys::Win32::System::ErrorReporting::{WER_FAULT_REPORTING_NO_UI, WerGetFlags};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        quiet_crashes().unwrap();
        // SAFETY: a plain query of the process's error mode.
        let mode = unsafe { GetErrorMode() };
        assert_ne!(mode & SEM_FAILCRITICALERRORS, 0, "{mode:#x}");
        assert_ne!(mode & SEM_NOGPFAULTERRORBOX, 0, "{mode:#x}");
        let mut flags = 0u32;
        // SAFETY: the current-process pseudo handle and a writable flag word.
        let hr = unsafe { WerGetFlags(GetCurrentProcess(), &mut flags) };
        assert_eq!(hr, 0);
        assert_ne!(flags & WER_FAULT_REPORTING_NO_UI, 0, "{flags:#x}");
        // Twice is the same.
        quiet_crashes().unwrap();
        // SAFETY: as above.
        assert_eq!(unsafe { GetErrorMode() }, mode);
    }
}
