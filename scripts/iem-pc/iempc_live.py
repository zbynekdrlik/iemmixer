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

Known limits: a run counts as possibly running for WINDOW_S after its
dispatch, the sum of live.yml's job bounds; a run that waited longer for the
PC's runner (held by a run dispatched from elsewhere) is not covered, and a
live run dispatched from another box is not seen. A run of an earlier dev
entry is not looked at: a switch out of dev ends it (`left-dev`).

iempc.py passes itself in (`ip`), so this module never imports it (#36:
iempc.py is over its size budget)."""
from __future__ import annotations

import datetime as dt

import iempc_soak

LIVE_WORKFLOW = "live.yml"
RECORD = "live.json"
KEEP = 200   # live runs kept in RECORD, the newest
# live.yml's jobs that hold the PC, one after another (plan Task 25), as their
# `timeout-minutes`, and 5 min for `verify` and the runner's pick-up. `browser`
# (45 min, Playwright's globalTimeout of 40 min inside it) runs beside `pc`,
# which ends when browser has; `report` runs on a hosted runner after pc-end.
JOB_MINUTES = {"verify": 5, "pc-begin": 15, "pc": 60, "pc-end": 10}
BROWSER_MINUTES = 45
WINDOW_S = sum(JOB_MINUTES.values()) * 60   # 5400 s: how long after its dispatch a live run may hold the PC
NOTHING = {"dispatch-soak": ("no soak", "(nothing was dispatched)"),
           "switch-test": ("no switch test", "a switch would end it (nothing was switched)")}


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


def may_run(d: dict, entry: int, now: dt.datetime) -> bool:
    """The record is a live run of dev entry `entry` that may still run at
    `now` (an aware time). A record whose entry or time cannot be read may
    (fail safe)."""
    own = d.get("entry")
    if type(own) is int and own != entry:
        return False
    try:
        at = dt.datetime.fromisoformat(str(d.get("at")))
    except ValueError:
        return True
    if at.tzinfo is None or type(own) is not int:
        return True
    return now < at + dt.timedelta(seconds=WINDOW_S)


def running_live(ip, entry: int, now: dt.datetime) -> dict | None:
    """The newest live run dispatched in dev entry `entry` that may still run
    at `now`, or None."""
    return next((d for d in reversed(load_runs(ip)) if may_run(d, entry, now)), None)


def described(d: dict) -> str:
    return f"{d.get('sha')}, dispatched {d.get('at')}; live.yml's jobs hold the PC up to {WINDOW_S // 60} min"


def refuse_while_live(ip, command: str) -> None:
    """`dispatch-soak` and `switch-test` first: refused while a live run of
    this dev entry may still run (a soak would meet its engine restart, a
    switch would end it)."""
    d = running_live(ip, ip.current_entry(), dt.datetime.now().astimezone())
    if d is not None:
        head, tail = NOTHING[command]
        raise ip.Refused(f"{head}: a live run dispatched in this dev entry may still run ({described(d)}): {tail}")


def refuse_overlap(ip, entry: int) -> None:
    """dispatch-live: refused while another live run or a soak of this dev
    entry may still run."""
    now = dt.datetime.now().astimezone()
    d = running_live(ip, entry, now)
    if d is not None:
        raise ip.Refused(f"no live run: a live run dispatched in this dev entry may still run ({described(d)}) "
                         f"(nothing was dispatched)")
    soak = iempc_soak.running_soak(ip, entry, now)
    if soak is not None:
        raise ip.Refused(f"no live run: a soak dispatched in this dev entry may still run (dispatched "
                         f"{soak.get('at')}, {soak.get('hours')} h): the live run restarts the engine in a HIL job "
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
