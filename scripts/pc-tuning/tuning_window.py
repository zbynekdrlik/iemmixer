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
# The pure rules (profile, arguments, cuts, post-boot verdict), re-exported.
from tuning_rules import (APPROVAL, LABEL, LAYOUT_ROLES, MAX_CUTS, MEASURED, MODE_LEVERS, PROFILE_KEYS, boot_changed,  # noqa: E402,F401
                          check_approval, check_devices, check_layout, check_rss, label_ok, layout_lps, load_profile,
                          lp_list, lp_number, mode_only, parse_lps, post_boot_verdict, should_cut, watch_lps)

PROFILE = Path(os.environ.get("TUNING_PROFILE", str(Path.home() / ".config/iemmixer/pc-tuning.json")))
ADK_URL = "https://go.microsoft.com/fwlink/?linkid=2289980"  # ADK 10.1.26100.9457 (September 2026), design note [7]


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


# The tuning steps that change the PC (enter, exit, apply, undo) are PC changes
# with an intent (sw.pc_change, F2 round 3 MAJOR): their bounds are the ssh
# calls' bounds, and their bodies refuse on the PC while the spike's stop file
# exists, before the tuning modules load (tps_change).
MODE_S = 240
APPLY_S = 600


def tps_change(env: dict[str, str], body: str, **kw):
    return sw.ps(env, sw.changing(env, sw.tuning_body(env, body)), **kw)


def late_journal(env: dict[str, str], step: str):
    """A journal step that ended after the window was closed: the preempt
    deferred its tuning-exit to it (two journal writers at once lose entries),
    so the exit runs here, now that the step's own write is over — always after
    an enter that ran (it may have applied levers after the pre-emption),
    otherwise while the mode is recorded as entered (a refused step's state is
    the one before it: review of lane G2, finding 8). An apply or undo that ran
    changed global levers during the event: the owner hears it."""
    def follow_up(state: dict, ran: bool) -> None:
        if step in ("apply", "undo") and ran:
            sw.alarm(f"{step} ended after the window was pre-empted: the global levers it changed stay as they are (no "
                     "mode levers); compare the REAPER fingerprint (fingerprint --check) in the next window")
        if (step == "enter" and ran) or state.get("tuning_mode"):
            rows = as_list(tps(env, f"Exit-IemTuningMode -ProfilePath {sw.tuning_profile(env)}", timeout=MODE_S, event="ignore"))
            sw.update_state({"tuning_mode": False})
            print(json.dumps({"late-exit": rows}), flush=True)
            sw.alarm_exit_problems(rows)
    return follow_up


def cmd_enter(env, args) -> None:
    need_free(sw.open_state())
    only = mode_only(args.only)
    # tuning_mode is recorded with the intent, before the action: a preempt reverts
    # even a half-done enter (or the late enter does, MAJOR).
    rows = as_list(sw.pc_change("enter", MODE_S, lambda: tps_change(
        env, f"Enter-IemTuningMode -ProfilePath {sw.tuning_profile(env)} -Only @({', '.join(ps_quote(x) for x in only)}) "
             f"-Idle {ps_quote(args.idle)}", timeout=MODE_S),
        fields={"tuning_mode": True}, check=need_free, late=late_journal(env, "enter")))
    record_step({"enter": only, "idle": args.idle, "at": stamp()})
    print(json.dumps({"enter": rows}))
    failed = [r for r in rows if r.get("action") == "failed"]
    if failed:
        raise StepError(f"{len(failed)} mode item(s) failed: " + "; ".join(f"{r['key']}: {r['error']}" for r in failed))


def cmd_exit(env, args) -> None:
    sw.open_state()
    rows = as_list(sw.pc_change("exit", MODE_S, lambda: tps_change(env, f"Exit-IemTuningMode -ProfilePath {sw.tuning_profile(env)}",
                                                                   timeout=MODE_S),
                                after={"tuning_mode": False}, late=late_journal(env, "exit")))
    print(json.dumps({"exit": rows}))
    sw.alarm_exit_problems(rows)   # the exit completed; unconvertible journal entries reach the owner


def only_arg(text: str) -> str:
    groups = [x.strip() for x in text.split(",") if x.strip()]
    if any(not re.fullmatch(r"[a-z]+(:[a-z0-9-]+)?", g) for g in groups):
        raise StepError("--only takes group names such as services,updates or irq:card")
    return "@(" + ", ".join(ps_quote(g) for g in groups) + ")"


