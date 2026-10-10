#!/usr/bin/env python3
"""Dev-box control of the IEM PC (S6, design note §5.1, §5.5, §6, §7).

`iemmode` over ssh with the EVENT-NOW discipline, attested bundles from CI
(fetch, install, activate), HIL, soak and live-run dispatch on the private ops repo
(`dispatch-soak`: iempc_soak.py; `dispatch-live`: iempc_live.py), the switch
timing (`switch-test`: iempc_switch.py), PC bootstrap through
the bundle's IemPc.psm1 (dev time only), S1c's tuning modules and profile into
the PC's elevated tuning folder (`tuning-install`, refreshed after `activate`:
iempc_tuning.py), a kernel DPC/ISR trace on the guard's engine (`trace`:
iempc_trace.py, PC_XPERF below), the admin-only OpenSSH default shell
(`ssh-shell`: iempc_sshshell.py), and the hand-over of an open S1a window.

"ide event": the flag file (~/.config/iemmixer/EVENT-NOW) exists. `event`
writes it first when it is missing (a flag it cannot write is a warning,
never a stop), pre-empts an open S1a/S1c spike window (spike_window.py
preempt; after a failed one it closes the window under the window lock, so
no queued window preempt starts a second bring-back), stops a kernel trace
whose `iempc trace` died with this box (its record, iempc_trace.stop_recorded;
`dev` does too), then runs `iemmode event`, and `iemmode event --direct` when
the guard is unreachable (exit 4). The event path has one budget that fits one
Bash call (EVENT_BUDGET_S): the spike preempt gets SPIKE_SHARE_S of it, no
`iemmode` call starts while the preempt still runs, and none starts with
less than SWITCH_MIN_S left. `event` never waits for another iempc command;
it waits only, within its budget, for the window lock (the close after a
failed preempt) and for a window process's own settle (the spike preempt).

Every other PC step waits for dev time: commands that change the PC refuse
while the flag exists, and `status` then reports this box only (`--pc`
asks the guard anyway). Every PC wait sees a new flag within 2 s: a
read-only call or a switch the guard owns is abandoned (the guard pre-empts
itself), a change the call makes itself completes first; then the command
runs the event path itself (exit 10). `dev`, `rehearse-teardown`,
`install` (except `--first`) and `activate` refuse while an S1a/S1c window
is open: `handover-s1a` hands the card over first. `dispatch-hil`,
`dispatch-soak` and `dispatch-live` check the flag again right before they
dispatch. `activate`, `dispatch-hil` and `trace` refuse first, before any
call, while a live run or a soak of this dev entry may still run;
`dispatch-soak` and `switch-test` while a live run of it may (iempc_live;
switch-test's own soak check follows its status read, iempc_switch); `dev`
and `event` never refuse.

`activate --sha` runs `iemmode activate`, which the guard allows in dev
and in an idle event (#9 2026-09-28: none of iemmixer's processes runs, no
switch or HIL job waits; REAPER and the predecessor app are not touched). It is how a guard fix reaches a guard in event, whose
own code may refuse the dev entry: after `install`, `activate` in event,
then `dev`. It then waits for the hand-over: `iemmode status` until the
guard that answers names the SHA as its `guard_build` (the GITHUB_SHA its
exe was built with), at most HANDOVER_S; a status read that fails meanwhile
is read again. A guard built before that rule refuses it in event:
`activate --offline` then quits it gracefully (`iemmode quit`, then its
processes read until none runs, QUIT_S), runs the bundle's own
`iemmixer-guard.exe activate <sha>` (read once from `bundles\\<sha>`, its
sha256 checked on the PC, run from the admin-only stage: iempc_bin; it takes
the guard's mutex and activates an idle event only) and waits for the
hand-over the same way; the first status read starts the
guard's task, which runs the new exe. A refused offline step starts the
guard again.

Site values come only from the private env file ($PC_ENV, default
~/.config/iemmixer/iem-pc.env): PC_SSH (the ssh destination), PC_ROOT (the
root folder on the PC, Windows form), PC_ROOT_SCP (the same folder as scp
names it), PC_BIN (optional, default: bin under PC_ROOT; iemmode runs from
the admin-only %ProgramData%\\iemmixer\\bin copy instead when it reads back
and the guard last seen runs its build, iempc_bin) and PC_XPERF
(optional, xperf.exe's full path on the PC; `trace` refuses without it).
Nothing is ever ended by force.

Known limits (S6 Task 16): `iemmode event --direct` runs the switch inside
the ssh session, so a session cut before it ends (a Bash timeout) stops it
half-way; the next `iemmode event` resumes from the guard state. A
pre-emption inside another command adds a whole event budget to that
command's own time. The dev-entry count behind `dispatch-hil` and
`dispatch-soak` sees only `iempc dev` and switch-test's dev leg, not a dev
entry the guard makes by itself (rehearse-teardown's re-entry)."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import sys
import time
import zipfile
import zlib
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

import iempc_bin
import iempc_core as core
import iempc_live
import iempc_soak
import iempc_sshshell
import iempc_switch
import iempc_trace
import iempc_tuning
from iempc_core import (BOOTSTRAP_S, BRANCHES, INSTALL_S, OPS_REPO, REPO, SHA, SPIKE_DIR, STATUS_S, SWITCH_S, Ctx,
                        EventNow, Refused, StepError, StillRunning, call, check_sha, current_entry, emit, event_now,
                        guarded, hash_check, iemmode, load_env, next_entry, pause, pc_join, pc_mkdir, ps_quote,
                        read_json, refuse_open_window, remote, result, run_module, spike_module, spike_window_open,
                        spike_window_settling, state_dir, state_lock, write_json)

# The modules iempc.py is split into (#36). `iempc.<name>` reads any of their
# names live (PEP 562), so siblings handed this module (`ip`) and the tests
# reach the one binding a test patches, never a copy of it.
SPLIT = (core,)


def __getattr__(name: str):
    for module in SPLIT:
        if name in vars(module):
            return vars(module)[name]
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")

SPIKE = SPIKE_DIR / "spike_window.py"
CI_WORKFLOW = "ci.yml"
HIL_WORKFLOW = "hil.yml"
# The bundle job's files (plan Task 12); the guard's install checks the same set.
BUNDLE_REQUIRED = (
    "iem-engine.exe", "iem-server.exe", "iemmixer-guard.exe", "iemmode.exe", "iem-tray.exe", "iem-migrate.exe",
    "hil-v1.ps1", "IemPc.psm1", "manifest.json",
)
# A bundle member: a file name, or one directory level (`tuning/<name>`).
MEMBER = re.compile(r"[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)?")
SUMS_LINE = re.compile(r"([0-9a-f]{64})  (\S+)")
FUNCTION = re.compile(r"(Get|Test|Set|Add|Register|Grant|Remove)-Iem[A-Za-z0-9]+")
READ_ONLY_VERBS = ("Get", "Test")
PARAM_NAME = re.compile(r"-[A-Za-z][A-Za-z0-9]*")
RUNNER_FUNCTION = "Register-IemRunner"
RUNNER_TOKEN = re.compile(r"[A-Za-z0-9]{20,200}")
GUARD_UNREACHABLE = 4
PREEMPTED = 10
# The event path, all of it: one Bash call ends at 10 min, the plan's waits stay within 9.
EVENT_BUDGET_S = 540
# spike_window.py preempt's part of it (its bring-back starts REAPER itself).
SPIKE_SHARE_S = 360
# An iemmode call of the event path never starts with less than this left.
SWITCH_MIN_S = 120
DOWNLOAD_S = 540
# After `iemmode activate`: the old guard's last reply and exit, the new exe's mutex (<= 10 s) and pipe
# (<= 10 s), and iemmode's own start of the guard task (<= 15 s) when it reads in between.
HANDOVER_S = 90
# Between two status reads of the hand-over (each read has its own STATUS_S bound).
HANDOVER_POLL_S = 2.0
# The guard's process name, and how long `iemmode quit` may take to end it (its last reply <= 5 s, its pipe, its exit).
GUARD_IMAGE = "iemmixer-guard"
QUIT_S = 60
# What reading a zip member can raise besides StepError: bad JSON or UTF-8, a
# CRC error, a cut or corrupt deflate stream, an unknown compression method.
UNREADABLE = (ValueError, EOFError, NotImplementedError, zipfile.BadZipFile, zlib.error)
OWNER_ALARM = ("iempc: the event path did not complete: alarm the owner now with the prepared question (ops runbook "
               "docs/s6-pc-runbook.md); before the guard is installed, the interim switch (event runbook) applies; the "
               "last resort is the owner's reboot, which comes back in event mode. Never force-end anything.")


# ---- env, flag, output ----


def ensure_flag() -> bool:
    """Writes the "ide event" flag (`date -Iseconds`) unless it exists; True when written."""
    if event_now():
        return False
    core.EVENT_NOW.parent.mkdir(parents=True, exist_ok=True)
    try:
        with open(core.EVENT_NOW, "x", encoding="utf-8") as f:
            f.write(core.now_iso() + "\n")
    except FileExistsError:
        return False
    return True


def write_flag() -> None:
    """`event` writes the flag first; a flag it cannot write (no folder, a
    full or read-only disk) is a warning, never a stop of the event path."""
    try:
        written = ensure_flag()
    except OSError as e:
        print(f"iempc: WARNING: the flag {core.EVENT_NOW} was not written ({e}); the event path goes on; write the flag by "
              "hand so no dev-time command runs", file=sys.stderr, flush=True)
        emit({"flag": str(core.EVENT_NOW), "written": False, "error": str(e)})
        return
    if written:
        emit({"flag": str(core.EVENT_NOW), "written": True})


def event_clock() -> float:
    """The event path's one clock (its budget, each call's share); the tests
    run the path on a fake one, so no branch depends on this process's speed."""
    return time.monotonic()


