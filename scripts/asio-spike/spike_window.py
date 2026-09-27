#!/usr/bin/env python3
"""S1a ASIO-spike window driver on the dev box (design note §5); also the
interim switch between REAPER and iemmixer until S6's `iemmode`.

A window opens only with the owner's quoted "event skončil" and never while
the "ide event" flag file exists. Every wait checks that flag every 2 s; when
it appears the driver stops the spike through its stop file, restores the
driver's preferred buffer (read back) and brings REAPER back with the handover
checks (`preempt`). PC work runs in SpikePc.psm1 over ssh; site values come
only from the private env file ($SPIKE_ENV). Nothing is ever ended by force."""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "golden"))
from golden_window import StepError, check_signal, interlock_hits, parse_meter_peaks, ps_quote  # noqa: E402

REQUIRED = (
    "PC_SSH", "PC_ROOT", "PC_ROOT_SCP", "PC_ASIO_MODULE", "PC_ASIO_DRIVER",
    "PC_BUFFER_KEY", "PC_BUFFER_NAME", "PC_BUFFER_ORIGINAL",
    "PC_REAPER_HTTP", "PC_MAIN_PROJECT", "PC_REAPER_START_TASK_PATH", "PC_REAPER_START_TASK",
    "PC_NTRACK", "PC_METER_BRIDGE", "PC_METER_HEARTBEAT", "PC_METER_ACTION",
    "PC_APP_PROCESS", "PC_APP_HTTP", "RAW_DIR",
)
FRAMES = (32, 48, 64)
POLL_S = 2.0
REPO = "zbynekdrlik/iemmixer"
BUNDLE_FILES = ("GoldenPc.psm1", "SpikePc.psm1", "asio_spike.exe", "spike-task.ps1")
TASK = "-TaskPath '\\iemmixer\\' -TaskName 'iemmixer-asio-spike'"
STATE = Path(os.environ.get("SPIKE_STATE", str(Path.home() / ".local/state/iemmixer/spike-window.json")))
EVENT_NOW = Path(os.environ.get("IEMMIXER_EVENT_NOW", str(Path.home() / ".config/iemmixer/EVENT-NOW")))


class EventNow(Exception):
    """The owner said "ide event" (the flag file exists): pre-empt."""


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
    for k in ("PC_BUFFER_ORIGINAL", "PC_NTRACK"):
        if not env[k].isdigit():
            raise StepError(f"{path}: {k} must be a whole number")
    return env


def event_now() -> bool:
    return EVENT_NOW.exists()


def check_request(mode: str, frames: int | None, seconds: int, burn_us: int, stress: int, cycles: int) -> None:
    if mode not in ("probe", "duplex", "reopen"):
        raise StepError(f"unknown mode {mode}")
    if mode != "probe" and frames not in FRAMES:
        raise StepError(f"--frames must be one of {FRAMES}")
    if not (1 <= seconds <= 3600 and 0 <= burn_us <= 300 and 0 <= stress <= 8 and 1 <= cycles <= 20):
        raise StepError("limits: seconds 1..3600, burn-us 0..300, stress 0..8, cycles 1..20")


def run_timeout(mode: str, seconds: int, cycles: int) -> int:
    """Seconds after which the PC task writes the stop file itself."""
    return {"probe": 60, "duplex": seconds + 60, "reopen": 30 * cycles + 60}[mode]


def buffer_changed(state: dict) -> bool:
    return state.get("pref_current") not in (None, state.get("pref_original")) and not state.get("pref_restored")


def undo_plan(state: dict, spike_running: bool) -> list[str]:
    """What leaving the window (or "ide event") must do, in order."""
    plan: list[str] = []
    if spike_running:
        plan.append("stop-spike")
    if buffer_changed(state):
        plan.append("restore-buffer")
    if state.get("card") in ("switching", "free"):
        plan.append("bring-back")
    return plan


