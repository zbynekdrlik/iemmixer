//! `iem-migrate export` (S4 design note §3.3, program spec §4.3, #20 design
//! note §7): the engine's saved state written into a **new** project made
//! from the original, which is never overwritten; the result is self-checked
//! before it is written, and the values it cannot hold are reported.

use std::io::Write as _;
use std::path::Path;

use iem_engine::persist::{Source as Saved, Store};
use iem_rpp::aliases::parse_aliases;
use iem_rpp::export::export_checked;

use crate::args::parse;
use crate::{Failure, read_text, site};

fn write_new(path: &Path, text: &str) -> Result<(), Failure> {
    let io = |e: std::io::Error| Failure::io(format!("{}: {e}", path.display()));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(io)?;
    f.write_all(text.as_bytes()).map_err(io)?;
    f.sync_all().map_err(io)
}

pub fn run(args: &[String]) -> Result<String, Failure> {
    let a = parse(
        args,
        &["--rpp", "--aliases", "--site", "--state-dir", "--out"],
        &[],
    )?;
    let rpp = a.path("--rpp")?;
    let out = a.path("--out")?;
    let dir = a.path("--state-dir")?;
    if out.exists() {
        return Err(Failure::input(format!(
            "{} exists: an export never overwrites a file (the original project least of all)",
            out.display()
        )));
    }
    if !dir.is_dir() {
        return Err(Failure::input(format!(
            "{}: no state directory",
            dir.display()
        )));
    }
    let aliases = parse_aliases(&read_text(&a.path("--aliases")?)?).map_err(Failure::input)?;
    let site = site::open(&a.path("--site")?)?;
    let store = Store::open(&dir).map_err(|e| Failure::io(format!("{}: {e}", dir.display())))?;
    let loaded = store.load(&site.compiled);
    if loaded.source == Saved::Defaults {
        return Err(Failure::input(format!("{}: no saved state", dir.display())));
    }
    let text = read_text(&rpp)?;
    let exported = export_checked(&text, &aliases, &loaded.persisted.state)
        .map_err(|p| Failure::input(format!("{}: {p}", rpp.display())))?;
    write_new(&out, &exported.text)?;
    let changed = text
        .lines()
        .zip(exported.text.lines())
        .filter(|(x, y)| x != y)
        .count();
    let mut report = vec![
        format!(
            "export of {} with the state from {:?}",
            rpp.display(),
            loaded.source
        ),
        format!("{changed} line(s) changed; self-check passed"),
        format!("written to {}", out.display()),
    ];
    report.extend(
        exported
            .dropped
            .iter()
            .map(|d| format!("not carried back (iemmixer only): {d}")),
    );
    report.extend(
        loaded
            .rejected
            .iter()
            .map(|(p, why)| format!("note: skipped {}: {why}", p.display())),
    );
    Ok(report.join("\n"))
}