# ---- bundles (P5: a green push run on dev/main, attested by digest) ----

def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def check_member(name: str) -> str:
    if not MEMBER.fullmatch(name) or any(part in (".", "..") for part in name.split("/")):
        raise StepError(f"bundle entry refused: {name!r}")
    return name


def parse_sums(text: str) -> dict[str, str]:
    sums: dict[str, str] = {}
    for line in text.splitlines():
        if not line.strip():
            continue
        m = SUMS_LINE.fullmatch(line)
        if not m:
            raise StepError(f"malformed SHA256SUMS line: {line!r}")
        name = check_member(m.group(2))
        if name in sums:
            raise StepError(f"SHA256SUMS lists {name} twice")
        sums[name] = m.group(1)
    return sums


def check_manifest(manifest: dict, sha: str, branch: str, run: int) -> None:
    got = (manifest.get("sha"), manifest.get("branch"), str(manifest.get("run")))
    if got != (sha, branch, str(run)):
        raise StepError(f"manifest.json names {got}, expected {(sha, branch, str(run))}")


def verify_zip(path: Path, sha: str, branch: str, run: int) -> dict[str, str]:
    """Every file listed in SHA256SUMS with its hash (the sums file itself
    exempt), nothing unlisted, the required files present, and the manifest
    naming this SHA, branch and run."""
    try:
        zf = zipfile.ZipFile(path)
    except zipfile.BadZipFile as e:
        raise StepError(f"{path.name}: not a zip ({e})") from None
    member = "the directory"
    with zf:
        try:
            members: dict[str, zipfile.ZipInfo] = {}
            for info in zf.infolist():
                name = info.filename.replace("\\", "/")
                if name.endswith("/"):
                    continue
                if check_member(name) in members:
                    raise StepError(f"{path.name}: {name} appears twice")
                members[name] = info
            if "SHA256SUMS" not in members:
                raise StepError(f"{path.name}: no SHA256SUMS")
            member = "SHA256SUMS"
            sums = parse_sums(zf.read(members["SHA256SUMS"]).decode("utf-8-sig"))
            present = set(members) - {"SHA256SUMS"}
            if set(sums) != present:
                raise StepError(f"{path.name}: listed but absent {sorted(set(sums) - present)}, "
                                f"present but unlisted {sorted(present - set(sums))}")
            absent = [n for n in BUNDLE_REQUIRED if n not in sums]
            if absent:
                raise StepError(f"{path.name}: required files missing: {absent}")
            for name, want in sorted(sums.items()):
                member = name
                h = hashlib.sha256()
                with zf.open(members[name]) as f:
                    for chunk in iter(lambda: f.read(1 << 20), b""):
                        h.update(chunk)
                if h.hexdigest() != want:
                    raise StepError(f"{path.name}: {name} does not match SHA256SUMS")
            member = "manifest.json"
            manifest = json.loads(zf.read(members["manifest.json"]).decode("utf-8-sig"))
        except UNREADABLE as e:
            raise StepError(f"{path.name}: {member} is unreadable ({type(e).__name__}: {str(e)[:300]})") from None
    if not isinstance(manifest, dict):
        raise StepError(f"{path.name}: manifest.json is not an object")
    check_manifest(manifest, sha, branch, run)
    return sums


def pick_runs(runs: list[dict], sha: str, branches: tuple[str, ...]) -> list[dict]:
    """Successful push runs of exactly `sha` on the allowed branches (P5)."""
    return [r for r in runs if r.get("headSha") == sha and r.get("event") == "push"
            and r.get("headBranch") in branches and r.get("conclusion") == "success"]