def preflight_problems(r: dict, original: int) -> list[str]:
    problems = []
    if r["pref"] != original:
        problems.append(f"the driver's preferred buffer is {r['pref']}, the recorded original is {original}: stop and tell the owner")
    if not r["reaper"]:
        problems.append("REAPER is not running (a window starts from the event state)")
    if not r["app"]:
        problems.append("the predecessor app is not running")
    if r["spike"]:
        problems.append("a spike already runs")
    if any(not h.lower().startswith("reaper.exe:") for h in r["holders"] or []):
        problems.append(f"unexpected ASIO module holders {r['holders']}")
    if not r["task"]:
        problems.append("the spike task is not registered (run setup)")
    if r["files"] != len(BUNDLE_FILES):
        problems.append(f"{r['files']} verified bundle files on the PC, expected {len(BUNDLE_FILES)}")
    return problems


def ps_hashtable(fields: dict) -> str:
    parts = []
    for k, v in fields.items():
        if not re.fullmatch(r"[a-z_]+", k):
            raise StepError(f"bad request field {k!r}")
        parts.append(f"{k} = {v}" if isinstance(v, int) and not isinstance(v, bool) else f"{k} = {ps_quote(str(v))}")
    return "@{ " + "; ".join(parts) + " }"


def parse_sums(text: str) -> dict[str, str]:
    sums: dict[str, str] = {}
    for line in text.splitlines():
        if not line.strip():
            continue
        m = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.-]+)", line)
        if not m:
            raise StepError(f"malformed SHA256SUMS line: {line!r}")
        sums[m.group(2)] = m.group(1)
    return sums


def verify_bundle(bundle: Path) -> list[str]:
    sums = parse_sums((bundle / "SHA256SUMS").read_text(encoding="utf-8"))
    if sorted(sums) != sorted(BUNDLE_FILES):
        raise StepError(f"bundle lists {sorted(sums)}, expected {sorted(BUNDLE_FILES)}")
    for name, sha in sums.items():
        if hashlib.sha256((bundle / name).read_bytes()).hexdigest() != sha:
            raise StepError(f"bundle file {name} does not match SHA256SUMS")
    return sorted(sums)


def pick_run(runs: list[dict], sha: str) -> int:
    """The successful push run on dev for exactly `sha` (P5)."""
    for r in runs:
        if (r.get("headSha"), r.get("event"), r.get("headBranch"), r.get("conclusion")) == (sha, "push", "dev", "success"):
            return int(r["databaseId"])
    raise StepError(f"no successful push run on dev for {sha}")


def verdict(report: dict) -> dict:
    """Stable = ended as planned with no missed period, no overrun, no
    position gap and no driver reset."""
    tel = [s.get("telemetry") or {} for s in report.get("segments", [])]
    total = {k: sum(t.get(k, 0) for t in tel) for k in ("callbacks", "late", "missed", "overruns", "position_gaps")}
    resets = sum((t.get("messages") or {}).get("resets", 0) for t in tel)
    worst = max(((t.get("interval_us") or {}).get("p999", 0.0) for t in tel), default=0.0)
    stable = report.get("outcome") == "done" and bool(tel) and resets == 0 and all(
        total[k] == 0 for k in ("missed", "overruns", "position_gaps"))
    return {"outcome": report.get("outcome"), "stable": stable, "resets": resets, "interval_p999_us": worst, **total}


def guarded(cmd: list[str], stdin: str, timeout: float, event: str) -> str:
    """Runs `cmd` and checks the "ide event" flag every POLL_S seconds.
    event="abandon": a read-only call is left to end by itself and EventNow
    is raised at once; "finish": a changing call completes, then EventNow;
    "ignore": the pre-emption itself. Never kills anything."""
    proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    deadline = time.monotonic() + timeout
    seen = False
    data: str | None = stdin
    while True:
        try:
            out, err = proc.communicate(data, timeout=POLL_S)
            break
        except subprocess.TimeoutExpired:
            data = None  # already handed over; a retry must not send it again
            if event != "ignore" and event_now():
                seen = True
                if event == "abandon":
                    raise EventNow() from None
            if time.monotonic() > deadline:
                raise StepError(f"PC call still running after {timeout} s (bounded on the PC; check it, never kill)") from None
    if proc.returncode != 0:
        raise StepError(f"PC command failed (exit {proc.returncode}): {err.strip()[-1500:]}")
    if event != "ignore" and (seen or event_now()):
        raise EventNow()
    return out


# ---- PC access (the PC is the external dependency; no unit tests below) ----

def ssh_cmd(env: dict[str, str]) -> list[str]:
    return ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", env["PC_SSH"],
            "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command -"]


