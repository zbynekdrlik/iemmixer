#!/usr/bin/env python3
"""S1c tuning window on the dev box (design note §7, §8): the PC tuning
modules (IemTuning.psm1, IemMeasure.psm1 from the verified spike bundle) and
the measurement set, on top of spike_window.py's window, state, event guard
and unwind. A window opens only with spike_window's `new --signal`; every
command here checks the "ide event" flag and pre-empts like spike_window.
Site values come only from the private env ($SPIKE_ENV) and profile
($TUNING_PROFILE). Nothing is ever ended by force; a reboot happens only on
the owner's quoted approval, and it is an immediate restart that an app may
veto (never a delayed one: Windows forces those, I8)."""
from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "asio-spike"))
sys.path.insert(0, str(HERE))
import latency_report as lr  # noqa: E402
import spike_window as sw  # noqa: E402
from golden_window import StepError, ps_quote  # noqa: E402

PROFILE = Path(os.environ.get("TUNING_PROFILE", str(Path.home() / ".config/iemmixer/pc-tuning.json")))
ADK_URL = "https://go.microsoft.com/fwlink/?linkid=2289980"  # ADK 10.1.26100.9457 (September 2026), design note [7]
PROFILE_KEYS = ("version", "journal", "registry_root", "layout", "plan", "governor", "placement", "services_disable",
                "services_mode", "updates", "maintenance", "defender", "devices", "nic", "fingerprint")
LAYOUT_ROLES = ("housekeeping", "card", "nic", "audio")
MODE_LEVERS = ("plan", "governor", "placement", "services")
MAX_CUTS = 5
# The spike outcomes of a completed measure run: it ran to its end, or the stop
# file ended it. refused, band-activity, fault-caught, rate-changed and
# stop-hung end it without a measurement (outcome "error" already fails in cmd_run).
MEASURED = ("done", "stopped")
LABEL = re.compile(r"[a-z0-9][a-z0-9-]{0,39}")
APPROVAL = re.compile(r".*\d{1,2}:\d{2}.*\S.*")


def parse_lps(text: str) -> list[int]:
    out: list[int] = []
    for part in (p.strip() for p in text.split(",") if p.strip()):
        lo, _, hi = part.partition("-")
        try:
            a, b = int(lo), int(hi or lo)
        except ValueError:
            raise StepError(f"bad processor list {text!r}") from None
        if not (0 <= a <= b <= 63):
            raise StepError(f"bad range {part!r}: processors are 0..63, ascending")
        for lp in range(a, b + 1):
            if lp in out:
                raise StepError(f"{text!r} names processor {lp} twice")
            out.append(lp)
    return sorted(out)


def load_profile(path: Path) -> dict:
    if not path.is_file():
        raise StepError(f"{path}: missing (private profile, plan Task 12)")
    p = json.loads(path.read_text(encoding="utf-8"))
    missing = [k for k in PROFILE_KEYS if k not in p]
    if missing:
        raise StepError(f"{path}: missing {', '.join(missing)}")
    check_layout(p["layout"], path)
    return p


def check_layout(layout, path: Path) -> None:
    """The profile's layout rule (#32 MINOR-6), the same as IemTuning's
    Assert-IemLayout (shared cases: layout_cases.json): layout is an object; a
    role is absent (no processors) or a list of integers 0..63 — a null, a float
    (2.0 too), a bool, a string or a nested list is refused, never truncated or
    read as a number; the roles are disjoint."""
    if not isinstance(layout, dict):
        raise StepError(f"{path}: layout: not an object")
    roles: dict[int, str] = {}
    for role in LAYOUT_ROLES:
        if role not in layout:
            continue
        lps = layout[role]
        if not isinstance(lps, list):
            raise StepError(f"{path}: layout {role}: not a list of processor numbers")
        for i, lp in enumerate(lps):
            # type(), not isinstance(): bool is an int subclass in Python.
            if type(lp) is not int or not 0 <= lp <= 63:
                raise StepError(f"{path}: layout {role}: entry {i} is not a processor number 0..63 (integers only)")
            if lp in roles:
                raise StepError(f"{path}: layout: processor {lp} has two roles ({roles[lp]}, {role})")
            roles[lp] = role


def layout_lps(profile: dict, role: str) -> list[int]:
    """A layout role's processors; an absent role is none (the layout rule)."""
    return list(profile["layout"].get(role, []))


def watch_lps(profile: dict, audio_cpus: str) -> list[int]:
    """The CPUs whose DPC/ISR budget is watched: the card's and the audio one
    (the spike's --audio-cpus, else the profile's)."""
    audio = parse_lps(audio_cpus) if audio_cpus else layout_lps(profile, "audio")
    return sorted(set(layout_lps(profile, "card")) | set(audio))


def mode_only(text: str) -> list[str]:
    levers = [x.strip() for x in text.split(",") if x.strip()]
    bad = [x for x in levers if x not in MODE_LEVERS]
    if bad or not levers:
        raise StepError(f"--only takes {', '.join(MODE_LEVERS)}")
    return levers


def label_ok(text: str) -> bool:
    return bool(LABEL.fullmatch(text))


def check_approval(text: str) -> None:
    """The owner's approval of the reboot, quoted with its time (HH:MM)."""
    if not APPROVAL.fullmatch(text.strip()) or len(text.strip()) < 12:
        raise StepError("quote the owner's approval with its time, e.g. 'owner, 14:05: áno, reštartuj'")


def should_cut(progress: dict | None, seen: int, cuts: int, circular: bool) -> tuple[bool, int]:
    """A new missed period, overrun or position gap cuts a circular soak
    trace (at most MAX_CUTS times); returns (cut, glitches seen now)."""
    if not progress:
        return False, seen
    total = sum(int(progress.get(k, 0)) for k in ("missed", "overruns", "position_gaps"))
    return (circular and total > seen and cuts < MAX_CUTS), total