def job_ok(jobs: list[dict], name: str) -> bool:
    return any(j.get("name") == name and j.get("conclusion") == "success" for j in jobs)


def green_run(sha: str, branches: tuple[str, ...]) -> tuple[int, str]:
    listing = core.gh(["run", "list", "-R", REPO, "--workflow", CI_WORKFLOW, "--event", "push", "--commit", sha,
                  "--limit", "20", "--json", "databaseId,headSha,event,headBranch,conclusion"])
    for r in pick_runs(json.loads(listing or "[]"), sha, branches):
        jobs = json.loads(core.gh(["run", "view", str(r["databaseId"]), "-R", REPO, "--json", "jobs"])).get("jobs") or []
        if job_ok(jobs, "bundle") and job_ok(jobs, "attest"):
            return int(r["databaseId"]), r["headBranch"]
    raise StepError(f"no green push run of {CI_WORKFLOW} on {'/'.join(branches)} for {sha} with its 'bundle' and "
                    "'attest' jobs succeeded (P5)")


def branch_head(branch: str) -> str:
    head = core.gh(["api", f"repos/{REPO}/git/ref/heads/{branch}", "--jq", ".object.sha"]).strip()
    if not SHA.fullmatch(head):
        raise StepError(f"the head of {branch} reads {head!r}")
    return head


def bundle_dir(sha: str) -> Path:
    return state_dir() / "bundles" / sha


def zip_path(sha: str) -> Path:
    return bundle_dir(sha) / f"iemmixer-{sha}.zip"


def load_record(sha: str) -> dict | None:
    return read_json(bundle_dir(sha) / "fetch.json", None)


def need_record(sha: str) -> dict:
    rec = load_record(sha)
    if rec is None:
        raise Refused(f"bundle {sha} is not fetched: run 'iempc fetch-bundle --sha {sha}' first")
    return rec


def check_local_zip(sha: str, rec: dict) -> Path:
    z = zip_path(sha)
    if not z.is_file():
        raise StepError(f"{z} is missing: fetch the bundle again")
    got = "sha256:" + sha256_file(z)
    if got != rec.get("digest"):
        raise StepError(f"{z}: digest {got} differs from the fetched {rec.get('digest')} (P5): refused")
    return z


def latest_record_sha() -> str:
    found = []
    for p in (state_dir() / "bundles").glob("*/fetch.json"):
        doc = read_json(p, None)
        if isinstance(doc, dict) and doc.get("sha") == p.parent.name:
            found.append((str(doc.get("fetched_at", "")), doc["sha"]))
    if not found:
        raise Refused("no fetched bundle: run 'iempc fetch-bundle --sha SHA' first")
    return max(found)[1]


def extract_member(sha: str, rec: dict, name: str, nested: bool = False) -> tuple[Path, str]:
    """A top-level file of the fetched, verified zip, checked against its sums;
    with `nested` one under `tuning/` (S1c's modules, iempc_tuning)."""
    want = (rec.get("sums") or {}).get(name)
    if ("/" in name) != nested or (nested and not name.startswith("tuning/")) or want is None:
        raise StepError(f"{name} is not a listed {'tuning' if nested else 'top-level'} file of bundle {sha}")
    z = check_local_zip(sha, rec)
    with zipfile.ZipFile(z) as zf:
        infos = [i for i in zf.infolist() if i.filename.replace("\\", "/") == name]
        if len(infos) != 1:
            raise StepError(f"{name} is not in {z.name} exactly once")
        data = zf.read(infos[0])
    if hashlib.sha256(data).hexdigest() != want:
        raise StepError(f"{name} in {z.name} does not match SHA256SUMS")
    out = bundle_dir(sha) / name
    out.parent.mkdir(mode=0o700, exist_ok=True)
    tmp = out.with_name(out.name + ".tmp")
    tmp.write_bytes(data)
    os.chmod(tmp, 0o600)
    tmp.replace(out)
    return out, want


def fetch_bundle(sha: str, branch: str | None = None) -> tuple[dict, bool]:
    """(record, fetched now). A fetched bundle is reused after its digest check."""
    rec = load_record(sha)
    if rec is not None:
        check_local_zip(sha, rec)
        if branch is not None and rec.get("branch") != branch:
            raise StepError(f"bundle {sha} was fetched from {rec.get('branch')}, not {branch}")
        return rec, False
    run, run_branch = green_run(sha, (branch,) if branch else BRANCHES)
    dest = bundle_dir(sha)
    if dest.exists():
        raise StepError(f"{dest} exists without a fetch record: remove it, then fetch again")
    partial = dest.with_name(sha + ".partial")
    if partial.exists():
        shutil.rmtree(partial)
    partial.mkdir(parents=True)
    os.chmod(partial, 0o700)
    try:
        core.gh(["run", "download", str(run), "-R", REPO, "-n", f"iemmixer-bundle-{sha}", "-D", str(partial)], DOWNLOAD_S)
        z = partial / f"iemmixer-{sha}.zip"
        if not z.is_file():
            raise StepError(f"the artifact of run {run} has no iemmixer-{sha}.zip")
        digest = "sha256:" + sha256_file(z)
        sums = verify_zip(z, sha, run_branch, run)
        core.gh(["attestation", "verify", str(z), "-R", REPO, "--signer-workflow", f"{REPO}/.github/workflows/{CI_WORKFLOW}",
            "--source-ref", f"refs/heads/{run_branch}", "--deny-self-hosted-runners"])
    except BaseException:
        shutil.rmtree(partial)  # a refused or cut download is never kept
        raise
    rec = {"sha": sha, "branch": run_branch, "run": run, "digest": digest, "sums": sums, "fetched_at": core.now_iso()}
    write_json(partial / "fetch.json", rec)
    partial.rename(dest)
    return rec, True


# ---- commands ----

def box_state() -> dict:
    return {"event_now": event_now(), "spike_window_open": spike_window_open(), "dev_entry": current_entry()}


def cmd_status(ctx: Ctx) -> int:
    """The guard's status and this box's. While the flag exists only this
    box's, unless --pc: `iemmode status` may start the guard, and a guard's
    start runs the event plan's checks (and restarts what does not serve)."""
    if ctx.flag_at_start and not ctx.args.pc:
        emit({"iemmode": None, "skipped": f"{core.EVENT_NOW} exists: no PC step during an event ('status --pc' asks the "
                                          "guard anyway)", **box_state()})
        return 0
    code, reply, raw = iemmode(ctx.env, ["status"], STATUS_S, ctx.watch(abandon=True))
    out = result("iemmode", ["status"], code, reply, raw)
    out.update(box_state())
    emit(out)
    return code


