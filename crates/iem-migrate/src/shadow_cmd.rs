//! `iem-migrate shadow` (S8 lane 4, iemmixer#11; design note
//! `docs/superpowers/specs/2026-10-10-s8-cutover-rollback-design.md` §3.5):
//! a report-only import. It reads what `import` reads (the saved project,
//! the aliases, `site.toml`) and the engine's saved state as the engine
//! would load it, and prints one JSON object ([`crate::shadow`]): what an
//! import would do, the project's counts, its topology against `site.toml`
//! and the state it would write against the saved one.
//!
//! It writes nothing: no state file, no recovery of an interrupted save,
//! no `engine.lock` (a running engine's lock never stops it, and it never
//! makes an import wait), and a missing state directory is not created
//! (`Store::open` would create it). The guard runs it at each entry from
//! event and keeps its line in `<root>\shadow\history.jsonl`.

use std::path::Path;

use iem_engine::persist::{Loaded, Store};
use iem_rpp::aliases::parse_aliases;
use iem_rpp::import::import;
use iem_rpp::legacy::LegacyProject;

use crate::args::parse;
use crate::site::SiteFile;
use crate::{Failure, read_text, shadow, site};

/// The engine's saved state as it would load it; none without a state
/// directory, which is then left uncreated.
fn saved(dir: &Path, site: &SiteFile) -> Result<Option<Loaded>, Failure> {
    if !dir.is_dir() {
        return Ok(None);
    }
    let store = Store::open(dir).map_err(|e| Failure::io(format!("{}: {e}", dir.display())))?;
    Ok(Some(store.load(&site.compiled)))
}

pub fn run(args: &[String]) -> Result<String, Failure> {
    let a = parse(args, &["--rpp", "--aliases", "--site", "--state-dir"], &[])?;
    let rpp = a.path("--rpp")?;
    let dir = a.path("--state-dir")?;
    let site_path = a.path("--site")?;
    let aliases = parse_aliases(&read_text(&a.path("--aliases")?)?).map_err(Failure::input)?;
    let site = site::open_optional(&site_path)?.ok_or_else(|| {
        Failure::input(format!(
            "{}: no [engine] table, nothing to compare with",
            site_path.display()
        ))
    })?;
    let project = LegacyProject::parse(&read_text(&rpp)?)
        .map_err(|e| Failure::input(format!("{}: {e}", rpp.display())))?;
    let live = saved(&dir, &site)?;
    let report = match import(&project, &aliases) {
        Ok(imp) => shadow::shadow(&imp, &site, live.as_ref()).json(),
        Err(problems) => shadow::unmappable(problems.0.len()),
    };
    Ok(report.to_string())
}
