//! The cutover's effects (S8 lane 2, design note §3.2): the elevated
//! cutover task (`\iemmixer\iemmixer-cutover`, `IemCutover.psm1`: the
//! predecessor's autostarts exported, disabled and re-enabled, the guard
//! task's logon trigger), and the server's config read and replaced whole.
//! The decisions are `crate::cutover`'s (mutated).

use std::fs;
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