def post_boot_verdict(c: dict) -> list[str]:
    problems = []
    if not c["booted_after_request"]:
        problems.append("the PC did not reboot after the request")
    if not c["reaper"]:
        problems.append("REAPER did not start by itself within 5 min")
    if "error" in (c.get("handover") or {}):
        problems.append(f"handover checks failed: {c['handover']['error']}")
    if c["fingerprint"]:
        problems.append("REAPER mode differs: " + ", ".join(d["key"] for d in c["fingerprint"]))
    if c["pending"]:
        problems.append("still pending after the reboot: " + ", ".join(c["pending"]))
    if c["failed_items"]:
        problems.append("items not as applied: " + ", ".join(c["failed_items"]))
    if c.get("boot_problem"):
        # Without a boot token nothing reads as pending, so an empty "pending"
        # proves nothing (#32 MINOR-4).
        problems.append(f"the boot identity is unknown ({c['boot_problem']}): what is still pending cannot be told")
    return problems


# ---- PC access (the PC is the external dependency; no unit tests below) ----

def tps(env: dict[str, str], body: str, **kw):
    return sw.ps(env, sw.tuning_body(env, body), **kw)


def as_list(value) -> list:
    """PowerShell returns one row as an object and none as null."""
    if value is None:
        return []
    return value if isinstance(value, list) else [value]


def baseline_path(env: dict[str, str]) -> Path:
    """The REAPER-mode fingerprint baseline: one file across windows."""
    return Path(env["RAW_DIR"]).expanduser() / "pc-tuning" / "fingerprint-baseline.json"


def xperf(env: dict[str, str]) -> str:
    return ps_quote(env["PC_XPERF"])


def need_free(state: dict) -> None:
    if state["card"] != "free":
        raise StepError("the card is not free (run spike_window to-dev first)")


def raw(env: dict[str, str], state: dict) -> Path:
    d = Path(env["RAW_DIR"]).expanduser() / "pc-tuning" / state["id"]
    d.mkdir(parents=True, exist_ok=True)
    os.chmod(d, 0o700)
    return d


def stamp() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def cmd_tuning_setup(env, args) -> None:
    state = sw.open_state()
    profile = load_profile(PROFILE)
    root = ps_quote(env["PC_TUNING_ROOT"])
    # sw.ps runs this under $ErrorActionPreference='Stop' and returns a PC error
    # as a StepError; a native exit code never throws by itself, so icacls' is
    # checked. Nothing is copied into a folder whose ACL was not set.
    sw.ps(env, f"New-Item -ItemType Directory -Force -Path {root}, (Join-Path {root} 'runs') | Out-Null ; "
               f"& icacls.exe {root} /inheritance:r /grant:r '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-18:(OI)(CI)F' | Out-Null ; "
               "if ($LASTEXITCODE -ne 0) { throw \"icacls exited $LASTEXITCODE\" } ; 'ok'", timeout=60)
    sw.scp(str(PROFILE), f"{env['PC_SSH']}:{env['PC_TUNING_ROOT_SCP']}/profile.json")
    r = tps(env, f"(Read-IemProfile -Path {sw.tuning_profile(env)}).version", timeout=60)
    if baseline_path(env).is_file():
        state = sw.update_state({"fingerprint": str(baseline_path(env))})   # to-event and preempt compare against it
    print(json.dumps({"tuning-setup": {"profile_version": r, "local_version": profile["version"], "window": state["id"],
                                       "fingerprint": state.get("fingerprint")}}))


def cmd_inventory(env, args) -> None:
    state = sw.open_state()
    f = env["PC_TUNING_ROOT"] + f"\\inventory-{stamp()}.json"
    tps(env, f"$i = Get-IemInventory -ProfilePath {sw.tuning_profile(env)} ; [IO.File]::WriteAllText({ps_quote(f)}, ($i | ConvertTo-Json -Depth 8)) ; 'ok'",
        timeout=900, event="abandon")
    local = raw(env, state) / Path(f.replace("\\", "/")).name
    sw.scp(f"{env['PC_SSH']}:{env['PC_TUNING_ROOT_SCP']}/{local.name}", str(local))
    print(json.dumps({"inventory": str(local), "bytes": local.stat().st_size}))


def cmd_fingerprint(env, args) -> None:
    state = sw.open_state()
    current = tps(env, f"Get-IemReaperFingerprint -ProfilePath {sw.tuning_profile(env)}", timeout=120, event="abandon")
    if args.baseline:
        if state["card"] != "reaper":
            raise StepError("the baseline is read while REAPER runs (before to-dev)")
        path = baseline_path(env)
        path.parent.mkdir(parents=True, exist_ok=True)
        text = json.dumps(current, indent=1)
        (raw(env, state) / f"fingerprint-baseline-{stamp()}.json").write_text(text, encoding="utf-8")
        path.write_text(text, encoding="utf-8")
        sw.update_state({"fingerprint": str(path)})
        print(json.dumps({"fingerprint-baseline": str(path), "keys": len(current)}))
        return
    if not state.get("fingerprint"):
        raise StepError("no baseline in this window (fingerprint --baseline)")
    diff = sw.fingerprint_diff(json.loads(Path(state["fingerprint"]).read_text(encoding="utf-8")), current)
    print(json.dumps({"fingerprint-check": diff}))
    if diff:
        raise StepError("the fingerprint differs from the baseline: " + ", ".join(d["key"] for d in diff))


