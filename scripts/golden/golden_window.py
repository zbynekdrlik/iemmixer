#!/usr/bin/env python3
"""S1b render-window driver on the dev box (design note §4). One subcommand
per step, in order; PC-side work runs in GoldenPc.psm1 over ssh. Site values
come only from the private env file ($GOLDEN_ENV). Never ends a process by
force; a window starts only after the owner's "event skončil"."""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

REQUIRED = (
    "PC_SSH", "PC_ROOT", "PC_ROOT_SCP", "PC_TREES", "PC_REGISTRY_KEYS", "PC_TASKS",
    "PC_ASIO_MODULE", "PC_REAPER_EXE", "PC_REAPER_HTTP", "PC_REAPER_START_TASK",
    "PC_MAIN_PROJECT", "PC_APP_EXE", "PC_APP_PROCESS", "PC_APP_HTTP", "PC_DUMMY_MODE", "RAW_DIR",
)
STEPS = ("preflight", "interlock", "save-quit", "app-stopped", "backup", "stage", "seed-res", "render", "fetch", "verify-restore", "bring-back")
CHANGING = STEPS[2:]
TASK = "iemmixer-golden"
INTERLOCK_DB10 = -500
STATE = Path(os.environ.get("GOLDEN_STATE", str(Path.home() / ".local/state/iemmixer/golden-window.json")))


class StepError(Exception):
    """A step failed; the message says what to do next."""


def load_env(path: Path) -> dict[str, str]:
    if not path.is_file():
        raise StepError(f"{path}: missing (private env, plan Task 10)")
    env: dict[str, str] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        key, sep, value = line.partition("=")
        if not sep:
            raise StepError(f"{path}: not KEY=VALUE: {key}")
        env[key.strip()] = value.strip().strip('"')
    missing = [k for k in REQUIRED if not env.get(k)]
    if missing:
        raise StepError(f"{path}: missing {', '.join(missing)}")
    if env["PC_DUMMY_MODE"] == "3":
        raise StepError("PC_DUMMY_MODE=3 is ASIO: refused")
    return env


def volatile_trees(env: dict[str, str]) -> list[str]:
    """Optional PC_VOLATILE_TREES: trees of a program that keeps running during
    the window (best-effort backup, reported, never restored)."""
    return [v.strip() for v in env.get("PC_VOLATILE_TREES", "").split(";") if v.strip()]


def source_sha_path(bundle: Path) -> Path:
    """The reviewed SHA sits next to the bundle, never inside it (the bundle
    gates refuse any file bundle.json does not list)."""
    return bundle.with_name(bundle.name + ".source-sha")


def vst_path(reaper_exe: str) -> str:
    return reaper_exe.rsplit("\\", 1)[0] + "\\Plugins\\FX"


def bring_back_wants(state: dict) -> tuple[bool, bool]:
    """(REAPER, app) to bring back: what ran before the window, never a kept app."""
    pre = state.get("pre", {})
    return bool(pre.get("reaper", True)), bool(pre.get("app", True)) and not state.get("keep_app", False)


