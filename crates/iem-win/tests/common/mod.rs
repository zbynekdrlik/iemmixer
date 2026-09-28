//! The Ctrl-Break helper child of the Windows spawn tests (`ctrl_break.rs`,
//! `spawn_job.rs`): it runs without a console window and ends by itself
//! on Ctrl-Break through tokio's listener, as iem-server does.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use iem_win::console;

pub fn helper() -> Command {
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

pub fn assert_ctrl_break_ends(mut child: Child) {
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
