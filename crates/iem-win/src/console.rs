//! Console control events (S6 design note §5.2, "back to event" step 3): the
//! guard asks a child to stop with Ctrl-Break, which the child handles as a
//! graceful shutdown (tokio's `ctrl_break` in `iem-server`).

use std::io;

/// Sends Ctrl-Break to the process group `pid`: a child started with
/// `CREATE_NEW_PROCESS_GROUP` on the caller's console
/// ([`crate::spawn::spawn_detached`] with `new_group`). Only that group
/// receives it. Refuses 0, which would signal every process on the console,
/// the caller included.
pub fn ctrl_break(pid: u32) -> io::Result<()> {
    if pid == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Ctrl-Break goes to one process group, never to group 0",
        ));
    }
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

    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};

    use crate::win::check;

    pub(super) fn ctrl_break(pid: u32) -> io::Result<()> {
        // SAFETY: a plain call with a documented event and a group id.
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
}
