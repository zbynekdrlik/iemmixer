"""`iempc ssh-shell --sha SHA [--dry-run]` (#15, the last item of the elevated
chain; the design on #15 of 2026-10-08): the admin-only OpenSSH default shell.

The PC's sshd runs every command through cmd.exe, and with no
HKLM\\SOFTWARE\\OpenSSH DefaultShell as `cmd.exe /c`, which first runs the
desktop user's HKCU AutoRun inside the elevated session. IemSshShell.psm1
writes three admin-only REG_SZ values (VALUES: the PC's own System32 cmd.exe,
`/d /c`, `/d`); sshd then builds the same command line as before with /d in
it, `"<shell>" /d /c "<command>"`, so no command iempc sends changes. A wrong
value could cut ssh to the PC, so:

1. Set-IemSshShell, elevated, imported only from the admin-only stage: the
   three modules (STAGE) come out of the fetched, attested bundle
   (`extract_member`, the sums checked again), go by scp into the bootstrap
   run folder, are read once on the PC, checked by their sha256 and staged
   (elevated_ps.staged), IemPc.psm1 and S1c's IemTuningStore.psm1 (its exact
   registry save and restore) first because the new module imports both
   from its own folder, and only the stage copy of the new one is imported. It saves the prior values, arms a
   one-shot SYSTEM task (UNDO_TASK, now + UNDO_MIN min) that restores them,
   then writes and reads back the values and the key's rights.
2. A FRESH ssh session (every call is one; sshd reads the key per
   connection) runs PROBE through the normal path (ssh_cmd, module_script)
   and reads its own parent process: the shell sshd started must be
   System32's cmd.exe run as `"<shell>" /d /c "<command>"` (parse_probe).
3. Only then Confirm-IemSshShell removes the task and the saved values.

A failed probe (an ssh call that fails, an answer that is no shell, a shell
without /d) never confirms: iempc names the undo task's time and exits 1,
with no further ssh call. Values that were already ours with nothing armed
(`unchanged`) are probed too, and nothing is confirmed. Dev time only (the
EVENT-NOW flag refuses it, --dry-run too); `--dry-run` prints the plan and
touches nothing. A new flag during the run lets a change finish, abandons the
probe and runs the event path; whatever was not confirmed is undone by the
task.

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import re
import sys

MODULE = "IemSshShell.psm1"
# (bundle member, stage name): IemSshShell.psm1 imports IemPc.psm1 (the
# elevated root, file and task helpers) and IemTuningStore.psm1 (Get-/Set-
# IemRegRaw, the exact registry save and restore) from its own folder, so the
# stage gets both first and those imports never reach outside it. The store is
# the bundle's tuning/ copy.
STAGE = (("IemPc.psm1", "IemPc.psm1"), ("tuning/IemTuningStore.psm1", "IemTuningStore.psm1"), (MODULE, MODULE))
KEY = "HKLM:\\SOFTWARE\\OpenSSH"
OPTION = "/d /c"
ARGUMENTS = "/d"
VALUES = {"DefaultShell": "System32\\cmd.exe of the PC, a literal path", "DefaultShellCommandOption": OPTION,
          "DefaultShellArguments": ARGUMENTS}
UNDO_TASK = "\\iemmixer\\iemmixer-ssh-shell-undo"
UNDO_MIN = 10
SET = "Set-IemSshShell"
CONFIRM = "Confirm-IemSshShell"
ARMED = ("set", "rearmed")
# The fresh session's own parent process: the shell sshd started for it.
PROBE = ("$iemSelf = Get-CimInstance -ClassName Win32_Process -Filter ('ProcessId = ' + $PID) ; "
         "$iemShell = Get-CimInstance -ClassName Win32_Process -Filter ('ProcessId = ' + $iemSelf.ParentProcessId) ; "
         "[pscustomobject]@{ exe = [string]$iemShell.ExecutablePath; line = [string]$iemShell.CommandLine }")
# sshd's line for a cmd shell with our option: `"<shell>" /d /c "<command>"`.
LINE = re.compile(r'"(?P<shell>[^"]+)"\s+/d /c\s+"')
SHELL_TAIL = "\\system32\\cmd.exe"


def plan(sha: str) -> dict:
    return {"sha": sha, "modules": [member for member, _ in STAGE], "key": KEY, "values": VALUES, "undo_task": UNDO_TASK,
            "undo_after_min": UNDO_MIN,
            "steps": [f"scp {', '.join(member for member, _ in STAGE)} of bundle {sha} into bootstrap/{sha}",
                      f"{SET} from the admin-only stage: save the prior values, arm {UNDO_TASK} (now + {UNDO_MIN} min), "
                      "write and read back the values and the key's rights",
                      'a fresh ssh session: its shell must be System32\'s cmd.exe run as "<shell>" /d /c "<command>"',
                      f"{CONFIRM}: remove the undo task and the saved values"]}


def undo_note(s: dict | None) -> str:
    """What happens to an unconfirmed change, for `s` the answer of Set (None: no answer)."""
    if s is None:
        return f"if it armed {UNDO_TASK}, that task restores the prior OpenSSH default shell within {UNDO_MIN} min"
    if s["state"] not in ARMED:
        return "no undo task is armed (the values were already ours)"
    return f"the SYSTEM task {UNDO_TASK} restores the prior OpenSSH default shell at {s['undo']['at']} (PC time)"


def check_set(ip, r) -> dict:
    if not isinstance(r, dict) or r.get("state") not in (*ARMED, "unchanged"):
        raise ip.StepError(f"{SET} answered {str(r)[:300]!r}, not set, rearmed or unchanged")
    undo = r.get("undo")
    if r["state"] in ARMED and not (isinstance(undo, dict) and isinstance(undo.get("at"), str) and undo["at"]):
        raise ip.StepError(f"{SET} answered {r['state']} without its undo task's time: {str(r)[:300]!r}")
    return r


def check_confirm(ip, r) -> dict:
    if not isinstance(r, dict) or r.get("state") not in ("confirmed", "unchanged"):
        raise ip.StepError(f"{CONFIRM} answered {str(r)[:300]!r}, not confirmed")
    return r


def parse_probe(ip, r) -> dict:
    """The fresh session's shell as PROBE read it (its parent process): sshd
    ran System32's cmd.exe as `"<shell>" /d /c "<command>"`, so cmd ran no
    AutoRun. Returns {shell, line}; anything else raises StepError naming
    what was read."""
    if not isinstance(r, dict) or not isinstance(r.get("exe"), str) or not isinstance(r.get("line"), str):
        raise ip.StepError(f"the probe answered {str(r)[:300]!r}, not the session's shell and its command line")
    exe, line = r["exe"], r["line"]
    m = LINE.match(line)
    if m is None:
        raise ip.StepError(f"the fresh session's shell did not run with /d before the command: {line[:300]}")
    shell = m.group("shell")
    if not shell.lower().endswith(SHELL_TAIL) or shell.lower() != exe.lower():
        raise ip.StepError(f"the fresh session's shell is {exe}, run as {shell}: not System32's cmd.exe")
    return {"shell": shell, "line": line}


def staged_import(ctx, ip, sha: str, rec: dict) -> tuple[list, str]:
    """The bundle's three modules (their sums checked again) and the
    statements that stage them on the PC in STAGE's order and import the new
    one from its stage copy only (module_script's `pre`)."""
    ep = ip.elevated_ps()
    rel = f"bootstrap/{sha}"
    uploads, mods = [], []
    for member, name in STAGE:
        local, hexd = ip.extract_member(sha, rec, member, nested="/" in member)
        uploads.append((local, name))
        mods.append((ip.ps_quote(ip.pc_join(ctx.env["PC_ROOT"], f"{rel}/{name}")), name, hexd))
    return uploads, f"{ep.staged(mods)} ; Import-Module $iemMod -Force ; "


def run(ctx, ip) -> int:
    """`iempc ssh-shell` (dev time, locked)."""
    sha = ip.check_sha(ctx.args.sha)
    rec = ip.need_record(sha)
    missing = [member for member, _ in STAGE if member not in (rec.get("sums") or {})]
    if missing:
        raise ip.Refused(f"bundle {sha} has no {', '.join(missing)}: fetch a bundle built with #15's ssh-shell")
    if ctx.args.dry_run:
        ip.emit({"ssh_shell": "dry-run", **plan(sha)})
        return 0
    env = ctx.env
    mode = ctx.watch(abandon=False)
    uploads, pre = staged_import(ctx, ip, sha, rec)
    rel = f"bootstrap/{sha}"
    ip.pc_mkdir(ctx, rel, mode)
    for local, name in uploads:
        ip.scp(str(local), ip.remote(env, f"{rel}/{name}"), mode)
    try:
        s = check_set(ip, ip.run_module(env, SET, ip.BOOTSTRAP_S, mode, pre=pre))
    except ip.EventNow:
        print(f"iempc: {SET} may have run; {undo_note(None)}", file=sys.stderr, flush=True)
        raise
    except ip.StepError as e:
        raise ip.StepError(f"{SET} failed: {e}; {undo_note(None)}") from None
    again = f"run 'iempc ssh-shell --sha {sha}' again once that is settled"
    try:
        probe = parse_probe(ip, ip.run_module(env, PROBE, ip.STATUS_S, ctx.watch(abandon=True)))
    except ip.EventNow:
        print(f"iempc: {SET} {s['state']}, not confirmed: {undo_note(s)}", file=sys.stderr, flush=True)
        raise
    except ip.StepError as e:
        raise ip.StepError(f"{e}; not confirmed: {undo_note(s)}; {again} (no further ssh call was made)") from None
    confirm = None
    if s["state"] in ARMED:
        # Confirm changes the PC: a new flag lets it finish, so it may have confirmed before the event path.
        try:
            confirm = check_confirm(ip, ip.run_module(env, CONFIRM, ip.BOOTSTRAP_S, mode, pre=pre))
        except ip.EventNow:
            print(f"iempc: {CONFIRM} may have finished before the event path; if it did not, {undo_note(s)}",
                  file=sys.stderr, flush=True)
            raise
        except ip.StepError as e:
            raise ip.StepError(f"{CONFIRM} failed: {e}; unless it removed the undo task, {undo_note(s)}; {again}") from None
    ip.emit({"ssh_shell": confirm["state"] if confirm else s["state"], "sha": sha, "set": s["state"],
             "confirm": confirm["state"] if confirm else None, "shell": probe["shell"], "line": probe["line"],
             "values": s.get("values"), "undo_log": s.get("undo_log")})
    return 0