def cmd_apply(env, args) -> None:
    need_free(sw.open_state())
    only = only_arg(args.only)
    rows = as_list(sw.pc_change("apply", APPLY_S, lambda: tps_change(
        env, f"Invoke-IemTuningApply -ProfilePath {sw.tuning_profile(env)} -Tier {args.tier} -Only {only}", timeout=APPLY_S),
        check=need_free, late=late_journal(env, "apply")))
    record_step({"apply": args.tier, "only": args.only, "at": stamp()})
    print(json.dumps({"apply": rows}))
    failed = [r for r in rows if r.get("action") == "failed"]
    if failed:
        raise StepError(f"{len(failed)} item(s) failed: " + "; ".join(f"{r['key']}: {r['error']}" for r in failed))


def cmd_undo(env, args) -> None:
    need_free(sw.open_state())
    only = only_arg(args.only)
    rows = as_list(sw.pc_change("undo", APPLY_S, lambda: tps_change(
        env, f"Undo-IemTuning -ProfilePath {sw.tuning_profile(env)} -Tier {args.tier} -Only {only}", timeout=APPLY_S),
        check=need_free, late=late_journal(env, "undo")))
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
    fails) before EventNow goes on (review round 3, m5). That stop runs under
    the window lock (F2 round 3, m4): never at the same time as the unwind's
    own trace-stop, whose logman stop would then fail; Stop-IemTraceSessions
    is idempotent, so whichever comes second finds nothing to stop."""
    try:
        tps(env, f"Start-IemTrace -Xperf {xperf(env)} -Dir {ps_quote(run_dir)}{opt}", timeout=120, event="finish")
        check_event()
    except sw.EventNow:
        try:
            with sw.window_lock():
                sw.check_trace_stop(sw.ps(env, sw.trace_stop_body(env, run_dir), timeout=sw.TRACE_STOP_CALL_S, event="ignore"))
                sw.update_state(change=lambda st: st.update(trace=None) if st.get("trace") == run_dir else None)
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
    files are deleted once the merge succeeded and the merged trace exists and
    is not empty (a soak would otherwise double its disk use); a failed merge,
    or one that wrote nothing, throws first and keeps them (F2 round 3, m10)."""
    out = f"{base[:-1]}.etl" if base else "trace.etl"
    return (f"$m = @(foreach ($n in @('{base}kernel.etl', '{base}markers.etl')) {{ $p = Join-Path {d} $n ; "
            f"if (Test-Path -LiteralPath $p) {{ $p }} }}) ; "
            f"[void](Invoke-IemXperf -Xperf {x} -Arguments (@('-merge') + $m + @((Join-Path {d} '{out}')))) ; "
            f"$o = Join-Path {d} '{out}' ; "
            f"if (-not (Test-Path -LiteralPath $o) -or (Get-Item -LiteralPath $o).Length -le 0) "
            f"{{ throw \"xperf -merge left no trace in $o (the raw files are kept)\" }} ; "
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


def analysis_step(root: str, since: str, body: str) -> str:
    """One analysis step as the PC receives it inside sw.ps: the guard first,
    then the tuning modules' import (an Add-Type compile, so at Idle and never
    for a refused step; F2 round 3, m6), then the step. `analysis-script`
    prints the same composition for the Windows CI runner (m11)."""
    return f"{analysis_guard(root, since)} ; {sw.measure_import(root)} ; {body}"


# The step body the Windows CI runner sends through analysis_step: the priority
# the guard set, IemMeasure's clock, whether IemTuning loaded too (IemMeasure
# keeps a failed IemTuning load to itself, so Get-IemNow alone proves nothing),
# and the TEMP its Add-Type compiled in (#15: the admin-only <elevated root>\temp).
ANALYSIS_PROBE = ("[pscustomobject]@{ priority = \"$((Get-Process -Id $PID).PriorityClass)\"; now = Get-IemNow; "
                  "tuning = [bool](Get-Command -Name ConvertTo-IemLpNumber -ErrorAction SilentlyContinue); temp = $env:TEMP }")


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
        names = []
        for body, produced in analysis(xperf(env), ps_quote(run_dir), cut["n"], diag):
            check_event()
            try:
                sw.ps(env, analysis_step(env["PC_ROOT"], began, body), timeout=1800, event="abandon")
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
        intent = state.get("in_flight")
        if sw.intent_live(intent):
            # Another process's step still changes the PC: no cleanly preempted window (MAJOR).
            raise StepError(f"a PC step is in flight ({intent['step']}): wait for it; no reboot prepared")
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
    # Read-only, so outside the lock and abandonable (F2 round 3, m3): the tuning
    # state compiles IemTuning, and a preempt never waits for it.
    st = tps(env, f"Get-IemTuningState -ProfilePath {sw.tuning_profile(env)}", timeout=120, event="abandon")
    prepared_at = tps(env, "Get-IemNow", timeout=60, event="abandon")
    if not st.get("boot_token"):
        # post-boot tells the reboot by this token alone (decision 5): without it the
        # reboot could never be confirmed (review of lane G2, finding 7).
        sw.alarm(f"the boot identity is unknown ({st.get('boot_problem') or 'no boot token'}): no reboot is prepared, "
                 "since post-boot could not tell whether the PC rebooted; fix the boot key, then reboot-prepare again "
                 "(the card stays free and the window open)")
        raise StepError("no boot token on the PC: no reboot prepared")

    def prepare(current: dict) -> None:
        # The state as saved now: a preempt may have closed the window (REAPER back)
        # or taken the card meanwhile; then no reboot is prepared.
        if current.get("closed") or current.get("card") != "free":
            raise StepError(f"the window was closed or its card taken meanwhile (card {current.get('card')!r}): "
                            "no reboot prepared")
        # A step begun while the lock was free (review of lane G2, finding 3): a change
        # in flight, a buffer not verified as restored (after the reboot REAPER may
        # hold the driver, I2) or a mode recorded as entered is no clean window (I1).
        if sw.intent_live(current.get("in_flight")) or (sw.buffer_touched(current) and not current.get("pref_restored")) \
                or current.get("tuning_mode"):
            raise StepError("a window step ran while the reboot was being prepared (a change in flight, the buffer or "
                            "the mode): no reboot prepared")
        # The boot token tells post-boot whether the PC rebooted (decision 5); the
        # time is information only.
        current.update(card="rebooting", reboot={"prepared_at": prepared_at, "boot_token": st.get("boot_token")})

    sw.update_state(change=prepare)
    items = as_list(st["items"])
    print(json.dumps({"reboot-prepare": done, "pending": [i["key"] for i in items if i["pending"]],
                      "revert_pending": [i["key"] for i in items if i["revert_pending"]]}))


# An immediate, planned restart (reason: operating system reconfiguration) and
# never a forced one (I8): Microsoft documents that a timeout above 0 implies
# the force flag, so the timeout is 0. An app may then veto the restart;
# post-boot reports that as "the PC did not reboot after the request". The
# repository's one restart: the integrity scan passes it only marked and in
# this literal form (check_integrity RESTART_SAFE).
REBOOT_REQUEST = "& shutdown.exe /r /t 0 /d p:2:4 /c 'iemmixer S1c: owner-approved restart' ; $LASTEXITCODE"  # iemmixer:graceful-restart


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
    # Whether the PC rebooted is told by the boot token, read with the tuning
    # state below (decision 5): unknown until then (the event path skips it).
    checks = {"booted_after_request": None, "reaper": False, "handover": None,
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
                # reboot-prepare's unwind wrote the spike's stop file and kept it (the
                # window stayed open); this close removes it (F2 round 3, m1).
                checks["stop_file"] = sw.clear_stop(env)
    if sw.event_now():
        # The read-only checks wait for a dev window; main() pre-empts (a window
        # still open because the bring-back failed gets another one there).
        said = "REAPER brought back" if back else "the bring-back failed (the preempt tries again)"
        print(json.dumps({"post-boot": f"ide event: {said}, the checks skipped", "handover": checks["handover"]}), flush=True)
        raise sw.EventNow()
    current = tps(env, f"Get-IemReaperFingerprint -ProfilePath {sw.tuning_profile(env)}", timeout=120, event="ignore")
    checks["fingerprint"] = sw.fingerprint_diff(json.loads(baseline_path(env).read_text(encoding="utf-8")), current)
    st = tps(env, f"Get-IemTuningState -ProfilePath {sw.tuning_profile(env)}", timeout=120, event="ignore")
    checks["booted_after_request"], checks["boot_unknown"] = boot_changed(state["reboot"].get("boot_token"), st.get("boot_token"))
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
    asc = sub.add_parser("analysis-script", help="print an analysis step's start (guard, import, a probe) exactly as sw.ps "
                                                 "sends it (for the Windows CI runner)")
    asc.add_argument("--root", required=True, help="a folder whose bin holds the spike bundle")
    asc.add_argument("--since", required=True, help="the analysis start, PC time (Get-IemNow)")
    args = ap.parse_args(argv)
    if args.cmd == "poll-script":   # no window, no private env
        print(sw.ps_script(args.root, poll_body(args.governor, args.pid, args.tid)))
        return 0
    if args.cmd == "analysis-script":   # no window, no private env
        print(sw.ps_script(args.root, analysis_step(args.root, args.since, ANALYSIS_PROBE)))
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
    sw.exit_on_signals()
    sys.exit(main(sys.argv[1:]))
