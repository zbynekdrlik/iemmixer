"""`iempc roster-install --file PATH [--dry-run]` (#58 plan Task 14, design
§9; the review of Part D on #62): a roster file from this box onto the IEM
PC, checked there by the active bundle's own server, put beside the server
config and followed until the server applied it; a roster the server does
not apply is taken back out. The server polls `roster.toml` every 2 s and
applies a valid change live (`roster_watch.rs`), so no restart and no
maintenance.

In this order (dev time: the EVENT-NOW flag at the start refuses it, design
§9 "a roster install is refused while EVENT-NOW exists"; locked):

0. Before any call: the file (its absolute path). An install (not a dry run)
   also refuses while a live run or a soak of this dev entry may still run
   (`iempc_live.refuse_while_running`): a reload closes every person's mixer
   socket with 4003, the soak's and the live run's included.
1. `iemmode status` (a read): the guard answers ok, in dev or live (a server
   runs), with no switch and no HIL job (`guard_refusal`); the active bundle
   (the detail's `bundle <sha>`), whose fetch record on this box holds
   iem-server.exe's sha256.
2. The guard's pc.toml (`PC_ROOT\\guard\\pc.toml`, a read): its
   `server_config`, the config the guard starts the server with, is the one
   source of the server's folder (`server_config`).
3. The file by scp into PC_ROOT\\incoming\\roster-<sha12>.toml (a change).
4. One ssh call (a read): the upload's sha256 checked first; the active
   bundle's iem-server.exe read once from bundles\\<sha>, checked by the
   fetch record, staged admin-only and run from there (#15: the elevated
   session runs only admin-only copies of our programs, iempc_bin); then
   `iem-server roster check <upload>` with IEMMIXER_CONFIG = server_config.
   Its one JSON line `{ok, problems, sha256}` decides (`check_result`):
   problems install nothing (exit 1); `--dry-run` ends here (exit 0 when ok).
5. The flag again, then one ssh call (a change: a new flag lets it finish):
   the upload checked again; `roster.status.json` as it stands (`before`)
   and the sha256 of `lineups\\applied-roster.toml` (the roster the server
   applied last), both read with read, write and delete sharing so the
   server's own replace of them never fails on this read; the roster.toml
   there kept as `roster.toml.prev` and its sha256 noted (none when there is
   none); the upload copied as `roster.toml.new`, checked, and put in place
   by `[IO.File]::Replace` when roster.toml exists (no moment without one),
   else by a move; checked again.
6. The applied roster is the upload already: `unchanged`, the server writes
   no status for it. Else `roster.status.json` every POLL_S, at most WAIT_S,
   until it names the file's sha256 (`outcome`). `applied` true: in use
   (exit 0; `ok` false and `problems` name what failed after it went into
   use, an archive move). Else refused (exit 1), with the server's problems,
   among them its fallback line "in use instead: the roster applied last"
   while it runs on that one. The server writes no status for a roster it
   defers (an engine not synced, a switch a restart cut short): a wait that
   runs out is "not applied" (exit 1) and says what the status named. The
   same refusal as `before` (the same bytes refused earlier) is waited out,
   since the server reads the new file again; the same applied status as
   `before` is the roster in use already (`unchanged`).
7. Not applied: one more change call (`restore_body`) while roster.toml
   still holds the upload: the kept previous file goes back the same atomic
   way, or roster.toml is removed when there was none, so a refused roster
   never waits in roster.toml for the server's next start (which a roster
   with no roster applied before it would fail). `restored: true`, or
   `restore_failed` naming why. Right before the replace or the remove it
   reads the applied roster and the status again: when either names the
   upload as applied (a deferred roster the server applied after the wait
   ran out), it changes nothing and the install counts as applied
   (`late: true`, exit 0).

It prints one JSON object `{"roster_install": {sha256, ok, problems,
applied, added, removed}}` (`dry_run`, `unchanged`, `restored`,
`restore_failed` when so): ids and counts only, never a person's name (the
server's problems name ids only, P6). Exit 0 applied, or a dry run that
checked ok; 1 refused or not applied.

Known limit: a flag that appears during the wait runs the event path at
once, before any restore (the event path's budget comes first), so a
refused roster may stay in roster.toml. Never silently: one OWNER ALARM line
on stderr (`cut_alarm`) names it and the way back by hand, the kept
`roster.toml.prev` or the removal of roster.toml.

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import hashlib
import json
import re
import sys
import time
import tomllib
from pathlib import Path

import iempc_bin
import iempc_live
import iempc_soak

SERVER = "iem-server.exe"
ROSTER = "roster.toml"
NEW = "roster.toml.new"
PREV = "roster.toml.prev"
STATUS = "roster.status.json"
APPLIED = "lineups/applied-roster.toml"   # the server's LineupStore::applied_roster
PC_TOML = "guard/pc.toml"                 # iem-guard site::pc_toml_path, below the root PC_ROOT names
WAIT_S = 20.0
# Each PC body's own marker, in its text and nowhere else.
TOML_READ, INSTALLED, RESTORED, STATUS_READ = "$iemToml", "installed = $true", "'restored'", "$iemStatus"
# A full Windows path of a file on a drive: no UNC, nothing a PowerShell
# string or a file name could not hold.
CONFIG_PATH = re.compile(r"[A-Za-z]:(?:\\[^\\/:*?\"<>|\x00-\x1f]+)+")
HEX64 = re.compile(r"[0-9a-f]{64}")
ID = re.compile(r"[A-Za-z0-9_-]{1,64}")
PROBLEM_CHARS = 400


class CheckError(Exception):
    """An answer that is not what was asked for (pure); run turns it into
    iempc's StepError or Refused."""


