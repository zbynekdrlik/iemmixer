"""The bundle path of iempc.py (P5/G8, S6 design note §5.1, §7; split out of
iempc.py, #36): `fetch-bundle` (a green push run of ci.yml on dev or main
with its bundle and attest jobs; the zip downloaded, checked against its
SHA256SUMS and manifest and by `gh attestation verify`; the fetch record),
a fetched bundle's members (`extract_member`), `install` (the verified zip to
the PC, then `iemmode install`, or the zip's own guard for the first bundle),
`activate` (then the hand-over: `iemmode status` until the guard names the
SHA; `--offline` for a guard too old to activate in event) and
`dispatch-hil` (the ops hil.yml, once per SHA per dev entry).

iempc.py passes itself in (`ip`) for the siblings. The names a test patches
here (HANDOVER_S, HANDOVER_POLL_S, QUIT_S) live in this module only; the
shared ones are read as `core.<name>` (iempc_core.py)."""
from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import time
import zipfile
import zlib
from pathlib import Path

import iempc_bin
import iempc_core as core
import iempc_live
import iempc_tuning
from iempc_core import (BRANCHES, INSTALL_S, OPS_REPO, REPO, SHA, STATUS_S, SWITCH_S, Ctx, Refused, StepError,
                        StillRunning, call, check_sha, current_entry, emit, event_now, hash_check, iemmode, pause,
                        pc_join, pc_mkdir, ps_quote, read_json, refuse_open_window, remote, result, run_module,
                        state_dir, write_json)

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


def stage_modules(ctx: Ctx, sha: str, rec: dict, stage, event: str) -> tuple[str, dict[str, str]]:
    """The bundle's modules `stage` names, as (bundle member, stage name)
    pairs, the ones the last one imports from its own folder first (#15
    ssh-shell, #11 cutover): each checked again against the zip's sums and
    uploaded by scp into bootstrap/<sha> of the PC's root. Returns the
    statements that stage them admin-only on the PC in that order and import
    the last one from its stage copy only (module_script's `pre`,
    elevated_ps.staged), and each one's sha256 by stage name."""
    ep = core.elevated_ps()
    rel = f"bootstrap/{sha}"
    uploads, mods, sums = [], [], {}
    for member, name in stage:
        local, hexd = extract_member(sha, rec, member, nested="/" in member)
        uploads.append((local, name))
        mods.append((core.ps_quote(core.pc_join(ctx.env["PC_ROOT"], f"{rel}/{name}")), name, hexd))
        sums[name] = hexd
    core.pc_mkdir(ctx, rel, event)
    for local, name in uploads:
        core.scp(str(local), core.remote(ctx.env, f"{rel}/{name}"), event)
    return f"{ep.staged(mods)} ; Import-Module $iemMod -Force ; ", sums


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

def cmd_fetch_bundle(ctx: Ctx) -> int:
    rec, fetched = fetch_bundle(check_sha(ctx.args.sha))
    emit({"bundle": str(zip_path(rec["sha"])), "fetched": fetched,
          **{k: rec[k] for k in ("sha", "branch", "run", "digest")}, "files": sorted(rec["sums"])})
    return 0


def cmd_install(ctx: Ctx, ip) -> int:
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
        guard, then = iempc_bin.staged_guard(ip, exe, hexd)   # run from the stage (#15)
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


# The bundle's guard keeps the S8 lifecycle (manifest.json `guard_lifecycle`,
# the guard's bundle::GUARD_LIFECYCLE; S8 lane 5).
GUARD_LIFECYCLE = 1


def keeps_lifecycle(manifest) -> bool:
    """The manifest names `guard_lifecycle` GUARD_LIFECYCLE or later (pure)."""
    v = manifest.get("guard_lifecycle") if isinstance(manifest, dict) else None
    return isinstance(v, int) and not isinstance(v, bool) and v >= GUARD_LIFECYCLE


def refuse_older_guard(ctx: Ctx, sha: str, rec: dict) -> None:
    """Offline the bundle's OWN guard activates, so the running guard's
    refusal (S8 lane 5: in prod no activation of a guard that would drop the
    lifecycle) never runs for a bundle built before `guard_lifecycle`. Such a
    bundle is refused here, before any quit, unless `iemmode status` (a
    read; a new flag abandons it) says the PC is before the cutover; an
    unreadable lifecycle refuses too."""
    path, _ = extract_member(sha, rec, "manifest.json")
    try:
        manifest = json.loads(path.read_text(encoding="utf-8-sig"))
    except (OSError, ValueError) as e:
        raise StepError(f"bundle {sha}'s manifest.json is unreadable ({type(e).__name__})") from None
    if keeps_lifecycle(manifest):
        return
    code, reply, raw = iemmode(ctx.env, ["status"], STATUS_S, ctx.watch(abandon=True))
    emit(result("iemmode", ["status"], code, reply, raw))
    state = core.guard_lifecycle(reply) if code == 0 else None
    if state != "trial":
        raise Refused(f"bundle {sha}'s guard predates the lifecycle (its manifest names no guard_lifecycle), and the "
                      f"guard is {state or 'unreadable'} (status exit {code}): offline its own guard would activate "
                      "and drop prod; activate a bundle that names guard_lifecycle")


