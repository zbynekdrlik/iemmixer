//! Console control events (S6 design note §5.2, "back to event" step 3, and
//! §5.5): the guard asks a child to stop with Ctrl-Break, which the child
//! handles as a graceful shutdown (tokio's `ctrl_break` in `iem-server`).
//!
//! Children run without a console window ([`crate::spawn::spawn_detached`]),
//! so each has a console of its own and the guard shares none of them. A
//! Ctrl-Break reaches a child only through its own console: the caller
//! detaches from any console, attaches to the child's, ignores Ctrl events
//! itself, sends the event to the child's group, detaches and restores its
//! handling. A restarted guard therefore reaches the children it adopted.

use std::io;

/// Sends Ctrl-Break to the process group `pid` on that process's own console:
/// a child started with [`crate::spawn::spawn_detached`] and `new_group`
/// (its pid is its group id). Only that group receives it. Refuses 0, which
/// would signal every process on the console, the caller included.
///
/// One call at a time per process (a process-wide lock): attaching changes
/// the caller's console for the whole process. Afterwards the caller has no
/// console; its handling of Ctrl-C is restored on every path.
pub fn ctrl_break(pid: u32) -> io::Result<()> {
    crate::decide::one_group(pid)?;
    imp::ctrl_break(pid)
}

#[cfg(not(windows))]
mod imp {
    use std::io;

    pub(super) fn ctrl_break(_pid: u32) -> io::Result<()> {
        crate::unsupported()
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::sync::{Mutex, PoisonError};

    use windows_sys::Win32::Foundation::{FALSE, TRUE};
    use windows_sys::Win32::System::Console::{
        AttachConsole, CTRL_BREAK_EVENT, FreeConsole, GenerateConsoleCtrlEvent,
        SetConsoleCtrlHandler,
    };

    use crate::win::check;

    /// Attaching to a child's console changes the console of the whole
    /// process: one Ctrl-Break at a time.
    static LOCK: Mutex<()> = Mutex::new(());

    /// Attached to a child's console: dropping it detaches first, then
    /// restores the caller's Ctrl-C handling (in this order, so an event
    /// still arriving while attached stays ignored).
    struct Attached;

    impl Drop for Attached {
        fn drop(&mut self) {
            // SAFETY: plain calls; detaching fails harmlessly when already
            // detached, and a null routine with FALSE restores Ctrl-C.
            unsafe {
                FreeConsole();
                SetConsoleCtrlHandler(None, FALSE);
            }
        }
    }

    pub(super) fn ctrl_break(pid: u32) -> io::Result<()> {
        let _one_at_a_time = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        // SAFETY: a plain call; it fails (ignored) when the process has no
        // console, which is the state it asks for.
        unsafe { FreeConsole() };
        // SAFETY: a plain call with a process id.
        check(unsafe { AttachConsole(pid) })?;
        let _attached = Attached;
        // SAFETY: a null routine with TRUE: this process ignores Ctrl-C while
        // attached (the Ctrl-Break goes to the child's group only).
        check(unsafe { SetConsoleCtrlHandler(None, TRUE) })?;
        // SAFETY: a documented event and a group id other than 0
        // (`decide::one_group`).
        check(unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind;

    #[test]
    fn group_zero_is_refused_everywhere() {
        assert_eq!(kind(ctrl_break(0)), Some(io::ErrorKind::InvalidInput));
    }

    #[cfg(not(windows))]
    #[test]
    fn ctrl_break_is_unsupported_off_windows() {
        assert_eq!(kind(ctrl_break(1)), Some(io::ErrorKind::Unsupported));
        assert_eq!(kind(ctrl_break(4242)), Some(io::ErrorKind::Unsupported));
    }

    // The Windows path detaches the whole test process from its console, so
    // it runs in its own test binary, one test at a time: tests/ctrl_break.rs.
}
