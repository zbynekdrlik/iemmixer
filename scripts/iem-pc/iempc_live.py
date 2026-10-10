"""`iempc dispatch-live --sha <bundle>` (S7 part 4, #10; plan Task 24, Review
Focus 7): dispatches the private ops repo's live.yml with this box's gh
authentication (no token in the public repo), and the reverse guards that
keep a live run, a soak and a switch test apart.

live.yml's `pc-begin` begins a HIL job on the bundle the PC already runs and
restarts its engine inside it (`iemmode activate`, HIL flags); its `pc` job
fires the listen-probe bursts and leaves on any mode but dev (`left-dev`).
So the three never overlap within a dev entry:
- `dispatch-live` refuses while a soak of this dev entry may still run (the
  restart would end it red: another engine, frames lost), or another live
  run of it (a second run would queue behind the first, live.yml's
  concurrency group, and outlive the window its record promises);
- `dispatch-soak` and `switch-test` refuse while a live run of this dev entry
  may still run (`refuse_while_live`, their call sites in iempc.py): a soak
  would meet the run's engine restart, a switch would end the run.
- `activate`, `dispatch-hil` and `trace` refuse while a live run or a soak
  of this dev entry may still run (`refuse_while_running`, their call sites
  in iempc.py): an activation restarts the engine, a HIL run restarts it in
  its own job, a kernel trace weighs on the times both measure. `dev` (the
  owner's "event skončil") and `event` are never refused.

In this order, nothing dispatched on any refusal:

1. Before any call: dev time (iempc's Spec: no EVENT-NOW flag at the start),
   a full SHA, no live run of this SHA in this dev entry yet (RECORD in the
   state dir; the entry is the count of `iempc dev` and switch-test's dev
   leg, `current_entry`), no live run of this entry that may still run, no
   soak of this entry that may still run (`iempc_soak.running_soak`).
2. `iemmode status` (a new flag abandons the read and runs the event path):
   the guard answers ok, in dev, no switch, no HIL job; the active bundle and
   the running engine's build are both the SHA; the engine plays (neither
   parked nor faulted). `live_refusal` judges the reply (pure), the soak's
   rule (`iempc_soak.runs_refusal`): pc-begin checks the same again.
3. A green push run of ci.yml for the SHA on dev or main, with its bundle and
   attest jobs (`green_run`, P5); the run's id and branch are what live.yml
   gets.
4. The flag again, right before the dispatch: one that came during the gh
   waits refuses it.
5. `gh workflow run live.yml -R <ops> -f sha -f branch -f run`, then the
   record (only after gh succeeded: a failed dispatch is no live run).

A failed ops run of the same SHA and dev entry is repeated with `gh run rerun
<id> -R <ops repo>` (the same verified inputs), never a second dispatch.

The window: from pc-begin's `iemmode job-begin` to pc-end's `iemmode
job-end` the guard's HIL job itself refuses a soak and a switch test
(`iempc_soak.settled_refusal`: no HIL job). WINDOW_S covers the rest, the
dispatch up to job-begin and pc-end's steps around job-end: a run counts as
possibly running for WINDOW_S after its dispatch, the sum of live.yml's job
bounds that hold the PC one after another (JOB_MINUTES). Task 25 keeps every
step of pc-end that changes the PC (the import of the saved project) before
its `iemmode job-end`.

Known limits: a run that waited longer than the window's pick-up allowance for
a runner (the PC's, held by a run dispatched from elsewhere; or browser's
hosted one, which pc-end waits for) is not covered past its job-end; a live
run dispatched from another box is not seen. A run of an earlier dev entry is
not looked at: a switch out of dev ends it (`left-dev`), and a dev entry while
its HIL job runs is refused by the guard. The record is written only after gh
succeeded, as the soak's: a dispatch GitHub took whose gh call then failed or
timed out is not recorded.

iempc.py passes itself in (`ip`), so this module never imports it (#36:
iempc.py is over its size budget)."""
from __future__ import annotations

import datetime as dt