def server_config(text) -> str:
    """The guard's pc.toml (`text`, None when there is none) → its
    `server_config` (pure; CheckError says why not)."""
    if text is None:
        raise CheckError(f"no pc.toml (PC_ROOT\\{PC_TOML.replace('/', chr(92))}) on the PC: the guard is not set up")
    if not isinstance(text, str) or not text.strip():
        raise CheckError("the guard's pc.toml could not be read now; try again")
    try:
        doc = tomllib.loads(text.lstrip("\ufeff"))
    except tomllib.TOMLDecodeError as e:
        raise CheckError(f"the guard's pc.toml is not TOML ({e})") from None
    path = doc.get("server_config")
    if not isinstance(path, str):
        raise CheckError("the guard's pc.toml names no server_config")
    if not CONFIG_PATH.fullmatch(path):
        raise CheckError("the guard's pc.toml server_config is not a full Windows path of a file on a drive")
    return path


def guard_refusal(reply) -> str | None:
    """Why the guard does not run a server that could apply a roster now,
    from one `iemmode status` reply (pure): ok, dev or live, no switch, no
    HIL job; None when it does."""
    if not isinstance(reply, dict):
        return "iemmode status gave no reply"
    if reply.get("ok") is not True:
        return "the guard's status did not answer ok"
    if reply.get("mode") not in ("dev", "live"):
        return f"the guard is in mode {reply.get('mode')}: no server runs to apply it (dev or live only)"
    if reply.get("switching") is not None:
        return f"a switch runs ({json.dumps(reply['switching'])})"
    job = iempc_soak.hil_job(reply)
    if job is not None:
        return f"a HIL job runs ({job})"
    return None


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


def unchanged(sha: str) -> dict:
    """The roster in use installed again: the server writes no status for it."""
    return {"sha256": sha, "ok": True, "problems": [], "applied": True, "added": [], "removed": [], "unchanged": True}


def outcome(doc: dict | None, before: dict | None, sha: str) -> dict | None:
    """What the status says of the file with `sha` (pure; see the module
    docs, step 6); None while it says nothing of it yet."""
    if not isinstance(doc, dict) or doc.get("sha256") != sha:
        return None
    applied = doc.get("applied") is True
    if doc == before:
        return unchanged(sha) if applied else None
    return {"sha256": sha, "ok": applied and doc.get("ok") is True, "problems": _texts(doc.get("problems")),
            "applied": applied, "added": _ids(doc.get("added")), "removed": _ids(doc.get("removed"))}


