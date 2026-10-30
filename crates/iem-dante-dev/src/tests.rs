// Ported from dante-stack fdfc321 crates/dante-stack/src/daemon/tests.rs (#72).
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