def activate_offline(ctx: Ctx, ip, sha: str) -> int:
    """`activate --offline` (#9 2026-09-28), for a guard too old to activate
    in event: a graceful `iemmode quit` of the running guard (skipped when
    none runs), the guard's processes read until none runs (QUIT_S), then the
    bundle's own `iemmixer-guard.exe activate <sha>` (read once from
    `bundles\\<sha>`, checked by this box's fetch record and run from the
    admin-only stage: iempc_bin.staged_guard, #15), which takes the guard's
    mutex and activates in an idle event only; then the admin-only iemmode and
    the hand-over as online (the first `iemmode status` starts the guard's
    task, which runs the new exe from bin\\). A refused or failed offline step
    starts the guard again (`iemmode status`). After the cutover a bundle
    built before `guard_lifecycle` is refused first (`refuse_older_guard`,
    S8 lane 5). The quit and the offline step
    are changes: a new flag lets each finish, then the event path runs (it
    starts a guard); the reads are abandoned."""
    env = ctx.env
    rec = need_record(sha)
    want = (rec.get("sums") or {}).get("iemmixer-guard.exe")
    if not want:
        raise StepError(f"bundle {sha}'s fetch record lists no iemmixer-guard.exe: fetch it again")
    refuse_older_guard(ctx, sha, rec)
    exe = pc_join(env["PC_ROOT"], f"bundles/{sha}/iemmixer-guard.exe")
    checks, then = iempc_bin.staged_guard(ip, exe, want)   # run from the stage (#15)
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
    iempc_bin.install_after_activate(ctx, ip, sha)   # the new bins are in place (#15)
    emit({"handover": await_guard_build(ctx, sha)})
    iempc_tuning.refresh_after_activate(ctx, ip, sha)
    return 0


def cmd_activate(ctx: Ctx, ip) -> int:
    """`iemmode activate <sha>`, then the hand-over: `iemmode status` until
    the guard that answers names the SHA as its build. The guard allows it
    in dev and in an idle event (#9 2026-09-28: none of iemmixer's processes
    runs, no switch or HIL job waits; REAPER and the app are not touched), which is how a guard fix reaches a guard in event.
    The guard makes the change itself: a new flag lets the activation finish
    (then the event path runs, so "ide event" is not queued behind it on the
    guard that is about to hand over) and abandons the status reads.
    `--offline`: `activate_offline`, for a guard too old for that."""
    iempc_live.refuse_while_running(ip, "activate")   # a live run or soak of this entry (#10)
    env, sha = ctx.env, check_sha(ctx.args.sha)
    refuse_open_window("activate")
    if ctx.args.offline:
        return activate_offline(ctx, ip, sha)
    args = ["activate", sha]
    code, reply, raw = iemmode(env, args, SWITCH_S, ctx.watch(abandon=False))
    emit(result("iemmode", args, code, reply, raw))
    if code != 0:
        return code
    iempc_bin.install_after_activate(ctx, ip, sha)   # the new bins are in place (#15)
    emit({"handover": await_guard_build(ctx, sha)})
    iempc_tuning.refresh_after_activate(ctx, ip, sha)
    return 0


def load_dispatches() -> list[dict]:
    return list(read_json(state_dir() / "dispatch.json", {}).get("dispatches", []))


def cmd_dispatch_hil(ctx: Ctx, ip) -> int:
    """Dispatches the ops hil.yml with this box's gh authentication (design
    §7): the SHA is a branch head with a green push run; branch, run and the
    attested digest come from that run; once per SHA per dev entry."""
    iempc_live.refuse_while_running(ip, "dispatch-hil")   # a live run or soak of this entry (#10)
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
    iempc_bin.hil_dispatched(ip, sha)   # the run activates `sha`: awaited from now (#15)
    core.gh(["workflow", "run", HIL_WORKFLOW, "-R", OPS_REPO, "-f", f"sha={sha}", "-f", f"branch={branch}",
        "-f", f"run={run}", "-f", f"digest={rec['digest']}"])
    record = {"sha": sha, "branch": branch, "run": run, "digest": rec["digest"], "entry": entry, "at": core.now_iso()}
    write_json(state_dir() / "dispatch.json", {"dispatches": (done + [record])[-200:]})
    emit({"dispatched": record})
    return 0
