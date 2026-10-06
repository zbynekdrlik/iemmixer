//! `iem-migrate import` (S4 design note §3.2, #20 design note §7): the saved
//! project (and the newest backup JSON, cross-checked) → the engine's
//! `current.json` and `baseline.json`, only when the project's topology
//! equals `site.toml`.

use std::time::{SystemTime, UNIX_EPOCH};

use iem_core::legacy::MixerBackup;
use iem_engine::core::{reconcile, to_state};
use iem_engine::engine::STATE_WAIT;
use iem_engine::persist::{Persisted, Source, Store};
use iem_engine::topology::Topology;
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
        &["--dry-run", "--seed-if-absent"],
    )?;
    let dry = a.flag("--dry-run");
    // `--seed-if-absent` writes baseline.json but keeps an existing
    // current.json: a data command runs on every dev entry, and a re-seed
    // must never wipe the band's live changes (iemmixer#9 2026-09-28).
    let seed_if_absent = a.flag("--seed-if-absent");
    let rpp = a.path("--rpp")?;
    let state_dir = a.opt_path("--state-dir");
    if !dry && state_dir.is_none() {
        return Err(Failure::input("--state-dir is required unless --dry-run"));
    }
    let aliases = parse_aliases(&read_text(&a.path("--aliases")?)?).map_err(Failure::input)?;
    let site = site::open_optional(&a.path("--site")?)?;
    let project = LegacyProject::parse(&read_text(&rpp)?)
        .map_err(|e| Failure::input(format!("{}: {e}", rpp.display())))?;
    let imp = import(&project, &aliases).map_err(|p| Failure::input(p.to_string()))?;
    let mut report = vec![
        format!("import of {}", rpp.display()),
        format!("counts: {}", imp.counts),
    ];
    report.extend(imp.notes.iter().map(|n| format!("note: {n}")));
    if let Some(path) = a.opt_path("--emit-topology") {
        let channels = site
            .as_ref()
            .map_or_else(|| imp.topology.max_channel(), |s| s.site.channels);
        std::fs::write(&path, imp.topology.engine_toml(channels))
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
        let check = cross_check(&backup, &imp)
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
    let Some(site) = site else {
        report.push(
            "site.toml has no [engine] table yet: the project's topology is the proposal \
             (--emit-topology writes it for the ops PR); nothing written"
                .into(),
        );
        return Err(Failure {
            code: EXIT_TOPOLOGY,
            msg: report.join("\n"),
        });
    };
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
    let (reconciled, dropped) = reconcile(&site.compiled, &imp.state);
    let state = to_state(&site.compiled, &reconciled);
    let capped = compare(
        &imp.topology,
        &imp.routing,
        &state,
        &imp.state,
        CAP_TOLERANCE_DB,
    );
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
    // The data step runs only after iemmixer stopped (#32): a running engine
    // holds engine.lock, and the import stops before it reads or writes a
    // state file. An engine that just ended may hold it a moment longer, so
    // the import waits as the engine does (minor-4). Held until the import
    // returns.
    let _state_lock = store.lock_within(STATE_WAIT).map_err(|e| {
        if e.kind() == std::io::ErrorKind::WouldBlock {
            Failure::io(format!(
                "{}: the state directory is in use by a running engine; stop it first",
                dir.display()
            ))
        } else {
            Failure::io(format!("{}: {e}", dir.display()))
        }
    })?;
    // `rev` stays at the default 0: an import starts a new revision count.
    let persisted = Persisted {
        topology_hash: site.compiled.hash.clone(),
        saved_unix_ms: now_ms(),
        state,
        ..Persisted::default()
    };
    let io = |e: std::io::Error| Failure::io(format!("{}: {e}", dir.display()));
    // Keep any existing live state (current.json, a generation, or save.tmp
    // left by a crash mid-save: `Store::live_state`); only seed current.json
    // when there is none. Looked at before anything is written: a directory
    // it cannot read refuses the seed whole (#32). The baseline goes through
    // its own temp file, so an interrupted save stays untouched. The report
    // names the file kept, as it is on disk (`Store::live_file`).
    let kept = if seed_if_absent {
        store.live_file().map_err(io)?
    } else {
        None
    };
    if kept.is_none() {
        report.extend(recover_before_save(&store, &site.compiled).map_err(|why| {
            Failure::io(format!(
                "{}: {why}; nothing written, the live state is as it was",
                dir.display()
            ))
        })?);
    }
    store.save_baseline(&persisted).map_err(io)?;
    if kept.is_none() {
        let committed = store.save(&persisted).map_err(io)?;
        // #32 MAJOR-1: a save.tmp that was no state is kept, never loaded.
        if let Some(aside) = committed.orphaned {
            report.push(format!("save.tmp moved aside to {}", aside.display()));
        }
    }
    let also = kept.map_or_else(
        || ", current.json".to_owned(),
        |file| format!("; {file} kept (--seed-if-absent)"),
    );
    report.push(format!(
        "state written to {} (baseline.json{also})",
        dir.display()
    ));
    Ok(report.join("\n"))
}

/// The engine's boot recovery before an import saves over the live state
/// (#32 P4): an interrupted save in `save.tmp` (the newest live state)
/// becomes `current.json` first, so the import's save turns it into a
/// generation instead of replacing it. Report lines, or why the import must
/// not go on: the load raised an alarm (a state file it cannot read, a
/// save.tmp it could not compare: #32 MAJOR-2; the engine boots past them
/// with an alarm, an import has nobody to hear one, so it stops before the
/// recovery or the save touch anything), or the interrupted save could not
/// be finished.
fn recover_before_save(store: &Store, topo: &Topology) -> Result<Vec<String>, String> {
    let loaded = store.load(topo);
    if !loaded.alarms.is_empty() {
        return Err(format!(
            "the import cannot use the state directory as it is: {}",
            loaded.alarms.join("; ")
        ));
    }
    let recovery = store.recover(&loaded);
    if loaded.source == Source::Interrupted && !recovery.finished {
        let why: Vec<String> = recovery
            .failed
            .into_iter()
            .chain(recovery.warnings)
            .collect();
        return Err(format!(
            "the interrupted save in save.tmp could not be finished ({})",
            why.join("; ")
        ));
    }
    let mut lines = Vec::new();
    if let Some(aside) = recovery.quarantined {
        lines.push(format!(
            "recovery: the damaged current.json moved aside to {}",
            aside.display()
        ));
    }
    if recovery.finished {
        lines.push("recovery: save.tmp finished as current.json".to_owned());
    }
    lines.extend(
        recovery
            .failed
            .into_iter()
            .chain(recovery.warnings)
            .map(|why| format!("recovery: {why}")),
    );
    Ok(lines)
}
