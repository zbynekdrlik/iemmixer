//! A job that allows no breakaway and does not end its processes when it
//! closes (the PC's task job allows no breakaway, #9 2026-09-28):
//! `spawn_detached` starts the child inside it, on a console of its own,
//! and Ctrl-Break ends it (S6 design note §5.1, §5.5). The test nests this
//! process in a new job without limits (Windows 8 and later), so it does
//! not depend on the job the test runner starts it in. The nesting lasts for
//! the process's life: this binary holds this one test only.
//!
//! Windows only; the helper binary needs `--features test-helper`, and
//! without it this test fails (it is never skipped).

#![cfg(windows)]

mod common;

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::ptr;

use common::{assert_ctrl_break_ends, helper};
use iem_win::spawn::{self, JobLimits, Placement};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::core::BOOL;

/// A new job without limits, with this process nested in it.
fn nest_in_a_plain_job() -> OwnedHandle {
    // SAFETY: default security, no name; the handle is checked below.
    let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
    assert!(
        !job.is_null(),
        "CreateJobObjectW: {}",
        io::Error::last_os_error()
    );
    // SAFETY: a job handle just created, owned from here on.
    let job = unsafe { OwnedHandle::from_raw_handle(job) };
    // SAFETY: a live job handle and the current-process pseudo handle.
    let assigned = unsafe { AssignProcessToJobObject(job.as_raw_handle(), GetCurrentProcess()) };
    assert_ne!(
        assigned,
        0,
        "AssignProcessToJobObject: {}",
        io::Error::last_os_error()
    );
    job
}

/// Whether `process` runs in `job`.
fn in_job(process: RawHandle, job: RawHandle) -> bool {
    let mut result: BOOL = 0;
    // SAFETY: two live handles; `result` is written on success.
    let ok = unsafe { IsProcessInJob(process, job, &mut result) };
    assert_ne!(ok, 0, "IsProcessInJob: {}", io::Error::last_os_error());
    result != 0
}

#[test]
fn a_child_starts_inside_a_job_that_allows_no_breakaway_and_ends_nothing() {
    let job = nest_in_a_plain_job();
    let limits = spawn::job_limits().expect("the job query");
    assert_eq!(
        limits,
        JobLimits {
            in_job: true,
            ..JobLimits::default()
        }
    );
    assert_eq!(spawn::placement(limits), Placement::InJob);
    let child = spawn::spawn_detached(&mut helper(), true).expect("a start inside the job");
    assert!(
        in_job(child.as_raw_handle(), job.as_raw_handle()),
        "the child runs in this process's job"
    );
    assert_ctrl_break_ends(child);
}