def seen(text) -> str:
    """What the status file held at the last read, for the late message."""
    if text is None:
        return "is absent"
    doc = parse_status(text)
    if doc is None:
        return "cannot be read"
    other = doc.get("sha256")
    return f"names {other[:12]}" if isinstance(other, str) and HEX64.fullmatch(other) else "names no file"


def late(sha: str, text, reads: int) -> dict:
    return {"sha256": sha, "ok": False, "applied": False, "added": [], "removed": [], "problems": [
        f"not applied within {WAIT_S:g} s: {STATUS} {seen(text)} after {reads} reads; the server writes none for a "
        "roster it defers (an engine not synced, a switch a restart cut short) and reads it again every 2 s"]}


def read_shared(ip, path: str, var: str) -> str:
    """Statements that set `var` to the text of `path`: $null when there is
    no such file, '' when it cannot be read now. Opened with read, write and
    delete sharing: the server replaces its files by a rename (lib.rs
    atomic_write), which a handle without delete sharing makes fail."""
    p = ip.ps_quote(path)
    return (f"{var} = $null ; if (Test-Path -LiteralPath {p} -PathType Leaf) {{ try {{ "
            f"$iemS = [IO.File]::Open({p}, 'Open', 'Read', 'ReadWrite, Delete') ; "
            f"try {{ {var} = (New-Object IO.StreamReader($iemS)).ReadToEnd() }} finally {{ $iemS.Dispose() }} "
            f"}} catch {{ {var} = '' }} }}")


def sha_shared(ip, path: str, var: str) -> str:
    """Statements that set `var` to the sha256 of `path` (lower-case hex), $null
    when there is none or it cannot be read now; shared as read_shared."""
    p = ip.ps_quote(path)
    return (f"{var} = $null ; if (Test-Path -LiteralPath {p} -PathType Leaf) {{ try {{ "
            f"$iemS = [IO.File]::Open({p}, 'Open', 'Read', 'ReadWrite, Delete') ; "
            "try { $iemX = [Security.Cryptography.SHA256]::Create() ; "
            f"{var} = -join @($iemX.ComputeHash($iemS) | ForEach-Object {{ $_.ToString('x2') }}) ; $iemX.Dispose() }} "
            f"finally {{ $iemS.Dispose() }} }} catch {{ {var} = $null }} }}")


def toml_body(ip, root: str) -> str:
    """One read of the guard's pc.toml: its text, or $null without one."""
    return f"{read_shared(ip, ip.pc_join(root, PC_TOML), TOML_READ)} ; {TOML_READ}"


def install_body(ip, upload: str, config_dir: str, sha: str) -> str:
    """Step 5's PowerShell (module_script's body)."""
    q = ip.ps_quote
    new, dst, prev, status, applied = (ip.pc_join(config_dir, n) for n in (NEW, ROSTER, PREV, STATUS, APPLIED))
    return " ; ".join([
        ip.hash_check(upload, sha),
        read_shared(ip, status, "$iemBefore"),
        sha_shared(ip, applied, "$iemApplied"),
        f"$iemPrev = $null ; if (Test-Path -LiteralPath {q(dst)} -PathType Leaf) {{ "
        f"Copy-Item -LiteralPath {q(dst)} -Destination {q(prev)} -Force ; "
        f"$iemPrev = (Get-FileHash -LiteralPath {q(prev)} -Algorithm SHA256).Hash.ToLowerInvariant() }}",
        f"Copy-Item -LiteralPath {q(upload)} -Destination {q(new)} -Force",
        ip.hash_check(new, sha),
        f"if (Test-Path -LiteralPath {q(dst)} -PathType Leaf) {{ [IO.File]::Replace({q(new)}, {q(dst)}, [NullString]::Value) }} "
        f"else {{ Move-Item -LiteralPath {q(new)} -Destination {q(dst)} }}",
        ip.hash_check(dst, sha),
        f"[pscustomobject]@{{ {INSTALLED}; before = $iemBefore; prev = $iemPrev; applied = $iemApplied }}",
    ])