def ps(env: dict[str, str], body: str, timeout: float = 300, event: str = "finish"):
    """Runs `body` after importing SpikePc (single-line statements: `-Command -`
    reads stdin line by line); PC errors come back as {ok: false}."""
    script = "\n".join([
        "$ErrorActionPreference = 'Stop'",
        "$ProgressPreference = 'SilentlyContinue'",
        f"try {{ Import-Module (Join-Path {ps_quote(env['PC_ROOT'])} 'bin\\SpikePc.psm1') -Force ; $r = & {{ {body} }} ; $o = [pscustomobject]@{{ ok = $true; r = $r }} }} "
        f"catch {{ $o = [pscustomobject]@{{ ok = $false; error = \"$_\" }} }} ; ConvertTo-Json -InputObject $o -Depth 8 -Compress",
    ])
    out = [line for line in guarded(ssh_cmd(env), script + "\n", timeout, event).splitlines() if line.strip()]
    doc = json.loads(out[-1]) if out else {"ok": False, "error": "no output from the PC"}
    if not doc["ok"]:
        raise StepError(f"PC step failed: {doc['error']}")
    return doc["r"]


def scp(src: str, dst: str) -> None:
    proc = subprocess.run(["scp", "-q", "-o", "BatchMode=yes", src, dst], capture_output=True, text=True, check=False, timeout=600)
    if proc.returncode != 0:
        raise StepError(f"scp failed: {proc.stderr.strip()[-800:]}")


def remote(env: dict[str, str], rel: str) -> str:
    return f"{env['PC_SSH']}:{env['PC_ROOT_SCP']}/{rel}"


def pc(env: dict[str, str], rel: str) -> str:
    return ps_quote(env["PC_ROOT"] + "\\" + rel.replace("/", "\\"))


def raw_dir(env: dict[str, str], state: dict) -> Path:
    d = Path(env["RAW_DIR"]).expanduser() / "asio-spike" / state["id"]
    d.mkdir(parents=True, exist_ok=True)
    os.chmod(d, 0o700)
    return d


def bundle_dir(env: dict[str, str], sha: str) -> Path:
    return Path(env["RAW_DIR"]).expanduser() / "asio-spike" / "bundles" / sha


# ---- state ----

def load_state() -> dict:
    if not STATE.is_file():
        raise StepError("no window: run 'new --signal ...' first")
    return json.loads(STATE.read_text(encoding="utf-8"))


def save_state(state: dict) -> None:
    STATE.parent.mkdir(parents=True, exist_ok=True)
    tmp = STATE.with_suffix(".tmp")
    tmp.write_text(json.dumps(state, indent=1), encoding="utf-8")
    tmp.replace(STATE)


def open_state() -> dict:
    state = load_state()
    if state.get("closed"):
        raise StepError(f"window {state['id']} is closed: open a new one")
    if event_now():
        raise EventNow()
    return state


def spike_running(env: dict[str, str], event: str = "abandon") -> bool:
    return bool(ps(env, "@(Get-Process -Name asio_spike -ErrorAction SilentlyContinue).Count", timeout=60, event=event))


# ---- commands ----

def cmd_new(env, args) -> None:
    check_signal(args.signal)
    if event_now():
        raise StepError(f"{EVENT_NOW} exists: an event is on, no window")
    if STATE.is_file() and not json.loads(STATE.read_text(encoding="utf-8")).get("closed"):
        raise StepError("the last window is still open: finish it (to-event) or run preempt")
    wid = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    save_state({"id": wid, "signal": args.signal, "card": "reaper", "pref_original": int(env["PC_BUFFER_ORIGINAL"]),
                "pref_current": None, "pref_restored": False, "runs": [], "closed": False})
    print(wid)