def spike_preempt(timeout: float) -> dict:
    """spike_window.py preempt in its own process, bounded by its share of
    the event budget. A failure lets the event path go on; a preempt still
    running at the end of its share is reported as `running`."""
    try:
        out = guarded([sys.executable, str(SPIKE), "preempt"], "", timeout, "ignore")
    except StillRunning as e:
        print(f"iempc: spike preempt: {e}", file=sys.stderr, flush=True)
        return {"ok": False, "running": True, "error": str(e)[-1500:]}
    except StepError as e:
        print(f"iempc: spike preempt: {e}", file=sys.stderr, flush=True)
        return {"ok": False, "error": str(e)[-1500:]}
    return {"ok": True, "output": out[-4000:]}


def close_failed_window(deadline: float, error: str) -> None:
    """After a FAILED spike preempt (F2 round 3, m2 and decision 2): the window
    is closed under spike_window's lock before `iemmode event`, so a window
    preempt another process still has queued finds it closed and starts no
    second bring-back next to the guard's (one meter-bridge trigger, the #9
    lesson). The lock is waited for at most what the event budget leaves above
    the guard's SWITCH_MIN_S; a lock that stays taken means a window process
    may be bringing REAPER back itself: no iemmode call. Nothing else in the
    state changes: the guard's event plan brings REAPER back."""
    sw = spike_module()
    seen: dict = {}

    def close(st: dict) -> None:
        if not st.get("closed"):
            st["closed"] = True
            st["closed_by"] = {"by": "iempc event after a failed spike preempt", "at": core.now_iso(), "error": error[-500:]}
        if sw.intent_live(st.get("in_flight")):
            seen["in_flight"] = st["in_flight"]

    wait = deadline - event_clock() - SWITCH_MIN_S
    try:
        if wait <= 0:
            raise sw.StepError(f"no time left in the event budget to wait for the window lock ({max(wait, 0):.0f} s)")
        sw.update_state(change=close, wait_s=wait)
    except sw.StepError as e:
        raise StepError(f"the S1a/S1c window could not be closed after the failed spike preempt ({e}): no iemmode call "
                        "while a window process may hold the window lock and bring REAPER back itself (one bring-back, "
                        "the #9 lesson); run 'iempc event' again once it is free (spike_window.py status)") from None
    except (OSError, ValueError) as e:
        # An unreadable window state: no window process can bring REAPER back from it
        # either (each preempt reads it first), so the guard's event path goes on.
        print(f"iempc: WARNING: the S1a/S1c window state could not be read to close it ({e}); the event path goes on",
              file=sys.stderr, flush=True)
        emit({"spike_window": "unreadable", "after": "a failed spike preempt"})
        return
    if seen:
        # No settle watches it on this path (review of lane G2, finding 5): the step's
        # own late handler sees the window closed, and the guard takes REAPER.
        print(f"iempc: WARNING: {seen['in_flight'].get('step')} is still in flight in the closed window: its own "
              "follow-up runs when its call is back; check REAPER once iemmode event is done", file=sys.stderr, flush=True)
    emit({"spike_window": "closed", "after": "a failed spike preempt", **seen})


def switch_timeout(deadline: float) -> float:
    """What an iemmode call of the event path may take: the rest of the one
    budget. It never starts with less than SWITCH_MIN_S left, since a cut
    `--direct` session stops its switch half-way."""
    left = deadline - event_clock()
    if left < SWITCH_MIN_S:
        raise StepError(f"the event path has {max(left, 0):.0f} s of its {EVENT_BUDGET_S} s budget left, less than the "
                        f"{SWITCH_MIN_S} s an iemmode call gets: run 'iempc event' again (a new budget)")
    return left


def cmd_event(ctx: Ctx) -> int:
    """The flag, the spike preempt when a window is open, then `iemmode
    event` (and `--direct` on exit 4), all within EVENT_BUDGET_S."""
    dry = bool(getattr(ctx.args, "dry_run", False))
    deadline = event_clock() + EVENT_BUDGET_S
    if not dry:
        write_flag()
    if spike_window_open() or spike_window_settling():
        if dry:
            emit({"spike_window": "open", "plan": "spike_window.py preempt"})
        else:
            pre = spike_preempt(SPIKE_SHARE_S)
            emit({"spike_preempt": pre})
            if pre.get("running"):
                raise StepError(f"spike_window.py preempt still runs after its {SPIKE_SHARE_S} s share of the event "
                                "budget: no iemmode call while it may still be bringing REAPER back (one meter-bridge "
                                "trigger, the #9 lesson); run 'iempc event' again once it has ended "
                                "(spike_window.py status)")
            if not pre["ok"]:
                close_failed_window(deadline, pre.get("error", ""))
    if not dry:   # a trace whose dev-box process died (#15); never raises. guarded sees its
        # bound only at its next poll: two polls stay with iemmode event's minimum.
        iempc_trace.stop_recorded(ctx, sys.modules[__name__], deadline - event_clock() - SWITCH_MIN_S - 2 * core.POLL_S)
    args = ["event", "--dry-run"] if dry else ["event"]
    code, reply, raw = iemmode(ctx.env, args, switch_timeout(deadline), "ignore")
    emit(result("iemmode", args, code, reply, raw))
    if code == GUARD_UNREACHABLE:
        args = [*args, "--direct"]
        code, reply, raw = iemmode(ctx.env, args, switch_timeout(deadline), "ignore")
        emit(result("iemmode", args, code, reply, raw))
    if code != 0 and not dry:
        print(OWNER_ALARM, file=sys.stderr, flush=True)
    return code


def cmd_dev(ctx: Ctx) -> int:
    """The guard owns the switch: a new flag abandons this client at once and
    the event path pre-empts the switch."""
    refuse_open_window("dev")
    args = ["dev"]
    if ctx.args.build:
        args += ["--build", check_sha(ctx.args.build)]
    dry = bool(ctx.args.dry_run)
    if dry:
        args.append("--dry-run")
    else:   # a trace whose dev-box process died (#15); never raises
        iempc_trace.stop_recorded(ctx, sys.modules[__name__], float("inf"))
        if ctx.watch(abandon=True) != "ignore" and event_now():
            raise EventNow()   # that stop ran with "ignore": a dev entry now would reach the guard after "ide event"
    code, reply, raw = iemmode(ctx.env, args, STATUS_S if dry else SWITCH_S, ctx.watch(abandon=True))
    out = result("iemmode", args, code, reply, raw)
    if code == 0 and not dry:
        out["dev_entry"] = next_entry(ctx.args.build)
    emit(out)
    return code


