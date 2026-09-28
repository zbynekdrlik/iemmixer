//! Children that outlive a restart of their parent (S6 design note §5.1,
//! I9): the guard starts the engine, server, tray and runner outside its own
//! job, so ending the guard's task never touches audio. When the job forbids
//! breakaway the start fails and the caller alarms; it never starts the child
//! inside the job instead.
//!
//! Every child runs without a console window (design §5.5): it gets a console
//! of its own, which no one can close and which the caller does not share, so
//! a restarted guard still reaches it with [`crate::console::ctrl_break`].

use std::io;
use std::process::{Child, Command};

pub use crate::decide::{creation_flags, helper_flags};

/// `CreateProcess` flags (winbase.h), portable so the choice is tested on
/// every OS.
pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
pub const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Starts `cmd` outside the caller's job, on a console of its own and in its
/// own process group when `new_group` ([`creation_flags`]). A job without
/// breakaway refuses the start (`PermissionDenied`).
pub fn spawn_detached(cmd: &mut Command, new_group: bool) -> io::Result<Child> {
    imp::spawn(cmd, creation_flags(new_group))
}

/// Whether a child of the calling process can leave its job: true when the
/// process is in no job, or its immediate job allows breakaway (a nested
/// job's parents are then left as far as they allow it).
pub fn breakaway_allowed() -> io::Result<bool> {
    imp::breakaway_allowed()
}

#[cfg(not(windows))]
mod imp {
    use std::io;
    use std::process::{Child, Command};

    pub(super) fn spawn(_cmd: &mut Command, _flags: u32) -> io::Result<Child> {
        crate::unsupported()
    }

    pub(super) fn breakaway_allowed() -> io::Result<bool> {
        crate::unsupported()
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command};
    use std::ptr;

    use windows_sys::Win32::System::JobObjects::{
        IsProcessInJob, JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        QueryInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    use windows_sys::core::BOOL;

    use crate::win::check;

    pub(super) fn spawn(cmd: &mut Command, flags: u32) -> io::Result<Child> {
        cmd.creation_flags(flags).spawn()
    }

    pub(super) fn breakaway_allowed() -> io::Result<bool> {
        let mut in_job: BOOL = 0;
        // SAFETY: the current-process pseudo handle; a null job handle asks
        // about any job.
        check(unsafe { IsProcessInJob(GetCurrentProcess(), ptr::null_mut(), &mut in_job) })?;
        if in_job == 0 {
            return Ok(true);
        }
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // SAFETY: a null job handle queries the calling process's immediate
        // job; `info` is the structure of this class, with its size.
        check(unsafe {
            QueryInformationJobObject(
                ptr::null_mut(),
                JobObjectExtendedLimitInformation,
                (&raw mut info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ptr::null_mut(),
            )
        })?;
        let breakaway = JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;
        Ok((info.BasicLimitInformation.LimitFlags & breakaway) != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_has_a_console_of_its_own_and_leaves_only_a_job_that_allows_it() {
        assert_eq!(
            creation_flags(true, Placement::Breakaway).unwrap(),
            CREATE_BREAKAWAY_FROM_JOB | CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP
        );
        assert_eq!(
            creation_flags(false, Placement::InJob).unwrap(),
            CREATE_NO_WINDOW
        );
        assert_eq!(
            placement(JobLimits::default()),
            Placement::NoJob,
            "a process in no job"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn spawning_is_unsupported_off_windows() {
        use crate::kind;
        use std::io::ErrorKind::Unsupported;

        assert_eq!(
            kind(spawn_detached(&mut Command::new("true"), true)),
            Some(Unsupported)
        );
        assert_eq!(
            kind(spawn_detached(&mut Command::new("true"), false)),
            Some(Unsupported)
        );
        assert_eq!(kind(job_limits()), Some(Unsupported));
    }

    #[cfg(windows)]
    #[test]
    fn the_flags_are_the_win32_values() {
        use windows_sys::Win32::System::JobObjects as j;
        use windows_sys::Win32::System::Threading as t;

        assert_eq!(CREATE_NEW_PROCESS_GROUP, t::CREATE_NEW_PROCESS_GROUP);
        assert_eq!(CREATE_BREAKAWAY_FROM_JOB, t::CREATE_BREAKAWAY_FROM_JOB);
        assert_eq!(CREATE_NO_WINDOW, t::CREATE_NO_WINDOW);
        assert_eq!(JOB_BREAKAWAY_OK, j::JOB_OBJECT_LIMIT_BREAKAWAY_OK);
        assert_eq!(
            JOB_SILENT_BREAKAWAY_OK,
            j::JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK
        );
        // JOB_ENDS_ON_CLOSE is winnt.h's 0x2000 (decide's tests): its
        // windows-sys name is a force-end verb for the integrity scan.
    }

    /// This process's job reads, whatever it is (the test runner may start
    /// the tests in a job of its own).
    #[cfg(windows)]
    #[test]
    fn the_job_of_this_process_reads() {
        let limits = job_limits().expect("the job query");
        let placed = placement(limits);
        eprintln!("this process's job: {limits:?}; a child: {placed:?}");
    }
}
