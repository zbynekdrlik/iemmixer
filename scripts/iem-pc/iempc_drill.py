#!/usr/bin/env python3
"""The rollback drill (S8 lane 3, design note
docs/superpowers/specs/2026-10-10-s8-cutover-rollback-design.md section 3.3;
#11): before the owner's first cutover, in dev time, the main session proves
the way back on the PC.

    python3 scripts/iem-pc/iempc_drill.py --sha SHA --signal "<the owner's 'event skončil', its time>" \\
        --approval "<the owner's approval of the reboot, its time>" [--dry-run]

1. `iempc dev --build SHA` (the cutover runs from dev on the active bundle);
2. `iempc cutover --sha SHA` (the current main build, its live/iem-pc green);
3. `iempc status --pc`: prod on the pin SHA, live;
4. `iempc rollback`: done, REAPER on the export;
5. `iempc status --pc`: event, trial (no prod, nothing rolling back), the
   last switch done in event; its end (`last_switch.ended`) is noted;
6. the existing graceful reboot (S1c's window, scripts/pc-tuning):
   `spike_window.py new --signal`, `spike_window.py to-dev`,
   `tuning_window.py reboot-prepare`, `tuning_window.py reboot --approval`
   (an answer lost to the restart is no failure), `tuning_window.py
   post-boot`: REAPER came back by itself (the predecessor's autostarts the
   rollback restored), the post-boot checks clean;
7. `iempc status --pc` again (it starts the guard, whose start runs the
   event plan's checks), read again every SETTLE_POLL_S, the flag looked at
   before each, until no switch runs and the last one ended later than the
   rollback's (the start's checks, not the rollback's record; S8 lane 5),
   at most SETTLE_S: event, trial, done in event.

It stops at the first failure. Every step is a process of its own (each
honours the "ide event" flag itself: an iempc command runs the event path);
the drill looks at the flag before each step and stops there. A step that
outlives its bound is left running (never ended by force, I8).

Output: one JSON object, fixed codes and numbers only (exits, seconds; no
command's text: it names site values, P6): `{"drill": {"sha", "code", "steps":
[{"step", "exit", "seconds"}]}}`; exit 0 when the code is GREEN, else 1. The
failing command's own words go to stderr: private, never pasted into a
ticket."""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import iempc_core as core  # noqa: E402
import iempc_rollback as rb  # noqa: E402

IEMPC = HERE / "iempc.py"
SPIKE = HERE.parent / "asio-spike" / "spike_window.py"
TUNING = HERE.parent / "pc-tuning" / "tuning_window.py"
POLL_S = 2.0
# After the reboot: the status is read until the start's checks ended (s).
SETTLE_S = 900
SETTLE_POLL_S = 10.0
# Each step's bound (s): an iempc command's own bounds lie inside it (the
# cutover: the GitHub read, the dry run, the install and the guard's cutover).
BOUND_S = {"dev": 900, "cutover": 1800, "status": 300, "rollback": 900, "window": 900, "reboot-prepare": 900,
           "reboot": 300, "post-boot": 1500}
# The fixed codes.
GREEN = "GREEN"
EVENT = "EVENT"
STILL_RUNNING = "STILL_RUNNING"
DEV_FAILED = "DEV_FAILED"
CUTOVER_FAILED = "CUTOVER_FAILED"
NOT_PROD = "NOT_PROD"
ROLLBACK_FAILED = "ROLLBACK_FAILED"
NOT_ON_EXPORT = "NOT_ON_EXPORT"
NOT_EVENT = "NOT_EVENT"
WINDOW_FAILED = "WINDOW_FAILED"
REBOOT_FAILED = "REBOOT_FAILED"
POST_BOOT_FAILED = "POST_BOOT_FAILED"
REAPER_NOT_BACK = "REAPER_NOT_BACK"
NOT_EVENT_AFTER_REBOOT = "NOT_EVENT_AFTER_REBOOT"
# What tuning_window says when the restart took its answer with it.
LOST_ANSWER = "run post-boot"


class Stop(Exception):
    """The drill ends here with a code."""

    def __init__(self, code: str, why: str = "") -> None:
        super().__init__(code)
        self.code = code
        self.why = why


def call(argv: list[str], bound: float) -> tuple[int, str, str]:
    """One step's process: its exit code, stdout and stderr. Past `bound` it
    is left running (never ended by force) and the drill stops."""
    proc = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            text=True, encoding="utf-8", errors="replace")
    deadline = time.monotonic() + bound
    while True:
        try:
            out, err = proc.communicate(timeout=POLL_S)
            return proc.returncode, out, err
        except subprocess.TimeoutExpired:
            if time.monotonic() > deadline:
                raise Stop(STILL_RUNNING, f"{Path(argv[1]).name} {argv[2]} still runs after {bound} s; it is left "
                                          "to end by itself") from None


