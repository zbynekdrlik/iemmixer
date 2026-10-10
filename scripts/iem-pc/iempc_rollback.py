"""`iempc rollback [--dry-run]` (S8 lane 3, design note
docs/superpowers/specs/2026-10-10-s8-cutover-rollback-design.md section 3.3;
#11): the rollback to REAPER on the owner's word, through `iemmode rollback`.

The guard runs it (iemmixer stopped by the event plan's stops, the band's
data exported into a new project beside the original and put in its place,
the original kept; REAPER and the predecessor app with the handover checks,
REAPER on the original when it cannot open the export; the predecessor's
autostarts back, then the guard's logon trigger off; pin_changes closed;
trial and event saved and read back). Anything left keeps the PC rolling back:
a guard restart, or this command again, continues it.

Dev time only (the EVENT-NOW flag refuses it, --dry-run too: during an event
the engineer's "Back to REAPER" button is the rollback), the dev-box lock,
and never with an S1a/S1c window open (REAPER could not take the card). A new
flag during the guard's rollback abandons this client at once (the guard
goes on: its end is REAPER either way), then the event path runs, which
after the rollback is the event plan's checks. A rollback that outlives this
client's bound goes on (`iempc status --pc` reads where it is).

Also the lifecycle as `iemmode status` names it (`lifecycle`), which the
drill and `iempc switch-test` read: in prod a plain `iemmode event` is the
rollback, so the switch test refuses there.

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

# What the guard's status says of its lifecycle (`lifecycle::status`, Rust):
# nothing before the cutover.
PROD = "prod since "
ROLLING_BACK = "rolling back to REAPER"
# What the guard's rollback reply says when REAPER runs on the export
# (`rollback::ON_EXPORT`, Rust) or on the original (`ON_ORIGINAL`).
ON_EXPORT = "REAPER runs on the export"
ON_ORIGINAL = "REAPER runs on the original project"


def lifecycle(reply) -> str | None:
    """`trial`, `prod` or `rolling_back` from one guard reply's detail (pure);
    None without a reply or a detail."""
    detail = reply.get("detail") if isinstance(reply, dict) else None
    if not isinstance(detail, str):
        return None
    if ROLLING_BACK in detail:
        return "rolling_back"
    if PROD in detail:
        return "prod"
    return "trial"


def run(ctx, ip) -> int:
    """`iempc rollback` (dev time, locked)."""
    ip.refuse_open_window("rollback")
    dry = bool(ctx.args.dry_run)
    args = ["rollback", "--dry-run"] if dry else ["rollback"]
    try:
        code, reply, raw = ip.iemmode(ctx.env, args, ip.STATUS_S if dry else ip.SWITCH_S, ctx.watch(abandon=True))
    except ip.StillRunning as e:
        raise ip.StepError(f"{e}; the guard's rollback goes on: it ends in trial with REAPER, or stays rolling back "
                           "('iempc status --pc' reads which)") from None
    out = ip.result("iemmode", args, code, reply, raw)
    # The guard's reply says why when it did not end in trial (refused before
    # the cutover, stopped, or steps left).
    out["rollback"] = ("dry-run" if dry else "done") if code == 0 else "failed"
    ip.emit(out)
    return code
