//! The rollback's file effects (S8 lane 3, design note §3.3): the band's
//! data exported into a new project beside the original
//! (`pc.toml`'s `rollback_export`, `iem-migrate export`), and the renames
//! that put the export in the project's place or the original back. The
//! decisions (the names, which renames, what they settle) are
//! `crate::rollback`'s (mutated).

use std::fs;
use std::path::Path;

use tracing::info;

use super::WinPc;
use super::procs;
use super::tasks;
use crate::cancel::Cancel;
use crate::pc::{R, StepError};
use crate::rollback::{self, Files, Placed, Want};

/// The project's path as `[guard]` names it.
fn project(pc: &WinPc) -> String {
    pc.s.guard.reaper_project.to_string_lossy().into_owned()
}

/// `rollback_export` with `{project}` and `{out}`, in the active bundle's
/// folder; then the new file must be there. Never pre-empted: a started
/// export finishes (it writes one new file).
pub(super) fn export(pc: &WinPc, at: u64) -> R<String> {
    let template = &pc.s.pc.rollback_export;
    if template.is_empty() {
        return Err(StepError::failed(
            "pc.toml has no rollback_export: the band's data cannot be exported",
        ));
    }
    let project = project(pc);
    let out = rollback::export_path(&project, at);
    let dir = pc.bundle_dir()?;
    let mut vars = pc.s.vars(&dir);
    vars.push(("project", project));
    vars.push(("out", out.clone()));
    let said = tasks::command(&dir, template, &vars, &Cancel::default())?;
    if !Path::new(&out).is_file() {
        return Err(StepError::failed(format!(
            "the export exited 0 but {out} is not there"
        )));
    }
    info!("the band's data exported to {out}: {said}");
    Ok(format!("exported to {out}: {said}"))
}

fn files(project: &str, export: &str, kept: &str) -> Files {
    let there = |p: &str| fs::symlink_metadata(p).is_ok();
    Files {
        project: there(project),
        export: there(export),
        kept: there(kept),
    }
}

/// The renames `rollback::moves` asks for, each target looked at again
/// right before it (a rename never replaces a file), then read back.
pub(super) fn swap(pc: &WinPc, want: Want, at: u64) -> R<Placed> {
    let project = project(pc);
    let export = rollback::export_path(&project, at);
    let kept = rollback::kept_path(&project, at);
    let before = files(&project, &export, &kept);
    for m in rollback::moves(want, before).map_err(StepError::Failed)? {
        let (from, to) = m.paths(&project, &export, &kept);
        if fs::symlink_metadata(to).is_ok() {
            return Err(StepError::failed(format!(
                "{to} exists: a rename never replaces a file"
            )));
        }
        fs::rename(from, to).map_err(|e| procs::failed(&format!("{from} -> {to}"), e))?;
        info!("renamed {from} -> {to}");
    }
    let after = files(&project, &export, &kept);
    rollback::placed(want, after).ok_or_else(|| {
        StepError::failed(format!(
            "the project files read back as {after:?}, not settled for {want:?}"
        ))
    })
}
