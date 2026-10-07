"""`iempc dispatch-soak --sha <bundle> [--hours 1..9]` (S7 design note §4, #10;
plan Task 8): dispatches the private ops repo's soak.yml with this box's gh
authentication (no token in the public repo).

The soak changes no guard state, so the PC must already run the bundle. In
this order, nothing dispatched on any refusal:

1. Before any call: dev time (iempc's Spec: no EVENT-NOW flag at the start),
   a full SHA, hours HOURS_MIN..HOURS_MAX (the ops job's 600 min hold 9 h of
   polls, the client's start and its end), and no soak of this SHA in this dev
   entry yet (RECORD in the state dir; the entry is the count of `iempc dev`
   and switch-test's dev leg, `current_entry`).
2. `iemmode status` (a new flag abandons the read and runs the event path):
   the guard answers ok, in dev, no switch, no HIL job; the active bundle and
   the running engine's build are both the SHA; the engine plays (neither
   parked nor faulted). `soak_refusal` judges the reply (pure).
3. A green push run of ci.yml for the SHA on dev or main, with its bundle and
   attest jobs (`green_run`, P5); the run's id and branch are what soak.yml
   gets.
4. The flag again, right before the dispatch: one that came during the gh
   waits refuses it.
5. `gh workflow run soak.yml -R <ops> -f sha -f branch -f run -f hours`, then
   the record (only after gh succeeded: a failed dispatch is no soak).

A failed ops run of the same SHA and dev entry is repeated with `gh run rerun
<id> -R <ops repo>` (the same verified inputs), never a second dispatch.

iempc.py passes itself in (`ip`), so this module never imports it (#36:
iempc.py is over its size budget)."""
from __future__ import annotations

import json
import re

SOAK_WORKFLOW = "soak.yml"
HOURS_DEFAULT = 8
HOURS_MIN = 1
HOURS_MAX = 9   # the ops job's 600 min hold 9 h of polls, the client's start and its end
RECORD = "soak.json"
KEEP = 200      # soaks kept in RECORD, the newest
# `iemmode status`'s detail is `mode <m>; bundle <sha>|no bundle[; HIL job <run>][; <notes>]`
# (iem-guard daemon::status_text): the active bundle is its second part.
SEP = "; "
BUNDLE = re.compile(r"bundle ([0-9a-f]{40})")
HIL_JOB = "HIL job"


def detail_parts(reply: dict) -> list[str]:
    detail = reply.get("detail")
    return detail.split(SEP) if isinstance(detail, str) else []


def active_bundle(reply: dict) -> str | None:
    """The bundle the guard's status names active (the detail's second part,
    `bundle <40 hex>`); None for `no bundle` or any other shape."""
    parts = detail_parts(reply)
    m = BUNDLE.fullmatch(parts[1]) if len(parts) > 1 else None
    return m.group(1) if m else None


def hil_job(reply: dict) -> str | None:
    """The detail's part naming a HIL job, if any (any part that starts with
    it: an unknown shape of it refuses, never passes)."""
    return next((p for p in detail_parts(reply) if p.startswith(HIL_JOB)), None)


def settled_refusal(reply) -> str | None:
    """Why the guard is not settled in dev, from one `iemmode status` reply
    (pure): it answers ok, in dev, no switch, no HIL job; None when it is.
    Also `iempc switch-test`'s first checks (iempc_switch.py)."""
    if not isinstance(reply, dict):
        return "iemmode status gave no reply"
    if reply.get("ok") is not True:
        return "the guard's status did not answer ok"
    if reply.get("mode") != "dev":
        return f"the guard is in mode {reply.get('mode')}, not dev"
    if reply.get("switching") is not None:
        return f"a switch runs ({json.dumps(reply['switching'])})"
    job = hil_job(reply)
    if job is not None:
        return f"a HIL job runs ({job})"
    return None


def playing_refusal(engine: dict, what: str) -> str | None:
    """Why the reply's `engine` does not play (`parked` and `faulted` must be
    exactly false), for `what` that measures it; None when it plays."""
    for state in ("parked", "faulted"):
        if engine.get(state) is not False:
            return f"the engine is {state} ({engine.get(state)!r}); {what} measures an engine that plays"
    return None


def soak_refusal(reply, sha: str) -> str | None:
    """Why the PC cannot be soaked at `sha` now, from one `iemmode status`
    reply (pure); None when it can."""
    why = settled_refusal(reply)
    if why:
        return why
    bundle = active_bundle(reply)
    if bundle != sha:
        return f"the active bundle is {bundle or 'none'}, not {sha}"
    engine = reply.get("engine")
    if not isinstance(engine, dict):
        return "no engine runs (the guard's status shows none)"
    if engine.get("build") != sha:
        return f"the running engine's build is {engine.get('build')!r}, not {sha}"
    return playing_refusal(engine, "a soak")


def check_hours(ip, hours) -> int:
    if type(hours) is not int or not HOURS_MIN <= hours <= HOURS_MAX:
        raise ip.Refused(f"dispatch-soak: --hours must be {HOURS_MIN}..{HOURS_MAX} (the ops soak job's 600 min "
                         f"hold at most {HOURS_MAX} h), not {hours!r}")
    return hours


def load_soaks(ip) -> list[dict]:
    """The soaks dispatched so far (RECORD); a record of another shape is an
    error, never read as none."""
    path = ip.state_dir() / RECORD
    soaks = ip.read_json(path, {}).get("soaks", [])
    if not isinstance(soaks, list) or not all(isinstance(d, dict) for d in soaks):
        raise ip.StepError(f"{path}: 'soaks' is not a list of objects; check it by hand")
    return soaks


def dispatch(ctx, ip) -> int:
    """`iempc dispatch-soak` (dev time, locked)."""
    sha = ip.check_sha(ctx.args.sha)
    hours = check_hours(ip, ctx.args.hours)
    entry = ip.current_entry()
    done = load_soaks(ip)
    if any(d.get("sha") == sha and d.get("entry") == entry for d in done):
        raise ip.Refused(f"a soak of {sha} was already dispatched in dev entry {entry} (a failed ops run of it: "
                         f"gh run rerun <id> -R {ip.OPS_REPO})")
    code, reply, _ = ip.iemmode(ctx.env, ["status"], ip.STATUS_S, ctx.watch(abandon=True))
    why = f"iemmode status failed (exit {code})" if code != 0 else soak_refusal(reply, sha)
    if why:
        raise ip.Refused(f"no soak: {why} (nothing was dispatched)")
    run, branch = ip.green_run(sha, ip.BRANCHES)
    if ip.event_now():   # "ide event" during the gh waits above: the soak is dev-time work
        raise ip.Refused(f"{ip.EVENT_NOW} appeared: no soak dispatch during an event (nothing was dispatched)")
    ip.gh(["workflow", "run", SOAK_WORKFLOW, "-R", ip.OPS_REPO, "-f", f"sha={sha}", "-f", f"branch={branch}",
           "-f", f"run={run}", "-f", f"hours={hours}"])
    record = {"sha": sha, "branch": branch, "run": run, "hours": hours, "entry": entry, "at": ip.now_iso()}
    ip.write_json(ip.state_dir() / RECORD, {"soaks": (done + [record])[-KEEP:]})
    ip.emit({"dispatched_soak": record})
    return 0