def cmd_wpt_install(env, args) -> None:
    state = sw.open_state()
    if state["card"] != "reaper":
        raise StepError("install WPT while REAPER still holds the card (before to-dev)")
    local = Path(env["RAW_DIR"]).expanduser() / "pc-tuning" / "adksetup.exe"
    local.parent.mkdir(parents=True, exist_ok=True)
    if not local.is_file():
        subprocess.run(["curl", "-fsSL", "-o", str(local), ADK_URL], check=True, timeout=300)
    sw.scp(str(local), f"{env['PC_SSH']}:{env['PC_TUNING_ROOT_SCP']}/adksetup.exe")
    r = tps(env, f"Install-IemWpt -Setup {ps_quote(env['PC_TUNING_ROOT'] + chr(92) + 'adksetup.exe')} -Xperf {xperf(env)}", timeout=1800)
    print(json.dumps({"wpt-install": r}))


def record_step(step: dict) -> None:
    """Adds a tuning step to the state as saved now (sw.update_state, under the window lock)."""
    sw.update_state(change=lambda st: st.setdefault("tuning_steps", []).append(step))


def record_measurement(row: dict) -> None:
    """Adds a measurement row to the state as saved now (sw.update_state, under the window lock)."""
    sw.update_state(change=lambda st: st.setdefault("measurements", []).append(row))


def cmd_enter(env, args) -> None:
    state = sw.open_state()
    need_free(state)
    only = mode_only(args.only)
    sw.update_state({"tuning_mode": True})   # recorded before the action: preempt reverts even a half-done enter
    rows = as_list(tps(env, f"Enter-IemTuningMode -ProfilePath {sw.tuning_profile(env)} -Only @({', '.join(ps_quote(x) for x in only)}) -Idle {ps_quote(args.idle)}", timeout=240))
    record_step({"enter": only, "idle": args.idle, "at": stamp()})
    print(json.dumps({"enter": rows}))
    failed = [r for r in rows if r.get("action") == "failed"]
    if failed:
        raise StepError(f"{len(failed)} mode item(s) failed: " + "; ".join(f"{r['key']}: {r['error']}" for r in failed))


def cmd_exit(env, args) -> None:
    sw.open_state()
    rows = as_list(tps(env, f"Exit-IemTuningMode -ProfilePath {sw.tuning_profile(env)}", timeout=240))
    sw.update_state({"tuning_mode": False})
    print(json.dumps({"exit": rows}))
    sw.alarm_exit_problems(rows)   # the exit completed; unconvertible journal entries reach the owner


def only_arg(text: str) -> str:
    groups = [x.strip() for x in text.split(",") if x.strip()]
    if any(not re.fullmatch(r"[a-z]+(:[a-z0-9-]+)?", g) for g in groups):
        raise StepError("--only takes group names such as services,updates or irq:card")
    return "@(" + ", ".join(ps_quote(g) for g in groups) + ")"


def cmd_apply(env, args) -> None:
    state = sw.open_state()
    need_free(state)
    rows = as_list(tps(env, f"Invoke-IemTuningApply -ProfilePath {sw.tuning_profile(env)} -Tier {args.tier} -Only {only_arg(args.only)}", timeout=600))
    record_step({"apply": args.tier, "only": args.only, "at": stamp()})
    print(json.dumps({"apply": rows}))
    failed = [r for r in rows if r.get("action") == "failed"]
    if failed:
        raise StepError(f"{len(failed)} item(s) failed: " + "; ".join(f"{r['key']}: {r['error']}" for r in failed))


def cmd_undo(env, args) -> None:
    state = sw.open_state()
    need_free(state)
    rows = as_list(tps(env, f"Undo-IemTuning -ProfilePath {sw.tuning_profile(env)} -Tier {args.tier} -Only {only_arg(args.only)}", timeout=600))
    record_step({"undo": args.tier, "only": args.only, "at": stamp()})
    print(json.dumps({"undo": rows}))
    # Fail loud on any un-reverted item, like cmd_apply (I2, script-failure-policy):
    # post_boot_verdict's failed_items cannot catch it (a failed revert stays
    # journaled but still matches its tuned value → counts ok), so a silent
    # exit 0 would hide a global lever left applied.
    failed = [r for r in rows if r.get("action") == "failed"]
    if failed:
        raise StepError(f"{len(failed)} revert item(s) failed: " + "; ".join(f"{r['key']}: {r['error']}" for r in failed))
    # A "problem" row (the revert's boot unknown, #32 MINOR-4) does not undo the
    # revert; the owner hears it.
    problems = [r for r in rows if r.get("action") == "problem"]
    if problems:
        sw.alarm("the revert completed, but: " + "; ".join(f"{r.get('key')}: {r.get('error')}" for r in problems))


def cmd_state(env, args) -> None:
    print(json.dumps({"state": tps(env, f"Get-IemTuningState -ProfilePath {sw.tuning_profile(env)}", timeout=120, event="abandon")}, indent=1))


def no_leftover_trace(state: dict) -> None:
    """A kernel trace still recorded from an earlier measure would load the PC
    being measured (and a new start would fail on its running session)."""
    if state.get("trace"):
        raise StepError(f"a kernel trace is still recorded ({state['trace']}): run trace-stop first")


def clear_trace() -> dict:
    """Clears the recorded trace in the state as saved NOW, changing nothing
    else: a preempt in another process may have saved its own changes
    meanwhile (closed, card, the buffer restore)."""
    return sw.update_state({"trace": None})


def stop_trace(env: dict[str, str], state: dict, event: str):
    """Stops the recorded kernel trace without merging (quick; the raw files
    stay on the PC) and clears it from the state once the stop is confirmed
    (sw.check_trace_stop, #32 MAJOR-1); any other reply keeps it recorded."""
    r = sw.check_trace_stop(sw.ps(env, sw.trace_stop_body(env, state["trace"]), timeout=sw.TRACE_STOP_CALL_S, event=event))
    clear_trace()
    return r


def cmd_trace_stop(env, args) -> None:
    """Stops a kernel trace a failed measure left recorded (#32 B5). Like every
    window command it gives way to "ide event": the stop completes, then
    EventNow (main pre-empts)."""
    state = sw.open_state()
    if not state.get("trace"):
        print(json.dumps({"trace-stop": "no kernel trace recorded in this window"}))
        return
    print(json.dumps({"trace-stop": stop_trace(env, state, event="finish")}))