def ps_quote(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def check_signal(text: str) -> None:
    if "event skončil" not in text.lower():
        raise StepError("--signal must quote the owner's 'event skončil' message (with its time)")


def check_order(state: dict, step: str) -> None:
    done = state["done"]
    expected = STEPS[len(done)] if len(done) < len(STEPS) else None
    if step != expected:
        raise StepError(f"step '{step}' is out of order; the next step is '{expected}'")


def undo_plan(done: list[str], started: list[str], render_running: bool) -> list[str]:
    plan: list[str] = []
    if render_running:
        plan.append("stop-render")
    if "backup" in done and "verify-restore" not in done:
        plan.append("verify-restore")
    if any(s in started for s in CHANGING) and "bring-back" not in done:
        plan.append("bring-back")
    return plan


def parse_holders(text: str) -> list[tuple[str, int]]:
    out = []
    for line in text.splitlines():
        line = line.strip()
        if line.startswith('"'):
            cells = [c.strip('"') for c in line.split('","')]
            out.append((cells[0], int(cells[1])))
    return out


def parse_meter_peaks(text: str) -> dict[int, int]:
    peaks: dict[int, int] = {}
    for line in text.splitlines():
        f = line.split("\t")
        if len(f) > 6 and f[0] == "TRACK" and f[1].isdigit() and f[1] != "0":
            peaks[int(f[1])] = int(f[6])
    return peaks


def interlock_hits(samples: list[dict[int, int]]) -> dict[int, int]:
    worst: dict[int, int] = {}
    for s in samples:
        for idx, db10 in s.items():
            worst[idx] = max(worst.get(idx, -10_000), db10)
    return {i: v for i, v in worst.items() if v > INTERLOCK_DB10}


# ---- ssh / scp (the PC is the external dependency; no unit tests below) ----

def ssh_raw(env: dict[str, str], script: str, timeout: int = 900) -> str:
    proc = subprocess.run(
        ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", env["PC_SSH"],
         "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command -"],
        input=script + "\n", text=True, capture_output=True, timeout=timeout, check=False)
    if proc.returncode != 0:
        raise StepError(f"PC command failed (exit {proc.returncode}): {proc.stderr.strip()[-1500:]}")
    return proc.stdout


def ps(env: dict[str, str], body: str, timeout: int = 900):
    """Runs `body` (single-line statements, `-Command -` reads stdin line by
    line) after importing the module. Errors are caught on the PC and come
    back as {ok: false}; -InputObject keeps one-element arrays as arrays."""
    script = "\n".join([
        "$ErrorActionPreference = 'Stop'",
        "$ProgressPreference = 'SilentlyContinue'",
        f"try {{ Import-Module (Join-Path {ps_quote(env['PC_ROOT'])} 'bin\\GoldenPc.psm1') -Force ; $r = & {{ {body} }} ; $o = [pscustomobject]@{{ ok = $true; r = $r }} }} "
        f"catch {{ $o = [pscustomobject]@{{ ok = $false; error = \"$_\" }} }} ; ConvertTo-Json -InputObject $o -Depth 8 -Compress",
    ])
    out = [line for line in ssh_raw(env, script, timeout).splitlines() if line.strip()]
    doc = json.loads(out[-1]) if out else {"ok": False, "error": "no output from the PC"}
    if not doc["ok"]:
        raise StepError(f"PC step failed: {doc['error']}")
    return doc["r"]


def scp(env: dict[str, str], src: str, dst: str) -> None:
    proc = subprocess.run(["scp", "-q", "-r", "-o", "BatchMode=yes", src, dst], capture_output=True, text=True, check=False, timeout=3600)
    if proc.returncode != 0:
        raise StepError(f"scp failed: {proc.stderr.strip()[-800:]}")


def remote(env: dict[str, str], rel: str) -> str:
    return f"{env['PC_SSH']}:{env['PC_ROOT_SCP']}/{rel}"


def pc(env: dict[str, str], rel: str) -> str:
    return ps_quote(env["PC_ROOT"] + "\\" + rel.replace("/", "\\"))


# ---- state ----

def load_state() -> dict:
    if not STATE.is_file():
        raise StepError("no open window: run 'new --signal ...' first")
    return json.loads(STATE.read_text(encoding="utf-8"))


def save_state(state: dict) -> None:
    STATE.parent.mkdir(parents=True, exist_ok=True)
    tmp = STATE.with_suffix(".tmp")
    tmp.write_text(json.dumps(state, indent=1), encoding="utf-8")
    tmp.replace(STATE)


def raw_dir(env: dict[str, str], state: dict) -> Path:
    d = Path(env["RAW_DIR"]).expanduser() / state["id"]
    d.mkdir(parents=True, exist_ok=True)
    os.chmod(d, 0o700)
    return d


# ---- steps ----