def last_object(out: str, key: str) -> dict | None:
    """The last JSON object on its own line of `out` that has `key`."""
    for line in reversed(out.splitlines()):
        try:
            doc = json.loads(line)
        except ValueError:
            continue
        if isinstance(doc, dict) and key in doc:
            return doc
    return None


def guard_reply(out: str) -> dict | None:
    """The guard's reply an iempc command printed (`{"iemmode": …, "reply"}`)."""
    doc = last_object(out, "iemmode")
    reply = doc.get("reply") if doc else None
    return reply if isinstance(reply, dict) else None


def prod_problem(reply: dict | None, sha: str) -> str | None:
    """Why the status is not prod live on the pin `sha`."""
    if rb.lifecycle(reply) != "prod":
        return f"the lifecycle is {rb.lifecycle(reply)}, not prod"
    if reply.get("mode") != "live":
        return f"the mode is {reply.get('mode')}, not live"
    if f"pin {sha}" not in reply.get("detail", ""):
        return f"the pin is not {sha}"
    return None


def switch_end(reply: dict | None) -> int | None:
    """The end of the last switch a guard reply names (`last_switch.ended`,
    epoch seconds), None without one."""
    last = reply.get("last_switch") if isinstance(reply, dict) else None
    ended = last.get("ended") if isinstance(last, dict) else None
    return ended if isinstance(ended, int) and not isinstance(ended, bool) else None


def settled_after(reply: dict | None, ended: int) -> bool:
    """No switch runs and the last one ended later than `ended` (pure)."""
    end = switch_end(reply)
    return isinstance(reply, dict) and reply.get("switching") is None and end is not None and end > ended


def event_problem(reply: dict | None) -> str | None:
    """Why the status is not event in trial with its last switch done in event."""
    if rb.lifecycle(reply) != "trial":
        return f"the lifecycle is {rb.lifecycle(reply)}, not trial"
    if reply.get("mode") != "event":
        return f"the mode is {reply.get('mode')}, not event"
    last = reply.get("last_switch")
    if not isinstance(last, dict) or (last.get("outcome"), last.get("ended_in")) != ("done", "event"):
        return "the last switch did not end done in event"
    return None


