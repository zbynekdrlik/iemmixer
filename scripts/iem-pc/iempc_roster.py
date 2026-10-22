"""`iempc roster-install --file PATH [--dry-run]` (#58 plan Task 14, design
§9): a roster file from this box onto the IEM PC, checked there by the
active bundle's own server, put beside the server config and followed until
the server applied it. The server polls `roster.toml` every 2 s and applies
a valid change live (`roster_watch.rs`), so no restart and no maintenance.

In this order (dev time: the EVENT-NOW flag at the start refuses it, design
§9 "a roster install is refused while EVENT-NOW exists"; locked):

0. Before any call: PC_SERVER_CONFIG (an optional key of the private env:
   the server config's full Windows path on the PC) and the file. An install
   (not a dry run) also refuses while a live run or a soak of this dev entry
   may still run (`iempc_live.refuse_while_running`): a reload closes every
   person's mixer socket with 4003, the soak's and the live run's included.
1. `iemmode status` (a read): the active bundle (the detail's `bundle
   <sha>`), whose fetch record on this box holds iem-server.exe's sha256.
2. The file by scp into PC_ROOT\\incoming\\roster-<sha12>.toml (a change).
3. One ssh call (a read): the upload's sha256 checked first; the active
   bundle's iem-server.exe read once from bundles\\<sha>, checked by the
   fetch record, staged admin-only and run from there (#15: the elevated
   session runs only admin-only copies of our programs, iempc_bin); then
   `iem-server roster check <upload>` with IEMMIXER_CONFIG = PC_SERVER_CONFIG.
   Its one JSON line `{ok, problems, sha256}` decides (`check_result`):
   problems install nothing (exit 1); `--dry-run` ends here (exit 0 when ok).
4. The flag again, then one ssh call (a change: a new flag lets it finish):
   the upload checked again, `roster.status.json` read as it stands
   (`before`), the upload copied beside the server config as
   `roster.toml.new`, checked, `Move-Item -Force` onto `roster.toml`,
   checked. Windows PowerShell's move deletes the old file first, so for an
   instant there is none: the server keeps the roster in use and reads the
   file at its next poll.
5. `roster.status.json` every POLL_S, at most WAIT_S, until it names the
   file's sha256 (`outcome`). `applied` true: in use (exit 0; `ok` false and
   `problems` name what failed after it went into use, an archive move). Else
   refused (exit 1), with the server's problems, among them its fallback
   line "in use instead: the roster applied last" while it runs on that one.
   The server writes no status for a roster it defers (an engine not synced,
   a switch a restart cut short) or for the roster it already uses, and none
   runs while the guard is in event: a wait that runs out is "not applied"
   (exit 1) and says what the status named. The same refusal as `before`
   (the same bytes refused earlier) is waited out, since the server reads
   the new file again; the same applied status as `before` is the roster in
   use already (`unchanged`).

It prints one JSON object `{"roster_install": {sha256, ok, problems,
applied, added, removed}}` (`dry_run`, `unchanged` when so): ids and counts
only, never a person's name (the server's problems name ids only, P6).
Exit 0 applied, or a dry run that checked ok; 1 refused or not applied.

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import hashlib
import json
import re
import time
from pathlib import Path

import iempc_bin
import iempc_live
import iempc_soak

SERVER = "iem-server.exe"
ROSTER = "roster.toml"
NEW = "roster.toml.new"
STATUS = "roster.status.json"
WAIT_S = 20.0
# A full Windows path of a file on a drive: no UNC, nothing a PowerShell
# string or a file name could not hold.
CONFIG_PATH = re.compile(r"[A-Za-z]:(?:\\[^\\/:*?\"<>|\x00-\x1f]+)+")
ID = re.compile(r"[A-Za-z0-9_-]{1,64}")
PROBLEM_CHARS = 400


def server_config(ip, env: dict[str, str]) -> tuple[str, str]:
    """PC_SERVER_CONFIG and its folder (the server's config directory)."""
    path = env.get("PC_SERVER_CONFIG", "")
    if not path:
        raise ip.Refused("roster-install: PC_SERVER_CONFIG missing in the private env (the server config's full "
                         "path on the PC)")
    if not CONFIG_PATH.fullmatch(path):
        raise ip.Refused("roster-install: PC_SERVER_CONFIG is not a full Windows path of a file on a drive")
    return path, path.rsplit("\\", 1)[0]


class CheckError(Exception):
    """An answer of the server's check that is not one (check_result, pure);
    run turns it into iempc's StepError."""


def check_result(code: int, out: str, sha: str) -> list[str]:
    """`iem-server roster check`'s problems (pure; empty: it may be
    installed); CheckError for an answer that is not the check's."""
    if code == 2:
        raise CheckError(f"the active bundle's {SERVER} does not know 'roster check' (exit 2): a bundle of the band "
                         "lineups plan must be active")
    lines = [line for line in out.splitlines() if line.strip()]
    try:
        doc = json.loads(lines[-1].lstrip("\ufeff")) if lines else None
    except ValueError:
        doc = None
    if not isinstance(doc, dict):
        raise CheckError(f"{SERVER} roster check (exit {code}) printed no JSON object: {out[-300:]!r}")
    problems = doc.get("problems")
    if not isinstance(problems, list) or not all(isinstance(p, str) for p in problems):
        raise CheckError(f"{SERVER} roster check (exit {code}): its problems are not a list of texts")
    ok = doc.get("ok")
    if code not in (0, 1) or ok is not (code == 0) or ok is not (not problems):
        raise CheckError(f"{SERVER} roster check's answer (ok {ok!r}, {len(problems)} problems) does not match its "
                         f"exit {code}")
    if doc.get("sha256") != sha:
        raise CheckError(f"{SERVER} roster check checked another file (sha256 {doc.get('sha256')!r}, this box sent "
                         f"{sha})")
    return problems


def parse_status(text) -> dict | None:
    """roster.status.json's text as a JSON object; None when absent or unreadable."""
    if not isinstance(text, str):
        return None
    try:
        doc = json.loads(text.lstrip("\ufeff"))
    except ValueError:
        return None
    return doc if isinstance(doc, dict) else None


def _ids(value) -> list[str]:
    """A status's people: ids as the server writes them, anything else `?`."""
    if not isinstance(value, list):
        return []
    return [v if isinstance(v, str) and ID.fullmatch(v) else "?" for v in value]


def _texts(value) -> list[str]:
    if not isinstance(value, list):
        return []
    return [v[:PROBLEM_CHARS] if isinstance(v, str) else "?" for v in value]


def outcome(doc: dict | None, before: dict | None, sha: str) -> dict | None:
    """What the status says of the file with `sha` (pure; see the module
    docs, step 5); None while it says nothing of it yet."""
    if not isinstance(doc, dict) or doc.get("sha256") != sha:
        return None
    applied = doc.get("applied") is True
    unchanged = doc == before
    if unchanged and not applied:
        return None
    out = {"sha256": sha, "ok": applied and doc.get("ok") is True, "problems": _texts(doc.get("problems")),
           "applied": applied, "added": [] if unchanged else _ids(doc.get("added")),
           "removed": [] if unchanged else _ids(doc.get("removed"))}
    if unchanged:
        out["unchanged"] = True
    return out


def seen(text) -> str:
    """What the status file held at the last read, for the late message."""
    if text is None:
        return "is absent"
    doc = parse_status(text)
    if doc is None:
        return "cannot be read"
    other = doc.get("sha256")
    return f"names {other[:12]}" if isinstance(other, str) and re.fullmatch(r"[0-9a-f]{64}", other) else "names no file"


def late(sha: str, text, reads: int) -> dict:
    return {"sha256": sha, "ok": False, "applied": False, "added": [], "removed": [], "problems": [
        f"not applied within {WAIT_S:g} s: {STATUS} {seen(text)} after {reads} reads; the server writes none for a "
        "roster it defers (an engine not synced, a switch a restart cut short) and reads it again every 2 s, and no "
        f"server runs while the guard is in event: read {STATUS} again later"]}


def install_body(ip, upload: str, config_dir: str, sha: str) -> str:
    """Step 4's PowerShell (module_script's body)."""
    q = ip.ps_quote
    new, dst, status = (ip.pc_join(config_dir, name) for name in (NEW, ROSTER, STATUS))
    return " ; ".join([
        ip.hash_check(upload, sha),
        "$iemBefore = $null",
        f"if (Test-Path -LiteralPath {q(status)} -PathType Leaf) {{ $iemBefore = [IO.File]::ReadAllText({q(status)}) }}",
        f"Copy-Item -LiteralPath {q(upload)} -Destination {q(new)} -Force",
        ip.hash_check(new, sha),
        f"Move-Item -LiteralPath {q(new)} -Destination {q(dst)} -Force",
        ip.hash_check(dst, sha),
        "[pscustomobject]@{ installed = $true; before = $iemBefore }",
    ])


def status_body(ip, config_dir: str) -> str:
    """One read of the status file: its text, '' while it cannot be read (the
    server replaces it whole), $null when there is none."""
    p = ip.ps_quote(ip.pc_join(config_dir, STATUS))
    return (f"if (Test-Path -LiteralPath {p} -PathType Leaf) {{ try {{ [IO.File]::ReadAllText({p}) }} catch {{ '' }} }} "
            "else { $null }")


def active_server(ctx, ip) -> tuple[str, str]:
    """The active bundle's iem-server.exe on the PC and its sha256 from this
    box's fetch record (step 1)."""
    code, reply, _ = ip.iemmode(ctx.env, ["status"], ip.STATUS_S, ctx.watch(abandon=True))
    if code != 0:
        raise ip.Refused(f"no roster install: iemmode status failed (exit {code}) (nothing was installed)")
    sha = iempc_soak.active_bundle(reply)
    if sha is None:
        raise ip.Refused("no roster install: the guard names no active bundle (nothing was installed)")
    want = (ip.need_record(sha).get("sums") or {}).get(SERVER)
    if not isinstance(want, str) or not ip.HEX64.fullmatch(want):
        raise ip.StepError(f"bundle {sha}'s fetch record lists no {SERVER}: fetch it again")
    return ip.pc_join(ctx.env["PC_ROOT"], f"bundles/{sha}/{SERVER}"), want


def await_status(ctx, ip, config_dir: str, sha: str, before: dict | None) -> dict:
    """Step 5: the status every POLL_S until it names the file, at most
    WAIT_S; the reads only read, so a new flag abandons them."""
    deadline = time.monotonic() + WAIT_S
    reads = 0
    while True:
        reads += 1
        text = ip.run_module(ctx.env, status_body(ip, config_dir), ip.STATUS_S, ctx.watch(abandon=True))
        got = outcome(parse_status(text), before, sha)
        if got is not None:
            return got
        if time.monotonic() >= deadline:
            return late(sha, text, reads)
        ip.pause(ctx, ip.POLL_S)


def run(ctx, ip) -> int:
    """`iempc roster-install` (dev time, locked)."""
    env, dry = ctx.env, bool(ctx.args.dry_run)
    if not dry:
        iempc_live.refuse_while_running(ip, "roster-install")
    config, config_dir = server_config(ip, env)
    path = Path(ctx.args.file)
    try:
        data = path.read_bytes()
    except OSError as e:
        raise ip.Refused(f"roster-install: {path}: {e.strerror or type(e).__name__}") from None
    sha = hashlib.sha256(data).hexdigest()
    exe, want = active_server(ctx, ip)
    change = ctx.watch(abandon=False)
    rel = f"incoming/roster-{sha[:12]}.toml"
    upload = ip.pc_join(env["PC_ROOT"], rel)
    ip.pc_mkdir(ctx, "incoming", change)
    ip.scp(str(path), ip.remote(env, rel), change)
    stage, then = iempc_bin.staged_program(ip, exe, SERVER, want)
    checks = (ip.hash_check(upload, sha), *stage, f"$env:IEMMIXER_CONFIG = {ip.ps_quote(config)}")
    code, _, raw = ip.call(env, exe, ["roster", "check", upload], ip.STATUS_S, ctx.watch(abandon=True), checks,
                           json_reply=False, then=then)
    try:
        problems = check_result(code, raw["out"], sha)
    except CheckError as e:
        raise ip.StepError(str(e)) from None
    empty = {"sha256": sha, "ok": not problems, "problems": problems, "applied": False, "added": [], "removed": []}
    if problems or dry:
        ip.emit({"roster_install": {**empty, **({"dry_run": True} if dry else {})}})
        return 1 if problems else 0
    if ip.event_now():   # "ide event" since the check: an install is dev-time work
        raise ip.EventNow()
    r = ip.run_module(env, install_body(ip, upload, config_dir, sha), ip.STATUS_S, change)
    if not isinstance(r, dict) or r.get("installed") is not True:
        raise ip.StepError(f"the PC did not confirm the install of {ROSTER} (it answered {str(r)[-200:]!r}); "
                           f"read {STATUS} on the PC")
    out = await_status(ctx, ip, config_dir, sha, parse_status(r.get("before")))
    ip.emit({"roster_install": out})
    return 0 if out["applied"] else 1
