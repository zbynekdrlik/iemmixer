#!/usr/bin/env python3
"""Dev-box control of the Gen 3 box (#72, #86).

The box's own gates (`gen3-gate hil|live|soak|reboot|reset`: iempc_gen3_gate.py,
iempc_gen3_live.py, iempc_gen3_soak.py, iempc_gen3_reboot.py; the verdicts are the
commit statuses hil/gen3, live/gen3 and soak/gen3), the box deployed in place
(`gen3-deploy`: iempc_gen3_deploy.py and scripts/gen3/deploy.sh), the box's data
into the private store, a new box from a fresh OS and the drift between a box and
this checkout (`gen3-backup`, `gen3-new-box`, `gen3-drift`: iempc_gen3_newbox.py).

"ide event": the flag file (~/.config/iemmixer/EVENT-NOW) exists. `event` writes
it when it is missing (a flag it cannot write is a warning, never a stop) and
does nothing else: the box serves the band at every event (#86).

Every other command waits for dev time: it refuses while the flag exists, and
every wait sees a new flag within 2 s: a read-only call is abandoned, a change
the call makes itself completes first; then the command runs the event path
itself (exit 10).

Site values come only from the private env file ($PC_ENV, default
~/.config/iemmixer/iem-pc.env): GEN3_HOST (the box's ssh destination),
GEN3_DANTE_IP (our device's unicast IPv4 address on the Dante network; `gen3-gate`
refuses without it, GEN3_HOST and GEN3_DEVICE_CONFIG, iempc_gen3_gate.py);
GEN3_PUBLIC_URL (the band's public https:// URL, the live specs' origin in
`gen3-gate live`), LIVE_MEMBER, LIVE_TEST_INPUT and LIVE_TALKBACK_INPUT (the live
specs' member, burst input, `mic1`, and talkback input; `gen3-gate live` refuses
without them, iempc_gen3_live.py), SOAK_MEMBER (the member whose mix `gen3-gate
soak`'s client listens to, one not active; it refuses without it,
iempc_gen3_soak.py), and GEN3_DEVICE_CONFIG, GEN3_SITE_FILE and GEN3_ROSTER_FILE
(the ops repo's device config, the device's whole identity, and the band's
site.toml and roster.toml, which `gen3-deploy` gives deploy.sh; it refuses without
them and GEN3_HOST, iempc_gen3_deploy.py).
Nothing is ever ended by force."""
from __future__ import annotations

import argparse
import sys
from dataclasses import dataclass
from typing import Callable

import iempc_core as core
import iempc_event
import iempc_gen3_deploy
import iempc_gen3_gate
import iempc_gen3_newbox
from iempc_core import Ctx, EventNow, Refused, StepError, emit, event_now, load_env, state_lock

# The modules iempc.py is split into (#36). `iempc.<name>` reads any of their
# names live (PEP 562), so siblings handed this module (`ip`) and the tests
# reach the one binding a test patches, never a copy of it.
SPLIT = (core, iempc_event)


def __getattr__(name: str):
    for module in SPLIT:
        if name in vars(module):
            return vars(module)[name]
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


PREEMPTED = 10


# ---- commands ----

def cmd_event(ctx: Ctx) -> int:
    """The event path ("ide event"); the code lives in iempc_event.py (#36)."""
    return iempc_event.cmd_event(ctx)


def cmd_gen3_gate(ctx: Ctx) -> int:
    """The Gen 3 box's own gates (phase H: hil, live, soak); the code lives in iempc_gen3_gate.py,
    iempc_gen3_live.py and iempc_gen3_soak.py (#72)."""
    return iempc_gen3_gate.run(ctx, sys.modules[__name__])


def cmd_gen3_deploy(ctx: Ctx) -> int:
    """The Gen 3 box deployed in place;
    the code lives in iempc_gen3_deploy.py (#72)."""
    return iempc_gen3_deploy.run(ctx, sys.modules[__name__])