import iempc_soak

LIVE_WORKFLOW = "live.yml"
RECORD = "live.json"
KEEP = 200   # live runs kept in RECORD, the newest
# live.yml's jobs that hold the PC, one after another (plan Task 25), as their
# `timeout-minutes`, and 5 min for `verify` and the runners' pick-up. `browser`
# (45 min, Playwright's globalTimeout of 40 min inside it) runs beside `pc`,
# which waits for it to start (15 min) and to end; `report` runs on a hosted
# runner after pc-end. Task 25's live.yml must use these bounds.
JOB_MINUTES = {"verify": 5, "pc-begin": 15, "pc": 60, "pc-end": 10}
WINDOW_S = sum(JOB_MINUTES.values()) * 60   # 5400 s after its dispatch a live run may still touch the PC
NOTHING = {"dispatch-soak": ("no soak", "(nothing was dispatched)"),
           "switch-test": ("no switch test", "a switch would end it (nothing was switched)")}
# The commands refused while a live run or a soak of this dev entry may still
# run (`refuse_while_running`): (head, why and what was not done).
RUNNING = {"activate": ("no activation", "the activation restarts the engine (nothing was activated)"),
           "dispatch-hil": ("no HIL dispatch", "the HIL run restarts the engine in its own job "
                                               "(nothing was dispatched)"),
           "trace": ("no trace", "a kernel trace weighs on the times it measures (nothing was traced)")}


def live_refusal(reply, sha: str) -> str | None:
    """Why the PC cannot run a live run of `sha` now, from one `iemmode
    status` reply (pure); None when it can."""
    return iempc_soak.runs_refusal(reply, sha, "a live run")


def load_runs(ip) -> list[dict]:
    """The live runs dispatched so far (RECORD); a record of another shape is
    an error, never read as none."""
    path = ip.state_dir() / RECORD
    runs = ip.read_json(path, {}).get("runs", [])
    if not isinstance(runs, list) or not all(isinstance(d, dict) for d in runs):
        raise ip.StepError(f"{path}: 'runs' is not a list of objects; check it by hand")
    return runs


def dispatched_at(d: dict) -> dt.datetime | None:
    """The record's dispatch time (aware), or None when it cannot be read."""
    try:
        at = dt.datetime.fromisoformat(str(d.get("at")))
    except ValueError:
        return None
    return at if at.tzinfo is not None else None


def may_run(d: dict, entry: int, now: dt.datetime) -> bool:
    """The record is a live run of dev entry `entry` that may still run at
    `now` (an aware time). Fail safe: a record whose time cannot be read may;
    one whose entry cannot be read (not an integer) may be of this entry,
    bounded by its time as any."""
    own = d.get("entry")
    if type(own) is int and own != entry:
        return False
    at = dispatched_at(d)
    return at is None or now < at + dt.timedelta(seconds=WINDOW_S)


def running_live(ip, entry: int, now: dt.datetime) -> dict | None:
    """The newest live run dispatched in dev entry `entry` that may still run
    at `now`, or None."""
    return next((d for d in reversed(load_runs(ip)) if may_run(d, entry, now)), None)


def described(ip, d: dict) -> str:
    """A refusal's words for record `d`; one that cannot be read names the
    file to check by hand (it may otherwise refuse until fixed)."""
    text = f"{d.get('sha')}, dispatched {d.get('at')}; live.yml's jobs hold the PC up to {WINDOW_S // 60} min"
    if type(d.get("entry")) is not int or dispatched_at(d) is None:
        text += f"; its time or dev entry cannot be read: check {ip.state_dir() / RECORD} by hand"
    return text


def live_described(ip, d: dict) -> str:
    """A refusal's words for a live run record that may still run."""
    return f"a live run dispatched in this dev entry may still run ({described(ip, d)})"