def cmd_rehearse_teardown(ctx: Ctx) -> int:
    refuse_open_window("rehearse-teardown")
    code, reply, raw = iemmode(ctx.env, ["rehearse-teardown"], SWITCH_S, ctx.watch(abandon=True))
    emit(result("iemmode", ["rehearse-teardown"], code, reply, raw))
    return code


def cmd_probe_task(ctx: Ctx) -> int:
    code, reply, raw = iemmode(ctx.env, ["probe-task"], STATUS_S, ctx.watch(abandon=False))
    emit(result("iemmode", ["probe-task"], code, reply, raw))
    return code


def cmd_fetch_bundle(ctx: Ctx) -> int:
    rec, fetched = fetch_bundle(check_sha(ctx.args.sha))
    emit({"bundle": str(zip_path(rec["sha"])), "fetched": fetched,
          **{k: rec[k] for k in ("sha", "branch", "run", "digest")}, "files": sorted(rec["sums"])})
    return 0


def cmd_install(ctx: Ctx) -> int:
    """The verified zip to the PC, then the guard installs it: `iemmode
    install`, or for the first bundle (no iemmode on the PC yet) the zip's
    own `iemmixer-guard install`, which takes the guard mutex itself. Only
    the first bundle goes in while an S1a/S1c window is open: it starts no
    guard and touches no card, while `iemmode` may start the guard."""
    env, sha = ctx.env, check_sha(ctx.args.sha)
    if not ctx.args.first:
        refuse_open_window("install")
    rec = need_record(sha)
    z = check_local_zip(sha, rec)
    mode = ctx.watch(abandon=False)
    zip_rel = f"incoming/iemmixer-{sha}.zip"
    pc_zip = pc_join(env["PC_ROOT"], zip_rel)
    pc_mkdir(ctx, "incoming", mode)
    core.scp(str(z), remote(env, zip_rel), mode)
    checks = [hash_check(pc_zip, rec["digest"].split(":", 1)[1])]
    if ctx.args.first:
        local, hexd = extract_member(sha, rec, "iemmixer-guard.exe")
        exe_rel = f"incoming/iemmixer-guard-{sha}.exe"
        exe = pc_join(env["PC_ROOT"], exe_rel)
        core.scp(str(local), remote(env, exe_rel), mode)
        guard, then = iempc_bin.staged_guard(sys.modules[__name__], exe, hexd)   # run from the stage (#15)
        code, reply, raw = call(env, exe, ["install", pc_zip], INSTALL_S, mode, (*checks, *guard), json_reply=False,
                                then=then)
    else:
        code, reply, raw = iemmode(env, ["install", pc_zip], INSTALL_S, mode, tuple(checks))
    out = result("install", [sha], code, reply, raw)
    out["via"] = "iemmixer-guard (first bundle)" if ctx.args.first else "iemmode"
    emit(out)
    return code


def await_guard_build(ctx: Ctx, sha: str) -> dict:
    """Reads `iemmode status` every HANDOVER_POLL_S until the guard that
    answers names `sha` as its own build (`guard_build`, the GITHUB_SHA its
    exe was built with), at most HANDOVER_S. A read that fails in between
    (the old guard has ended, the new one's pipe is not up yet: exit 4, an
    ssh error) is read again; the reads are read-only, so a new flag
    abandons them. Past the bound the hand-over is unverified: StepError."""
    deadline = time.monotonic() + HANDOVER_S
    reads = 0
    last = "no read"
    while True:
        reads += 1
        try:
            code, reply, _ = iemmode(ctx.env, ["status"], STATUS_S, ctx.watch(abandon=True))
        except StillRunning:
            raise
        except StepError as e:
            last = f"the read failed: {str(e)[-300:]}"
        else:
            build = reply.get("guard_build") if isinstance(reply, dict) else None
            if code == 0 and build == sha:
                return {"guard_build": build, "reads": reads, "mode": reply.get("mode"), "detail": reply.get("detail")}
            last = f"exit {code}, guard_build {build!r}"
        if time.monotonic() >= deadline:
            raise StepError(f"the guard did not name build {sha} within {HANDOVER_S} s after 'iemmode activate' "
                            f"({reads} status reads, the last: {last}): the hand-over is unverified; check 'iempc "
                            "status' and the guard's log on the PC (logs\\guard.log under PC_ROOT), never force-end")
        pause(ctx, HANDOVER_POLL_S)


def guard_processes(ctx: Ctx) -> int:
    """How many guard processes run on the PC (a read; a new flag abandons it)."""
    r = run_module(ctx.env, f"@(Get-Process -Name {ps_quote(GUARD_IMAGE)} -ErrorAction SilentlyContinue).Count",
                   STATUS_S, ctx.watch(abandon=True))
    if isinstance(r, bool) or not isinstance(r, int) or r < 0:
        raise StepError(f"the PC's count of guard processes reads {r!r}")
    return r


def await_guard_gone(ctx: Ctx) -> int:
    """After `iemmode quit`: the guard's processes every HANDOVER_POLL_S
    until none runs, at most QUIT_S. Nothing is ever ended by force."""
    deadline = time.monotonic() + QUIT_S
    reads = 0
    while True:
        reads += 1
        n = guard_processes(ctx)
        if n == 0:
            return reads
        if time.monotonic() >= deadline:
            raise StepError(f"{n} {GUARD_IMAGE} process(es) still run {QUIT_S} s after 'iemmode quit': nothing was "
                            "activated; the next iemmode call finds the guard or starts one; never force-end")
        pause(ctx, HANDOVER_POLL_S)


def bring_guard_back(ctx: Ctx) -> None:
    """After a failed offline activation no guard runs: one `iemmode status`
    starts the guard's task (bin\\ as it stands), so the PC is not left
    without one. Its own failure is reported, never raised over the first."""
    try:
        code, reply, raw = iemmode(ctx.env, ["status"], STATUS_S, ctx.watch(abandon=True))
    except StepError as e:
        emit({"guard_restart": None, "error": str(e)[-800:]})
        return
    emit(result("iemmode", ["status"], code, reply, raw))


