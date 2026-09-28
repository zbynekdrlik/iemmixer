//! The predecessor app (design §5.3): its exit through the tray menu's own
//! Exit command, its start, its answers, and the precheck of an entry.

use std::fs::{self, File, Metadata};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use iem_win::process::{self, Handle};
use iem_win::window;
use tracing::{info, warn};

use super::procs::{self, OnCancel};
use super::{WinPc, tasks, web};
use crate::bundle;
use crate::cancel::Cancel;
use crate::effects::{app as decide, tasks as task_names, web as http};
use crate::handover::{self, AppExit};
use crate::pc::{Kid, PrecheckFacts, R, StepError, foreign_engine, precheck as verdict};
use crate::plan::Mode;
use crate::site::{ENGINE_EXE, SERVER_EXE};

/// The app's data directory is walked this deep for temp files.
const TEMP_DEPTH: u32 = 4;
/// And for at most this many files.
const TEMP_FILES: usize = 10_000;

fn modified_ms(meta: &Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Every file below `dir` with its modification time (ms). A missing `dir`
/// fails; an unreadable subdirectory is skipped.
fn files_under(dir: &Path) -> R<Vec<(String, u64)>> {
    if !dir.is_dir() {
        return Err(StepError::failed(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    let mut out = Vec::new();
    let mut todo: Vec<(PathBuf, u32)> = vec![(dir.to_path_buf(), 0)];
    while let Some((d, depth)) = todo.pop() {
        let entries = match fs::read_dir(&d) {
            Ok(entries) => entries,
            Err(e) => {
                warn!("{}: {e}", d.display());
                continue;
            }
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                if depth < TEMP_DEPTH {
                    todo.push((entry.path(), depth + 1));
                }
            } else {
                out.push((
                    entry.file_name().to_string_lossy().into_owned(),
                    modified_ms(&meta),
                ));
            }
            if out.len() >= TEMP_FILES {
                return Ok(out);
            }
        }
    }
    Ok(out)
}

/// Whether the app's newest log file has the exit line at or after the
/// command: corroboration only (its buffered logger may never write it).
fn log_says(dir: &Path, line: &str, after_ms: u64) -> bool {
    let files: Vec<(String, u64)> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .filter_map(|e| {
                let meta = e.metadata().ok()?;
                if meta.is_file() {
                    Some((
                        e.file_name().to_string_lossy().into_owned(),
                        modified_ms(&meta),
                    ))
                } else {
                    None
                }
            })
            .collect(),
        Err(e) => {
            warn!("the app's log directory: {e}");
            return false;
        }
    };
    let Some(newest) = decide::newest(&files) else {
        return false;
    };
    match fs::read(dir.join(newest)) {
        Ok(bytes) => decide::logged_after(&String::from_utf8_lossy(&bytes), line, after_ms),
        Err(e) => {
            warn!("the app's log {newest}: {e}");
            false
        }
    }
}

/// One app, its tray window, a handle opened before the command, the tray
/// menu's Exit command, then ≤ 30 s for the exit, the ports, temp files and
/// the log line. The verdict is `handover::app_exit`.
pub(super) fn stop(pc: &WinPc, c: &Cancel) -> R<AppExit> {
    let g = &pc.s.guard;
    let pid =
        decide::one_pid(&procs::list(pc).app, "the predecessor app").map_err(StepError::Failed)?;
    let hwnd = window::find_owned(&g.app_tray_class, pid)
        .map_err(|e| procs::failed("the app's windows", e))?
        .ok_or_else(|| StepError::failed("the app has no window of the tray's class"))?;
    // The handle first: a recycled pid can never answer for the app.
    let handle = Handle::open_waitable(pid).map_err(|e| procs::failed("the app", e))?;
    let posted = procs::now_ms();
    window::post_command(hwnd, g.app_exit_id)
        .map_err(|e| procs::failed("the tray's Exit command", e))?;
    info!("posted the tray's Exit command to the app (pid {pid})");
    let exit_code = procs::wait_exit(&handle, Duration::from_secs(30), c)?;
    let (http_pid, https_pid) = web::ports()?;
    let newer_temp = decide::newer_temp(&files_under(&g.app_data_dir)?, posted);
    let logged = log_says(&g.app_log_dir, &g.app_exit_line, posted);
    Ok(AppExit {
        exit_code,
        ports_free: http_pid.is_none() && https_pid.is_none(),
        newer_temp,
        logged,
    })
}

/// Our task (its exe directly), or the direct start.
pub(super) fn start(pc: &WinPc) -> R<()> {
    if pc.s.guard.start_direct {
        procs::start_detached(&pc.s.pc.app_exe)
    } else {
        tasks::run_task(task_names::APP)
    }
}

/// `/api/version`, the member count and the public host, ≤ 60 s.
pub(super) fn answers(pc: &WinPc, c: &Cancel) -> R<()> {
    let mut problems = Vec::new();
    let answered = procs::poll(Duration::from_secs(60), c, || {
        problems = app_problems(pc, c)?;
        Ok(problems.is_empty())
    })?;
    if answered {
        Ok(())
    } else {
        Err(StepError::failed(problems.join("; ")))
    }
}

fn app_problems(pc: &WinPc, c: &Cancel) -> R<Vec<String>> {
    let g = &pc.s.guard;
    let mut bad = Vec::new();
    match web::get(pc, &http::local_url("/api/version")) {
        Ok((status, _)) if http::is_success(status) => {}
        Ok((status, _)) => bad.push(format!("/api/version: HTTP {status}")),
        Err(e) => bad.push(e),
    }
    match web::get(pc, &http::local_url("/api/members")) {
        Ok((status, body)) if http::is_success(status) => {
            bad.extend(http::members_problem(&body, g.app_members));
        }
        Ok((status, _)) => bad.push(format!("/api/members: HTTP {status}")),
        Err(e) => bad.push(e),
    }
    match web::get_tls(&g.public_host, "/api/version", false, c) {
        Ok(_) => {}
        Err(StepError::Preempted) => return Err(StepError::Preempted),
        Err(StepError::Failed(why)) => bad.push(format!("public host: {why}")),
    }
    Ok(bad)
}

fn exe_sha256(path: &Path) -> Result<String, String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    bundle::sha256_read(file).map_err(|e| format!("{}: {e}", path.display()))
}

/// `iem-server notify --count alarm` from the bundle.
fn recipients(pc: &WinPc) -> Option<u32> {
    let exe = pc.bundle_dir().ok()?.join(SERVER_EXE);
    let mut cmd = Command::new(exe);
    cmd.args(["notify", "--count", "alarm"])
        .env("IEMMIXER_CONFIG", &pc.s.pc.server_config);
    match procs::run(
        "iem-server notify --count",
        &mut cmd,
        Duration::from_secs(30),
        &Cancel::default(),
        OnCancel::Finish,
    ) {
        Ok(out) => http::recipients(out.code, &out.stdout),
        Err(e) => {
            warn!("the alarm recipients: {e}");
            None
        }
    }
}

/// Bundle installed, `pc_tests_passed` (trial), ≥ 1 alarm recipient (live
/// and trials; dev only names a missing one), no foreign engine, the
/// predecessor's exe as recorded (design §5.2 step 1). The hash is of
/// `pc.toml app_exe`, and a running app must have been started from that
/// file, so the hash is the running binary's.
pub(super) fn precheck(pc: &WinPc, to: Mode, trial: bool) -> R<Option<String>> {
    let bundle = pc.bundle_dir().is_ok_and(|d| d.join(ENGINE_EXE).is_file());
    let running = procs::list(pc);
    let images: Vec<Result<String, String>> = running
        .app
        .iter()
        .map(|&pid| process::image_path(pid).map_err(|e| e.to_string()))
        .collect();
    let app_exe = &pc.s.pc.app_exe;
    let app_binary = decide::running_from(&app_exe.to_string_lossy(), &images)
        .and_then(|()| exe_sha256(app_exe))
        .and_then(|now| handover::app_binary(&pc.s.guard.app_exe_sha256, &now));
    verdict(&PrecheckFacts {
        to,
        trial,
        bundle,
        pc_tests_passed: pc.s.guard.pc_tests_passed,
        recipients: recipients(pc),
        foreign_engine: foreign_engine(&running.engine, pc.kids.pid(Kid::Engine)),
        app_binary,
    })
}