def step_preflight(env, state, args):
    r = ps(env, " ; ".join([
        f"$v = (Get-Item -LiteralPath {ps_quote(env['PC_REAPER_EXE'])}).VersionInfo.FileVersion",
        f"$h = Get-GoldenAsioHolders -Module {ps_quote(env['PC_ASIO_MODULE'])}",
        f"$ri = @(Get-CimInstance Win32_Process -Filter \"Name = 'reaper.exe'\" | Where-Object {{ $_.CommandLine -and $_.CommandLine.Contains({ps_quote(env['PC_ROOT'])}) }}).Count",
        f"[pscustomobject]@{{ version = $v; reaper = @(Get-Process reaper -EA SilentlyContinue).Count; app = @(Get-Process {ps_quote(env['PC_APP_PROCESS'])} -EA SilentlyContinue).Count; holders = @($h); render_instances = $ri; free_gb = [math]::Floor((Get-PSDrive C).Free / 1GB); task = [bool](Get-ScheduledTask -TaskName {TASK} -EA SilentlyContinue) }}",
    ]))
    problems = []
    if not str(r["version"]).startswith("7.65"):
        problems.append(f"REAPER version {r['version']} (expected 7.65)")
    if r["render_instances"]:
        problems.append("a render instance runs")
    if any(not h.lower().startswith("reaper.exe:") for h in r["holders"] or []):
        problems.append(f"unexpected ASIO module holders {r['holders']}")
    if r["free_gb"] < 10:
        problems.append(f"only {r['free_gb']} GB free")
    if not r["task"]:
        problems.append(f"task {TASK} not registered (run setup)")
    if problems:
        raise StepError("; ".join(problems))
    state["pre"] = {"reaper": r["reaper"] > 0, "app": r["app"] > 0}
    return r


def step_interlock(env, state, args):
    if not state["pre"]["reaper"]:
        return {"skipped": "REAPER was not running before the window"}
    texts = ps(env, f"Get-GoldenMeterSamples -Http {ps_quote(env['PC_REAPER_HTTP'])} -Seconds 60", timeout=180)
    hits = interlock_hits([parse_meter_peaks(t) for t in texts])
    if hits:
        raise StepError(f"band activity: peaks above -50 dBFS on tracks {sorted(hits)}; window aborted, alarm the owner")
    return {"samples": len(texts), "quiet": True}


def step_save_quit(env, state, args):
    if not state["pre"]["reaper"]:
        return {"skipped": "REAPER was not running"}
    return ps(env, f"Invoke-GoldenSaveQuit -Http {ps_quote(env['PC_REAPER_HTTP'])} -Project {ps_quote(env['PC_MAIN_PROJECT'])} -AsioModule {ps_quote(env['PC_ASIO_MODULE'])}")


def step_app_stopped(env, state, args):
    if state.get("keep_app"):
        return {"skipped": "the predecessor app stays running in this window (new --keep-app)"}
    return ps(env, f"Wait-GoldenProcessGone -Name {ps_quote(env['PC_APP_PROCESS'])} -Seconds 30; [pscustomobject]@{{ app = 'gone' }}")


def step_backup(env, state, args):
    keys = ",".join(ps_quote(k) for k in env["PC_REGISTRY_KEYS"].split(";"))
    tasks = ",".join(ps_quote(t) for t in env["PC_TASKS"].split(";"))
    vol = ",".join(ps_quote(v) for v in volatile_trees(env))
    r = ps(env, f"$roots = Get-GoldenTrees -Path {pc(env, 'bin/trees.json')} ; Invoke-GoldenBackup -Roots $roots -Dest {pc(env, 'backups/' + state['id'])} -RegistryKeys @({keys}) -TaskNames @({tasks}) -VolatileRoots @({vol})", timeout=1800)
    scp(env, remote(env, f"backups/{state['id']}/manifest.json"), str(raw_dir(env, state) / "backup-manifest.json"))
    return r