def activate_offline(ctx: Ctx, sha: str) -> int:
    """`activate --offline` (#9 2026-09-28), for a guard too old to activate
    in event: a graceful `iemmode quit` of the running guard (skipped when
    none runs), the guard's processes read until none runs (QUIT_S), then the
    bundle's own `iemmixer-guard.exe activate <sha>` (read once from
    `bundles\\<sha>`, checked by this box's fetch record and run from the
    admin-only stage: iempc_bin.staged_guard, #15), which takes the guard's
    mutex and activates in an idle event only; then the admin-only iemmode and
    the hand-over as online (the first `iemmode status` starts the guard's
    task, which runs the new exe from bin\\). A refused or failed offline step
    starts the guard again (`iemmode status`). The quit and the offline step
    are changes: a new flag lets each finish, then the event path runs (it
    starts a guard); the reads are abandoned."""
    env = ctx.env
    want = (need_record(sha).get("sums") or {}).get("iemmixer-guard.exe")
    if not want:
        raise StepError(f"bundle {sha}'s fetch record lists no iemmixer-guard.exe: fetch it again")
    exe = pc_join(env["PC_ROOT"], f"bundles/{sha}/iemmixer-guard.exe")
    checks, then = iempc_bin.staged_guard(sys.modules[__name__], exe, want)   # run from the stage (#15)
    if guard_processes(ctx):
        code, reply, raw = iemmode(env, ["quit"], STATUS_S, ctx.watch(abandon=False))
        emit(result("iemmode", ["quit"], code, reply, raw))
        if code != 0:
            return code
        emit({"guard_stopped": {"reads": await_guard_gone(ctx)}})
    args = ["activate", sha]
    try:
        code, reply, raw = call(env, exe, args, INSTALL_S, ctx.watch(abandon=False), checks, then=then)
    except StillRunning:
        raise  # it may still hold the guard's mutex: a guard started now would only wait for it
    except StepError:
        bring_guard_back(ctx)
        raise
    emit(result("iemmixer-guard", args, code, reply, raw))
    if code != 0:
        bring_guard_back(ctx)
        return code
    iempc_bin.install_after_activate(ctx, sys.modules[__name__], sha)   # the new bins are in place (#15)
    emit({"handover": await_guard_build(ctx, sha)})
    iempc_tuning.refresh_after_activate(ctx, sys.modules[__name__], sha)
    return 0


def cmd_activate(ctx: Ctx) -> int:
    """`iemmode activate <sha>`, then the hand-over: `iemmode status` until
    the guard that answers names the SHA as its build. The guard allows it
    in dev and in an idle event (#9 2026-09-28: none of iemmixer's processes
    runs, no switch or HIL job waits; REAPER and the app are not touched), which is how a guard fix reaches a guard in event.
    The guard makes the change itself: a new flag lets the activation finish
    (then the event path runs, so "ide event" is not queued behind it on the
    guard that is about to hand over) and abandons the status reads.
    `--offline`: `activate_offline`, for a guard too old for that."""
    iempc_live.refuse_while_running(sys.modules[__name__], "activate")   # a live run or soak of this entry (#10)
    env, sha = ctx.env, check_sha(ctx.args.sha)
    refuse_open_window("activate")
    if ctx.args.offline:
        return activate_offline(ctx, sha)
    args = ["activate", sha]
    code, reply, raw = iemmode(env, args, SWITCH_S, ctx.watch(abandon=False))
    emit(result("iemmode", args, code, reply, raw))
    if code != 0:
        return code
    iempc_bin.install_after_activate(ctx, sys.modules[__name__], sha)   # the new bins are in place (#15)
    emit({"handover": await_guard_build(ctx, sha)})
    iempc_tuning.refresh_after_activate(ctx, sys.modules[__name__], sha)
    return 0


def cmd_tuning_install(ctx: Ctx) -> int:
    """S1c's tuning modules and the profile into the elevated tuning folder, then
    the bundle's iemmode.exe into the admin-only bin (iempc_tuning.py, iempc_bin.py; #15, #36)."""
    code = iempc_tuning.install(ctx, sys.modules[__name__])
    emit({"elevated_bin": ctx.args.sha, **iempc_bin.install(ctx, sys.modules[__name__], ctx.args.sha)})
    return code


def cmd_ssh_shell(ctx: Ctx) -> int:
    """The admin-only OpenSSH default shell, set, probed and confirmed; the code lives in iempc_sshshell.py (#15, #36)."""
    return iempc_sshshell.run(ctx, sys.modules[__name__])


def cmd_trace(ctx: Ctx) -> int:
    """A kernel DPC/ISR trace on the guard's engine; the code lives in iempc_trace.py (#15, #36)."""
    iempc_live.refuse_while_running(sys.modules[__name__], "trace")   # a live run or soak of this entry (#10)
    return iempc_trace.trace(ctx, sys.modules[__name__])


def load_dispatches() -> list[dict]:
    return list(read_json(state_dir() / "dispatch.json", {}).get("dispatches", []))


def cmd_dispatch_hil(ctx: Ctx) -> int:
    """Dispatches the ops hil.yml with this box's gh authentication (design
    §7): the SHA is a branch head with a green push run; branch, run and the
    attested digest come from that run; once per SHA per dev entry."""
    iempc_live.refuse_while_running(sys.modules[__name__], "dispatch-hil")   # a live run or soak of this entry (#10)
    sha = check_sha(ctx.args.sha) if ctx.args.sha else branch_head("dev")
    branch = next((b for b in BRANCHES if branch_head(b) == sha), None)
    if branch is None:
        raise Refused(f"{sha} is not the head of {' or '.join(BRANCHES)}")
    run, _ = green_run(sha, (branch,))
    entry = current_entry()
    done = load_dispatches()
    if any(d.get("sha") == sha and d.get("entry") == entry for d in done):
        raise Refused(f"HIL for {sha} was already dispatched in dev entry {entry}")
    rec, _ = fetch_bundle(sha, branch)
    if int(rec["run"]) != run:
        raise StepError(f"bundle {sha} was fetched from run {rec['run']}, the green run is {run}: remove the local "
                        "bundle and fetch again")
    check_local_zip(sha, rec)
    if event_now():  # "ide event" during the gh waits above: HIL is dev-time work
        raise Refused(f"{core.EVENT_NOW} appeared: no HIL dispatch during an event (nothing was dispatched)")
    iempc_bin.hil_dispatched(sys.modules[__name__], sha)   # the run activates `sha`: awaited from now (#15)
    core.gh(["workflow", "run", HIL_WORKFLOW, "-R", OPS_REPO, "-f", f"sha={sha}", "-f", f"branch={branch}",
        "-f", f"run={run}", "-f", f"digest={rec['digest']}"])
    record = {"sha": sha, "branch": branch, "run": run, "digest": rec["digest"], "entry": entry, "at": core.now_iso()}
    write_json(state_dir() / "dispatch.json", {"dispatches": (done + [record])[-200:]})
    emit({"dispatched": record})
    return 0


def cmd_dispatch_soak(ctx: Ctx) -> int:
    """S7 soak dispatch; the code lives in iempc_soak.py (#10, #36)."""
    iempc_live.refuse_while_live(sys.modules[__name__], "dispatch-soak")
    return iempc_soak.dispatch(ctx, sys.modules[__name__])


