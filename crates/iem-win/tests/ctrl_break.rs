//! Ctrl-Break reaches a child in its own process group on our console, and
//! the child ends by itself through tokio's listener: the server's graceful
//! stop (S6 design note §5.2, "back to event" step 3). `spawn_detached`
//! either breaks the child away from our job or reports the job's refusal.
//!
//! Windows only; the helper binary needs `--features test-helper`, and
//! without it these tests fail (they are never skipped).

#![cfg(windows)]

use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use iem_win::{console, spawn};

fn helper() -> Command {
    // option_env!, not env!: without the feature the tests still compile
    // (clippy --all-targets) and then fail here.
    let Some(path) = option_env!("CARGO_BIN_EXE_iem-win-ctrlbreak-helper") else {
        panic!("the Ctrl-Break helper is built only with --features test-helper");
    };
    let mut cmd = Command::new(path);
    cmd.stdout(Stdio::piped());
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
    console::ctrl_break(child.id()).expect("GenerateConsoleCtrlEvent");
    match exit_within(&mut child, Duration::from_secs(10)) {
        Some(status) => assert_eq!(status.code(), Some(0), "{status:?}"),
        None => {
            // Nothing is force-ended: the helper gives up by itself after 30 s.
            let late = child.wait().expect("wait");
            panic!("the helper ignored Ctrl-Break and ended later with {late:?}");
        }
    }
}

#[test]
fn ctrl_break_ends_a_new_group_child_on_our_console() {
    let child = helper()
        .creation_flags(spawn::CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .expect("start the helper");
    assert_ctrl_break_ends(child);
}

#[test]
fn spawn_detached_breaks_away_or_reports_the_refusal() {
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