def abandon_trace(env: dict[str, str]) -> None:
    """After a failed measure: stop the trace it started. A stop that fails
    keeps the trace recorded (trace-stop or a preempt retries it) and alarms
    the owner; it never replaces the measure's own error. While the "ide
    event" flag exists the preempt owns the cleanup: nothing is done here."""
    state = sw.load_state()
    if not state.get("trace") or sw.event_now():
        return
    try:
        stop_trace(env, state, event="ignore")
    except StepError as e:
        sw.alarm(f"the kernel trace of a failed measure did not stop ({e}): run tuning_window trace-stop")


def cmd_measure(env, args) -> None:
    state = sw.open_state()
    need_free(state)
    no_leftover_trace(state)
    profile = load_profile(PROFILE)
    if not label_ok(args.label):
        raise StepError("--label: lower-case letters, digits and dashes, at most 40")
    current = state["pref_current"] if state["pref_current"] is not None else state["pref_original"]
    if args.frames != current:
        raise StepError(f"the driver's preferred buffer is {current}: spike_window set-buffer --frames {args.frames} first")
    run_dir = env["PC_TUNING_ROOT"] + f"\\runs\\{args.label}-{stamp()}"
    since = tps(env, "Get-IemNow", timeout=60, event="abandon")
    tracing = args.trace != "none"
    try:
        _measure(env, args, profile, state, run_dir, since, tracing)
    except sw.EventNow:
        raise   # "ide event": the preempt's unwind stops the recorded trace (trace-stop); nothing delays it here
    except BaseException:
        if tracing:
            abandon_trace(env)
        raise


def trace_options(trace: str, circular_mb: int) -> str:
    """Start-IemTrace's options, the same at the start and at every cut's restart."""
    return (" -CSwitch" if trace == "diag" else "") + (f" -CircularMB {circular_mb}" if circular_mb else "")


def start_trace(env: dict[str, str], run_dir: str, opt: str) -> None:
    """Starts the kernel trace (event="finish": the start completes). A start
    the "ide event" overtook — its call ended in EventNow, or the flag exists
    right after it — may have ended after a preempt's trace-stop found no
    session, so this process stops it too (quick, no merge; an alarm if that
    fails) before EventNow goes on (review round 3, m5)."""
    try:
        tps(env, f"Start-IemTrace -Xperf {xperf(env)} -Dir {ps_quote(run_dir)}{opt}", timeout=120, event="finish")
        check_event()
    except sw.EventNow:
        try:
            sw.check_trace_stop(sw.ps(env, sw.trace_stop_body(env, run_dir), timeout=sw.TRACE_STOP_CALL_S, event="ignore"))
        except StepError as e:
            sw.alarm(f"a kernel trace started as 'ide event' came did not stop ({e}): run tuning_window trace-stop")
        raise


def set_aside(d: str, cut: int) -> str:
    """After a cut's stop: its raw session files become cut-N.kernel.etl and
    cut-N.markers.etl (a missing marker file stays missing), so the restart
    writes fresh ones and the merge waits for the analysis."""
    return " ; ".join(f"if (Test-Path -LiteralPath (Join-Path {d} '{raw}.etl')) {{ Rename-Item -LiteralPath (Join-Path {d} '{raw}.etl') "
                      f"-NewName 'cut-{cut}.{raw}.etl' }}" for raw in ("kernel", "markers"))


def merge(x: str, d: str, base: str) -> str:
    """xperf -merge of one trace's raw session files (<base>kernel.etl and
    <base>markers.etl, the ones present) into its .etl — what a merging stop
    (`xperf -stop ... -d`) does, as a separate, abandonable call. The raw
    files are deleted once the merge succeeded (a soak would otherwise double
    its disk use); a failed merge throws first and keeps them."""
    out = f"{base[:-1]}.etl" if base else "trace.etl"
    return (f"$m = @(foreach ($n in @('{base}kernel.etl', '{base}markers.etl')) {{ $p = Join-Path {d} $n ; "
            f"if (Test-Path -LiteralPath $p) {{ $p }} }}) ; "
            f"[void](Invoke-IemXperf -Xperf {x} -Arguments (@('-merge') + $m + @((Join-Path {d} '{out}')))) ; "
            f"Remove-Item -LiteralPath $m")


def analysis(x: str, d: str, cuts: int, diag: bool) -> list[tuple[str, str | None]]:
    """The merge and xperf analysis of a stopped trace and of each cut, one
    step at a time: (PowerShell body, the file it leaves in the run folder or
    None) — per trace a merge, its dpcisr and, for a diag run, its near-glitch
    view. Each step is its own PC call (decision B of the #32 review)."""
    steps: list[tuple[str, str | None]] = []
    for etl in [None] + [f"cut-{i}.etl" for i in range(1, cuts + 1)]:
        name, base = (f" -Name '{etl}'", etl[:-len(".etl")] + ".") if etl else ("", "")
        steps.append((merge(x, d, base), None))
        steps.append((f"Invoke-IemDpcIsr -Xperf {x} -Dir {d}{name}", f"{base}dpcisr.txt"))
        if diag:
            steps.append((f"Export-IemNearGlitch -Xperf {x} -Dir {d}{name}", f"{base}near.txt"))
    return steps


ANALYSIS_REFUSED = "ide event: the analysis step did not start"