def restore_body(ip, config_dir: str, sha: str, prev: str | None) -> str:
    """Step 7's PowerShell: while roster.toml still holds the upload, the kept
    previous file back by the same atomic replace, or roster.toml removed;
    unless, right before that, the applied roster or the status names the
    upload as applied (the server applied it after the wait ran out): then
    nothing changes and the answer is `{late, status}`."""
    q = ip.ps_quote
    new, dst, kept, status, applied = (ip.pc_join(config_dir, n) for n in (NEW, ROSTER, PREV, STATUS, APPLIED))
    prepare = [ip.hash_check(dst, sha)]
    if prev is None:
        act = [f"Remove-Item -LiteralPath {q(dst)}",
               f"if (Test-Path -LiteralPath {q(dst)}) {{ throw ('still there: ' + {q(dst)}) }}"]
    else:
        prepare += [ip.hash_check(kept, prev),
                    f"Copy-Item -LiteralPath {q(kept)} -Destination {q(new)} -Force",
                    ip.hash_check(new, prev)]
        act = [f"[IO.File]::Replace({q(new)}, {q(dst)}, [NullString]::Value)",
               ip.hash_check(dst, prev)]
    late_check = [
        sha_shared(ip, applied, "$iemApplied"),
        read_shared(ip, status, "$iemNow"),
        f"$iemLate = ($iemApplied -eq '{sha}')",
        "if (-not $iemLate -and $iemNow) { try { $iemDoc = ConvertFrom-Json -InputObject $iemNow ; "
        f"$iemLate = ($iemDoc.sha256 -eq '{sha}' -and $iemDoc.applied -eq $true) }} catch {{ $iemLate = $false }} }}",
    ]
    tail = (f"if ($iemLate) {{ [pscustomobject]@{{ late = $true; status = $iemNow }} }} "
            f"else {{ {' ; '.join(act)} ; {RESTORED} }}")
    return " ; ".join([*prepare, *late_check, tail])


def status_body(ip, config_dir: str) -> str:
    """One read of the status file: its text, '' while it cannot be read (the
    server replaces it whole), $null when there is none."""
    return f"{read_shared(ip, ip.pc_join(config_dir, STATUS), STATUS_READ)} ; {STATUS_READ}"


def active_server(ctx, ip) -> tuple[str, str]:
    """The active bundle's iem-server.exe on the PC and its sha256 from this
    box's fetch record, once the guard runs a server that can apply it (step 1)."""
    code, reply, _ = ip.iemmode(ctx.env, ["status"], ip.STATUS_S, ctx.watch(abandon=True))
    if code != 0:
        raise ip.Refused(f"no roster install: iemmode status failed (exit {code}) (nothing was installed)")
    why = guard_refusal(reply)
    if why:
        raise ip.Refused(f"no roster install: {why} (nothing was installed)")
    sha = iempc_soak.active_bundle(reply)
    if sha is None:
        raise ip.Refused("no roster install: the guard names no active bundle (nothing was installed)")
    want = (ip.need_record(sha).get("sums") or {}).get(SERVER)
    if not isinstance(want, str) or not HEX64.fullmatch(want):
        raise ip.StepError(f"bundle {sha}'s fetch record lists no {SERVER}: fetch it again")
    return ip.pc_join(ctx.env["PC_ROOT"], f"bundles/{sha}/{SERVER}"), want


def guard_config(ctx, ip) -> str:
    """The guard's server_config (step 2; a read)."""
    text = ip.run_module(ctx.env, toml_body(ip, ctx.env["PC_ROOT"]), ip.STATUS_S, ctx.watch(abandon=True))
    try:
        return server_config(text)
    except CheckError as e:
        raise ip.Refused(f"no roster install: {e} (nothing was installed)") from None


