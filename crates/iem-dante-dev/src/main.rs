// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! `iem-dante-dev`: our own Dante device on the Gen 3 box (#72), run by
//! systemd as `iem-dante.service` (`scripts/gen3/iem-dante.service`). The
//! device and its command line live in dante-stack (`daemon::cli`); this
//! binary only names itself and stamps iemmixer's version and build, which
//! `scripts/gen3/deploy.sh` checks with `iem-dante-dev --version`.

fn main() -> std::process::ExitCode {
    iem_dante::daemon::cli::main(iem_dante::daemon::Build {
        name: "iem-dante-dev",
        version: env!("CARGO_PKG_VERSION"),
        sha: iem_dante::daemon::build_sha(option_env!("GITHUB_SHA")),
    })
}