def cmd_switch_test(ctx: Ctx) -> int:
    """S7 switch timing; the code lives in iempc_switch.py (#10, #36)."""
    iempc_live.refuse_while_live(sys.modules[__name__], "switch-test")
    return iempc_switch.switch_test(ctx, sys.modules[__name__])


def cmd_dispatch_live(ctx: Ctx) -> int:
    """S7 live-run dispatch; the code lives in iempc_live.py (#10, #36)."""
    return iempc_live.dispatch(ctx, sys.modules[__name__])


def read_only_function(name: str) -> bool:
    return FUNCTION.fullmatch(name) is not None and name.split("-", 1)[0] in READ_ONLY_VERBS


def ps_params(params: list[str]) -> str:
    """`-Name value` pairs and `-Switch` flags for an IemPc function."""
    out = []
    for p in params:
        if p.startswith("--"):
            raise Refused(f"{p}: iempc options go before the function name")
        if PARAM_NAME.fullmatch(p):
            out.append(p)
        elif '"' in p or any(ord(c) < 32 for c in p):
            raise Refused(f"parameter value not allowed on the PC: {p!r}")
        else:
            out.append(ps_quote(p))
    return "".join(" " + p for p in out)


def cmd_bootstrap(ctx: Ctx) -> int:
    """An IemPc.psm1 function over ssh, from the fetched and verified bundle
    (the module's sha256 is checked on the PC before it is imported). Dev
    time only, read-only functions too (design §6): each run makes a folder,
    copies the module and runs it elevated on the PC. A new flag abandons a
    read-only function and lets a changing one finish."""
    env, fn = ctx.env, ctx.args.step
    if not FUNCTION.fullmatch(fn):
        raise Refused(f"not an IemPc function: {fn!r}")
    params = ps_params(ctx.args.params)
    sha = check_sha(ctx.args.sha) if ctx.args.sha else latest_record_sha()
    rec = need_record(sha)
    local, hexd = extract_member(sha, rec, "IemPc.psm1")
    pre = fin = ""
    if fn == RUNNER_FUNCTION:
        # The one-time registration token reaches the PC on stdin only, never a command line.
        token = core.gh(["api", "-X", "POST", f"repos/{OPS_REPO}/actions/runners/registration-token", "--jq", ".token"]).strip()
        if not RUNNER_TOKEN.fullmatch(token):
            raise StepError("the runner registration token has an unexpected form")
        pre = f"$env:ACTIONS_RUNNER_INPUT_TOKEN = {ps_quote(token)} ; "
        fin = "Remove-Item -Path 'Env:\\ACTIONS_RUNNER_INPUT_TOKEN' -ErrorAction SilentlyContinue"
    mode = ctx.watch(abandon=read_only_function(fn))
    rel = f"bootstrap/{sha}"
    pc_mkdir(ctx, rel, mode)
    core.scp(str(local), remote(env, f"{rel}/IemPc.psm1"), mode)
    r = run_module(env, fn + params, BOOTSTRAP_S, mode, module=pc_join(env["PC_ROOT"], f"{rel}/IemPc.psm1"),
                   module_hex=hexd, pre=pre, fin=fin)
    emit({"bootstrap": fn, "sha": sha, "result": r})
    return 0


def handover_problems(r: dict, original: int) -> list[str]:
    problems = []
    if r.get("pref") != original:
        problems.append(f"the driver's preferred buffer reads {r.get('pref')}, the original is {original}")
    if r.get("holders"):
        problems.append(f"the card is held: {r.get('holders')}")
    if r.get("spike"):
        problems.append("a spike runs")
    if r.get("task"):
        problems.append("the spike task runs")
    return problems


def cmd_handover_s1a(ctx: Ctx) -> int:
    """Closes an open S1a window whose card is free: no driver-module holder,
    no spike or spike task running, the preference reads the original. From
    then on "ide event" is the guard's (`iempc event`)."""
    sw = spike_module()
    try:
        spike_env = sw.load_env(Path(os.environ.get("SPIKE_ENV", str(Path.home() / ".config/iemmixer/asio-spike.env"))))
        state = sw.load_state()
        if state.get("closed"):
            raise Refused(f"S1a window {state.get('id')} is already closed")
        if state.get("card") != "free":
            raise Refused(f"S1a window {state.get('id')}: the card is '{state.get('card')}', not free; close it with "
                          "spike_window.py to-event")
        if sw.intent_live(state.get("in_flight")):
            raise Refused(f"S1a window {state.get('id')}: a PC step is in flight ({state['in_flight'].get('step')}): "
                          "wait for it")
        body = (f"$p = Get-SpikeBufferPref -Key {ps_quote(spike_env['PC_BUFFER_KEY'])} -Name {ps_quote(spike_env['PC_BUFFER_NAME'])} ; "
                f"$h = Get-GoldenAsioHolders -Module {ps_quote(spike_env['PC_ASIO_MODULE'])} ; "
                f"$t = Get-ScheduledTask {sw.TASK} -ErrorAction SilentlyContinue ; "
                "[pscustomobject]@{ pref = $p.value; holders = @($h); "
                "spike = @(Get-Process -Name asio_spike -ErrorAction SilentlyContinue).Count; "
                "task = [bool]($t -and $t.State -eq 'Running') }")
        r = sw.ps(spike_env, body, timeout=STATUS_S, event=ctx.watch(abandon=True))
    except sw.EventNow:
        raise EventNow() from None
    except sw.StepError as e:
        raise StepError(str(e)) from None
    problems = handover_problems(r, int(state["pref_original"]))
    if problems:
        raise StepError("S1a window stays open: " + "; ".join(problems))

    def hand_over(st: dict) -> None:
        # The state as saved now, under the window lock (F2 round 3, m5): another
        # window process may have changed it during the checks.
        if st.get("id") != state.get("id") or st.get("closed"):
            raise Refused(f"S1a window {state.get('id')} was closed meanwhile (a preempt or to-event): nothing handed over")
        if st.get("card") != "free":
            raise StepError(f"S1a window {state.get('id')}: the card is '{st.get('card')}' now, not free: the window stays open")
        if sw.intent_live(st.get("in_flight")):
            raise StepError(f"S1a window {state.get('id')}: a PC step is in flight now ({st['in_flight'].get('step')}): "
                            "the window stays open")
        st["closed"] = True
        st["handed_over"] = {"to": "iemmixer guard (S6)", "at": core.now_iso(), "checks": r}

    try:
        sw.update_state(change=hand_over)
    except sw.StepError as e:   # the window lock was not free within its bound (an owner alarm was printed)
        raise StepError(str(e)) from None
    emit({"handover-s1a": state.get("id"), "closed": True, "checks": r})
    return 0


# ---- main ----

