// Ported from dstack fdfc321 crates/dante-stack/src/daemon/tests.rs (#72).
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

use super::*;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn health_takes_a_socket_and_a_server_port() {
    let health = |socket: &str, server_port: u16| {
        Command::Health(health::Options {
            socket: PathBuf::from(socket),
            server_port,
        })
    };
    let default = "/run/iem-dante/control.sock";
    for (a, want) in [
        (&["health"][..], health(default, 80)),
        (
            &["health", "--socket", "/tmp/c.sock"],
            health("/tmp/c.sock", 80),
        ),
        (
            &[
                "health",
                "--server-port",
                "18080",
                "--socket",
                "/tmp/c.sock",
            ],
            health("/tmp/c.sock", 18_080),
        ),
        (&["health", "--server-port", "1"], health(default, 1)),
        (
            &["health", "--server-port", "65535"],
            health(default, 65_535),
        ),
        (&["health", "--help", "--bogus"], Command::Help),
        (&["health", "--socket", "/tmp/c.sock", "-h"], Command::Help),
    ] {
        assert_eq!(parse_args(&args(a)), Some(Ok(want)), "{a:?}");
    }
    let cases: &[(&[&str], &str)] = &[
        (&["health", "--socket"], "--socket needs a value"),
        (&["health", "--socket", ""], "--socket needs a path"),
        (&["health", "--server-port"], "--server-port needs a value"),
        (
            &["health", "--server-port", "0"],
            "--server-port \"0\" is not a port from 1 to 65535",
        ),
        (&["health", "--server-port", "65536"], "is not a port"),
        (&["health", "--server-port", "-1"], "is not a port"),
        (&["health", "--server-port", "http"], "is not a port"),
        (
            &["health", "--socket", "a", "--socket", "b"],
            "--socket given twice",
        ),
        (
            &["health", "--server-port", "1", "--server-port", "2"],
            "--server-port given twice",
        ),
        (
            &["health", "--version"],
            "health: unknown argument \"--version\"",
        ),
        (&["health", "status"], "health: unknown argument \"status\""),
    ];
    for (a, want) in cases {
        let got = parse_args(&args(a))
            .expect("health's command line")
            .expect_err(&format!("{a:?} is refused"));
        assert!(got.contains(want), "{a:?}: {got}");
    }
}

#[test]
fn only_a_first_word_health_is_ours_the_rest_is_the_stack_s() {
    for a in [
        &[][..],
        &["--version"],
        &["--help"],
        &["ctl", "status"],
        &["clock", "--seconds", "3"],
        &["--config", "c", "--iface", "e", "health"],
        &["Health"],
    ] {
        assert_eq!(parse_args(&args(a)), None, "{a:?}");
    }
}

#[test]
fn the_health_usage_names_the_binary_its_options_and_exit_codes() {
    let usage = health_usage("iem-dante-dev");
    assert!(
        usage.starts_with("usage: iem-dante-dev health [--socket <path>] [--server-port <n>]\n"),
        "{usage}"
    );
    for part in [
        "(default\n/run/iem-dante/control.sock)",
        "(default port 80)",
        "Exit 0 healthy, 1 degraded or failed, 3 stopped",
        "2 for\nthe command line",
    ] {
        assert!(usage.contains(part), "{part:?} in {usage}");
    }
}

#[test]
fn ctl_without_a_socket_is_ours_and_every_other_command_line_the_stacks() {
    assert_eq!(
        parse_ctl(&args(&["ctl", "status"])).as_deref(),
        Some("status")
    );
    assert_eq!(
        parse_ctl(&args(&["ctl", "source", "1", "echo", "1", "96"])).as_deref(),
        Some("source 1 echo 1 96")
    );
    for line in [
        &["ctl"][..],
        &["ctl", "--socket", "/tmp/c.sock", "status"],
        &["ctl", "--help"],
        &["ctl", "-h"],
        &["clock", "--seconds", "1"],
        &["--version"],
        &[],
    ] {
        assert_eq!(parse_ctl(&args(line)), None, "{line:?}");
    }
}

/// A device's control socket answering one request with `answer`: the path
/// and what it was asked.
#[cfg(target_os = "linux")]
fn one_answer(name: &str, answer: &'static str) -> (PathBuf, std::thread::JoinHandle<String>) {
    use std::io::{BufRead, BufReader, Write};
    let path =
        std::env::temp_dir().join(format!("iem-dante-dev-{}-{name}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let asked = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        (&stream).write_all(answer.as_bytes()).unwrap();
        line
    });
    (path, asked)
}

#[cfg(target_os = "linux")]
#[test]
fn ctl_exchange_sends_one_line_and_returns_the_answer_without_its_newline() {
    let (path, asked) = one_answer("exchange", "{\"clock\":{}}\r\n");
    assert_eq!(ctl_exchange(&path, "status").unwrap(), "{\"clock\":{}}");
    assert_eq!(asked.join().unwrap(), "status\n");
    std::fs::remove_file(&path).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn run_ctl_exits_0_only_when_the_device_did_it() {
    use std::process::ExitCode;
    let (path, _) = one_answer("ok", "ok\n");
    assert_eq!(run_ctl("t", &path, "source 1 silence"), ExitCode::SUCCESS);
    std::fs::remove_file(&path).unwrap();
    let (path, _) = one_answer("refused", "error: no such channel\n");
    assert_eq!(run_ctl("t", &path, "source 999 silence"), ExitCode::from(1));
    std::fs::remove_file(&path).unwrap();
    assert_eq!(run_ctl("t", &path, "status"), ExitCode::from(1));
}
