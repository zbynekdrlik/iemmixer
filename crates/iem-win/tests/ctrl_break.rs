//! Ctrl-Break reaches a child that runs without a console window, in its own
//! process group, through the child's own console; the child ends by itself
//! through tokio's listener, and the test process survives: the server's
//! graceful stop (S6 design note §5.2, "back to event" step 3, and §5.5).
//! `spawn_detached` places the child by the job this process runs in, as it
//! reads it (§5.1).
//!
//! Windows only; the helper binary needs `--features test-helper`, and
//! without it these tests fail (they are never skipped).

#![cfg(windows)]

mod common;

use std::io::ErrorKind;
use std::os::windows::process::CommandExt;
use std::sync::{Mutex, MutexGuard, PoisonError};

use common::{assert_ctrl_break_ends, helper};
use iem_win::spawn::{self, Placement};

/// `ctrl_break` detaches this whole process from its console and attaches it
/// to the child's: one test at a time, from the child's start to its end.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The PC's shape (its task job allows no breakaway, #9 2026-09-28): a
/// child without a console window, in its own group, inside the job.
#[test]
fn ctrl_break_ends_a_child_on_its_own_console() {
    let _serial = serial();
    let flags = spawn::creation_flags(true, Placement::InJob).expect("an in-job start");
    assert_eq!(
        flags,
        spawn::CREATE_NO_WINDOW | spawn::CREATE_NEW_PROCESS_GROUP
    );
    let child = helper()
        .creation_flags(flags)
        .spawn()
        .expect("start the helper");
    assert_ctrl_break_ends(child);
}

/// `spawn_detached` places the child by this process's job as read: out of
/// a job that allows breakaway, outside every job when the job lets its
/// children out silently or there is none, inside a job that allows no
/// breakaway and does not end its processes when it closes, and nowhere in
/// one that does (the reason, `PermissionDenied`). Cargo's own job, which
/// the hosted runner's tests run in, allows no breakaway and ends its
/// processes when it closes, so there the start is refused;
/// `spawn_job.rs` starts a child inside a job.
#[test]
fn spawn_detached_places_the_child_by_the_job_it_reads() {
    let _serial = serial();
    let limits = spawn::job_limits().expect("the job query");
    let placed = spawn::placement(limits);
    eprintln!("this process's job: {limits:?}; a child: {placed:?}");
    match (placed, spawn::spawn_detached(&mut helper(), true)) {
        (Placement::Refuse(why), Err(e)) => {
            assert_eq!(e.kind(), ErrorKind::PermissionDenied, "{e}");
            assert_eq!(e.to_string(), why);
        }
        (Placement::Refuse(why), Ok(child)) => {
            assert_ctrl_break_ends(child);
            panic!("started although the job refuses it: {why}");
        }
        (_, Ok(child)) => assert_ctrl_break_ends(child),
        (placed, Err(e)) => panic!("{placed:?}, but the start failed: {e}"),
    }
}
