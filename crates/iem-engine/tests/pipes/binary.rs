//! The `iem-engine` binary through its command line (split from
//! `pipes.rs`, #32): run and shut down, the SEH test's abort off Windows,
//! the offline render, `check-site` and the card off Windows.

use super::*;
use iem_audio_io::{Planar, wav};

#[test]
fn the_binary_runs_and_shuts_down() {
    let dir = tempfile::tempdir().unwrap();
    let pipe = pipe_name(&dir);
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_iem-engine"))
        .args(["run", "--site"])
        .arg(common::site_path())
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .args(["--pipe", &pipe])
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut c = Client::new(&pipe);
    c.hello(Role::Control);
    assert!(c.request(1, Cmd::Shutdown).error.is_none());
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        assert!(start.elapsed() < WAIT, "the binary did not exit");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(0));
    let usage = std::process::Command::new(env!("CARGO_BIN_EXE_iem-engine"))
        .args(["run", "--bogus"])
        .output()
        .unwrap();
    assert_eq!(usage.status.code(), Some(2));
    let help = std::process::Command::new(env!("CARGO_BIN_EXE_iem-engine"))
        .arg("--help")
        .output()
        .unwrap();
    assert_eq!(help.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&help.stdout).contains("iem-engine render"));
}

/// The owner-approved SEH test (design §10) off Windows: with
/// `--fault-injection`, `InjectSeh` reaches the RT thread, whose
/// `inject_seh` aborts the process (no SEH filter exists here; like the
/// structured exception on the PC, nothing can catch it). The binary ends by
/// SIGABRT, not by a clean exit or a caught panic. Should it keep running,
/// the test asks it to shut down (never a forced end) before it fails.
#[cfg(unix)]
#[test]
fn inject_seh_aborts_the_binary_off_windows() {
    use std::os::unix::process::ExitStatusExt;
    /// SIGABRT on Linux and macOS.
    const SIGABRT: i32 = 6;
    let exited = |child: &mut std::process::Child| {
        let start = Instant::now();
        loop {
            if let Some(s) = child.try_wait().unwrap() {
                return Some(s);
            }
            if start.elapsed() >= WAIT {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let dir = tempfile::tempdir().unwrap();
    let pipe = pipe_name(&dir);
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_iem-engine"))
        .args(["run", "--site"])
        .arg(common::site_path())
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .args(["--pipe", &pipe, "--fault-injection"])
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut c = Client::new(&pipe);
    c.hello(Role::Control);
    // Not `request`: the process may end before its reply is written.
    c.send(&ClientMsg::Request {
        id: 1,
        origin: None,
        cmd: Cmd::InjectSeh,
    });
    let Some(status) = exited(&mut child) else {
        c.send(&ClientMsg::Request {
            id: 2,
            origin: None,
            cmd: Cmd::Shutdown,
        });
        let after = exited(&mut child);
        panic!("InjectSeh did not end the engine within {WAIT:?} (after Shutdown: {after:?})");
    };
    assert_eq!(status.signal(), Some(SIGABRT), "{status:?}");
    assert_eq!(status.code(), None, "{status:?}");
}

#[test]
fn the_binary_renders_offline() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    let output = dir.path().join("out.wav");
    let mut audio = Planar::new(32, 960);
    for ch in 0..32 {
        audio.channel_mut(ch).fill(0.1);
    }
    wav::write_file(&input, 96_000, &audio).unwrap();
    let run = |extra: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_iem-engine"))
            .arg("render")
            .arg("--site")
            .arg(common::site_path())
            .arg("--in")
            .arg(&input)
            .arg("--out")
            .arg(&output)
            .args(extra)
            .output()
            .unwrap()
    };
    let ok = run(&["--block", "97"]);
    assert_eq!(
        ok.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let (rate, out) = wav::read_file(&output).unwrap();
    assert_eq!((rate, out.channels(), out.frames()), (96_000, 21, 960));
    wav::write_file(&input, 48_000, &audio).unwrap();
    let refused = run(&[]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("96000"));
}

#[test]
fn the_binary_checks_a_site() {
    let check = |site: &std::path::Path| {
        std::process::Command::new(env!("CARGO_BIN_EXE_iem-engine"))
            .args(["check-site", "--site"])
            .arg(site)
            .output()
            .unwrap()
    };
    let ok = check(common::site_path().as_path());
    assert_eq!(
        ok.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let summary: serde_json::Value = serde_json::from_slice(&ok.stdout).unwrap();
    assert_eq!(summary["topology"], common::topology().hash.as_str());
    assert_eq!(summary["inputs"], 24);
    assert_eq!(summary["groups"], 1);
    assert_eq!(summary["mixes"], 11);
    assert_eq!(summary["card"], true);
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("site.toml");
    let text = std::fs::read_to_string(common::site_path()).unwrap();
    std::fs::write(&bad, text.replace("frames = 32", "frames = 64")).unwrap();
    let refused = check(bad.as_path());
    assert_eq!(refused.status.code(), Some(2));
    assert!(refused.stdout.is_empty());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("frames must be 32"));
}

/// Off Windows the card cannot open: `run --backend asio` and `interlock`
/// are usage errors (exit 2), never a card refusal (exit 3).
#[cfg(not(windows))]
#[test]
fn off_windows_the_binary_refuses_the_card_as_usage() {
    let dir = tempfile::tempdir().unwrap();
    let engine = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_iem-engine"))
            .args(args)
            .arg("--site")
            .arg(common::site_path())
            .output()
            .unwrap()
    };
    let interlock = engine(&["interlock", "--seconds", "5"]);
    assert_eq!(interlock.status.code(), Some(2));
    assert!(interlock.stdout.is_empty());
    let state = dir.path().join("state").to_string_lossy().into_owned();
    let pipe = pipe_name(&dir);
    let asio = engine(&[
        "run",
        "--backend",
        "asio",
        "--state-dir",
        state.as_str(),
        "--pipe",
        pipe.as_str(),
    ]);
    assert_eq!(asio.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&asio.stderr).contains("Windows"));
}