def soak_described(ip, soak: dict) -> str:
    """A refusal's words for a soak record that may still run; one whose
    time, hours or dev entry cannot be read names the file to check by hand
    (it may otherwise refuse until its window has passed or it is fixed)."""
    text = f"a soak dispatched in this dev entry may still run (dispatched {soak.get('at')}, {soak.get('hours')} h)"
    try:
        at = dt.datetime.fromisoformat(str(soak.get("at")))
    except ValueError:
        at = None
    if at is None or at.tzinfo is None or type(soak.get("hours")) is not int or type(soak.get("entry")) is not int:
        text += f"; its time, hours or dev entry cannot be read: check {ip.state_dir() / iempc_soak.RECORD} by hand"
    return text


def refuse_while_live(ip, command: str) -> None:
    """`dispatch-soak` and `switch-test` first: refused while a live run of
    this dev entry may still run (a soak would meet its engine restart, a
    switch would end it)."""
    d = running_live(ip, ip.current_entry(), dt.datetime.now().astimezone())
    if d is not None:
        head, tail = NOTHING[command]
        raise ip.Refused(f"{head}: {live_described(ip, d)}: {tail}")


def refuse_while_running(ip, command: str) -> None:
    """`activate`, `dispatch-hil` and `trace` first, before any call: refused
    while a live run (named first) or a soak of this dev entry may still run.
    An activation restarts the engine (inside the live run's HIL job too), a
    HIL run begins its own job and restarts it, a kernel trace weighs on the
    times both measure. `dev` (the owner's "event skončil") and `event` are
    never refused."""
    entry, now = ip.current_entry(), dt.datetime.now().astimezone()
    head, tail = RUNNING[command]
    d = running_live(ip, entry, now)
    soak = None if d is not None else iempc_soak.running_soak(ip, entry, now)
    if d is not None or soak is not None:
        raise ip.Refused(f"{head}: {live_described(ip, d) if d is not None else soak_described(ip, soak)}: {tail}")


def refuse_overlap(ip, entry: int) -> None:
    """dispatch-live: refused while another live run or a soak of this dev
    entry may still run."""
    now = dt.datetime.now().astimezone()
    d = running_live(ip, entry, now)
    if d is not None:
        raise ip.Refused(f"no live run: {live_described(ip, d)} (nothing was dispatched)")
    soak = iempc_soak.running_soak(ip, entry, now)
    if soak is not None:
        raise ip.Refused(f"no live run: {soak_described(ip, soak)}: the live run restarts the engine in a HIL job "
                         f"(nothing was dispatched)")


def dispatch(ctx, ip) -> int:
    """`iempc dispatch-live` (dev time, locked)."""
    sha = ip.check_sha(ctx.args.sha)
    entry = ip.current_entry()
    done = load_runs(ip)
    if any(d.get("sha") == sha and type(d.get("entry")) is int and d["entry"] == entry for d in done):
        raise ip.Refused(f"a live run of {sha} was already dispatched in dev entry {entry} (a failed ops run of it: "
                         f"gh run rerun <id> -R {ip.OPS_REPO})")
    refuse_overlap(ip, entry)
    code, reply, _ = ip.iemmode(ctx.env, ["status"], ip.STATUS_S, ctx.watch(abandon=True))
    why = f"iemmode status failed (exit {code})" if code != 0 else live_refusal(reply, sha)
    if why:
        raise ip.Refused(f"no live run: {why} (nothing was dispatched)")
    run, branch = ip.green_run(sha, ip.BRANCHES)
    if ip.event_now():   # "ide event" during the gh waits above: the live run is dev-time work
        raise ip.Refused(f"{ip.EVENT_NOW} appeared: no live run dispatch during an event (nothing was dispatched)")
    ip.gh(["workflow", "run", LIVE_WORKFLOW, "-R", ip.OPS_REPO, "-f", f"sha={sha}", "-f", f"branch={branch}",
           "-f", f"run={run}"])
    record = {"sha": sha, "branch": branch, "run": run, "entry": entry, "at": ip.now_iso()}
    ip.write_json(ip.state_dir() / RECORD, {"runs": (done + [record])[-KEEP:]})
    ip.emit({"dispatched_live": record})
    return 0