def cmd_fetch_bundle(env, args) -> None:
    sha = args.sha
    subprocess.run(["git", "fetch", "-q", "origin", "dev"], check=True)
    if subprocess.run(["git", "merge-base", "--is-ancestor", sha, "origin/dev"], check=False).returncode != 0:
        raise StepError(f"{sha} is not on origin/dev")
    listing = subprocess.run(
        ["gh", "run", "list", "-R", REPO, "--workflow", "ci.yml", "--branch", "dev", "--event", "push", "--limit", "50",
         "--json", "databaseId,headSha,event,headBranch,conclusion"], check=True, capture_output=True, text=True)
    run_id = pick_run(json.loads(listing.stdout), sha)
    dest = bundle_dir(env, sha)
    if dest.exists():
        raise StepError(f"{dest} exists")
    subprocess.run(["gh", "run", "download", str(run_id), "-R", REPO, "-n", f"asio-spike-{sha}", "-D", str(dest)], check=True)
    files = verify_bundle(dest)
    (dest.parent / f"{sha}.source-sha").write_text(sha + "\n", encoding="utf-8")
    print(json.dumps({"bundle": str(dest), "run": run_id, "files": files}))


def cmd_setup(env, args) -> None:
    state = open_state()
    bundle = bundle_dir(env, args.sha)
    if (bundle.parent / f"{args.sha}.source-sha").read_text(encoding="utf-8").strip() != args.sha:
        raise StepError("bundle .source-sha differs from --sha (P5: only the reviewed dev commit's bundle)")
    verify_bundle(bundle)
    dirs = ", ".join(pc(env, d) for d in ("bin", "queue", "status"))
    guarded(ssh_cmd(env), f"New-Item -ItemType Directory -Force -Path {dirs} | Out-Null\n", 60, "finish")
    for name in (*BUNDLE_FILES, "SHA256SUMS"):
        scp(str(bundle / name), remote(env, f"bin/{name}"))
    names = ps(env, f"$n = Test-SpikeSums -Bin {pc(env, 'bin')} ; Register-SpikeTask -Root {ps_quote(env['PC_ROOT'])} ; $n")
    state["bundle_sha"] = args.sha
    save_state(state)
    print(json.dumps({"setup": args.sha, "verified": names, "task": "registered"}))


def cmd_preflight(env, args) -> None:
    state = open_state()
    if state["card"] != "reaper":
        raise StepError("preflight belongs before to-dev")
    r = ps(env, " ; ".join([
        f"$p = Get-SpikeBufferPref -Key {ps_quote(env['PC_BUFFER_KEY'])} -Name {ps_quote(env['PC_BUFFER_NAME'])}",
        f"$h = Get-GoldenAsioHolders -Module {ps_quote(env['PC_ASIO_MODULE'])}",
        f"$s = Test-SpikeSums -Bin {pc(env, 'bin')}",
        "[pscustomobject]@{ pref = $p.value; kind = $p.kind; holders = @($h); "
        "reaper = @(Get-Process reaper -ErrorAction SilentlyContinue).Count; "
        f"app = @(Get-Process -Name {ps_quote(env['PC_APP_PROCESS'])} -ErrorAction SilentlyContinue).Count; "
        "spike = @(Get-Process -Name asio_spike -ErrorAction SilentlyContinue).Count; "
        f"task = [bool](Get-ScheduledTask {TASK} -ErrorAction SilentlyContinue); files = @($s).Count }}",
    ]), timeout=120, event="abandon")
    problems = preflight_problems(r, state["pref_original"])
    if problems:
        raise StepError("; ".join(problems))
    state["preflight"] = r
    save_state(state)
    print(json.dumps({"preflight": r}))


def cmd_to_dev(env, args) -> None:
    state = open_state()
    if state["card"] != "reaper" or "preflight" not in state:
        raise StepError("run preflight first (REAPER must hold the card)")
    texts = ps(env, f"Get-GoldenMeterSamples -Http {ps_quote(env['PC_REAPER_HTTP'])} -Seconds 60", timeout=180, event="abandon")
    hits = interlock_hits([parse_meter_peaks(t) for t in texts])
    if hits:
        raise StepError(f"band activity: peaks above -50 dBFS on tracks {sorted(hits)}; no switch, alarm the owner")
    state["card"] = "switching"
    save_state(state)
    r = ps(env, f"Invoke-GoldenSaveQuit -Http {ps_quote(env['PC_REAPER_HTTP'])} -Project {ps_quote(env['PC_MAIN_PROJECT'])} -AsioModule {ps_quote(env['PC_ASIO_MODULE'])}", timeout=120)
    state["card"] = "free"
    save_state(state)
    print(json.dumps({"to-dev": r, "app": "kept running"}))


