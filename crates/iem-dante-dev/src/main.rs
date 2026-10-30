// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! `iem-dante-dev`: our own Dante device on the Gen 3 box (#72), run by
//! systemd as `iem-dante.service` (`scripts/gen3/iem-dante.service`). The
//! device and its command line live in dante-stack (`daemon::cli`); this
//! binary names itself, stamps iemmixer's version and build, which
//! `scripts/gen3/deploy.sh` checks with `iem-dante-dev --version`, and runs
//! the box's `health` itself (`iem_dante_dev::health`, since dante-stack
//! v0.2.0).

fn main() -> std::process::ExitCode {
    let build = iem_dante::daemon::Build {
        name: "iem-dante-dev",
        version: env!("CARGO_PKG_VERSION"),
        sha: iem_dante::daemon::build_sha(option_env!("GITHUB_SHA")),
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    match iem_dante_dev::parse_args(&args) {
        Some(command) => iem_dante_dev::run_health(build, command),
        None => iem_dante::daemon::cli::main(build),
    }
}
