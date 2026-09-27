//! The band's address (design §5.2 step 8, §6): plain HTTP on this PC
//! through `ureq`, HTTPS through Windows' own `curl.exe` (schannel with the
//! system's roots; the guard carries no TLS stack), the owners of ports
//! 80/443, and the identity check of a dev/live entry.

use std::process::Command;
use std::time::Duration;

use iem_win::process;

use super::WinPc;
use super::procs::{self, OnCancel};
use crate::cancel::Cancel;
use crate::effects::{self, web as decide};
use crate::pc::{Ports, R, StepError};

/// curl's own bound for one HTTPS request.
const CURL_MAX_S: u32 = 10;
/// The server needs a moment to bind and the tunnel to reconnect.
const IDENTITY_LIMIT: Duration = Duration::from_secs(90);

/// A plain-HTTP GET on this PC: its status and body.
pub(super) fn get(pc: &WinPc, url: &str) -> Result<(u16, String), String> {
    let mut resp = pc
        .http
        .get(url)
        .call()
        .map_err(|e| format!("GET {url}: {e}"))?;
    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("GET {url}: {e}"))?;
    Ok((status, body))
}

/// An HTTPS GET through curl: the body, or why not. A wait: "ide event"
/// asks curl to stop (Ctrl-Break) and returns at once.
pub(super) fn get_tls(host: &str, path: &str, local: bool, c: &Cancel) -> R<String> {
    let mut cmd = Command::new(procs::system_exe("curl.exe"));
    cmd.args(decide::curl_args(host, path, local, CURL_MAX_S));
    let limit = Duration::from_secs(u64::from(CURL_MAX_S) + 5);
    let out = procs::run("curl", &mut cmd, limit, c, OnCancel::Break)?;
    if out.code == Some(0) {
        Ok(out.stdout)
    } else {
        Err(StepError::failed(format!(
            "{} ended with {:?}: {}",
            decide::https_url(host, path),
            out.code,
            effects::tail(&out.stderr, 200)
        )))
    }
}

/// The listening pids of ports 80 and 443.
pub(super) fn ports() -> R<Ports> {
    let http = process::listening(80).map_err(|e| procs::failed("port 80's owner", e))?;
    let https = process::listening(443).map_err(|e| procs::failed("port 443's owner", e))?;
    Ok((http, https))
}

/// LAN 80/443 and the public host answer `/api/version` with `sha`; the
/// tunnel has a ready connection. Checked until all hold or 90 s passed.
pub(super) fn identity(pc: &WinPc, sha: &str, c: &Cancel) -> R<()> {
    let mut problems = Vec::new();
    let named = procs::poll(IDENTITY_LIMIT, c, || {
        problems = identity_problems(pc, sha, c)?;
        Ok(problems.is_empty())
    })?;
    if named {
        Ok(())
    } else {
        Err(StepError::failed(problems.join("; ")))
    }
}

fn identity_problems(pc: &WinPc, sha: &str, c: &Cancel) -> R<Vec<String>> {
    let host = &pc.s.guard.public_host;
    let mut bad = Vec::new();
    match get(pc, &decide::local_url("/api/version")) {
        Ok((status, body)) if decide::is_success(status) => {
            if let Err(e) = decide::version_matches(&body, sha) {
                bad.push(format!("LAN 80: {e}"));
            }
        }
        Ok((status, _)) => bad.push(format!("LAN 80: HTTP {status}")),
        Err(e) => bad.push(format!("LAN 80: {e}")),
    }
    for (label, local) in [("LAN 443", true), ("public host", false)] {
        match get_tls(host, "/api/version", local, c) {
            Ok(body) => {
                if let Err(e) = decide::version_matches(&body, sha) {
                    bad.push(format!("{label}: {e}"));
                }
            }
            Err(StepError::Preempted) => return Err(StepError::Preempted),
            Err(StepError::Failed(why)) => bad.push(format!("{label}: {why}")),
        }
    }
    match get(pc, &pc.s.pc.tunnel_ready) {
        Ok((_, body)) => {
            if decide::ready_connections(&body) == 0 {
                bad.push("tunnel: no ready connection".to_owned());
            }
        }
        Err(e) => bad.push(format!("tunnel: {e}")),
    }
    Ok(bad)
}