def await_status(ctx, ip, config_dir: str, sha: str, before: dict | None) -> dict:
    """Step 6: the status every POLL_S until it names the file, at most
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


def applied_late(sha: str, status_text) -> dict:
    """The restore found the upload applied after all (pure): the status's
    fields when it names the upload as applied, else only that it is."""
    got = outcome(parse_status(status_text), None, sha)
    base = got if got is not None and got["applied"] else {
        "sha256": sha, "ok": True, "problems": [], "applied": True, "added": [], "removed": []}
    return {**base, "late": True}


def restore(ctx, ip, config_dir: str, sha: str, prev, out: dict) -> dict:
    """Step 7 (a change) after the outcome `out` that is not applied: `out`
    with `restored`, or `restore_failed` naming why, or the late apply the
    restore found instead (`applied_late`). `prev` is the install's answer:
    None (there was no roster.toml), a sha256, or anything else, which leaves
    roster.toml as it is (removing it would lose a previous file the answer
    failed to name)."""
    if prev is not None and not (isinstance(prev, str) and HEX64.fullmatch(prev)):
        return {**out, "restore_failed": f"{ROSTER} was not put back: the PC named the previous file as "
                                         f"{str(prev)[-80:]!r}, no sha256"}
    try:
        r = ip.run_module(ctx.env, restore_body(ip, config_dir, sha, prev), ip.STATUS_S, ctx.watch(abandon=False))
    except ip.StepError as e:
        return {**out, "restore_failed": f"{ROSTER} was not put back: {str(e)[-400:]}"}
    if isinstance(r, dict) and r.get("late") is True:
        return applied_late(sha, r.get("status"))
    if r != "restored":
        return {**out, "restore_failed": f"{ROSTER} was not put back: the PC answered {str(r)[-200:]!r}"}
    return {**out, "restored": True}


def cut_alarm(config_dir: str, sha: str, prev, why: str) -> str:
    """The OWNER ALARM line when `why` cut the wait before any restore (a
    documented limit, never silent): the upload may still be in roster.toml,
    and the way back by hand."""
    dst, kept, status = (f"{config_dir}\\{n}" for n in (ROSTER, PREV, STATUS))
    if prev is None:
        back = f"remove {dst} (there was none before)"
    elif isinstance(prev, str) and HEX64.fullmatch(prev):
        back = f"put {kept} back as {dst} (sha256 {prev[:12]})"
    else:
        back = f"put {kept} back as {dst} if the install kept one, else remove {dst}"
    return (f"iempc: OWNER ALARM: {why} cut roster-install's wait, so nothing was put back: the new roster "
            f"({sha[:12]}) may still be in {dst} on the PC, applied or not ({status} names the last one the server "
            f"read). If it is not applied, the way back in dev time: {back}, or 'iempc roster-install' with the "
            "previous roster.")


def run(ctx, ip) -> int:
    """`iempc roster-install` (dev time, locked)."""
    env, dry = ctx.env, bool(ctx.args.dry_run)
    if not dry:
        iempc_live.refuse_while_running(ip, "roster-install")
    path = Path(ctx.args.file).resolve()
    try:
        data = path.read_bytes()
    except OSError as e:
        raise ip.Refused(f"roster-install: {path}: {e.strerror or type(e).__name__}") from None
    sha = hashlib.sha256(data).hexdigest()
    exe, want = active_server(ctx, ip)
    config = guard_config(ctx, ip)
    config_dir = config.rsplit("\\", 1)[0]
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
    if r.get("applied") == sha:
        out = unchanged(sha)
    else:
        try:
            out = await_status(ctx, ip, config_dir, sha, parse_status(r.get("before")))
        except ip.EventNow:
            # The event path comes first, so no restore; but never silently.
            print(cut_alarm(config_dir, sha, r.get("prev"), "'ide event'"), file=sys.stderr, flush=True)
            raise
    if not out["applied"]:
        out = restore(ctx, ip, config_dir, sha, r.get("prev"), out)
    ip.emit({"roster_install": out})
    return 0 if out["applied"] else 1