def cmd_gen3_backup(ctx: Ctx) -> int:
    """The box's band data and pepper into the private store; the code lives in iempc_gen3_newbox.py (#72)."""
    return iempc_gen3_newbox.run_backup(ctx, sys.modules[__name__])


def cmd_gen3_new_box(ctx: Ctx) -> int:
    """A fresh OS to the band's state, one command; the code lives in iempc_gen3_newbox.py (#72)."""
    return iempc_gen3_newbox.run_new_box(ctx, sys.modules[__name__])


def cmd_gen3_drift(ctx: Ctx) -> int:
    """The box against this checkout; the code lives in iempc_gen3_newbox.py (#72)."""
    return iempc_gen3_newbox.run_drift(ctx, sys.modules[__name__])


# ---- main ----

@dataclass(frozen=True)
class Spec:
    fn: Callable[[Ctx], int]
    dev_time: bool  # refused while the flag exists
    locked: bool    # one at a time on this box


COMMANDS: dict[str, Spec] = {
    "event": Spec(cmd_event, dev_time=False, locked=False),
    # The Gen 3 box over ssh and our own device over ARC (#72 phase H).
    "gen3-gate": Spec(cmd_gen3_gate, dev_time=True, locked=True),
    # The box through deploy.sh (#72).
    "gen3-deploy": Spec(cmd_gen3_deploy, dev_time=True, locked=True),
    # The box over ssh, its data into the private store, a new box, a read of the drift (#72).
    "gen3-backup": Spec(cmd_gen3_backup, dev_time=True, locked=True),
    "gen3-new-box": Spec(cmd_gen3_new_box, dev_time=True, locked=True),
    "gen3-drift": Spec(cmd_gen3_drift, dev_time=True, locked=True),
}


def build_parser() -> argparse.ArgumentParser:
    ap = argparse.ArgumentParser(prog="iempc", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("event").add_argument("--dry-run", action="store_true")
    deploy = sub.add_parser("gen3-deploy")
    deploy.add_argument("--bundle", required=True, help="the downloaded CI bundle gen3-bundle-<sha> (a folder)")
    deploy.add_argument("--sha", required=True,
                        help="its commit, 40 hex: this checkout must be at it with a clean tree (deploy.sh's check)")
    deploy.add_argument("--dry-run", action="store_true",
                        help="the plan: deploy.sh's options; no call")
    sub.add_parser("gen3-backup")
    sub.add_parser("gen3-drift")
    fresh = sub.add_parser("gen3-new-box")
    fresh.add_argument("--host", required=True, help="the fresh box's ssh destination, never GEN3_HOST")
    fresh.add_argument("--bundle", required=True, help="the downloaded CI bundle gen3-bundle-<sha> (a folder)")
    fresh.add_argument("--sha", required=True, help="its commit, 40 hex: this checkout must be at it, clean")
    fresh.add_argument("--iface", help="the Dante interface (default: the box's default route's)")
    fresh.add_argument("--from-backup", help="a gen3-backup stamp, for a box that no longer answers")
    gate = sub.add_parser("gen3-gate")
    gate.add_argument("gate", choices=("hil", "live", "soak", "reboot", "reset"),
                      help="hil: the box's HIL job and checks (versions, health, the test signal, the D5(b) loop, the "
                           "alarm path's count), posted as hil/gen3 on the box's build; live: the live specs in a "
                           "browser through the band's public URL with a burst every minute, posted as live/gen3; "
                           "soak: iem-soakclient on the box beside reads of its counters, posted as soak/gen3")
    gate.add_argument("--minutes", type=int,
                      help="soak only: the run's length, 10, 30, 60 or 120 (default 120)")
    return ap


def preempt(env: dict[str, str]) -> int:
    cmd_event(Ctx(env, argparse.Namespace(dry_run=False), True))
    return PREEMPTED


def main(argv: list[str]) -> int:
    args = build_parser().parse_args(argv)
    flag_at_start = event_now()
    try:
        env = load_env(core.env_path())
    except StepError as e:
        print(f"iempc: {e}", file=sys.stderr)
        return 1
    spec = COMMANDS[args.cmd]
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
        return 1
    return preempt(env)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