def cmd_set_buffer(env, args) -> None:
    state = open_state()
    if state["card"] != "free":
        raise StepError("the card is not free (run to-dev)")
    if args.frames not in FRAMES:
        raise StepError(f"--frames must be one of {FRAMES}")
    if spike_running(env):
        raise StepError("a spike runs")
    state["pref_current"], state["pref_restored"] = args.frames, False   # recorded before the write: preempt restores
    save_state(state)
    r = ps(env, f"Set-SpikeBufferPref -Key {ps_quote(env['PC_BUFFER_KEY'])} -Name {ps_quote(env['PC_BUFFER_NAME'])} -Value {args.frames} -Original {state['pref_original']}")
    print(json.dumps({"set-buffer": r}))


def cmd_run(env, args) -> None:
    state = open_state()
    if state["card"] != "free":
        raise StepError("the card is not free (run to-dev)")
    check_request(args.mode, args.frames, args.seconds, args.burn_us, args.stress, args.cycles)
    current = state["pref_current"] if state["pref_current"] is not None else state["pref_original"]
    if args.mode != "probe" and args.frames != current:
        raise StepError(f"the driver's preferred buffer is {current}: run set-buffer --frames {args.frames} first")
    fields = {"mode": args.mode, "driver": env["PC_ASIO_DRIVER"], "module": env["PC_ASIO_MODULE"], "frames": args.frames or 0,
              "seconds": args.seconds, "burn_us": args.burn_us, "stress": args.stress, "panic_at": args.panic_at,
              "cycles": args.cycles, "timeout": run_timeout(args.mode, args.seconds, args.cycles)}
    rid = ps(env, f"Remove-Item -LiteralPath {pc(env, 'queue/stop')} -ErrorAction SilentlyContinue ; "
                  f"$id = Write-GoldenRequest -Root {ps_quote(env['PC_ROOT'])} -Kind 'spike' -Fields {ps_hashtable(fields)} ; Start-SpikeTask ; $id")
    state["runs"].append({"request": rid, **fields})
    save_state(state)
    status_path, progress_path = pc(env, f"status/{rid}.json"), pc(env, f"status/{rid}.progress.json")
    watch = (f"$s = {status_path} ; $p = {progress_path} ; [pscustomobject]@{{ "
             "status = $(if (Test-Path -LiteralPath $s) { Get-Content -LiteralPath $s -Raw | ConvertFrom-Json } else { $null }); "
             "progress = $(if (Test-Path -LiteralPath $p) { Get-Content -LiteralPath $p -Raw | ConvertFrom-Json } else { $null }) }")
    deadline = time.monotonic() + fields["timeout"] + 120
    next_pc = 0.0
    while True:
        if event_now():
            raise EventNow()
        if time.monotonic() >= next_pc:
            next_pc = time.monotonic() + 10
            st = ps(env, watch, timeout=60, event="abandon")
            if st.get("progress"):
                print(json.dumps({"progress": st["progress"]}), flush=True)
            phase = (st.get("status") or {}).get("state")
            if phase in ("exited", "failed", "refused"):
                break
            if time.monotonic() > deadline:
                ps(env, f"New-Item -ItemType File -Force -Path {pc(env, 'queue/stop')} | Out-Null ; 'stop'", timeout=60)
                raise StepError("the spike has not exited: stop file written; watch it, never kill; alarm the owner if it stays")
        time.sleep(POLL_S)
    if phase != "exited":
        raise StepError(f"spike request {phase}: {json.dumps(st['status'].get('results'))}")
    out = raw_dir(env, state)
    scp(remote(env, f"status/{rid}.report.json"), str(out / f"{rid}.report.json"))
    scp(remote(env, f"status/{rid}.stderr.txt"), str(out / f"{rid}.stderr.txt"))
    report = json.loads((out / f"{rid}.report.json").read_text(encoding="utf-8"))
    v = verdict(report)
    state["runs"][-1]["verdict"] = v
    save_state(state)
    print(json.dumps({"run": rid, "exit": st["status"]["results"][0]["exit"], "verdict": v}))


