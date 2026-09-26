//! `iem-migrate` (S4 design note `docs/superpowers/specs/2026-09-26-s4-migration-design.md`):
//! moves the predecessor's mixes and band data into iemmixer and back. It
//! reads files only and never touches REAPER, the predecessor app or the PC:
//!
//! - `import`: saved project (+ backup JSON) → engine state, checked against
//!   `site.toml` (exit 3 and a diff when the topology differs);
//! - `export`: engine state → a **new** project from the original (rollback);
//! - `band`: the predecessor's data directory → the server's band directory
//!   (presets, snapshots, customizations, photos, PINs, secrets, push,
//!   certificate, PIN-free backup archive).
//!
//! GPL-3.0-or-later: it links `iem-engine` (D1).

#![forbid(unsafe_code)]

pub mod args;
pub mod band_cmd;
pub mod export_cmd;
pub mod import_cmd;
pub mod site;

use std::path::Path;

/// Exit codes.
pub const EXIT_IO: u8 = 1;
pub const EXIT_INPUT: u8 = 2;
pub const EXIT_TOPOLOGY: u8 = 3;

pub const USAGE: &str = "\
iem-migrate import --rpp PROJECT --aliases ALIASES --site SITE [--backup BACKUP.json]
                   [--state-dir DIR] [--emit-topology FILE] [--expect tracks=N,sends=N,…] [--dry-run]
iem-migrate export --rpp ORIGINAL --aliases ALIASES --site SITE --state-dir DIR --out NEW
iem-migrate band   --legacy DIR --aliases ALIASES --eras ERAS --site SITE --out BAND_DIR
                   [--legacy-default-pins FILE] [--partial] [--dry-run]

Reads files only. Exit codes: 0 ok, 1 I/O, 2 usage or unmappable input,
3 the project's topology differs from site.toml. Reports never print a PIN,
secret or key.";

/// Why a command stopped: the exit code and the message for stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub code: u8,
    pub msg: String,
}

impl Failure {
    pub fn io(msg: impl Into<String>) -> Self {
        Self {
            code: EXIT_IO,
            msg: msg.into(),
        }
    }

    pub fn input(msg: impl Into<String>) -> Self {
        Self {
            code: EXIT_INPUT,
            msg: msg.into(),
        }
    }
}

/// Reads a text file, naming it in the error.
pub fn read_text(path: &Path) -> Result<String, Failure> {
    std::fs::read_to_string(path).map_err(|e| Failure::io(format!("{}: {e}", path.display())))
}

/// Runs one command; `Ok` is the report for stdout.
pub fn run(args: &[String]) -> Result<String, Failure> {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("import") => import_cmd::run(rest),
        Some("export") => export_cmd::run(rest),
        Some("band") => band_cmd::run(rest),
        Some("help" | "--help" | "-h") => Ok(USAGE.to_owned()),
        _ => Err(Failure::input(USAGE)),
    }
}
