//! REAPER project (RPP) generator for the S1b golden renders. S4 adds the
//! importer and exporter to this crate.

pub mod bundle;
pub mod cases;
pub mod fx;
pub mod golden;
pub mod oracle;
pub mod project;
pub mod reaeq;
pub mod rpp;
pub mod stimulus;
pub mod wav;

use std::path::PathBuf;

/// `iem-rpp-gen --out DIR [--only FAMILY,…]` → one-line summary.
pub fn cli(args: &[String]) -> Result<String, String> {
    let mut out = None;
    let mut only = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--out" => out = it.next().map(PathBuf::from),
            "--only" => {
                only = it
                    .next()
                    .map(|s| s.split(',').map(str::to_owned).collect())
                    .unwrap_or_default()
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let out = out.ok_or_else(|| "usage: iem-rpp-gen --out DIR [--only FAMILY,...]".to_owned())?;
    let cat = cases::catalogue(&only).map_err(|e| e.to_string())?;
    let m = bundle::write_bundle(&cat, &out).map_err(|e| e.to_string())?;
    let cases: usize = m.projects.iter().map(|p| p.tracks.len()).sum();
    let bytes: u64 = m.files.iter().map(|f| f.bytes).sum();
    Ok(format!(
        "{} projects, {} cases, {} stimuli, {} bytes",
        m.projects.len(),
        cases,
        m.stimuli.len(),
        bytes
    ))
}
