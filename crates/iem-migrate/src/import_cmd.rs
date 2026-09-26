//! `iem-migrate import` (design note §3.2): the saved project (and the
//! newest backup JSON, cross-checked) → the engine's `current.json` and
//! `baseline.json`, only when the project's topology equals `site.toml`.

use std::time::{SystemTime, UNIX_EPOCH};

use iem_core::MixerBackup;
use iem_engine::core::{reconcile, to_mix};
use iem_engine::persist::{Persisted, Store};
use iem_rpp::aliases::parse_aliases;
use iem_rpp::backup::cross_check;
use iem_rpp::import::{compare, import};
use iem_rpp::legacy::LegacyProject;

use crate::args::parse;
use crate::{EXIT_TOPOLOGY, Failure, read_text, site};

/// A value the engine's caps change by more than this fails the import.
/// REAPER stores +12 dB as 3.981072, 7·10⁻⁷ dB over the fader cap: the
/// engine caps it on load and nobody can hear it.
pub const CAP_TOLERANCE_DB: f64 = 1e-5;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

pub fn run(args: &[String]) -> Result<String, Failure> {
    let a = parse(
        args,
        &[
            "--rpp",
            "--aliases",
            "--site",
            "--backup",
            "--state-dir",
            "--emit-topology",
            "--expect",
        ],
        &["--dry-run"],
    )?;
    let dry = a.flag("--dry-run");
    let rpp = a.path("--rpp")?;
    let state_dir = a.opt_path("--state-dir");
    if !dry && state_dir.is_none() {
        return Err(Failure::input("--state-dir is required unless --dry-run"));
    }
    let aliases = parse_aliases(&read_text(&a.path("--aliases")?)?).map_err(Failure::input)?;
    let site = site::open(&a.path("--site")?)?;
    let project = LegacyProject::parse(&read_text(&rpp)?)
        .map_err(|e| Failure::input(format!("{}: {e}", rpp.display())))?;
    let imp = import(&project, &aliases).map_err(|p| Failure::input(p.to_string()))?;
    let mut report = vec![
        format!("import of {}", rpp.display()),
        format!("counts: {}", imp.counts),
    ];
    report.extend(imp.notes.iter().map(|n| format!("note: {n}")));
    if let Some(path) = a.opt_path("--emit-topology") {
        std::fs::write(&path, imp.topology.engine_toml(site.site.channels))
            .map_err(|e| Failure::io(format!("{}: {e}", path.display())))?;
        report.push(format!("topology written to {}", path.display()));
    }
    if let Some(expect) = a.opt("--expect") {
        imp.counts.check(expect).map_err(Failure::input)?;
        report.push("counts match --expect".into());
    }
    if let Some(path) = a.opt_path("--backup") {
        let backup: MixerBackup = serde_json::from_str(&read_text(&path)?)
            .map_err(|e| Failure::input(format!("{}: {e}", path.display())))?;
        let check = cross_check(&backup, &imp, &aliases)
            .map_err(|p| Failure::input(format!("{}: {p}", path.display())))?;
        report.push(format!(
            "backup {} ({}): {} values compared, {} differ (the project's values are kept)",
            path.display(),
            backup.timestamp,
            check.compared,
            check.differing.len()
        ));
        report.extend(check.differing.iter().map(|d| format!("  differs: {d}")));
    }
    let diff = imp.topology.diff(&site.topology);
    if !diff.is_empty() {
        report.push(format!(
            "the project's topology differs from site.toml in {} place(s); nothing written",
            diff.len()
        ));
        report.extend(diff.iter().map(|d| format!("  {d}")));
        return Err(Failure {
            code: EXIT_TOPOLOGY,
            msg: report.join("\n"),
        });
    }
    let (reconciled, dropped) = reconcile(&site.graph, &imp.state);
    let state = to_mix(&site.graph, &reconciled);
    let capped = compare(&imp.topology, &state, &imp.state, CAP_TOLERANCE_DB);
    if !dropped.is_empty() || !capped.is_empty() {
        let lines: Vec<String> = dropped
            .iter()
            .map(|d| format!("  not in site.toml: {d}"))
            .chain(
                capped
                    .iter()
                    .map(|c| format!("  outside the engine's caps: {c}")),
            )
            .collect();
        return Err(Failure::input(format!(
            "{}\nthe state does not fit the engine:\n{}",
            report.join("\n"),
            lines.join("\n")
        )));
    }
    let Some(dir) = state_dir.filter(|_| !dry) else {
        report.push("dry run: nothing written".into());
        return Ok(report.join("\n"));
    };
    let store = Store::open(&dir).map_err(|e| Failure::io(format!("{}: {e}", dir.display())))?;
    let persisted = Persisted {
        rev: 0,
        topology_hash: site.graph.hash.clone(),
        saved_unix_ms: now_ms(),
        state,
        ..Persisted::default()
    };
    let io = |e: std::io::Error| Failure::io(format!("{}: {e}", dir.display()));
    store.save(&persisted).map_err(io)?;
    store.save_baseline(&persisted).map_err(io)?;
    report.push(format!(
        "state written to {} (current.json, baseline.json)",
        dir.display()
    ));
    Ok(report.join("\n"))
}