def step_stage(env, state, args):
    bundle = Path(args.bundle)
    if source_sha_path(bundle).read_text(encoding="utf-8").strip() != args.sha:
        raise StepError("bundle .source-sha differs from --sha (P5: only the reviewed dev commit's bundle)")
    check = subprocess.run([sys.executable, str(Path(__file__).with_name("check_bundle.py")), str(bundle)], capture_output=True, text=True, check=False)
    if check.returncode != 0:
        raise StepError(f"bundle check failed:\n{check.stdout}")
    ps(env, f"New-Item -ItemType Directory -Force -Path {pc(env, 'jobs/' + state['id'])} | Out-Null ; 'ok'")
    scp(env, str(bundle), remote(env, f"jobs/{state['id']}/bundle"))
    staged = ps(env, f"Invoke-GoldenStage -Bundle {pc(env, 'jobs/' + state['id'] + '/bundle')} -Job {pc(env, 'jobs/' + state['id'] + '/run')}", timeout=900)
    state["projects"] = staged
    (raw_dir(env, state) / "bundle.json").write_bytes((bundle / "bundle.json").read_bytes())
    return {"projects": len(staged), "sha": args.sha}


def step_seed_res(env, state, args):
    return ps(env, f"New-GoldenResourceDir -Path {pc(env, 'jobs/' + state['id'] + '/res')} -MainResource ([Environment]::ExpandEnvironmentVariables('%APPDATA%\\REAPER')) -DummyMode {int(env['PC_DUMMY_MODE'])} -Rate 96000 -VstPath {ps_quote(vst_path(env['PC_REAPER_EXE']))}")


def step_render(env, state, args):
    projects = state["projects"]
    if args.only:
        wanted = set(args.only.split(","))
        projects = [p for p in projects if Path(p.replace("\\", "/")).stem in wanted]
        if not projects:
            raise StepError(f"no staged project matches {sorted(wanted)}")
    ini = env["PC_ROOT"] + f"\\jobs\\{state['id']}\\res\\reaper.ini"
    listed = ",".join(ps_quote(p) for p in projects)
    rid = ps(env, f"Remove-Item -LiteralPath {pc(env, 'queue/stop')} -ErrorAction SilentlyContinue ; $id = Write-GoldenRequest -Root {ps_quote(env['PC_ROOT'])} -Kind 'render' -Fields @{{ reaper = {ps_quote(env['PC_REAPER_EXE'])}; ini = {ps_quote(ini)}; projects = @({listed}); timeout = 600 }} ; Start-ScheduledTask -TaskName {TASK} ; $id")
    state.setdefault("requests", []).append(rid)
    save_state(state)
    r = ps(env, f"Watch-GoldenRender -Root {ps_quote(env['PC_ROOT'])} -RequestId {ps_quote(rid)} -AsioModule {ps_quote(env['PC_ASIO_MODULE'])} -TimeoutSec {600 * len(projects) + 60}", timeout=600 * len(projects) + 300)
    if r["outcome"] == "asio-alarm":
        raise StepError(f"ASIO module loaded by {r['holders']}: queue stopped. Next: preempt (restore + bring-back), alarm the owner, post on #4")
    if r["outcome"] != "done":
        raise StepError(f"render outcome {r['outcome']}: inspect the dialog via MCP, then close-render; never kill")
    return r


def step_fetch(env, state, args):
    dest = raw_dir(env, state) / "renders"
    if dest.exists():
        raise StepError(f"{dest} exists")
    scp(env, remote(env, f"jobs/{state['id']}/run/out"), str(dest))
    listing = ps(env, f"Get-GoldenFileHashes -Path {pc(env, 'jobs/' + state['id'] + '/run/out')}", timeout=1800)
    bad = [f["rel"] for f in listing if hashlib.sha256((dest / f["rel"]).read_bytes()).hexdigest() != f["sha256"]]
    if bad:
        raise StepError(f"fetched renders differ from the PC: {bad[:5]}")
    (dest.parent / "renders-sha256.json").write_text(json.dumps(listing, indent=1), encoding="utf-8")
    return {"files": len(listing), "bytes": sum(f["size"] for f in listing)}


