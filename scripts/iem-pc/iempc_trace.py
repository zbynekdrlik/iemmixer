"""`iempc trace --label L --seconds N [--circular-mb M] [--profile PATH]` (#15,
the re-plan of 2026-10-07, approach 1 step 2): a kernel DPC/ISR trace on the
PC while the guard's engine plays, in dev time.

1. `iemmode status`: the guard settled in dev (no switch) with an engine; its
   build and its callbacks, missed periods and resets (`Reply.engine`).
2. On the PC, elevated over ssh: IemMeasure.psm1 imported from the elevated
   tuning folder (%ProgramData%\\iemmixer\\tuning, filled by `iempc
   tuning-install`; never the bundle's copy in the user's root), then
   `Start-IemTrace -Xperf <PC_XPERF> -Dir <PC_ROOT>\\traces\\<label>-<UTC stamp>`
   (the kernel's DPC and INTERRUPT events and the engine's marker session;
   `--circular-mb` keeps the kernel file circular at that size).
3. This box waits N seconds and looks at the "ide event" flag every POLL_S
   (2 s). A flag stops the trace at once (`Stop-IemTraceSessions`, IemMeasure
   imported 'stop-only': nothing compiles) and runs the event path (exit 10).
   SIGTERM and SIGHUP end the wait through SystemExit, so the stop runs too,
   as it does after any failure once the start was sent; a stop that fails
   there is reported (the trace may still run), never raised over the cause.
4. The counters again, the stop (confirmed: every session stopped, none kept;
   spike_window.check_trace_stop), then the merge (its raw files removed once
   the merged trace exists) and `xperf -a dpcisr`, the two bodies
   tuning_window.analysis composes, each its own call at Idle priority, given
   up at once on a flag (the step on the PC ends by itself at Idle).
5. The report copied back into the state dir (traces/<run>/dpcisr.txt) and
   parsed with latency_report: per-module bursts (top_modules), the budget
   findings on the profile's card and audio processors (budget_findings), and
   each module's usage on those processors. One JSON object, also saved as
   traces/<run>/summary.json: label, seconds, circular_mb, run, build, the
   callbacks/missed/resets deltas (None when the reads saw different engines:
   same_engine false), after_error, watched, top_modules, findings, report.

The report and the summary name drivers: private site data (P6), never pasted
raw into a public ticket. PC_XPERF (xperf.exe's full path on the PC: WPT 10 or
newer, Microsoft-signed, which Invoke-IemXperf checks) is an optional key of
iempc's private env; `trace` refuses without it. The profile is the private
~/.config/iemmixer/pc-tuning.json ($TUNING_PROFILE, or `--profile`), the one
`tuning-install` puts on the PC. The trace's files stay in its run folder on
the PC.

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import contextlib
import datetime as dt
import json
import signal
import sys
from pathlib import Path
from typing import Iterator

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "pc-tuning"))
import iempc_tuning  # noqa: E402
import latency_report as lr  # noqa: E402
import tuning_rules as tr  # noqa: E402

MAX_SECONDS = 24 * 3600
MAX_CIRCULAR_MB = 16384
# Start-IemTrace: the import (IemTuning's compile) and xperf -on.
START_S = 120
# One analysis step (a merge, or xperf -a dpcisr) at Idle priority.
ANALYSIS_S = 1800
COUNTERS = ("callbacks", "missed", "resets")
MEASURE = f"(Join-Path {iempc_tuning.TUNING_DIR_PS} 'IemMeasure.psm1')"
IMPORT = f"Import-Module {MEASURE} -Force -Global"
STOP_IMPORT = f"Import-Module {MEASURE} -ArgumentList 'stop-only' -Force -Global"
IDLE = "(Get-Process -Id $PID).PriorityClass = 'Idle'"


def s1c(ip):
    """spike_window and tuning_window (imported for `trace` only): the trace
    stop's check and bound, the merge and dpcisr bodies they already compose."""
    sw = ip.spike_module()
    import tuning_window as tw
    return sw, tw


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


def check_args(ip, args) -> tuple[str, int, int | None]:
    if not tr.label_ok(args.label or ""):
        raise ip.Refused(f"trace: --label {args.label!r}: 1 to 40 of a-z 0-9 -, starting with a letter or digit")
    if not 1 <= args.seconds <= MAX_SECONDS:
        raise ip.Refused(f"trace: --seconds must be 1..{MAX_SECONDS}")
    if args.circular_mb is not None and not 1 <= args.circular_mb <= MAX_CIRCULAR_MB:
        raise ip.Refused(f"trace: --circular-mb must be 1..{MAX_CIRCULAR_MB}")
    return args.label, args.seconds, args.circular_mb


def engine_seen(ip, code: int, reply: dict | None) -> dict:
    """The engine of a guard settled in dev, its counters numbers; else refused."""
    if code != 0 or not isinstance(reply, dict):
        raise ip.Refused(f"no trace: iemmode status failed (exit {code})")
    if reply.get("mode") != "dev":
        raise ip.Refused(f"no trace: the guard is in mode {reply.get('mode')}, not dev")
    if reply.get("switching"):
        raise ip.Refused(f"no trace: a switch runs ({json.dumps(reply['switching'])})")
    engine = reply.get("engine")
    if not isinstance(engine, dict):
        raise ip.Refused("no trace: no engine runs (the guard's status shows none)")
    for key in (*COUNTERS, "spawns"):
        value = engine.get(key)
        if type(value) is not int or value < 0:
            raise ip.Refused(f"no trace: the engine's {key} reads {value!r}")
    return engine


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


