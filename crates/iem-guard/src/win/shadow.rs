//! The shadow import's effects (S8 lane 4, #11; design note §3.5):
//! `pc.toml`'s `shadow` command (`iem-migrate shadow`, which writes
//! nothing) in the active bundle's folder, bounded by `shadow::LIMIT`, and
//! its line appended to `<root>\shadow\history.jsonl`. The decisions (where
//! it runs, how it ended, the line, that it never fails an entry) are
//! `crate::shadow`'s (mutated); `win::tests` runs these effects on the
//! Windows runner.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::Path;
use std::process::Command;

use tracing::warn;

use super::WinPc;
use super::procs::{self, OnCancel};
use crate::cancel::Cancel;
use crate::effects::argv;
use crate::pc::{R, StepError};
use crate::plan::Mode;
use crate::shadow::{self, Run};
use crate::site;

/// The command with `{root}`, `{bundle}`, `{site}`, `{server_config}` and
/// `{project}` (`[guard] reaper_project`), run in the bundle's folder. A
/// wait: "ide event", or the limit, sends Ctrl-Break to its own process
/// group and ends the wait at once; nothing is force-ended.
fn run(pc: &WinPc, c: &Cancel) -> Run {
    let dir = match pc.bundle_dir() {
        Ok(dir) => dir,
        Err(e) => return Run::Failed(e.to_string()),
    };
    let mut vars = pc.s.vars(&dir);
    vars.push((
        "project",
        pc.s.guard.reaper_project.to_string_lossy().into_owned(),
    ));
    let command = match argv::expand(&pc.s.pc.shadow, &vars) {
        Ok(command) => command,
        Err(why) => return Run::Failed(why),
    };
    let Some((exe, args)) = command.split_first() else {
        return Run::Failed("pc.toml has no shadow command".to_owned());
    };
    let mut cmd = Command::new(exe);
    cmd.args(args).current_dir(&dir);
    let out = procs::run(
        site::file_name(exe),
        &mut cmd,
        shadow::LIMIT,
        c,
        OnCancel::Break,
    );
    shadow::ended(out.map(|o| (o.code, o.stdout, o.stderr)))
}

/// Appends one line (the folder made when missing).
fn append(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(format!("{line}\n").as_bytes())
}

/// One shadow import of an entry into `to` (`Pc::shadow`).
pub(super) fn shadow(pc: &WinPc, to: Mode, c: &Cancel) -> R<String> {
    let run = run(pc, c);
    let preempted = run == Run::Preempted;
    let (line, mut said) = shadow::record(procs::now_ms(), to, pc.bundle.as_deref(), run);
    let history = pc.s.shadow_history();
    if let Err(e) = append(&history, &line) {
        warn!("the shadow history {}: {e}", history.display());
        said.push_str(&format!("; the history could not be written ({e})"));
    }
    // The runner's report logs `said` (`Guard::info`).
    if preempted {
        return Err(StepError::Preempted);
    }
    Ok(said)
}
