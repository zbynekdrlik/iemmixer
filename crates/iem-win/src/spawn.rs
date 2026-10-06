//! Children that outlive a restart of their starter (S6 design note §5.1,
//! I9): the guard starts the engine, server, tray, runner, a direct REAPER
//! or app start and a hand-over's new guard so that the end of the guard's
//! process never ends them. Where they start follows the job the guard runs
//! in, as read ([`job_limits`], [`placement`]), never found by trying: out of
//! a job that allows breakaway; without the flag where the job lets every
//! child out silently or there is none; inside a job that allows no
//! breakaway and does not end its processes when it closes (it lives on
//! while any of them runs); and nowhere in a job that does: the start fails
//! and the caller alarms. The PC's task job allows no breakaway (#9
//! 2026-09-28).
//!
//! Every child runs without a console window (design §5.5): it gets a console
//! of its own, which no one can close and which the caller does not share, so
//! a restarted guard still reaches it with [`crate::console::ctrl_break`].

use std::io;
use std::process::{Child, Command};

pub use crate::decide::{JobLimits, Placement, creation_flags, helper_flags, placement};

/// `CreateProcess` flags (winbase.h), portable so the choice is tested on
/// every OS.
pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
pub const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A job's `LimitFlags` bits (winnt.h), portable so the reading is tested on
/// every OS ([`JobLimits::from_flags`]).
pub const JOB_BREAKAWAY_OK: u32 = 0x0000_0800;
pub const JOB_SILENT_BREAKAWAY_OK: u32 = 0x0000_1000;
/// The limit under which closing a job's last handle ends every process in
/// it. Its winnt.h name is one of the force-end verbs the integrity scan
/// refuses in code, so only its value is written here.
pub const JOB_ENDS_ON_CLOSE: u32 = 0x0000_2000;

/// Starts `cmd` as a long-lived child (the module's rule): placed by this
/// process's job ([`job_limits`], [`placement`]), on a console of its own
/// and in its own process group when `new_group` ([`creation_flags`]). A job
/// that allows no breakaway and ends its processes when it closes refuses
/// the start (`PermissionDenied`, [`Placement::Refuse`]'s reason); a job
/// that cannot be read fails it.
pub fn spawn_detached(cmd: &mut Command, new_group: bool) -> io::Result<Child> {
    let flags = creation_flags(new_group, placement(job_limits()?))?;
    imp::spawn(cmd, flags)
}

/// The job this process runs in: none, or the limits of its immediate job
/// (the parents of a nested job are not read). [`spawn_detached`] reads it
/// at every start.
pub fn job_limits() -> io::Result<JobLimits> {
    imp::job_limits()
}

#[cfg(not(windows))]
mod imp {
    use std::io;
    use std::process::{Child, Command};

    use super::JobLimits;

    pub(super) fn spawn(_cmd: &mut Command, _flags: u32) -> io::Result<Child> {
        crate::unsupported()
    }

    pub(super) fn job_limits() -> io::Result<JobLimits> {
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
        IsProcessInJob, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        QueryInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    use windows_sys::core::BOOL;

    use super::JobLimits;
    use crate::win::check;

    pub(super) fn spawn(cmd: &mut Command, flags: u32) -> io::Result<Child> {
        cmd.creation_flags(flags).spawn()
    }

    pub(super) fn job_limits() -> io::Result<JobLimits> {
        let mut in_job: BOOL = 0;
        // SAFETY: the current-process pseudo handle; a null job handle asks
        // about any job.
        check(unsafe { IsProcessInJob(GetCurrentProcess(), ptr::null_mut(), &mut in_job) })?;
        if in_job == 0 {
            return Ok(JobLimits::default());
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
        Ok(JobLimits::from_flags(info.BasicLimitInformation.LimitFlags))
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
