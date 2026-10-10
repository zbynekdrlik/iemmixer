//! The cutover's effects (S8 lane 2, design note §3.2): the elevated
//! cutover task (`\iemmixer\iemmixer-cutover`, `IemCutover.psm1`: the
//! predecessor's autostarts exported, disabled and re-enabled, the guard
//! task's logon trigger), and the server's config read and replaced whole.
//! The decisions are `crate::cutover`'s (mutated).

use std::fs;
use std::io;
use std::time::Duration;

use tracing::info;

use super::WinPc;
use super::procs;
use super::tasks::elevated;
use crate::cutover::{self, Verb};
use crate::pc::{R, StepError};
use crate::state;

/// The task reads back every task and value it changes; a few seconds, the
/// bound of the exclude task.
const LIMIT: Duration = Duration::from_secs(120);

/// One verb through the cutover task; the export only as `cutover`
/// names it (the task refuses anything else too).
pub(super) fn task(pc: &WinPc, verb: Verb, export: Option<&str>) -> R<String> {
    if export.is_some_and(|e| !cutover::valid_export(e)) {
        return Err(StepError::failed(format!(
            "not an autostart export: {export:?}"
        )));
    }
    let id = format!("{}-{}", procs::now_ms(), verb.as_str());
    let request = cutover::request(&id, verb, export);
    let detail = elevated(pc, cutover::KIND, cutover::TASK, &id, &request, LIMIT)?;
    info!("the cutover task, {}: {detail}", verb.as_str());
    Ok(detail)
}

/// The autostart exports in `<elevated root>\cutover` never restored (S8
/// lane 5): each folder, whether it holds `export.json` and the restore
/// mark; `cutover::unrestored` decides. No cutover folder: none.
pub(super) fn unrestored_exports(pc: &WinPc) -> R<Vec<u64>> {
    let dir = pc.s.pc.elevated_root.join("cutover");
    let name = dir.display().to_string();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(procs::failed(&name, e)),
    };
    let mut seen = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| procs::failed(&name, e))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        seen.push(cutover::ExportSeen {
            name: entry.file_name().to_string_lossy().into_owned(),
            saved: path.join(cutover::EXPORT_FILE).is_file(),
            restored: path.join(cutover::RESTORED_FILE).exists(),
        });
    }
    let unrestored = cutover::unrestored(&seen);
    info!("autostart exports never restored: {unrestored:?}");
    Ok(unrestored)
}

/// The server's config as text.
pub(super) fn server_config(pc: &WinPc) -> R<String> {
    let path = &pc.s.pc.server_config;
    fs::read_to_string(path).map_err(|e| procs::failed(&path.display().to_string(), e))
}

/// The server's config replaced whole (a temp file renamed over it), read
/// back byte for byte.
pub(super) fn write_server_config(pc: &WinPc, text: &str) -> R<()> {
    let path = &pc.s.pc.server_config;
    let name = path.display().to_string();
    state::write_atomic(path, text.as_bytes()).map_err(|e| procs::failed(&name, e))?;
    let back = fs::read(path).map_err(|e| procs::failed(&name, e))?;
    if back != text.as_bytes() {
        return Err(StepError::failed(format!("{name} does not read back")));
    }
    info!("{name} written and read back");
    Ok(())
}