def analysis_guard(root: str, since: str) -> str:
    """The PC-side start of one analysis step: this PowerShell runs at Idle
    priority (the xperf it starts inherits the class: CreateProcess keeps an
    Idle or Below-normal parent's class), and it does not start when the
    spike's stop file was written after the analysis began (`since`, PC
    time): the first thing every preempt does is write it, so this is the
    PC's view of "ide event" when the dev box's call was already on its way."""
    return (f"(Get-Process -Id $PID).PriorityClass = 'Idle' ; $s = (Join-Path {ps_quote(root)} 'queue\\stop') ; "
            f"if ((Test-Path -LiteralPath $s) -and (Get-Item -LiteralPath $s).LastWriteTimeUtc -gt "
            f"[datetime]::Parse({ps_quote(since)}, [Globalization.CultureInfo]::InvariantCulture, "
            f"[Globalization.DateTimeStyles]::RoundtripKind).ToUniversalTime()) {{ throw '{ANALYSIS_REFUSED}' }}")


def read_text(path: Path) -> str:
    return path.read_text(encoding="utf-8", errors="replace")


def check_event() -> None:
    """Between PC calls, copies and parses: "ide event" pre-empts at once."""
    if sw.event_now():
        raise sw.EventNow()


# The raw per-CPU counters, in IemMeasure's Get-IemCpuSample fields (the ones
# latency_report.cpu_rates reads); rates are computed on the dev box.
POLL_COUNTERS = ("$c = @(Get-CimInstance -ClassName Win32_PerfRawData_PerfOS_Processor | Where-Object { $_.Name -match '^\\d+$' } | "
                 "ForEach-Object { [pscustomobject]@{ lp = [int]$_.Name; t100ns = [uint64]$_.Timestamp_Sys100NS; "
                 "interrupts = [uint64]$_.InterruptsPersec; dpcs = [uint64]$_.DPCsQueuedPersec; dpc_time = [uint64]$_.PercentDPCTime; "
                 "int_time = [uint64]$_.PercentInterruptTime; idle_time = [uint64]$_.PercentIdleTime; c1_time = [uint64]$_.PercentC1Time; "
                 "c2_time = [uint64]$_.PercentC2Time; c3_time = [uint64]$_.PercentC3Time } })")


def poll_body(governor: str, pid: int, tid: int) -> str:
    """One sentinel sample every 10 s during a measurement (design note §4.1
    items 3 and 5), kept light on the PC being measured (#32 B11): it runs
    through sw.ps (SpikePc's import, as the status poll does) and NOT through
    the tuning modules, whose import compiles C# (Add-Type) in every new
    PowerShell; one raw WMI counter query (the formatted class is not read,
    its frequency columns were unused), the active plan from powercfg, the
    governor's service state and, while the spike runs, its callback thread's
    priority (read from outside). Residual load per poll: one ssh session and
    PowerShell start with the SpikePc import (no compile), one WMI query, one
    powercfg run — the same order as the status poll it follows."""
    thread = "$null"
    if pid > 0 and tid > 0:
        thread = (f"$(foreach ($t in @((Get-Process -Id {pid} -ErrorAction SilentlyContinue).Threads)) {{ if ($t.Id -eq {tid}) "
                  "{ [pscustomobject]@{ base = $t.BasePriority; current = $t.CurrentPriority } } })")
    return " ; ".join([
        POLL_COUNTERS,
        # A plan that cannot be read is an error (reported by sw.ps), never a guess.
        "$pc = (powercfg.exe /getactivescheme | Out-String)",
        "if ($LASTEXITCODE -ne 0 -or -not ($pc -match '[0-9a-fA-F]{8}(-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}')) "
        "{ throw \"powercfg /getactivescheme (exit $LASTEXITCODE): $($pc.Trim())\" }",
        "$plan = $Matches[0].ToLowerInvariant()",
        f"$g = Get-Service -Name {ps_quote(governor)} -ErrorAction SilentlyContinue",
        "[pscustomobject]@{ at = (Get-Date).ToUniversalTime().ToString('o'); cpu = [pscustomobject]@{ cpus = $c }; plan = $plan; "
        f"governor = $(if ($g) {{ \"$($g.Status)\" }} else {{ 'absent' }}); thread = {thread} }}",
    ])


