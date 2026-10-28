//! The `iem-engine` binary through its command line (split from
//! `pipes.rs`, #32): run and shut down, the SEH and parked-engine tests'
//! abort off Windows, the offline render, `check-site` and the card off
//! Windows.

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

/// `--version` names the build as `Hello.engine_build` does (Gen 3 phase
/// C: the box's deploy reads it against the bundle's SHA).
#[test]
fn the_binary_names_its_build() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_iem-engine"))
        .arg("--version")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!(
            "iem-engine {}+{}\n",
            env!("CARGO_PKG_VERSION"),
            option_env!("GITHUB_SHA").unwrap_or("local")
        )
    );
}

/// Off Linux the Dante backend is a usage error (exit 2); on Linux a
/// device config that cannot be read is one too (the device's setup, which
/// its unit never restarts), before the device binds anything.
#[test]
fn the_binary_refuses_a_dante_device_it_cannot_set_up_as_usage() {
    let dir = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_iem-engine"))
        .args(["run", "--backend", "dante", "--site"])
        .arg(common::site_path())
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .args(["--pipe", &pipe_name(&dir), "--dante-config"])
        .arg(dir.path().join("missing-device.toml"))
        .args(["--dante-iface", "eth9", "--dante-control"])
        .arg(dir.path().join("c.sock"))
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The owner-approved SEH test (design §10 test #4) off Windows: with
/// `--fault-injection`, `InjectSeh` reaches the RT thread, whose
/// `inject_seh` aborts the process (no SEH filter exists here; like the
/// structured exception on the PC, nothing can catch it).
#[cfg(unix)]
#[test]
fn inject_seh_aborts_the_binary_off_windows() {
    aborts_the_binary_off_windows(Cmd::InjectSeh);
}

/// The parked-engine test (design §10 test #2, #35) off Windows: with
/// `--fault-injection`, `InjectPark` reaches the RT thread, whose
/// `inject_park` aborts the process like `inject_seh` (no SEH filter and no
/// card to keep exist here).
#[cfg(unix)]
#[test]
fn inject_park_aborts_the_binary_off_windows() {
    aborts_the_binary_off_windows(Cmd::InjectPark);
}

/// The binary with `--fault-injection` ends by SIGABRT after `cmd`, not by a
/// clean exit or a caught panic. Should it keep running, the test asks it to
/// shut down (never a forced end) before it fails.
#[cfg(unix)]
fn aborts_the_binary_off_windows(cmd: Cmd) {
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
    // The injections are the supervisor's alone (review of PR #40, #35).
    let mut c = Client::new(&pipe);
    c.hello(Role::Supervisor);
    let what = format!("{cmd:?}");
    // Not `request`: the process may end before its reply is written.
    c.send(&ClientMsg::Request {
        id: 1,
        origin: None,
        cmd,
    });
    let Some(status) = exited(&mut child) else {
        c.send(&ClientMsg::Request {
            id: 2,
            origin: None,
            cmd: Cmd::Shutdown,
        });
        let after = exited(&mut child);
        panic!("{what} did not end the engine within {WAIT:?} (after Shutdown: {after:?})");
    };
    assert_eq!(status.signal(), Some(SIGABRT), "{status:?}");
    assert_eq!(status.code(), None, "{status:?}");
}

/// #32 minor-4: a state directory another process holds past the engine's
/// wait (an engine that just ended may hold its lock a moment) ends the
/// binary with exit 75, which the guard retries without counting a crash;
/// no state file is touched. Should it keep waiting, the test frees the
/// directory and asks it to shut down (never a forced end) before it fails.
#[test]
fn a_held_state_dir_ends_the_binary_with_exit_75() {
    /// The engine waits 3 s; this bounds the test.
    const BOUND: Duration = Duration::from_secs(8);
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let held = iem_engine::persist::Store::open(&state)
        .unwrap()
        .lock()
        .unwrap();
    let pipe = pipe_name(&dir);
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_iem-engine"))
        .args(["run", "--site"])
        .arg(common::site_path())
        .arg("--state-dir")
        .arg(&state)
        .args(["--pipe", &pipe])
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if start.elapsed() >= BOUND {
            drop(held);
            let mut c = Client::new(&pipe);
            c.hello(Role::Control);
            let _ = c.request(1, Cmd::Shutdown);
            panic!("the engine still waited for its state directory after {BOUND:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(75), "{status:?}");
    let names: Vec<String> = std::fs::read_dir(&state)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["engine.lock"], "no state file touched");
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

/// Off Windows the card cannot open: `run --backend asio` is a usage error
/// (exit 2), never a card refusal (exit 3). `interlock` is no command (#38:
/// nothing listens to the stage before a switch): a usage error too.
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
    let gone = engine(&["interlock"]);
    assert_eq!(gone.status.code(), Some(2));
    assert!(gone.stdout.is_empty());
    assert!(String::from_utf8_lossy(&gone.stderr).contains("unknown command \"interlock\""));
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

/// `iem-engine supervise` (Gen 3 phase H, #72) on its command line: a
/// level above the HIL ceiling is exit 2 before anything is connected, a
/// socket nobody serves exit 1, each with one line on stderr and nothing on
/// stdout. Off Unix the command is refused as usage (exit 2): on the PC the
/// guard is the engine's supervisor.
#[test]
fn the_binary_refuses_a_supervise_call_it_cannot_make() {
    let dir = tempfile::tempdir().unwrap();
    let supervise = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_iem-engine"))
            .arg("supervise")
            .args(args)
            .output()
            .unwrap()
    };
    let loud = supervise(&["test-signal", "mic1", "-10", "5"]);
    assert_eq!(loud.status.code(), Some(2));
    assert!(loud.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&loud.stderr)
            .contains("-10 dBFS is above the HIL ceiling of -20 dBFS")
    );
    let none = dir.path().join("none.sock").to_string_lossy().into_owned();
    let unserved = supervise(&["--pipe", none.as_str(), "status"]);
    assert!(unserved.stdout.is_empty());
    let said = String::from_utf8_lossy(&unserved.stderr);
    if cfg!(unix) {
        assert_eq!(unserved.status.code(), Some(1), "{said}");
        assert_eq!(
            said,
            "iem-engine supervise: cannot connect to the engine's pipe (entity not found)\n"
        );
    } else {
        assert_eq!(unserved.status.code(), Some(2), "{said}");
        assert!(
            said.contains("the guard is the engine's supervisor"),
            "{said}"
        );
    }
}

/// `iem-engine supervise inject-fault` (Gen 3 phase H Task 6 Step 3, #72):
/// against an engine run without `--fault-injection` it is refused, exit 1
/// and one fixed line, and the engine plays on; against one run with it
/// (the box HIL gate's job) it is answered with one JSON line, exit 0, and
/// the engine faults its RT callback by the injected panic and ends itself
/// (exit 70 for the binary, which systemd starts again on the box).
#[cfg(unix)]
#[test]
fn the_binary_injects_the_rt_panic_only_into_an_engine_run_with_the_flag() {
    let inject = |pipe: &str| {
        std::process::Command::new(env!("CARGO_BIN_EXE_iem-engine"))
            .args(["supervise", "--pipe", pipe, "inject-fault"])
            .output()
            .unwrap()
    };
    let e = Engine::start(
        Flags {
            test_signal: true,
            fault_injection: false,
        },
        InputSignal::Silence,
    );
    // The engine's pipe is up once an observer has its hello.
    let mut obs = e.client();
    obs.hello(Role::Observe);
    let refused = inject(e.pipe.as_str());
    assert_eq!(refused.status.code(), Some(1));
    assert!(refused.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&refused.stderr),
        "iem-engine supervise: the engine refused it: forbidden\n"
    );
    drop(obs);
    e.shutdown();

    let mut e = Engine::start(
        Flags {
            test_signal: false,
            fault_injection: true,
        },
        InputSignal::Silence,
    );
    let mut obs = e.client();
    obs.hello(Role::Observe);
    let out = inject(e.pipe.as_str());
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(0), "{said}");
    let line = String::from_utf8(out.stdout).unwrap();
    assert_eq!(line.lines().count(), 1, "{line}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap(),
        serde_json::json!({"inject_fault": {"rev": 0}})
    );
    drop(obs);
    let Exit::Fault(why) = e.exit() else {
        panic!("the injected panic did not fault the engine")
    };
    assert!(
        why.contains("fault injection: panic on the RT thread"),
        "{why}"
    );
}

