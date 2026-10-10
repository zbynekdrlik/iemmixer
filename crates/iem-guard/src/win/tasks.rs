//! Our scheduled tasks (design §5.1) through `schtasks.exe` (arguments
//! only), the elevated tuning task and its drift check, the S1c
//! fingerprint, and the data refresh of an entry.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use iem_win::power;
use iem_win::registry::{Hklm, Kind};
use tracing::{info, warn};

use super::WinPc;
use super::procs::{self, OnCancel, Output};
use crate::bundle;
use crate::cancel::Cancel;
use crate::effects::data::refresh;
use crate::effects::tasks::{self, Probe, ProbeWatch, Row};
use crate::effects::{self, argv, tuning};
use crate::pc::{R, StepError};
use crate::plan::Mode;
use crate::site;
use crate::state;

/// The tuning task answers within this.
const TUNING_LIMIT: Duration = Duration::from_secs(120);
/// One data command (`iem-migrate band`, …) reads small files and writes
/// the guard's own data: it finishes within this. A started one is not
/// pre-empted (a mutation), so this bound is also the longest "ide event"
/// can wait on the refresh.
const DATA_LIMIT: Duration = Duration::from_secs(120);
/// The probe task's `cmd /c exit 0` ends within this.
const PROBE_LIMIT: Duration = Duration::from_secs(15);
/// The exclude task re-sums the bundle, then adds the exclusions.
const EXCLUDE_LIMIT: Duration = Duration::from_secs(120);

fn schtasks(args: &[&str]) -> R<Output> {
    let mut cmd = Command::new(procs::system_exe("schtasks.exe"));
    cmd.args(args);
    procs::run(
        "schtasks",
        &mut cmd,
        Duration::from_secs(30),
        &Cancel::default(),
        OnCancel::Finish,
    )
}

/// Starts one of our tasks.
pub(super) fn run_task(task: &str) -> R<()> {
    let out = schtasks(&tasks::run_args(task))?;
    if out.code == Some(0) {
        info!("started the task {task}");
        Ok(())
    } else {
        Err(StepError::failed(format!(
            "schtasks /Run {task} ended with {:?}: {}",
            out.code,
            effects::tail(&out.stderr, 300)
        )))
    }
}

/// The task's row of `schtasks /Query` (`None`: no row for it).
fn query(task: &str) -> R<Option<Row>> {
    let out = schtasks(&tasks::query_args(task))?;
    if out.code == Some(0) {
        Ok(tasks::row(&out.stdout, task))
    } else {
        Err(StepError::failed(format!(
            "schtasks /Query {task} ended with {:?}: {}",
            out.code,
            effects::tail(&out.stderr, 300)
        )))
    }
}

/// The guard (Limited, session 1) starts `\iemmixer\iemmixer-probe`, which
/// must end with 0 (design §5.1). The row read before `/Run` tells this
/// run's result from the previous one's.
pub(super) fn probe() -> R<()> {
    let mut watch = ProbeWatch::new(query(tasks::PROBE)?.as_ref());
    run_task(tasks::PROBE)?;
    let mut verdict = Probe::Wait;
    procs::poll(PROBE_LIMIT, &Cancel::default(), || {
        verdict = watch.observe(query(tasks::PROBE)?.as_ref());
        Ok(verdict != Probe::Wait)
    })?;
    match verdict {
        Probe::Passed => Ok(()),
        Probe::Failed(why) => Err(StepError::Failed(why)),
        Probe::Wait => Err(StepError::failed(format!(
            "the probe task did not end within {} s",
            PROBE_LIMIT.as_secs()
        ))),
    }
}

/// One request to an elevated task (design §5.1; `IemPc.psm1`
/// `Invoke-IemTaskRequest`): the request file in the user's root, the task,
/// then its answer with our id in the elevated root. The task changes the
/// PC, so its answer is awaited without the token (a mutation finishes
/// first).
pub(super) fn elevated(
    pc: &WinPc,
    kind: &str,
    task: &str,
    id: &str,
    request: &str,
    limit: Duration,
) -> R<String> {
    let requests = pc.s.requests_dir();
    fs::create_dir_all(&requests).map_err(|e| procs::failed("the requests directory", e))?;
    state::write_atomic(
        &requests.join(tuning::request_name(kind)),
        request.as_bytes(),
    )
    .map_err(|e| procs::failed(&format!("the {kind} request"), e))?;
    run_task(task)?;
    let result = pc.s.results_dir().join(tuning::result_name(kind));
    let mut answer = None;
    procs::poll(limit, &Cancel::default(), || {
        answer = fs::read_to_string(&result)
            .ok()
            .and_then(|text| tuning::result_for(&text, kind, id));
        Ok(answer.is_some())
    })?;
    match answer {
        Some(Ok(detail)) => Ok(detail),
        Some(Err(why)) => Err(StepError::failed(format!("the {kind} task: {why}"))),
        None => Err(StepError::failed(format!(
            "the {kind} task did not answer within {} s",
            limit.as_secs()
        ))),
    }
}

