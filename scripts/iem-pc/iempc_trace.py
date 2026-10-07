"""`iempc trace --label L --seconds N [--circular-mb M] [--profile PATH]` (#15,
the re-plan of 2026-10-07, approach 1 step 2): a kernel DPC/ISR trace on the
PC while the guard's engine plays, in dev time.

1. Refused before the PC: no PC_XPERF, an open S1a/S1c window, bad arguments
   (a trace over MAX_LINEAR_S needs `--circular-mb`: a linear kernel file
   grows with the run), a profile tuning_rules refuses.
2. `iemmode status`: the guard settled in dev (no switch) with an engine that
   plays (not parked, not faulted); its build (a bundle SHA, fetched on this
   box), frames and its callbacks, missed periods and resets (`Reply.engine`).
3. A read-only preflight: the elevated tuning folder (%ProgramData%\\iemmixer\\
   tuning as the PC resolves the known folder; filled by `iempc
   tuning-install`) must hold that bundle's two tuning modules (their sha256
   against its SHA256SUMS), else no trace and the hint to run tuning-install.
   Every later import re-checks both hashes on the PC first (`hash_check`):
   the elevated ssh session imports nothing else, and never the bundle's copy
   in the user's root.
4. `Start-IemTrace -Xperf <PC_XPERF> -Dir <PC_ROOT>\\traces\\<label>-<UTC
   stamp>` (the kernel's DPC and INTERRUPT events and the engine's marker
   session; `--circular-mb` keeps the kernel file circular at that size).
5. This box waits N seconds and looks at the "ide event" flag every POLL_S
   (2 s). A flag stops the trace at once (`Stop-IemTraceSessions`, IemMeasure
   imported 'stop-only': nothing compiles), then the event path runs (exit
   10). SIGTERM and SIGHUP end the wait through SystemExit, and any failure
   once the start was sent ends the command, both after the same stop. That
   stop's own failure is reported (`trace_stop: failed`), never raised over
   the cause; after a start that never returned (StillRunning, a signal during
   it) a stop that found nothing is `unconfirmed`: the start may still begin
   the trace on the PC.
6. The counters again (a failed read is `after_error`; the trace is analysed
   all the same), the stop (confirmed: every session stopped, none kept;
   spike_window.check_trace_stop; a failure names the trace left running),
   then the merge (its raw files removed once the merged trace exists) and
   `xperf -a dpcisr`, the two bodies tuning_window.analysis composes, each its
   own call at Idle priority, given up at once on a flag (the step on the PC
   ends by itself at Idle).
7. The report copied back into the state dir (traces/<run>/dpcisr.txt) and
   parsed with latency_report: per-module bursts (top_modules), the budget
   findings on the profile's card and audio processors (budget_findings), and
   each module's usage on those processors. One JSON object, also saved as
   traces/<run>/summary.json: label, seconds, circular_mb, run, build, frames,
   the callbacks/missed/resets deltas (None when the reads saw different
   engines: same_engine false), after_error, watched, top_modules, findings,
   report.

The report and the summary name drivers: private site data (P6), never pasted
raw into a public ticket. PC_XPERF (xperf.exe's full path on the PC: WPT 10 or
newer, Microsoft-signed, which Invoke-IemXperf checks) is an optional key of
iempc's private env. The profile is the private ~/.config/iemmixer/
pc-tuning.json ($TUNING_PROFILE, or `--profile`), the one `tuning-install` puts
on the PC. The trace's files stay in its run folder on the PC.

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import contextlib
import datetime as dt
import json
import re
import signal
import sys
from pathlib import Path
from typing import Iterator

import iempc_tuning

MAX_SECONDS = 24 * 3600
# A longer trace needs --circular-mb: a linear kernel file grows with the run.
MAX_LINEAR_S = 3600
MAX_CIRCULAR_MB = 16384
# Start-IemTrace: the import (IemTuning's compile) and xperf -on.
START_S = 120
# One analysis step (a merge, or xperf -a dpcisr) at Idle priority.
ANALYSIS_S = 1800
COUNTERS = ("callbacks", "missed", "resets")
IDLE = "(Get-Process -Id $PID).PriorityClass = 'Idle'"
# The preflight (read-only): the elevated tuning folder as the PC resolves it,
# and its two modules' sha256 (null for one that is absent).
PREFLIGHT = (f"$t = {iempc_tuning.TUNING_DIR_PS} ; "
             "$h = { param($p) if (Test-Path -LiteralPath $p -PathType Leaf) "
             "{ (Get-FileHash -LiteralPath $p -Algorithm SHA256).Hash.ToLowerInvariant() } } ; "
             "[pscustomobject]@{ dir = $t; tuning = (& $h (Join-Path $t 'IemTuning.psm1')); "
             "measure = (& $h (Join-Path $t 'IemMeasure.psm1')) }")
DRIVE_PATH = re.compile(r"[A-Za-z]:\\[^\x00-\x1f\"]+")


def s1c(ip):
    """S1c's modules, imported only when `trace` runs (the event path never
    depends on them): tuning_rules (the label, the watched processors),
    latency_report (the parse), spike_window (the trace stop's check and
    bound) and tuning_window (the merge and dpcisr bodies it composes)."""
    tr = iempc_tuning.tuning_rules()
    import latency_report as lr
    sw = ip.spike_module()
    import tuning_window as tw
    return tr, lr, sw, tw


@contextlib.contextmanager
def ended_by_signals() -> Iterator[None]:
    """SIGTERM and SIGHUP end the trace through SystemExit, so its stop runs
    (pc_change.exit_on_signals' rule, for this command only); the handlers
    before it come back. A signal the platform lacks is skipped."""
    def leave(signum, _frame) -> None:
        raise SystemExit(128 + signum)

    saved = {}
    for name in ("SIGTERM", "SIGHUP"):
        s = getattr(signal, name, None)
        if s is not None:
            saved[s] = signal.getsignal(s)
            signal.signal(s, leave)
    try:
        yield
    finally:
        for s, handler in saved.items():
            signal.signal(s, handler if handler is not None else signal.SIG_DFL)


def check_args(ip, tr, args) -> tuple[str, int, int | None]:
    if not tr.label_ok(args.label or ""):
        raise ip.Refused(f"trace: --label {args.label!r}: 1 to 40 of a-z 0-9 -, starting with a letter or digit")
    if not 1 <= args.seconds <= MAX_SECONDS:
        raise ip.Refused(f"trace: --seconds must be 1..{MAX_SECONDS}")
    if args.circular_mb is not None and not 1 <= args.circular_mb <= MAX_CIRCULAR_MB:
        raise ip.Refused(f"trace: --circular-mb must be 1..{MAX_CIRCULAR_MB}")
    if args.circular_mb is None and args.seconds > MAX_LINEAR_S:
        raise ip.Refused(f"trace: a trace longer than {MAX_LINEAR_S} s needs --circular-mb (a linear kernel file grows "
                         "with the run)")
    return args.label, args.seconds, args.circular_mb


def engine_seen(ip, code: int, reply: dict | None) -> dict:
    """The engine of a guard settled in dev, playing, its fields as the guard
    reports them; else refused."""
    if code != 0 or not isinstance(reply, dict):
        raise ip.Refused(f"no trace: iemmode status failed (exit {code})")
    if reply.get("mode") != "dev":
        raise ip.Refused(f"no trace: the guard is in mode {reply.get('mode')}, not dev")
    if reply.get("switching"):
        raise ip.Refused(f"no trace: a switch runs ({json.dumps(reply['switching'])})")
    engine = reply.get("engine")
    if not isinstance(engine, dict):
        raise ip.Refused("no trace: no engine runs (the guard's status shows none)")
    for key in (*COUNTERS, "spawns", "frames"):
        value = engine.get(key)
        if type(value) is not int or value < 0:
            raise ip.Refused(f"no trace: the engine's {key} reads {value!r}")
    if not isinstance(engine.get("build"), str) or not ip.SHA.fullmatch(engine["build"]):
        raise ip.Refused(f"no trace: the engine's build reads {engine.get('build')!r}, not a bundle SHA")
    for state in ("parked", "faulted"):
        if engine.get(state) is not False:
            raise ip.Refused(f"no trace: the engine is {state} ({engine.get(state)!r}); a trace measures an engine that plays")
    return engine


def tuning_modules(ctx, ip, build: str) -> dict[str, tuple[str, str]]:
    """The elevated tuning folder's two modules as (path on the PC, sha256): the
    running bundle's (fetched and attested here), read by the preflight."""
    sums = ip.need_record(build).get("sums") or {}
    want = {key: sums.get(f"tuning/{name}") for key, name in iempc_tuning.MODULES.items()}
    if not all(want.values()):
        raise ip.Refused(f"no trace: bundle {build} lists no tuning modules")
    fix = f"iempc tuning-install --sha {build}"
    r = ip.run_module(ctx.env, PREFLIGHT, ip.STATUS_S, ctx.watch(abandon=True))
    if not isinstance(r, dict) or not isinstance(r.get("dir"), str) or not DRIVE_PATH.fullmatch(r["dir"]):
        raise ip.Refused(f"no trace: the elevated tuning folder reads {r!r}: run '{fix}'")
    for key, name in iempc_tuning.MODULES.items():
        if r.get(key) != want[key]:
            raise ip.Refused(f"no trace: the elevated tuning folder's {name} is {r.get(key) or 'absent'}, not bundle "
                             f"{build}'s (the running engine's): run '{fix}'")
    return {key: (f"{r['dir']}\\{name}", want[key]) for key, name in iempc_tuning.MODULES.items()}


def run_measure(ctx, ip, mods: dict, body: str, timeout: float, event: str, pre: str = "", stop_only: bool = False):
    """`body` after IemMeasure's import from the elevated tuning folder, both
    modules' sha256 checked on the PC first; `stop_only`: IemMeasure alone,
    IemTuning never loads (nothing compiles)."""
    (tuning, tuning_hex), (measure, measure_hex) = mods["tuning"], mods["measure"]
    if stop_only:
        load = f"{ip.hash_check(measure, measure_hex)} ; Import-Module {ip.ps_quote(measure)} -ArgumentList 'stop-only' -Force ; "
        return ip.run_module(ctx.env, body, timeout, event, pre=pre + load)
    return ip.run_module(ctx.env, body, timeout, event, pre=f"{pre}{ip.hash_check(tuning, tuning_hex)} ; ",
                         module=measure, module_hex=measure_hex)


def stop(ctx, ip, sw, mods: dict, run_dir: str, event: str) -> dict:
    """The trace stop, confirmed (check_trace_stop); a failure names the trace that may still run."""
    body = f"Stop-IemTraceSessions -Dir {ip.ps_quote(run_dir)} -TimeoutSeconds {sw.TRACE_STOP_LOGMAN_S}"
    try:
        return sw.check_trace_stop(run_measure(ctx, ip, mods, body, sw.TRACE_STOP_CALL_S, event, stop_only=True))
    except (ip.StepError, sw.StepError) as e:   # sw's own StepError: check_trace_stop
        raise ip.StepError(f"{e}: the kernel trace may still run on the PC (stop it with Stop-IemTraceSessions -Dir "
                           f"{run_dir})") from None


def abandon(ctx, ip, sw, mods: dict, run_dir: str, cause: BaseException, start_over: bool) -> None:
    """The stop after a flag, a signal or a failure once the start was sent: at
    once, without the merge, whatever the flag says. Its own failure is
    reported, never raised over the cause. `start_over`: the start call
    returned; when it did not, a stop that found nothing proves nothing."""
    why = "ide event" if isinstance(cause, ip.EventNow) else type(cause).__name__
    try:
        r = stop(ctx, ip, sw, mods, run_dir, "ignore")
    except ip.StepError as e:
        print(f"iempc: WARNING: the kernel trace may still run on the PC after {why}: {e}", file=sys.stderr, flush=True)
        ip.emit({"trace_stop": "failed", "dir": run_dir, "error": str(e)[-800:]})
        return
    if not start_over and not r.get("stopped"):
        print(f"iempc: WARNING: the kernel trace may still start or run on the PC: its start did not return ({why}) and "
              f"the stop found nothing to stop; once the start is over, stop it with Stop-IemTraceSessions -Dir {run_dir}",
              file=sys.stderr, flush=True)
        ip.emit({"trace_stop": "unconfirmed", "dir": run_dir, "why": why})
        return
    print(f"iempc: the kernel trace was stopped ({why})", file=sys.stderr, flush=True)


def check_event(ctx, ip) -> None:
    if ctx.watch(abandon=True) != "ignore" and ip.event_now():
        raise ip.EventNow()


def deltas(before: dict, after) -> dict:
    """The counters over the trace: the same engine (its build and the guard's
    start count unchanged) on both reads, else None."""
    same = (isinstance(after, dict) and after.get("build") == before.get("build")
            and after.get("spawns") == before.get("spawns")
            and all(type(after.get(k)) is int and after[k] >= before[k] for k in COUNTERS))
    return {**{k: after[k] - before[k] if same else None for k in COUNTERS}, "same_engine": same}


def watched_usage(parsed: dict, lps: list[int]) -> dict:
    """Each module that ran on a watched processor: its usec there, per processor."""
    keys = [str(lp) for lp in lps]
    return {kind: {module: {k: cpus.get(k, 0) for k in keys}
                   for module, cpus in parsed["usage"][kind].items() if any(cpus.get(k, 0) > 0 for k in keys)}
            for kind in ("dpc", "isr")}


def trace(ctx, ip) -> int:
    """`iempc trace` (dev time)."""
    env = ctx.env
    if not env.get("PC_XPERF"):
        raise ip.Refused("trace: PC_XPERF missing in the private env (xperf.exe's full path on the PC)")
    ip.refuse_open_window("trace")
    tr, lr, sw, tw = s1c(ip)
    label, seconds, circular_mb = check_args(ip, tr, ctx.args)
    profile = iempc_tuning.load_profile(ip, Path(ctx.args.profile) if ctx.args.profile else iempc_tuning.PROFILE)
    lps = tr.watch_lps(profile, "")
    code, reply, _ = ip.iemmode(env, ["status"], ip.STATUS_S, ctx.watch(abandon=True))
    before = engine_seen(ip, code, reply)
    mods = tuning_modules(ctx, ip, before["build"])
    run = f"{label}-{dt.datetime.now(dt.timezone.utc).strftime('%Y%m%dT%H%M%SZ')}"
    rel = f"traces/{run}"
    run_dir = ip.pc_join(env["PC_ROOT"], rel)
    xperf, d = ip.ps_quote(env["PC_XPERF"]), ip.ps_quote(run_dir)
    after, after_error, start_over = None, None, False
    with ended_by_signals():
        try:
            try:
                run_measure(ctx, ip, mods, f"Start-IemTrace -Xperf {xperf} -Dir {d}{tw.trace_options('', circular_mb or 0)}",
                            START_S, ctx.watch(abandon=False))
            except ip.StillRunning:
                raise
            except (ip.StepError, ip.EventNow):
                start_over = True   # the PC answered, or the start completed before the flag ("finish")
                raise
            start_over = True
            ip.pause(ctx, seconds)
            try:
                code, reply, _ = ip.iemmode(env, ["status"], ip.STATUS_S, ctx.watch(abandon=True))
                after = reply.get("engine") if code == 0 and isinstance(reply, dict) else None
            except ip.StepError as e:   # the counters only: the trace is stopped and analysed all the same
                after_error = str(e)[-800:]
        except BaseException as e:
            abandon(ctx, ip, sw, mods, run_dir, e, start_over)
            raise
    try:
        stop(ctx, ip, sw, mods, run_dir, ctx.watch(abandon=False))
    except ip.StepError as e:
        ip.emit({"trace_stop": "failed", "dir": run_dir, "error": str(e)[-800:]})
        raise
    for body, _ in tw.analysis(xperf, d, 0, False):
        check_event(ctx, ip)
        run_measure(ctx, ip, mods, body, ANALYSIS_S, ctx.watch(abandon=True), pre=f"{IDLE} ; ")
    check_event(ctx, ip)
    out = ip.state_dir() / "traces" / run
    out.mkdir(parents=True, exist_ok=True)
    local = out / "dpcisr.txt"
    ip.scp(ip.remote(env, f"{rel}/dpcisr.txt"), str(local), ctx.watch(abandon=True))
    try:
        parsed = lr.parse_dpcisr(local.read_text(encoding="utf-8", errors="replace"))
    except ValueError as e:
        raise ip.StepError(f"{e} ({local})") from None
    doc = {"label": label, "seconds": seconds, "circular_mb": circular_mb, "run": run, "build": before["build"],
           "frames": before["frames"], **deltas(before, after), "after_error": after_error,
           "watched": {"lps": lps, **watched_usage(parsed, lps)},
           "top_modules": lr.top_modules(parsed), "findings": lr.budget_findings(parsed, lps), "report": str(local)}
    ip.write_json(out / "summary.json", doc)
    ip.emit(doc)
    return 0