/// `iem-engine supervise` (Gen 3 phase H, #72) against an engine run with
/// `--test-signal` (the box's HIL job): the burst sounds on both spare
/// outputs at its level (the `Status` the `status` call prints), the stop
/// is answered; one JSON line each, exit 0.
#[cfg(unix)]
#[test]
fn the_binary_sounds_reads_and_stops_the_hil_signal() {
    let e = Engine::start(
        Flags {
            test_signal: true,
            fault_injection: false,
        },
        InputSignal::Silence,
    );
    // The engine's pipe is up once an observer has its hello.
    let mut obs = e.client();
    obs.hello(Role::Observe);
    let supervise = |args: &[&str]| -> serde_json::Value {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_iem-engine"))
            .args(["supervise", "--pipe", e.pipe.as_str()])
            .args(args)
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&out.stderr).into_owned();
        assert_eq!(out.status.code(), Some(0), "{args:?}: {said}");
        let line = String::from_utf8(out.stdout).unwrap();
        assert_eq!(line.lines().count(), 1, "{line}");
        serde_json::from_str(&line).unwrap()
    };
    let burst = supervise(&["test-signal", "mic1", "-30", "5"]);
    assert_eq!(
        burst,
        serde_json::json!({"test_signal": {
            "rev": 1, "outputs": 2, "dbfs": -30.0, "seconds": 5, "listen": false
        }})
    );
    let status = supervise(&["status"]);
    let hil = status["status"]["hil"].as_array().unwrap();
    assert_eq!(hil.len(), 2, "{status}");
    for out in hil {
        let peak = out["peak"].as_f64().unwrap();
        assert!(
            (20.0 * peak.log10() + 30.0).abs() < 0.5,
            "a spare output at {peak} during the burst: {status}"
        );
    }
    assert_eq!(
        supervise(&["stop-test-signal"]),
        serde_json::json!({"stop_test_signal": {"rev": 2}})
    );
    drop(obs);
    e.shutdown();
}
