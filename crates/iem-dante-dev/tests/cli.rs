// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! The binary as deploy.sh and the gates run it (#72): `--version` is the
//! stack's with iemmixer's version and build, `health` is ours (its
//! command line's exit 2, its usage, its line and exit code).

#![cfg(target_os = "linux")]

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_iem-dante-dev"))
        .args(args)
        .output()
        .expect("iem-dante-dev runs")
}

#[test]
fn version_names_iemmixer_s_version_and_build() {
    let out = run(&["--version"]);
    assert_eq!(out.status.code(), Some(0));
    let sha = option_env!("GITHUB_SHA").unwrap_or("local");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("iem-dante-dev {} {sha}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn a_health_command_line_error_exits_2_with_the_usage() {
    let out = run(&["health", "--server-port", "x"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.starts_with(
            "iem-dante-dev: --server-port \"x\" is not a port from 1 to 65535\n\n\
             usage: iem-dante-dev health"
        ),
        "{err}"
    );
}

#[test]
fn health_help_prints_its_usage_and_exits_0() {
    let out = run(&["health", "--help"]);
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.starts_with("usage: iem-dante-dev health [--socket <path>] [--server-port <n>]"),
        "{text}"
    );
}

#[test]
fn health_prints_one_line_and_exits_with_its_state() {
    // The build machine runs none of the box's units: no socket, no server
    // on a port nobody holds. Its line is printed whatever the state, and
    // its exit code is that state's (3 stopped when systemd shows the
    // three inactive, 1 otherwise).
    let dir = std::env::temp_dir().join("iem-dante-dev-cli-test-none.sock");
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
        .to_string();
    let out = run(&[
        "health",
        "--socket",
        dir.to_str().unwrap(),
        "--server-port",
        &port,
    ]);
    let text = String::from_utf8_lossy(&out.stdout);
    let line: serde_json::Value = serde_json::from_str(text.trim_end()).expect("one JSON line");
    assert_eq!(text.lines().count(), 1, "{text}");
    let state = line["health"]["state"].as_str().expect("a state");
    let want = if state == "stopped" { 3 } else { 1 };
    assert_ne!(state, "healthy", "{text}");
    assert_eq!(out.status.code(), Some(want), "{text}");
    assert_eq!(line["health"]["server"]["answers"], 0, "{text}");
}