def stop_body(run_dir: str, sw, ip) -> str:
    return f"{STOP_IMPORT} ; Stop-IemTraceSessions -Dir {ip.ps_quote(run_dir)} -TimeoutSeconds {sw.TRACE_STOP_LOGMAN_S}"


def stop(ctx, ip, sw, run_dir: str, event: str) -> dict:
    try:
        return sw.check_trace_stop(ip.run_module(ctx.env, stop_body(run_dir, sw, ip), sw.TRACE_STOP_CALL_S, event))
    except sw.StepError as e:   # spike_window's own StepError (check_trace_stop)
        raise ip.StepError(f"{e}: the kernel trace may still run on the PC ({run_dir})") from None


def abandon(ctx, ip, sw, run_dir: str, cause: BaseException) -> None:
    """The stop after a flag, a signal or a failure once the start was sent:
    at once, without the merge, whatever the flag says. Its own failure is
    reported, never raised over the cause."""
    why = "ide event" if isinstance(cause, ip.EventNow) else type(cause).__name__
    try:
        stop(ctx, ip, sw, run_dir, "ignore")
    except ip.StepError as e:
        print(f"iempc: WARNING: the kernel trace may still run on the PC after {why} ({e}); stop it with "
              f"Stop-IemTraceSessions -Dir {run_dir}", file=sys.stderr, flush=True)
        ip.emit({"trace_stop": "failed", "dir": run_dir, "error": str(e)[-800:]})
        return
    print(f"iempc: the kernel trace was stopped ({why})", file=sys.stderr, flush=True)


def check_event(ctx, ip) -> None:
    if ctx.watch(abandon=True) != "ignore" and ip.event_now():
        raise ip.EventNow()


def trace(ctx, ip) -> int:
    """`iempc trace` (dev time)."""
    env = ctx.env
    if not env.get("PC_XPERF"):
        raise ip.Refused("trace: PC_XPERF missing in the private env (xperf.exe's full path on the PC)")
    label, seconds, circular_mb = check_args(ip, ctx.args)
    profile = iempc_tuning.load_profile(ip, Path(ctx.args.profile) if ctx.args.profile else iempc_tuning.PROFILE)
    lps = tr.watch_lps(profile, "")
    code, reply, _ = ip.iemmode(env, ["status"], ip.STATUS_S, ctx.watch(abandon=True))
    before = engine_seen(ip, code, reply)
    sw, tw = s1c(ip)
    run = f"{label}-{dt.datetime.now(dt.timezone.utc).strftime('%Y%m%dT%H%M%SZ')}"
    rel = f"traces/{run}"
    run_dir = ip.pc_join(env["PC_ROOT"], rel)
    xperf, d = ip.ps_quote(env["PC_XPERF"]), ip.ps_quote(run_dir)
    after, after_error = None, None
    with ended_by_signals():
        try:
            ip.run_module(env, f"{IMPORT} ; Start-IemTrace -Xperf {xperf} -Dir {d}{tw.trace_options('', circular_mb or 0)}",
                          START_S, ctx.watch(abandon=False))
            ip.pause(ctx, seconds)
            try:
                code, reply, _ = ip.iemmode(env, ["status"], ip.STATUS_S, ctx.watch(abandon=True))
                after = reply.get("engine") if code == 0 and isinstance(reply, dict) else None
            except ip.StepError as e:   # the counters only: the trace is stopped and analysed all the same
                after_error = str(e)[-800:]
        except BaseException as e:
            abandon(ctx, ip, sw, run_dir, e)
            raise
    stop(ctx, ip, sw, run_dir, ctx.watch(abandon=False))
    for body, _ in tw.analysis(xperf, d, 0, False):
        check_event(ctx, ip)
        ip.run_module(env, f"{IDLE} ; {IMPORT} ; {body}", ANALYSIS_S, ctx.watch(abandon=True))
    check_event(ctx, ip)
    out = ip.state_dir() / "traces" / run
    out.mkdir(parents=True, exist_ok=True)
    local = out / "dpcisr.txt"
    ip.scp(ip.remote(env, f"{rel}/dpcisr.txt"), str(local), ctx.watch(abandon=True))
    try:
        parsed = lr.parse_dpcisr(local.read_text(encoding="utf-8", errors="replace"))
    except ValueError as e:
        raise ip.StepError(f"{e} ({local})") from None
    doc = {"label": label, "seconds": seconds, "circular_mb": circular_mb, "run": run, "build": before.get("build"),
           **deltas(before, after), "after_error": after_error,
           "watched": {"lps": lps, **watched_usage(parsed, lps)},
           "top_modules": lr.top_modules(parsed), "findings": lr.budget_findings(parsed, lps), "report": str(local)}
    ip.write_json(out / "summary.json", doc)
    ip.emit(doc)
    return 0