def step_verify_restore(env, state, args):
    r = ps(env, f"Invoke-GoldenVerify -Backup {pc(env, 'backups/' + state['id'])} -Restore", timeout=1800)
    (raw_dir(env, state) / "verify.json").write_text(json.dumps(r, indent=1), encoding="utf-8")
    if not r["identical"]:
        raise StepError("NOT identical after restore: post verify.json facts on #4; no further window until explained")
    return {"identical": True, "restored": len(r["restored"]), "quarantined": len(r["quarantined"]), "touched": len(r["files"]["touched"])}


def step_bring_back(env, state, args):
    reaper, app = bring_back_wants(state)
    want_reaper = "$true" if reaper else "$false"
    want_app = "$true" if app else "$false"
    if state.get("keep_app"):
        # Read-only: the kept app must still answer; it is never restarted here.
        code = ps(env, f"Test-GoldenHttp -Uri {ps_quote(env['PC_APP_HTTP'])}")
        if not (isinstance(code, int) and 0 < code < 500):
            raise StepError(f"the kept predecessor app does not answer (HTTP {code}): alarm the owner")
    return ps(env, f"Invoke-GoldenBringBack -Root {ps_quote(env['PC_ROOT'])} -StartTask {ps_quote(env['PC_REAPER_START_TASK'])} -Http {ps_quote(env['PC_REAPER_HTTP'])} -AppExe {ps_quote(env['PC_APP_EXE'])} -AppProcess {ps_quote(env['PC_APP_PROCESS'])} -AppHttp {ps_quote(env['PC_APP_HTTP'])} -WantReaper {want_reaper} -WantApp {want_app}", timeout=300)


STEP_FUNCS = {
    "preflight": step_preflight, "interlock": step_interlock, "save-quit": step_save_quit, "app-stopped": step_app_stopped,
    "backup": step_backup, "stage": step_stage, "seed-res": step_seed_res, "render": step_render, "fetch": step_fetch,
    "verify-restore": step_verify_restore, "bring-back": step_bring_back,
}


def run_step(env, name: str, args) -> None:
    state = load_state()
    check_order(state, name)
    state.setdefault("started", []).append(name)
    save_state(state)
    result = STEP_FUNCS[name](env, state, args)
    state["done"].append(name)
    save_state(state)
    print(json.dumps({"step": name, "window": state["id"], "result": result}, ensure_ascii=False))


def cmd_new(env, args) -> None:
    check_signal(args.signal)
    if STATE.is_file():
        old = json.loads(STATE.read_text(encoding="utf-8"))
        if old.get("started") and "bring-back" not in old.get("done", []):
            raise StepError(f"window {old['id']} is still open: finish it or run preempt")
    wid = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    save_state({"id": wid, "signal": args.signal, "keep_app": bool(args.keep_app), "pre": {}, "started": [], "done": [], "projects": []})
    print(wid)


def cmd_setup(env, args) -> None:
    here = Path(__file__).resolve().parent
    ssh_raw(env, f"New-Item -ItemType Directory -Force -Path {', '.join(pc(env, d) for d in ('bin', 'queue', 'status', 'backups', 'jobs'))} | Out-Null")
    for f in ("GoldenPc.psm1", "golden-task.ps1"):
        scp(env, str(here / f), remote(env, f"bin/{f}"))
    scp(env, env["PC_TREES"], remote(env, "bin/trees.json"))
    ps(env, f"Register-GoldenTask -Root {ps_quote(env['PC_ROOT'])} -Name {TASK} ; [pscustomobject]@{{ task = 'registered' }}")
    print("setup: module uploaded, task registered")


def cmd_audiocfg(env, args) -> None:
    state = load_state()
    ini = ps(env, f"New-GoldenResourceDir -Path {pc(env, 'jobs/' + state['id'] + '/res-audiocfg')} -MainResource ([Environment]::ExpandEnvironmentVariables('%APPDATA%\\REAPER')) -DummyMode {int(env['PC_DUMMY_MODE'])} -Rate 96000 -VstPath {ps_quote(vst_path(env['PC_REAPER_EXE']))}")
    rid = ps(env, f"$id = Write-GoldenRequest -Root {ps_quote(env['PC_ROOT'])} -Kind 'audiocfg' -Fields @{{ reaper = {ps_quote(env['PC_REAPER_EXE'])}; ini = {ps_quote(ini)} }} ; Start-ScheduledTask -TaskName {TASK} ; $id")
    print(json.dumps({"audiocfg": rid, "ini": ini, "next": "MCP: read/select 'Dummy Audio' at 96000 Hz, OK, File > Quit; then read-mode"}))


