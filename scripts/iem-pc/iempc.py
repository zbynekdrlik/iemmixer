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
import os
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

import iempc_bin
import iempc_bundle
import iempc_core as core
import iempc_event
import iempc_live
import iempc_soak
import iempc_sshshell
import iempc_switch
import iempc_trace
import iempc_tuning
from iempc_bundle import cmd_fetch_bundle, extract_member, latest_record_sha, need_record
from iempc_core import (BOOTSTRAP_S, OPS_REPO, STATUS_S, SWITCH_S, Ctx, EventNow, Refused, StepError, check_sha,
                        current_entry, emit, event_now, iemmode, load_env, next_entry, pc_join, pc_mkdir, ps_quote,
                        refuse_open_window, remote, result, run_module, spike_module, spike_window_open, state_lock)
from iempc_event import OWNER_ALARM

# The modules iempc.py is split into (#36). `iempc.<name>` reads any of their
# names live (PEP 562), so siblings handed this module (`ip`) and the tests
# reach the one binding a test patches, never a copy of it.
SPLIT = (core, iempc_event, iempc_bundle)


def __getattr__(name: str):
    for module in SPLIT:
        if name in vars(module):
            return vars(module)[name]
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


FUNCTION = re.compile(r"(Get|Test|Set|Add|Register|Grant|Remove)-Iem[A-Za-z0-9]+")
READ_ONLY_VERBS = ("Get", "Test")
PARAM_NAME = re.compile(r"-[A-Za-z][A-Za-z0-9]*")
RUNNER_FUNCTION = "Register-IemRunner"
RUNNER_TOKEN = re.compile(r"[A-Za-z0-9]{20,200}")
PREEMPTED = 10


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


def cmd_event(ctx: Ctx) -> int:
    """The event path ("ide event"); the code lives in iempc_event.py (#36)."""
    return iempc_event.cmd_event(ctx, sys.modules[__name__])


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


def cmd_install(ctx: Ctx) -> int:
    """A verified bundle to the PC and the guard's install; the code lives in iempc_bundle.py (#36)."""
    return iempc_bundle.cmd_install(ctx, sys.modules[__name__])


def cmd_activate(ctx: Ctx) -> int:
    """`iemmode activate` and the hand-over; the code lives in iempc_bundle.py (#36)."""
    return iempc_bundle.cmd_activate(ctx, sys.modules[__name__])


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


def cmd_dispatch_hil(ctx: Ctx) -> int:
    """The ops hil.yml dispatch; the code lives in iempc_bundle.py (#36)."""
    return iempc_bundle.cmd_dispatch_hil(ctx, sys.modules[__name__])


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