@dataclass(frozen=True)
class Spec:
    fn: Callable[[Ctx], int]
    pc: bool        # talks to the PC: a failure after a new flag runs the event path
    dev_time: bool  # refused while the flag exists (PC changes only in dev time)
    locked: bool    # one at a time on this box


COMMANDS: dict[str, Spec] = {
    "status": Spec(cmd_status, pc=True, dev_time=False, locked=False),
    "event": Spec(cmd_event, pc=True, dev_time=False, locked=False),
    "dev": Spec(cmd_dev, pc=True, dev_time=True, locked=True),
    "rehearse-teardown": Spec(cmd_rehearse_teardown, pc=True, dev_time=True, locked=True),
    "probe-task": Spec(cmd_probe_task, pc=True, dev_time=True, locked=True),
    "fetch-bundle": Spec(cmd_fetch_bundle, pc=False, dev_time=False, locked=True),
    "install": Spec(cmd_install, pc=True, dev_time=True, locked=True),
    "activate": Spec(cmd_activate, pc=True, dev_time=True, locked=True),
    "dispatch-hil": Spec(cmd_dispatch_hil, pc=False, dev_time=True, locked=True),
    "dispatch-soak": Spec(cmd_dispatch_soak, pc=True, dev_time=True, locked=True),
    "switch-test": Spec(cmd_switch_test, pc=True, dev_time=True, locked=True),
    "dispatch-live": Spec(cmd_dispatch_live, pc=True, dev_time=True, locked=True),
    "bootstrap": Spec(cmd_bootstrap, pc=True, dev_time=True, locked=True),
    "handover-s1a": Spec(cmd_handover_s1a, pc=True, dev_time=True, locked=True),
    "tuning-install": Spec(cmd_tuning_install, pc=True, dev_time=True, locked=True),
    "trace": Spec(cmd_trace, pc=True, dev_time=True, locked=True),
    "ssh-shell": Spec(cmd_ssh_shell, pc=True, dev_time=True, locked=True),
}


def build_parser() -> argparse.ArgumentParser:
    ap = argparse.ArgumentParser(prog="iempc", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("rehearse-teardown", "probe-task", "handover-s1a", "switch-test"):
        sub.add_parser(name)
    sub.add_parser("status").add_argument("--pc", action="store_true",
                                          help="ask the guard even while the flag exists (it may start the guard)")
    sub.add_parser("event").add_argument("--dry-run", action="store_true")
    dev = sub.add_parser("dev")
    dev.add_argument("--build")
    dev.add_argument("--dry-run", action="store_true")
    sub.add_parser("fetch-bundle").add_argument("--sha", required=True)
    install = sub.add_parser("install")
    install.add_argument("--sha", required=True)
    install.add_argument("--first", action="store_true", help="no iemmode on the PC yet: the zip's own guard installs it")
    activate = sub.add_parser("activate")
    activate.add_argument("--sha", required=True, help="an installed bundle (dev, or an idle event)")
    activate.add_argument("--offline", action="store_true",
                          help="a guard too old to activate in event: quit it, activate with the bundle's own guard")
    sub.add_parser("dispatch-hil").add_argument("--sha")
    soak = sub.add_parser("dispatch-soak")
    soak.add_argument("--sha", required=True, help="the bundle the PC runs in dev (its engine's build)")
    soak.add_argument("--hours", type=int, default=iempc_soak.HOURS_DEFAULT,
                      help=f"the soak's length, {iempc_soak.HOURS_MIN} to {iempc_soak.HOURS_MAX}")
    sub.add_parser("dispatch-live").add_argument("--sha", required=True,
                                                 help="the bundle the PC runs in dev (its engine's build)")
    boot = sub.add_parser("bootstrap")
    boot.add_argument("--sha", help="the fetched bundle whose IemPc.psm1 runs (default: the newest fetched)")
    boot.add_argument("step", help="an IemPc.psm1 function, e.g. Get-IemBootstrapState")
    boot.add_argument("params", nargs=argparse.REMAINDER, help="-Name value pairs and -Switch flags")
    tuning = sub.add_parser("tuning-install")
    tuning.add_argument("--sha", required=True, help="the fetched bundle whose tuning modules and IemPc.psm1 run")
    tuning.add_argument("--profile", help="the private tuning profile (default: $TUNING_PROFILE or ~/.config/iemmixer/pc-tuning.json)")
    ssh_shell = sub.add_parser("ssh-shell")
    ssh_shell.add_argument("--sha", required=True, help="the fetched bundle whose IemSshShell.psm1 and IemPc.psm1 run")
    ssh_shell.add_argument("--dry-run", action="store_true", help="print the plan, touch nothing")
    trace = sub.add_parser("trace")
    trace.add_argument("--label", required=True, help="the run's name: 1 to 40 of a-z 0-9 -")
    trace.add_argument("--seconds", type=int, required=True, help="how long the kernel trace runs")
    trace.add_argument("--circular-mb", type=int, help="a circular kernel file of this size (a long soak)")
    trace.add_argument("--profile", help="the private tuning profile whose card and audio processors are watched")
    return ap


def preempt(env: dict[str, str]) -> int:
    try:
        code = cmd_event(Ctx(env, argparse.Namespace(dry_run=False), True))
    except StepError as e:
        print(f"iempc: event: {e}", file=sys.stderr, flush=True)
        print(OWNER_ALARM, file=sys.stderr, flush=True)
        return 1
    return PREEMPTED if code == 0 else 1


def main(argv: list[str]) -> int:
    args = build_parser().parse_args(argv)
    flag_at_start = event_now()
    try:
        env = load_env(core.env_path())
    except StepError as e:
        print(f"iempc: {e}", file=sys.stderr)
        return 1
    spec = COMMANDS[args.cmd]
    iempc_bin.NOTED.clear()   # one note per command (#15)
    try:
        if spec.dev_time and flag_at_start:
            raise Refused(f"{core.EVENT_NOW} exists: an event is on; '{args.cmd}' runs only in dev time")
        with state_lock(spec.locked):
            return spec.fn(Ctx(env, args, flag_at_start))
    except Refused as e:
        print(f"iempc: {e}", file=sys.stderr, flush=True)
        return 1
    except EventNow:
        emit({"event": "ide event (flag file)", "action": "iempc event"})
    except StepError as e:
        print(f"iempc: {e}", file=sys.stderr, flush=True)
        if args.cmd == "event":
            if not args.dry_run:
                print(OWNER_ALARM, file=sys.stderr, flush=True)
            return 1
        # A hung call or an ssh error after "ide event" arrived must still bring REAPER back.
        if not (spec.pc and event_now() and not flag_at_start):
            return 1
        emit({"event": "ide event (flag file) after a failed step", "action": "iempc event"})
    return preempt(env)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