/// The Defender process exclusions of the verified bundle `sha` through
/// `\iemmixer\iemmixer-exclude` (design §5.1): the task re-verifies the
/// bundle against its sums; `keep`'s exclusions stay, every other bundle's
/// go.
pub(super) fn exclude(pc: &WinPc, sha: &str, keep: &[String]) -> R<()> {
    if !bundle::valid_sha(sha) || !keep.iter().all(|k| bundle::valid_sha(k)) {
        return Err(StepError::failed(format!(
            "not a bundle SHA: {sha:?} (keep {keep:?})"
        )));
    }
    let id = format!("{}-exclude", procs::now_ms());
    let request = tuning::exclude_request(&id, sha, keep);
    let detail = elevated(
        pc,
        tuning::EXCLUDE,
        tasks::EXCLUDE,
        &id,
        &request,
        EXCLUDE_LIMIT,
    )?;
    info!("Defender exclusions for {sha}: {detail}");
    Ok(())
}

/// One verb through the elevated tuning task; "absent" while the bundle
/// ships no tuning module (S1c).
pub(super) fn tuning(pc: &WinPc, verb: &str) -> R<String> {
    if !tuning::valid_verb(verb) {
        return Err(StepError::failed(format!("not a tuning verb: {verb:?}")));
    }
    if !pc.bundle_dir()?.join(bundle::TUNING_DIR).is_dir() {
        warn!("the bundle has no tuning module: tuning {verb} is reported absent");
        return Ok(tuning::ABSENT.to_owned());
    }
    let id = format!("{}-{verb}", procs::now_ms());
    let request = tuning::tuning_request(&id, verb);
    let detail = elevated(
        pc,
        tuning::TUNING,
        tasks::TUNING,
        &id,
        &request,
        TUNING_LIMIT,
    )?;
    info!("tuning {verb}: {detail}");
    Ok(detail)
}

/// S1c's REAPER-mode fingerprint through the tuning task (`state`); a
/// bundle without the module is reported, never fatal.
pub(super) fn fingerprint(pc: &WinPc) -> R<()> {
    if tuning(pc, "state")? == tuning::ABSENT {
        warn!("no tuning module: the REAPER-mode fingerprint is not checked");
    }
    Ok(())
}

fn service_start(name: &str) -> Result<u32, String> {
    let key = format!(r"SYSTEM\CurrentControlSet\Services\{name}");
    match Hklm::read(&key, "Start") {
        Ok((Kind::Dword, raw)) => raw.parse().map_err(|e| format!("Start {raw:?}: {e}")),
        Ok((kind, raw)) => Err(format!("Start is {kind:?} {raw:?}")),
        Err(e) => Err(e.to_string()),
    }
}

/// The tuning module's record against native reads (P10): the active power
/// plan and the recorded services' start types. No record, no drift.
pub(super) fn drift(pc: &WinPc) -> R<Option<String>> {
    let path = pc.s.tuning_record();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(procs::failed(&path.display().to_string(), e)),
    };
    let expect = tuning::parse_expect(&text).map_err(StepError::Failed)?;
    let plan = power::active_scheme().map_err(|e| procs::failed("the active power plan", e))?;
    let services: BTreeMap<String, Result<u32, String>> = expect
        .services
        .keys()
        .map(|name| (name.clone(), service_start(name)))
        .collect();
    Ok(tuning::drift(&expect, &plan, &services))
}

/// The data refresh of an entry into `mode` (P9): `pc.toml`'s commands in
/// order, each with exit 0 (`effects::data::refresh`). A started command
/// writes the guard's own data, so it finishes; "ide event" stops the
/// refresh between two commands and after the last.
pub(super) fn data(pc: &WinPc, mode: Mode, c: &Cancel) -> R<String> {
    let dir = pc.bundle_dir()?;
    let vars = pc.s.vars(&dir);
    refresh(mode, pc.s.data_commands(mode), c, |template| {
        command(&dir, template, &vars, c)
    })
}

/// One `pc.toml` command (a data refresh's) with its
/// placeholders, run in the bundle's folder within [`DATA_LIMIT`]; it
/// finishes once started (a mutation) and must exit 0. What it printed.
pub(super) fn command(
    dir: &Path,
    template: &[String],
    vars: &[(&str, String)],
    c: &Cancel,
) -> R<String> {
    let command = argv::expand(template, vars).map_err(StepError::Failed)?;
    let Some((exe, args)) = command.split_first() else {
        return Err(StepError::failed("pc.toml has an empty data command"));
    };
    let mut cmd = Command::new(exe);
    cmd.args(args).current_dir(dir);
    let out = procs::run(exe, &mut cmd, DATA_LIMIT, c, OnCancel::Finish)?;
    let name = site::file_name(exe);
    if out.code != Some(0) {
        return Err(StepError::failed(format!(
            "{name} ended with {:?}: {}",
            out.code,
            effects::tail(&out.stderr, 300)
        )));
    }
    Ok(format!("{name}: {}", effects::tail(&out.stdout, 200)))
}
