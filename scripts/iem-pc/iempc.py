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
(`ssh-shell`: iempc_sshshell.py), the cutover on the owner's message
(`cutover`: iempc_cutover.py, PC_AUTOSTART_TASKS, PC_AUTOSTART_RUN and
PC_AUTOSTART_STARTUP below),
the shadow imports' summary (`shadow-report`: iempc_shadow.py, read-only), a
roster file onto the PC (`roster-install`: iempc_roster.py, the server config
from the guard's own pc.toml), the IEM card's Dante labels as a sheet, and
written onto the card by its IP with `--apply` (`dante-labels`:
iempc_dante.py and dante_card.py, no PC call; DANTE_IEM_HOST below), the
loop through the IEM card that proves the Gen 3 device's TX sample-exact
(`gen3-loop`: iempc_gen3.py, GEN3_HOST below; #72), the same loop through a
member's mix in the box's engine with the browser in it (`gen3-mix`:
iempc_gen3.py, GEN3_HOST and GEN3_UI_URL below; #72), and the hand-over of an
open S1a window.

"ide event": the flag file (~/.config/iemmixer/EVENT-NOW) exists. `event`
writes it first when it is missing (a flag it cannot write is a warning,
never a stop), pre-empts an open S1a/S1c spike window (spike_window.py
preempt; after a failed one it closes the window under the window lock, so
no queued window preempt starts a second bring-back), stops a kernel trace
whose `iempc trace` died with this box (its record, iempc_trace.stop_recorded;
`dev` does too), then runs `iemmode event`, and `iemmode event --direct` when
the guard is unreachable (exit 4). Right after the flag it also stops the
Gen 3 stack (#72): one `ssh <GEN3_HOST> sudo -n systemctl stop iem-server
iem-engine iem-dante` beside the PC's switch, waited for at most GEN3_STOP_S (5 s) at the end; a
failure or a stop still running then is one warning, never the exit code;
no GEN3_HOST is one info line. The event path has one budget that fits one
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
(optional, xperf.exe's full path on the PC; `trace` refuses without it),
PC_AUTOSTART_TASKS, PC_AUTOSTART_RUN and PC_AUTOSTART_STARTUP (the
predecessor's autostarts the cutover disables: `;`-separated task paths, Run
values and shortcut names `name.lnk` in the desktop user's Startup folder;
`cutover` refuses when all three are empty), and DANTE_IEM_HOST,
DANTE_IEM_NAME and NETAUDIO_PYTHON (optional: the IEM card's unicast IPv4
address, its expected Dante device name and the netaudio venv's python, an
executable file; `dante-labels --apply` refuses without all three, the
sheet reads the card with them), GEN3_HOST (optional: the Gen 3 box's ssh
destination, whose stack `event` stops and whose device `gen3-loop` and
`gen3-mix` drive; #72), GEN3_UI_URL (optional: the box's UI,
`http://<box>/`, the browser's origin in `gen3-mix`), and GEN3_DANTE_IP
and GEN3_DANTE_NAME (optional: our Gen 3 device's unicast IPv4 address on
the Dante network and its device name; `gen3-mirror` refuses without them
and the card's keys: iempc_gen3_switch.py, #72 phase E); `gen3-data` reads
the PC's band files (PC_ROOT) and writes them onto GEN3_HOST
(iempc_gen3_data.py); GEN3_PUBLIC_URL (optional: the band's public https://
URL, read by `gen3-tunnel` to see which connector serves it,
iempc_gen3_tunnel.py).
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
import sys
from dataclasses import dataclass
from typing import Callable

import iempc_bin
import iempc_bootstrap
import iempc_bundle
import iempc_core as core
import iempc_cutover
import iempc_dante
import iempc_event
import iempc_gen3
import iempc_gen3_data
import iempc_gen3_switch
import iempc_gen3_tunnel
import iempc_live
import iempc_roster
import iempc_shadow
import iempc_soak
import iempc_sshshell
import iempc_switch
import iempc_trace
import iempc_tuning
from iempc_bootstrap import cmd_bootstrap, cmd_handover_s1a
from iempc_bundle import cmd_fetch_bundle
from iempc_core import (STATUS_S, SWITCH_S, Ctx, EventNow, Refused, StepError, check_sha, current_entry, emit, event_now,
                        iemmode, load_env, next_entry, refuse_open_window, result, spike_window_open, state_lock)
from iempc_event import OWNER_ALARM

# The modules iempc.py is split into (#36). `iempc.<name>` reads any of their
# names live (PEP 562), so siblings handed this module (`ip`) and the tests
# reach the one binding a test patches, never a copy of it.
SPLIT = (core, iempc_event, iempc_bundle, iempc_bootstrap)


def __getattr__(name: str):
    for module in SPLIT:
        if name in vars(module):
            return vars(module)[name]
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


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


def cmd_cutover(ctx: Ctx) -> int:
    """The cutover on the owner's message; the code lives in iempc_cutover.py (#11)."""
    return iempc_cutover.run(ctx, sys.modules[__name__])


def cmd_shadow_report(ctx: Ctx) -> int:
    """The shadow imports' history, summarised; the code lives in iempc_shadow.py (#11)."""
    return iempc_shadow.run(ctx, sys.modules[__name__])


def cmd_roster_install(ctx: Ctx) -> int:
    """A roster file onto the PC, checked there and followed until applied; the code lives in iempc_roster.py (#58)."""
    return iempc_roster.run(ctx, sys.modules[__name__])


def cmd_dante_labels(ctx: Ctx) -> int:
    """The IEM card's Dante labels for the main lineup, as a sheet; the code lives in iempc_dante.py (#58)."""
    return iempc_dante.run(ctx, sys.modules[__name__])


def cmd_trace(ctx: Ctx) -> int:
    """A kernel DPC/ISR trace on the guard's engine; the code lives in iempc_trace.py (#15, #36)."""
    iempc_live.refuse_while_running(sys.modules[__name__], "trace")   # a live run or soak of this entry (#10)
    return iempc_trace.trace(ctx, sys.modules[__name__])


def cmd_gen3_loop(ctx: Ctx) -> int:
    """The loop through the IEM card that proves the Gen 3 device's TX; the code lives in iempc_gen3.py (#72)."""
    iempc_live.refuse_while_running(sys.modules[__name__], "gen3-loop")   # it restarts the engine in a HIL job
    return iempc_gen3.run(ctx, sys.modules[__name__])


def cmd_gen3_mix(ctx: Ctx) -> int:
    """The loop through the IEM card and a member's mix in the Gen 3 box's engine, the browser in it; the code lives
    in iempc_gen3.py (#72)."""
    iempc_live.refuse_while_running(sys.modules[__name__], "gen3-mix")   # it restarts the engine in a HIL job
    return iempc_gen3.run_mix(ctx, sys.modules[__name__])


def cmd_gen3_mirror(ctx: Ctx) -> int:
    """Our Gen 3 device's RX table made equal to the IEM card's (or cleared); the code lives in iempc_gen3_switch.py
    (#72 phase E)."""
    return iempc_gen3_switch.run_mirror(ctx, sys.modules[__name__])


def cmd_gen3_data(ctx: Ctx) -> int:
    """The band's data from the PC onto the Gen 3 box (the switch to 3.x); the code lives in iempc_gen3_data.py
    (#72 phase E)."""
    return iempc_gen3_data.run(ctx, sys.modules[__name__])


def cmd_gen3_tunnel(ctx: Ctx) -> int:
    """The band's tunnel connector between the PC and the Gen 3 box; the code lives in iempc_gen3_tunnel.py (#72
    phase E)."""
    return iempc_gen3_tunnel.run(ctx, sys.modules[__name__])


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
    "cutover": Spec(cmd_cutover, pc=True, dev_time=True, locked=True),
    "shadow-report": Spec(cmd_shadow_report, pc=True, dev_time=True, locked=False),
    "roster-install": Spec(cmd_roster_install, pc=True, dev_time=True, locked=True),
    # No PC call (a sheet); dev time and locked like the renames it is for (design §10).
    "dante-labels": Spec(cmd_dante_labels, pc=False, dev_time=True, locked=True),
    "gen3-loop": Spec(cmd_gen3_loop, pc=True, dev_time=True, locked=True),
    "gen3-mix": Spec(cmd_gen3_mix, pc=True, dev_time=True, locked=True),
    # No PC call: reads the card, writes only our own Gen 3 device (#72 phase E).
    "gen3-mirror": Spec(cmd_gen3_mirror, pc=False, dev_time=True, locked=True),
    # Reads the PC, writes only the Gen 3 box (#72 phase E).
    "gen3-data": Spec(cmd_gen3_data, pc=True, dev_time=True, locked=True),
    # Not dev time: `to-pc` is the way back to 2.x during an event too; `token` and `to-box` refuse the flag.
    "gen3-tunnel": Spec(cmd_gen3_tunnel, pc=True, dev_time=False, locked=True),
}


def build_parser() -> argparse.ArgumentParser:
    ap = argparse.ArgumentParser(prog="iempc", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("rehearse-teardown", "probe-task", "handover-s1a", "switch-test", "shadow-report"):
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
    cutover = sub.add_parser("cutover")
    cutover.add_argument("--sha", required=True, help="the active main bundle the PC runs in dev or a live trial")
    cutover.add_argument("--dry-run", action="store_true", help="the guard's refusals and the trial's precheck only")
    roster = sub.add_parser("roster-install")
    roster.add_argument("--file", required=True, help="the roster file (the private ops repo's site/roster.toml)")
    roster.add_argument("--dry-run", action="store_true",
                        help="check it on the PC with the active bundle's server, install nothing")
    labels = sub.add_parser("dante-labels")
    labels.add_argument("--roster", required=True, help="the roster whose main lineup names the labels")
    labels.add_argument("--site", help="the site file (default: site.toml beside the roster)")
    labels.add_argument("--before", help="the roster the card's labels follow now, the offline fallback when the "
                                         "card cannot be read: only the TX labels that differ are listed")
    labels.add_argument("--apply", action="store_true",
                        help="write the labels that differ onto the IEM card (DANTE_IEM_* and NETAUDIO_PYTHON in the "
                             "private env), then read them back")
    mirror = sub.add_parser("gen3-mirror")
    mirror.add_argument("--dry-run", action="store_true", help="list the changes, send nothing")
    mirror.add_argument("--clear", action="store_true", help="unsubscribe every RX of our device instead")
    tunnel = sub.add_parser("gen3-tunnel")
    tunnel.add_argument("verb", choices=("token", "to-box", "to-pc"),
                        help="token: the PC's connector token into the box's credential; to-box / to-pc: which "
                             "connector serves the band's public URL")
    data = sub.add_parser("gen3-data")
    data.add_argument("--pepper", action="store_true",
                      help="also the PC's PIN pepper into the box's credential (replaces the box's own)")
    data.add_argument("--dry-run", action="store_true", help="read and check the PC's files, send nothing")
    loop = sub.add_parser("gen3-loop")
    loop.add_argument("--offsets", required=True,
                      help=f"F1,F2: the two echo offsets of TX 1 behind RX 1, 1..{iempc_gen3.OFFSET_MAX} samples")
    loop.add_argument("--bursts", type=int, required=True,
                      help=f"test-signal bursts at each offset, {iempc_gen3.BURSTS_MIN}..{iempc_gen3.BURSTS_MAX}")
    loop.add_argument("--slot", type=int, required=True,
                      help="the card's spare HIL output whose return the Gen 3 device echoes (never printed)")
    loop.add_argument("--input", default=iempc_gen3.INPUT_DEFAULT, help="the engine input the test signal replaces")
    loop.add_argument("--dbfs", type=float, default=iempc_gen3.DBFS_DEFAULT,
                      help=f"the test signal's level, at most {iempc_gen3.DBFS_MAX:g}")
    loop.add_argument("--ttl", type=float, default=iempc_gen3.TTL_DEFAULT,
                      help=f"each burst's length in seconds, at most {iempc_gen3.TTL_MAX:g}")
    mix = sub.add_parser("gen3-mix")
    mix.add_argument("--slot", type=int, required=True,
                     help="the card's spare HIL output our RX R receives (never printed)")
    mix.add_argument("--member", required=True,
                     help="a person of the test roster: T is their mix's first TX, the browser opens their page")
    mix.add_argument("--box-input", required=True,
                     help="an input of the test site's [engine] table: R is its first RX, the browser opens it")
    mix.add_argument("--bursts", type=int, required=True,
                     help=f"test-signal bursts per phase, {iempc_gen3.BURSTS_MIN}..{iempc_gen3.BURSTS_MAX}")
    mix.add_argument("--input", default=iempc_gen3.INPUT_DEFAULT, help="the engine input the test signal replaces")
    mix.add_argument("--dbfs", type=float, default=iempc_gen3.DBFS_DEFAULT,
                     help=f"the test signal's level, at most {iempc_gen3.DBFS_MAX:g}")
    mix.add_argument("--ttl", type=float, default=iempc_gen3.TTL_DEFAULT,
                     help=f"each burst's length in seconds, at most {iempc_gen3.TTL_MAX:g}")
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
