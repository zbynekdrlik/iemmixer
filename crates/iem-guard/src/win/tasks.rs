//! Our scheduled tasks (design §5.1) through `schtasks.exe` (arguments
//! only), the elevated tuning task and its drift check, the S1c
//! fingerprint, and the data refresh of an entry.

use std::collections::BTreeMap;
use std::fs;
use std::io;
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

/// The Defender process exclusions of the verified bundle `sha` through
/// `\iemmixer\iemmixer-exclude` (design §5.1): its request file names the
/// bundle, the task re-verifies it against its sums; its result must be 0.
pub(super) fn exclude(pc: &WinPc, sha: &str) -> R<()> {
    if !bundle::valid_sha(sha) {
        return Err(StepError::failed(format!("not a bundle SHA: {sha:?}")));
    }
    let dir = pc.s.guard_dir();
    fs::create_dir_all(&dir).map_err(|e| procs::failed("the guard directory", e))?;
    state::write_atomic(
        &dir.join(tasks::EXCLUDE_REQUEST),
        tasks::exclude_request(sha).as_bytes(),
    )
    .map_err(|e| procs::failed("the exclude request", e))?;
    let mut watch = ProbeWatch::new(query(tasks::EXCLUDE)?.as_ref());
    run_task(tasks::EXCLUDE)?;
    let mut verdict = Probe::Wait;
    procs::poll(EXCLUDE_LIMIT, &Cancel::default(), || {
        verdict = watch.observe(query(tasks::EXCLUDE)?.as_ref());
        Ok(verdict != Probe::Wait)
    })?;
    match verdict {
        Probe::Passed => {
            info!("Defender exclusions added for {sha}");
            Ok(())
        }
        Probe::Failed(why) => Err(StepError::failed(format!(
            "the exclude task for {sha} failed ({why})"
        ))),
        Probe::Wait => Err(StepError::failed(format!(
            "the exclude task did not end within {} s",
            EXCLUDE_LIMIT.as_secs()
        ))),
    }
}

/// One verb through the elevated tuning task: the request file, the task,
/// then its answer. The task changes the PC, so its answer is awaited
/// without the token (a mutation finishes first).
pub(super) fn tuning(pc: &WinPc, verb: &str) -> R<String> {
    if !tuning::valid_verb(verb) {
        return Err(StepError::failed(format!("not a tuning verb: {verb:?}")));
    }
    if !pc.bundle_dir()?.join(bundle::TUNING_DIR).is_dir() {
        warn!("the bundle has no tuning module: tuning {verb} is reported absent");
        return Ok(tuning::ABSENT.to_owned());
    }
    let dir = pc.s.tuning_dir();
    fs::create_dir_all(&dir).map_err(|e| procs::failed("the tuning directory", e))?;
    let id = format!("{}-{verb}", procs::now_ms());
    state::write_atomic(
        &dir.join(tuning::REQUEST),
        tuning::request(&id, verb).as_bytes(),
    )
    .map_err(|e| procs::failed("the tuning request", e))?;
    run_task(tasks::TUNING)?;
    let result = dir.join(tuning::RESULT);
    let mut answer = None;
    procs::poll(TUNING_LIMIT, &Cancel::default(), || {
        answer = fs::read_to_string(&result)
            .ok()
            .and_then(|text| tuning::result_for(&text, &id));
        Ok(answer.is_some())
    })?;
    match answer {
        Some(Ok(detail)) => {
            info!("tuning {verb}: {detail}");
            Ok(detail)
        }
        Some(Err(detail)) => Err(StepError::failed(format!("tuning {verb}: {detail}"))),
        None => Err(StepError::failed(format!(
            "tuning {verb}: no answer within {} s",
            TUNING_LIMIT.as_secs()
        ))),
    }
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
    let path = pc.s.tuning_dir().join(tuning::EXPECT);
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
        let command = argv::expand(template, &vars).map_err(StepError::Failed)?;
        let Some((exe, args)) = command.split_first() else {
            return Err(StepError::failed("pc.toml has an empty data command"));
        };
        let mut cmd = Command::new(exe);
        cmd.args(args).current_dir(&dir);
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
    })
}