def unwind(env: dict[str, str], state: dict, running: bool) -> list:
    """Stop the spike, restore the buffer (read back), bring REAPER back."""
    done = []
    gone = True
    for step in undo_plan(state, running):
        if step == "stop-spike":
            gone = bool(ps(env, f"(Stop-SpikeGracefully -Root {ps_quote(env['PC_ROOT'])} -Seconds 60).gone", timeout=120, event="ignore"))
            done.append({"stop-spike": gone})
        elif step == "restore-buffer":
            r = ps(env, f"Set-SpikeBufferPref -Key {ps_quote(env['PC_BUFFER_KEY'])} -Name {ps_quote(env['PC_BUFFER_NAME'])} "
                        f"-Value {state['pref_original']} -Original {state['pref_original']}", event="ignore")
            state["pref_current"], state["pref_restored"] = state["pref_original"], True
            save_state(state)
            done.append({"restore-buffer": r})
        elif step == "bring-back":
            if not gone:
                raise StepError("the spike did not stop within 60 s, so REAPER cannot start (I3): alarm the owner now; "
                                "the last resort is the owner's reboot, which comes back in event mode")
            r = ps(env, "Invoke-SpikeBringBack " + " ".join([
                f"-Http {ps_quote(env['PC_REAPER_HTTP'])}",
                f"-StartTaskPath {ps_quote(env['PC_REAPER_START_TASK_PATH'])} -StartTask {ps_quote(env['PC_REAPER_START_TASK'])}",
                f"-NTrack {int(env['PC_NTRACK'])} -BridgeState {ps_quote(env['PC_METER_BRIDGE'])}",
                f"-BridgeAction {ps_quote(env['PC_METER_ACTION'])} -Heartbeat {ps_quote(env['PC_METER_HEARTBEAT'])}",
                f"-AsioModule {ps_quote(env['PC_ASIO_MODULE'])} -AppHttp {ps_quote(env['PC_APP_HTTP'])}",
            ]), timeout=240, event="ignore")
            state["card"] = "reaper"
            save_state(state)
            done.append({"bring-back": r})
    state["closed"] = True
    save_state(state)
    return done


def cmd_to_event(env, args) -> None:
    state = open_state()
    if spike_running(env):
        raise StepError("a spike runs: wait for it, or preempt")
    print(json.dumps({"to-event": unwind(env, state, running=False)}))


def cmd_preempt(env, args=None) -> None:
    state = load_state()
    if state.get("closed"):
        print(json.dumps({"preempt": state["id"], "plan": [], "note": "window already closed"}))
        return
    running = spike_running(env, event="ignore")
    print(json.dumps({"preempt": state["id"], "plan": undo_plan(state, running)}), flush=True)
    state["preempted"] = True
    print(json.dumps({"done": unwind(env, state, running)}))


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("new").add_argument("--signal", required=True)
    for name in ("fetch-bundle", "setup"):
        sub.add_parser(name).add_argument("--sha", required=True)
    for name in ("preflight", "to-dev", "to-event", "preempt", "status"):
        sub.add_parser(name)
    sub.add_parser("set-buffer").add_argument("--frames", type=int, required=True)
    run = sub.add_parser("run")
    run.add_argument("--mode", required=True, choices=("probe", "duplex", "reopen"))
    run.add_argument("--frames", type=int)
    run.add_argument("--seconds", type=int, default=600)
    run.add_argument("--burn-us", type=int, default=0)
    run.add_argument("--stress", type=int, default=0)
    run.add_argument("--panic-at", type=int, default=0)
    run.add_argument("--cycles", type=int, default=5)
    args = ap.parse_args(argv)
    if args.cmd == "status":
        print(json.dumps({"event_now": event_now(), "state": load_state() if STATE.is_file() else None}, indent=1))
        return 0
    handlers = {"new": cmd_new, "fetch-bundle": cmd_fetch_bundle, "setup": cmd_setup, "preflight": cmd_preflight,
                "to-dev": cmd_to_dev, "set-buffer": cmd_set_buffer, "run": cmd_run, "to-event": cmd_to_event, "preempt": cmd_preempt}
    try:
        env = load_env(Path(os.environ.get("SPIKE_ENV", str(Path.home() / ".config/iemmixer/asio-spike.env"))))
        try:
            handlers[args.cmd](env, args)
        except EventNow:
            print(json.dumps({"event": "ide event (flag file)", "action": "preempt"}), flush=True)
            cmd_preempt(env)
            return 10
        return 0
    except StepError as e:
        print(f"spike_window: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