def cmd_read_mode(env, args) -> None:
    state = load_state()
    r = ps(env, f"$t = Get-Content -LiteralPath {pc(env, 'jobs/' + state['id'] + '/res-audiocfg/reaper.ini')} ; [pscustomobject]@{{ mode = (($t | Where-Object {{ $_ -like 'mode=*' }}) -join ';'); asio = @($t | Where-Object {{ $_ -like 'asio*' }}).Count }}")
    print(json.dumps(r))


def cmd_close_render(env, args) -> None:
    state = load_state()
    ini = env["PC_ROOT"] + f"\\jobs\\{state['id']}\\res\\reaper.ini"
    rid = ps(env, f"$id = Write-GoldenRequest -Root {ps_quote(env['PC_ROOT'])} -Kind 'close-render' -Fields @{{ ini = {ps_quote(ini)} }} ; Start-ScheduledTask -TaskName {TASK} ; $id")
    print(json.dumps({"close-render": rid}))


def cmd_preempt(env, args) -> None:
    state = load_state()
    running = ps(env, f"@(Get-CimInstance Win32_Process -Filter \"Name = 'reaper.exe'\" | Where-Object {{ $_.CommandLine -and $_.CommandLine.Contains({ps_quote(env['PC_ROOT'])}) }}).Count") or 0
    plan = undo_plan(state["done"], state.get("started", []), bool(running))
    print(json.dumps({"preempt": state["id"], "plan": plan}))
    if "stop-render" in plan:
        ps(env, f"New-Item -ItemType File -Force -Path {pc(env, 'queue/stop')} | Out-Null ; $d = (Get-Date).AddSeconds(600) ; while ((Get-Date) -lt $d -and @(Get-CimInstance Win32_Process -Filter \"Name = 'reaper.exe'\" | Where-Object {{ $_.CommandLine -and $_.CommandLine.Contains({ps_quote(env['PC_ROOT'])}) }}).Count -gt 0) {{ Start-Sleep -Seconds 1 }} ; 'waited'", timeout=700)
    if "verify-restore" in plan:
        print(json.dumps(step_verify_restore(env, state, args)))
        state["done"].append("verify-restore")
    if "bring-back" in plan:
        print(json.dumps(step_bring_back(env, state, args)))
        state["done"].append("bring-back")
    state["preempted"] = True
    save_state(state)


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)
    new = sub.add_parser("new")
    new.add_argument("--signal", required=True)
    new.add_argument("--keep-app", action="store_true", help="leave the predecessor app running (no tray Exit in this window)")
    for name in ("setup", "audiocfg", "read-mode", "close-render", "preempt", "status", *STEPS):
        if name == "stage":
            p = sub.add_parser(name)
            p.add_argument("--bundle", required=True)
            p.add_argument("--sha", required=True)
        elif name == "render":
            sub.add_parser(name).add_argument("--only")
        elif name != "new":
            sub.add_parser(name)
    args = ap.parse_args(argv)
    try:
        if args.cmd == "status":
            print(json.dumps(load_state(), indent=1))
            return 0
        env = load_env(Path(os.environ.get("GOLDEN_ENV", str(Path.home() / ".config/iemmixer/golden.env"))))
        handlers = {"new": cmd_new, "setup": cmd_setup, "audiocfg": cmd_audiocfg, "read-mode": cmd_read_mode, "close-render": cmd_close_render, "preempt": cmd_preempt}
        if args.cmd in handlers:
            handlers[args.cmd](env, args)
        else:
            run_step(env, args.cmd, args)
        return 0
    except StepError as e:
        print(f"golden_window: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
