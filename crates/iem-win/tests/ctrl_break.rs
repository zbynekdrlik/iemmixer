//! Ctrl-Break reaches a child that runs without a console window, in its own
//! process group, through the child's own console; the child ends by itself
//! through tokio's listener, and the test process survives: the server's
//! graceful stop (S6 design note §5.2, "back to event" step 3, and §5.5).
//! `spawn_detached` either breaks the child away from our job or reports the
//! job's refusal.
//!
//! Windows only; the helper binary needs `--features test-helper`, and
//! without it these tests fail (they are never skipped).

#![cfg(windows)]

use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use iem_win::{console, spawn};

/// `ctrl_break` detaches this whole process from its console and attaches it
/// to the child's: one test at a time, from the child's start to its end.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner)
}

fn helper() -> Command {
    // option_env!, not env!: without the feature the tests still compile
    // (clippy --all-targets) and then fail here.
    let Some(path) = option_env!("CARGO_BIN_EXE_iem-win-ctrlbreak-helper") else {
        panic!("the Ctrl-Break helper is built only with --features test-helper");
    };
    let mut cmd = Command::new(path);
    // Nothing inherited: once this process has left its console, its own
    // standard handles may no longer be valid to hand on.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    cmd
}

/// The helper's first line is `ready` once its listener is installed.
fn wait_ready(child: &mut Child) {
    let out = child.stdout.take().expect("piped stdout");
    let mut line = String::new();
    BufReader::new(out)
        .read_line(&mut line)
        .expect("the helper's first line");
    assert_eq!(line.trim(), "ready");
}

fn exit_within(child: &mut Child, limit: Duration) -> Option<ExitStatus> {
    let start = Instant::now();
    while start.elapsed() < limit {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

fn assert_ctrl_break_ends(mut child: Child) {
    wait_ready(&mut child);
    console::ctrl_break(child.id()).expect("Ctrl-Break through the child's console");
    match exit_within(&mut child, Duration::from_secs(10)) {
        Some(status) => assert_eq!(status.code(), Some(0), "{status:?}"),
        None => {
            // Nothing is force-ended: the helper gives up by itself after 30 s.
            let late = child.wait().expect("wait");
            panic!("the helper ignored Ctrl-Break and ended later with {late:?}");
        }
    }
    // Reaching this line means the Ctrl-Break did not reach the test process
    // (its default handling would have ended it).
}

/// The PC's shape without the breakaway (cargo's job forbids it): a child
/// without a console window, in its own group.
#[test]
fn ctrl_break_ends_a_child_on_its_own_console() {
    let _serial = serial();
    let flags = spawn::creation_flags(true) & !spawn::CREATE_BREAKAWAY_FROM_JOB;
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

#[test]
fn spawn_detached_breaks_away_or_reports_the_refusal() {
    let _serial = serial();
    let allowed = spawn::breakaway_allowed().expect("the job query");
    match spawn::spawn_detached(&mut helper(), true) {
        Ok(child) => {
            assert!(allowed, "started although our job forbids breakaway");
            assert_ctrl_break_ends(child);
        }
        Err(e) => {
            assert!(!allowed, "refused although breakaway is allowed: {e}");
            // ERROR_ACCESS_DENIED: the job has no JOB_OBJECT_LIMIT_BREAKAWAY_OK.
            assert_eq!(e.raw_os_error(), Some(5), "{e}");
        }
    }
}