def _measure(env, args, profile: dict, state: dict, run_dir: str, since: str, tracing: bool) -> None:
    diag = args.trace == "diag"
    opt = trace_options(args.trace, args.circular_mb)
    if tracing:
        sw.update_state({"trace": run_dir})   # recorded before the start: preempt and the error path stop it
        start_trace(env, run_dir, opt)
    polls: list[dict] = []
    cut = {"n": 0, "seen": 0}

    def on_poll(st: dict) -> None:
        status, progress = st.get("status") or {}, st.get("progress")
        pid = next((r.get("pid") for r in status.get("results") or [] if isinstance(r, dict) and r.get("pid")), 0)
        tid = (progress or {}).get("callback_thread", 0)
        polls.append(sw.ps(env, poll_body(profile["governor"], int(pid or 0), int(tid or 0)), timeout=60, event="abandon"))
        do_cut, cut["seen"] = should_cut(progress, cut["seen"], cut["n"], bool(tracing and args.circular_mb))
        if do_cut:
            cut["n"] += 1
            # A quick stop without the merge (the raw files are set aside and
            # merged with the analysis); "ide event" during it ends the measure
            # here, and no new kernel trace starts once the flag exists.
            sw.check_trace_stop(sw.ps(env, f"{sw.trace_stop_body(env, run_dir)} ; {set_aside(ps_quote(run_dir), cut['n'])}",
                                      timeout=sw.TRACE_STOP_CALL_S, event="finish"))
            check_event()
            start_trace(env, run_dir, opt)

    # The proxy load's busy threads run on the housekeeping CPUs unless told otherwise
    # (design note §4.3), without any --audio-cpus among them (#32 C1, review m12);
    # check_request and the spike refuse any overlap.
    audio = set(parse_lps(args.audio_cpus)) if args.audio_cpus else set()
    stress_cpus = args.stress_cpus or ",".join(str(lp) for lp in sorted(set(layout_lps(profile, "housekeeping")) - audio))
    run_args = argparse.Namespace(mode="duplex", frames=args.frames, seconds=args.seconds, burn_us=args.burn_us, stress=args.stress,
                                  panic_at=0, cycles=5, cpu=None, threshold_us=10, audio_cpus=args.audio_cpus, stress_cpus=stress_cpus)
    result = sw.cmd_run(env, run_args, on_poll=on_poll)
    report = json.loads(Path(result["report"]).read_text(encoding="utf-8"))
    outcome = report.get("outcome")
    if outcome not in MEASURED:
        # Recorded as a failed row, never summarised as an unstable measurement;
        # the error path stops the trace (its raw files stay on the PC).
        record_measurement({"label": args.label, "outcome": outcome, "exit": result["exit"], "failed": True,
                            "report": result["report"]})
        raise StepError(f"measure {args.label}: the spike ended {outcome!r} (exit {result['exit']}), no measurement "
                        f"(report {result['report']})")
    out = raw(env, state) / Path(run_dir.replace("\\", "/")).name
    out.mkdir(exist_ok=True)
    dpcisr_text = None
    if tracing:
        # The stop changes the PC, so it completes even when "ide event" comes;
        # without the merge it is quick (the raw session files stay). Then the
        # trace is no longer recorded.
        began = sw.ps(env, f"{sw.trace_stop_import(env)} ; [void]({sw.trace_stop_call(run_dir)}) ; Get-IemNow",
                      timeout=sw.TRACE_STOP_CALL_S, event="finish")
        clear_trace()
        # The merges and the xperf analysis only read the stopped traces. Each
        # step is its own abandonable call at Idle priority, issued only while
        # no "ide event" came (check_event) and started on the PC only while no
        # preempt wrote the stop file (analysis_guard): after the flag at most
        # the one step already running goes on, at Idle, and ends by itself
        # (decision B of the #32 review). Each cut holds the glitches that
        # caused it: it gets its own views (#32 B7, review M1).
        guard = analysis_guard(env["PC_ROOT"], began)
        names = []
        for body, produced in analysis(xperf(env), ps_quote(run_dir), cut["n"], diag):
            check_event()
            try:
                tps(env, f"{guard} ; {body}", timeout=1800, event="abandon")
            except StepError as e:
                if ANALYSIS_REFUSED in str(e):
                    raise sw.EventNow() from None   # the PC saw a preempt's stop file first
                raise
            if produced:
                names.append(produced)
        # The downloads (a near dump can be hundreds of MB, one per cut) and the
        # parses below give way to "ide event" at once (review M3).
        scp_dir = env["PC_TUNING_ROOT_SCP"] + "/runs/" + out.name
        for name in names:
            check_event()
            sw.scp(f"{env['PC_SSH']}:{scp_dir}/{name}", str(out / name), event="abandon")
        dpcisr_text = read_text(out / "dpcisr.txt")
    events = as_list(tps(env, f"Get-IemSystemEvents -Since {ps_quote(since)}", timeout=120, event="abandon"))
    watched = watch_lps(profile, args.audio_cpus)
    check_event()
    # An unreadable dpcisr (parse_dpcisr fails closed with ValueError) fails the
    # step with the raw file named, never a traceback.
    try:
        summary = lr.summarize(args.label, result["verdict"], report, dpcisr_text, polls, events, watched)
    except ValueError as e:
        raise StepError(f"{e} ({out / 'dpcisr.txt'})") from None
    summary["cuts"] = []
    for i in range(1, cut["n"] + 1):
        check_event()
        path = out / f"cut-{i}.dpcisr.txt"
        try:
            entry = {"cut": i, "findings": lr.budget_findings(lr.parse_dpcisr(read_text(path)), watched)}
        except ValueError as e:
            raise StepError(f"{e} ({path})") from None
        if diag:
            entry["near_glitch"] = lr.near_glitch(out / f"cut-{i}.near.txt", period_us=lr.PERIOD_US, check=check_event)
        summary["cuts"].append(entry)
    if diag:
        summary["near_glitch"] = lr.near_glitch(out / "near.txt", period_us=lr.PERIOD_US, check=check_event)
    (out / "summary.json").write_text(json.dumps(summary, indent=1), encoding="utf-8")
    record_measurement({"label": args.label, "summary": str(out / "summary.json"), "stable": (result["verdict"] or {}).get("stable")})
    print(json.dumps(summary))


def cmd_hwlat(env, args) -> None:
    """One hwlat run per CPU. The rows are written after every CPU, so a failed
    run (cmd_run refuses outcome "error"; a row that is no measurement of its
    CPU fails here) keeps what was measured before it."""
    state = sw.open_state()
    need_free(state)
    no_leftover_trace(state)
    path = raw(env, state) / f"hwlat-{stamp()}.json"
    rows: list[dict] = []
    for lp in parse_lps(args.lps):
        ns = argparse.Namespace(mode="hwlat", frames=None, seconds=args.seconds, burn_us=0, stress=0, panic_at=0, cycles=1,
                                cpu=lp, threshold_us=args.threshold_us, audio_cpus="", stress_cpus="")
        r = sw.cmd_run(env, ns)
        row = lr.hwlat_summary(json.loads(Path(r["report"]).read_text(encoding="utf-8")))
        rows.append(row)
        path.write_text(json.dumps(rows, indent=1), encoding="utf-8")
        if row["failed"]:
            raise StepError(f"hwlat on CPU {lp} measured nothing: outcome {row['outcome']}, placed {row['placed']!r}, "
                            f"priority {row['priority']!r} (the rows so far: {path})")
    print(json.dumps({"hwlat": rows, "file": str(path)}))


def unwind_failures(done: list[dict]) -> list[str]:
    """The unwind steps that did not complete: a spike not confirmed gone, or
    a step recorded with an error (unwind alarms and carries on past those)."""
    return [name for step in done for name, r in step.items()
            if (name == "stop-spike" and not r) or (isinstance(r, dict) and "error" in r)]