class Drill:
    def __init__(self, sha: str, signal: str, approval: str) -> None:
        self.sha, self.signal, self.approval = sha, signal, approval
        self.steps: list[dict] = []

    def run(self, step: str, argv: list[str], bound: str, fail: str, ok_exits: tuple[int, ...] = (0,)) -> str:
        """One step: the flag first, then its process; a failed exit stops
        the drill with `fail`. Its stdout."""
        if core.EVENT_NOW.exists():
            raise Stop(EVENT, f"{core.EVENT_NOW} exists: the drill stops before {step}; 'iempc event' runs the "
                              "event path")
        began = time.monotonic()
        code, out, err = call(argv, BOUND_S[bound])
        self.steps.append({"step": step, "exit": code, "seconds": round(time.monotonic() - began, 1)})
        if code not in ok_exits:
            raise Stop(fail, err.strip()[-1500:])
        return out

    def iempc(self, step: str, args: list[str], bound: str, fail: str) -> dict | None:
        return guard_reply(self.run(step, [sys.executable, str(IEMPC), *args], bound, fail))

    def status(self, step: str, fail: str, problem) -> dict | None:
        reply = self.iempc(step, ["status", "--pc"], "status", fail)
        why = problem(reply)
        if why:
            raise Stop(fail, why)
        return reply

    def after_reboot(self, ended: int) -> None:
        """Step 7: the first `iemmode` call starts the guard, and its status
        may still name the rollback's switch before the start's checks ran
        (S8 lane 5). Read again, the flag looked at before each read, until
        no switch runs and the last one ended later than `ended`, at most
        SETTLE_S; then that status must be event, trial, done in event. One
        step in the output (the reads' time, the last exit)."""
        argv = [sys.executable, str(IEMPC), "status", "--pc"]
        began = time.monotonic()
        deadline = began + SETTLE_S
        reads = 0
        while True:
            if core.EVENT_NOW.exists():
                raise Stop(EVENT, f"{core.EVENT_NOW} exists: the drill stops after the reboot; 'iempc event' runs "
                                  "the event path")
            code, out, err = call(argv, BOUND_S["status"])
            reads += 1
            reply = guard_reply(out) if code == 0 else None
            if settled_after(reply, ended):
                break
            if time.monotonic() >= deadline:
                self.steps.append({"step": "after-reboot", "exit": code, "seconds": round(time.monotonic() - began, 1)})
                raise Stop(NOT_EVENT_AFTER_REBOOT, f"the start's checks were not seen ending within {SETTLE_S} s "
                                                   f"({reads} status reads; the last exited {code}): "
                                                   f"{err.strip()[-500:]}")
            time.sleep(SETTLE_POLL_S)
        self.steps.append({"step": "after-reboot", "exit": code, "seconds": round(time.monotonic() - began, 1)})
        why = event_problem(reply)
        if why:
            raise Stop(NOT_EVENT_AFTER_REBOOT, why)

    def window_reboot(self) -> None:
        """The existing graceful reboot: S1c's window, its restart request and
        post-boot. A restart request whose answer the restart took is no
        failure (post-boot tells whether it rebooted)."""
        py = sys.executable
        self.run("window-new", [py, str(SPIKE), "new", "--signal", self.signal], "window", WINDOW_FAILED)
        self.run("window-to-dev", [py, str(SPIKE), "to-dev"], "window", WINDOW_FAILED)
        self.run("reboot-prepare", [py, str(TUNING), "reboot-prepare"], "reboot-prepare", REBOOT_FAILED)
        if core.EVENT_NOW.exists():
            raise Stop(EVENT, "the flag came before the restart request: no reboot")
        began = time.monotonic()
        code, _, err = call([py, str(TUNING), "reboot", "--approval", self.approval], BOUND_S["reboot"])
        self.steps.append({"step": "reboot", "exit": code, "seconds": round(time.monotonic() - began, 1)})
        if code != 0 and LOST_ANSWER not in err:
            raise Stop(REBOOT_FAILED, err.strip()[-1500:])
        out = self.run("post-boot", [py, str(TUNING), "post-boot"], "post-boot", POST_BOOT_FAILED)
        doc = last_object(out, "post-boot")
        checks = doc.get("post-boot") if doc else None
        if not isinstance(checks, dict) or checks.get("reaper") is not True:
            raise Stop(REAPER_NOT_BACK, "REAPER did not come back by itself after the reboot (the predecessor's "
                                        "autostarts)")
        if doc.get("problems"):
            raise Stop(POST_BOOT_FAILED, f"{len(doc['problems'])} post-boot problem(s)")

    def drill(self) -> None:
        self.iempc("dev", ["dev", "--build", self.sha], "dev", DEV_FAILED)
        self.iempc("cutover", ["cutover", "--sha", self.sha], "cutover", CUTOVER_FAILED)
        self.status("prod", NOT_PROD, lambda r: prod_problem(r, self.sha))
        reply = self.iempc("rollback", ["rollback"], "rollback", ROLLBACK_FAILED)
        if rb.ON_EXPORT not in (reply or {}).get("detail", ""):
            raise Stop(NOT_ON_EXPORT, "the rollback did not say that REAPER runs on the export")
        rolled = self.status("event", NOT_EVENT, event_problem)
        ended = switch_end(rolled)
        if ended is None:
            raise Stop(NOT_EVENT, "the rollback's last switch names no end (last_switch.ended): the start's checks "
                                  "after the reboot could not be told from it")
        self.window_reboot()
        self.after_reboot(ended)


def plan(sha: str) -> list[str]:
    """`--dry-run`: the steps as they would run, no call."""
    return [f"iempc dev --build {sha}", f"iempc cutover --sha {sha}", "iempc status --pc (prod, live, the pin)",
            "iempc rollback (REAPER on the export)", "iempc status --pc (event, trial)",
            "spike_window.py new --signal", "spike_window.py to-dev", "tuning_window.py reboot-prepare",
            "tuning_window.py reboot --approval", "tuning_window.py post-boot (REAPER back by itself)",
            "iempc status --pc until the start's checks ended (event, trial)"]


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(prog="iempc_drill.py", description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--sha", required=True, help="the current main build, installed and active in dev")
    ap.add_argument("--signal", required=True, help="the owner's 'event skončil' message with its time (S1c window)")
    ap.add_argument("--approval", required=True, help="the owner's approval of the reboot with its time")
    ap.add_argument("--dry-run", action="store_true", help="print the steps, call nothing")
    args = ap.parse_args(argv)
    try:
        sha = core.check_sha(args.sha)
    except core.StepError as e:
        print(f"iempc_drill: {e}", file=sys.stderr)
        return 2
    if args.dry_run:
        print(json.dumps({"drill": {"sha": sha, "plan": plan(sha)}}), flush=True)
        return 0
    d = Drill(sha, args.signal, args.approval)
    code = GREEN
    try:
        d.drill()
    except Stop as e:
        code = e.code
        if e.why:
            print(f"iempc_drill: {code}: {e.why} (private: names site values, never paste it)", file=sys.stderr,
                  flush=True)
    print(json.dumps({"drill": {"sha": sha, "code": code, "steps": d.steps}}), flush=True)
    return 0 if code == GREEN else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