def cmd_reboot_prepare(env, args) -> None:
    # Under the window lock, like every unwind: the state is read where no
    # other unwind or bring-back runs (decision A of the #32 review).
    with sw.window_lock():
        state = sw.open_state()
        need_free(state)
        running = sw.spike_running(env)
        # A reboot is prepared only over a cleanly preempted window (I1). unwind
        # raises (after an owner alarm) before the buffer write when the spike was
        # not confirmed gone; a trace that did not stop or a mode lever not reverted
        # would carry into the reboot and the event mode it comes back in. Refuse on
        # any of them: the card stays free and the window open (preempt or to-event
        # brings REAPER back); the spike is never force-ended (I8, guard.md).
        done = sw.unwind(env, state, running, bring_back_reaper=False)
        failed = unwind_failures(done)
        if failed:
            sw.alarm(f"the unwind before the reboot did not complete ({', '.join(failed)}): no reboot is prepared; "
                     "the card stays free and the window open (preempt or to-event brings REAPER back)")
            raise StepError(f"the unwind failed at {', '.join(failed)}: no reboot prepared")
        st = tps(env, f"Get-IemTuningState -ProfilePath {sw.tuning_profile(env)}", timeout=120)
        sw.update_state({"card": "rebooting", "reboot": {"prepared_at": tps(env, "Get-IemNow", timeout=60)}})
    if st.get("boot_problem"):
        sw.alarm(f"the boot identity is unknown ({st['boot_problem']}): the pending and revert_pending lists of this "
                 "reboot prove nothing (#32 MINOR-4)")
    items = as_list(st["items"])
    print(json.dumps({"reboot-prepare": done, "pending": [i["key"] for i in items if i["pending"]],
                      "revert_pending": [i["key"] for i in items if i["revert_pending"]]}))


# An immediate, planned restart (reason: operating system reconfiguration) and
# never a forced one (I8): Microsoft documents that a timeout above 0 implies
# the force flag, so the timeout is 0. An app may then veto the restart;
# post-boot reports that as "the PC did not reboot after the request".
REBOOT_REQUEST = "& shutdown.exe /r /t 0 /d p:2:4 /c 'iemmixer S1c: owner-approved restart' ; $LASTEXITCODE"


def cmd_reboot(env, args) -> None:
    """Records the owner's quoted approval; without --by-owner it also asks
    Windows for an immediate, planned, graceful restart (REBOOT_REQUEST).
    Refused while the "ide event" flag exists (open_state; main pre-empts)."""
    state = sw.open_state()
    if state["card"] != "rebooting" or "reboot" not in state:
        raise StepError("run reboot-prepare first")
    check_approval(args.approval)
    sw.update_state(change=lambda st: st["reboot"].update(approval=args.approval, by="owner" if args.by_owner else "agent"))
    if args.by_owner:
        print(json.dumps({"reboot": "the owner restarts the PC himself; run post-boot afterwards"}))
        return
    try:
        code = sw.ps(env, REBOOT_REQUEST, timeout=60, event="ignore")
    except sw.NoReply as e:
        # The restart begins at once: the session may end before the exit code
        # comes back. An error REPLY (a plain StepError) propagates: nothing restarted.
        raise StepError(f"no answer to the restart request ({e}): the PC may be restarting; "
                        "run post-boot, which tells whether it rebooted") from None
    if int(code) != 0:
        raise StepError(f"the restart request exited {code}: nothing restarts; tell the owner")
    print(json.dumps({"reboot": "requested", "in_s": 0}))


def nap_unless_event(seconds: float) -> bool:
    """Waits `seconds` in 1 s slices; False as soon as the "ide event" flag
    exists (seen within a second, the window's 2 s rule)."""
    for _ in range(max(1, round(seconds))):
        if sw.event_now():
            return False
        time.sleep(1.0)
    return not sw.event_now()


def cmd_post_boot(env, args) -> None:
    state = sw.load_state()
    if "approval" not in state.get("reboot", {}):
        raise StepError("no approved reboot recorded in this window (reboot --approval ...)")
    deadline = time.monotonic() + 900
    while subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", env["PC_SSH"], "exit"], capture_output=True, check=False).returncode != 0:
        if time.monotonic() > deadline:
            sw.alarm("the PC is not reachable 15 min after the approved reboot: tell the owner (power cycle is his)")
            raise StepError("PC unreachable after the reboot")
        time.sleep(15)
    boot = tps(env, "Get-IemBootTime", timeout=60, event="ignore")
    checks = {"booted_after_request": boot > state["reboot"]["prepared_at"], "reaper": False, "handover": None,
              "fingerprint": [], "pending": [], "failed_items": []}
    # "ide event" is checked on every poll and every second between polls: on
    # the flag REAPER comes back at once (the event path) instead of after
    # ~5 min of waiting (review m10, round 3 m4).
    for _ in range(30):
        if sw.event_now():
            break
        if int(sw.ps(env, "@(Get-Process reaper -ErrorAction SilentlyContinue).Count", timeout=60, event="ignore")) > 0:
            checks["reaper"] = True
            break
        if not nap_unless_event(10):
            break
    # The window closes only with REAPER back (#32 B13): the bring-back starts it
    # through the start task when it did not start by itself (still a problem:
    # a reboot must come back in event mode) and runs the handover checks. A
    # failed bring-back keeps the window open (card "rebooting"): preempt or
    # to-event brings REAPER back later. Under the window lock, on the state as
    # read there (decision A): if a preempt in another process (`iempc event`)
    # already brought REAPER back and closed the window, nothing is done — two
    # bring-backs would trigger the meter bridge twice (a REAPER dialog, #9).
    with sw.window_lock():
        current = sw.load_state()
        if current.get("closed"):
            checks["handover"] = {"by": "another window process: the window was already closed with REAPER back"}
            back = True
        else:
            try:
                checks["handover"] = sw.bring_back(env, current)
            except StepError as e:
                checks["handover"] = {"error": str(e)}
            back = "error" not in checks["handover"]
            if back:
                sw.update_state({"card": "reaper", "closed": True})
    if sw.event_now():
        # The read-only checks wait for a dev window; main() pre-empts (a window
        # still open because the bring-back failed gets another one there).
        said = "REAPER brought back" if back else "the bring-back failed (the preempt tries again)"
        print(json.dumps({"post-boot": f"ide event: {said}, the checks skipped", "handover": checks["handover"]}), flush=True)
        raise sw.EventNow()
    current = tps(env, f"Get-IemReaperFingerprint -ProfilePath {sw.tuning_profile(env)}", timeout=120, event="ignore")
    checks["fingerprint"] = sw.fingerprint_diff(json.loads(baseline_path(env).read_text(encoding="utf-8")), current)
    st = tps(env, f"Get-IemTuningState -ProfilePath {sw.tuning_profile(env)}", timeout=120, event="ignore")
    items = as_list(st["items"])
    checks["pending"] = [i["key"] for i in items if i["pending"] or i["revert_pending"]]
    checks["failed_items"] = [i["key"] for i in items if i["journaled"] and not i["ok"]]
    checks["boot_problem"] = st.get("boot_problem")
    a = tps(env, "Get-IemCpuSample", timeout=60, event="ignore")
    time.sleep(10)
    b = tps(env, "Get-IemCpuSample", timeout=60, event="ignore")
    checks["interrupts"] = lr.cpu_rates([a, b])
    problems = post_boot_verdict(checks)
    state = sw.update_state({"post_boot": {"checks": checks, "problems": problems}})
    print(json.dumps({"post-boot": checks, "problems": problems}))
    if problems:
        sw.alarm("after the approved reboot: " + "; ".join(problems) + ". Revert: tuning_window undo --tier 3 in a dev window, "
                 "then the pre-approved revert reboot."
                 + ("" if state.get("closed") else " REAPER is not back, so the window stays open: spike_window to-event brings it back."))
        raise StepError("post-boot checks failed")


STEPS = ("tuning-setup", "enter", "exit", "apply", "undo", "measure", "trace-stop", "hwlat", "reboot-prepare")


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("tuning-setup", "inventory", "wpt-install", "exit", "state", "trace-stop", "reboot-prepare", "post-boot"):
        sub.add_parser(name)
    fp = sub.add_parser("fingerprint")
    g = fp.add_mutually_exclusive_group(required=True)
    g.add_argument("--baseline", action="store_true")
    g.add_argument("--check", action="store_true")
    en = sub.add_parser("enter")
    en.add_argument("--only", default="plan,governor,placement")
    en.add_argument("--idle", choices=("default", "c1", "disable"), default="default")
    for name in ("apply", "undo"):
        p = sub.add_parser(name)
        p.add_argument("--tier", type=int, choices=(2, 3), required=True)
        p.add_argument("--only", default="")
    m = sub.add_parser("measure")
    m.add_argument("--label", required=True)
    m.add_argument("--frames", type=int, default=32)
    m.add_argument("--seconds", type=int, default=600)
    m.add_argument("--burn-us", type=int, default=0)
    m.add_argument("--stress", type=int, default=0)
    m.add_argument("--audio-cpus", default="")
    m.add_argument("--stress-cpus", default="")
    m.add_argument("--trace", choices=("none", "dpc", "diag"), default="dpc")
    m.add_argument("--circular-mb", type=int, default=0)
    h = sub.add_parser("hwlat")
    h.add_argument("--lps", default="0-15")
    h.add_argument("--seconds", type=int, default=30)
    h.add_argument("--threshold-us", type=int, default=10)
    rb = sub.add_parser("reboot")
    rb.add_argument("--approval", required=True)
    rb.add_argument("--by-owner", action="store_true")
    pp = sub.add_parser("poll-script", help="print the measurement poll exactly as sw.ps sends it (for the Windows CI runner)")
    pp.add_argument("--root", required=True, help="a folder whose bin holds SpikePc.psm1 and GoldenPc.psm1")
    pp.add_argument("--governor", required=True)
    pp.add_argument("--pid", type=int, default=0)
    pp.add_argument("--tid", type=int, default=0)
    args = ap.parse_args(argv)
    if args.cmd == "poll-script":   # no window, no private env
        print(sw.ps_script(args.root, poll_body(args.governor, args.pid, args.tid)))
        return 0
    handlers = {"tuning-setup": cmd_tuning_setup, "inventory": cmd_inventory, "fingerprint": cmd_fingerprint, "wpt-install": cmd_wpt_install,
                "enter": cmd_enter, "exit": cmd_exit, "apply": cmd_apply, "undo": cmd_undo, "state": cmd_state, "measure": cmd_measure,
                "trace-stop": cmd_trace_stop,
                "hwlat": cmd_hwlat, "reboot-prepare": cmd_reboot_prepare, "reboot": cmd_reboot, "post-boot": cmd_post_boot}
    try:
        env = sw.load_env(Path(os.environ.get("SPIKE_ENV", str(Path.home() / ".config/iemmixer/asio-spike.env"))))
    except StepError as e:
        print(f"tuning_window: {e}", file=sys.stderr)
        return 1
    try:
        handlers[args.cmd](env, args)
        return 0
    except sw.EventNow:
        print(json.dumps({"event": "ide event (flag file)", "action": "preempt"}), flush=True)
    except StepError as e:
        print(f"tuning_window: {e}", file=sys.stderr, flush=True)
        if args.cmd not in STEPS or not sw.event_now():
            return 1
        print(json.dumps({"event": "ide event (flag file) after a failed step", "action": "preempt"}), flush=True)
    try:
        sw.cmd_preempt(env)
    except StepError as e:
        print(f"tuning_window: preempt: {e}", file=sys.stderr)
        return 1
    return 10


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
