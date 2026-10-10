"""`iempc cutover --sha SHA [--dry-run]` (S8 lane 2, design note
docs/superpowers/specs/2026-10-10-s8-cutover-rollback-design.md section 3.2;
#11): the owner's cutover message, and nothing else, runs it.

1. The guard's refusals and the live trial's precheck first, through `iemmode
   cutover --build SHA --dry-run` (only from trial, on the active bundle, a
   green main build, in dev or a live trial on it, no HIL job): a refusal
   changes nothing on the PC.
2. Install-IemCutover, elevated, imported only from the admin-only stage
   (STAGE: IemPc.psm1 and S1c's IemTuningStore.psm1 first, which the new
   module imports from its own folder, each checked by the zip's sha256, as
   iempc ssh-shell stages its module): the predecessor's autostarts from the
   private env (PC_AUTOSTART_TASKS, `;`-separated task paths
   `\\folder\\name`; PC_AUTOSTART_RUN, `;`-separated Run values
   `HKCU:\\...|name` or `HKLM:\\...|name`: site values, never in the
   repository) checked to exist, the module copies (only as the sha256 this
   box checked: the stage is shared), the entry script and the list
   admin-only into <elevated root>\\cutover, and the cutover task
   \\iemmixer\\iemmixer-cutover registered. Run again: the same.
3. `iemmode cutover --build SHA`: the guard runs the six steps (the final
   import as a live trial entry, the autostarts exported to
   <elevated root>\\cutover\\autostarts-<since> and disabled, the guard task's
   logon trigger, pin_changes = true and the server started again, Prod saved
   and read back, the post-cutover checks), each read back; any failure
   unwinds to trial and event (the guard's reply and alarm name the step).

Dev time only (the EVENT-NOW flag refuses it, --dry-run too), the dev-box
lock, and never with an S1a/S1c window open. `--dry-run` runs step 1 only. A
new flag during the install lets it finish, then the event path runs; during
the guard's cutover this client is abandoned at once (the guard pre-empts
itself and unwinds) and the event path runs.

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import re

MODULE = "IemCutover.psm1"
# (bundle member, stage name): IemCutover.psm1 imports both from its own folder.
STAGE = (("IemPc.psm1", "IemPc.psm1"), ("tuning/IemTuningStore.psm1", "IemTuningStore.psm1"), (MODULE, MODULE))
INSTALL = "Install-IemCutover"
TASKS_KEY = "PC_AUTOSTART_TASKS"
RUN_KEY = "PC_AUTOSTART_RUN"
TASK = "\\iemmixer\\iemmixer-cutover"
# What IemCutover's Test-IemAutostartList takes (checked here first, so a typo
# in the env never reaches the PC).
TASK_PATH = re.compile(r'\\[^"%!^&|<>\r\n\t]*[^"%!^&|<>\r\n\t\\]')
RUN_VALUE = re.compile(r'(HKCU|HKLM):\\[^|"\r\n]*[^|"\r\n\\]\|[^|"\r\n]+')


def autostarts(ip, env: dict[str, str]) -> tuple[list[str], list[str]]:
    """The predecessor's autostarts from the private env: task paths and Run
    values, at least one, each in IemCutover's form."""
    tasks = [t.strip() for t in env.get(TASKS_KEY, "").split(";") if t.strip()]
    run = [r.strip() for r in env.get(RUN_KEY, "").split(";") if r.strip()]
    if not tasks and not run:
        raise ip.Refused(f"the private env names no autostart of the predecessor ({TASKS_KEY}, {RUN_KEY}): "
                         "the cutover would disable nothing")
    bad = [t for t in tasks if not TASK_PATH.fullmatch(t)] + [r for r in run if not RUN_VALUE.fullmatch(r)]
    if bad:
        raise ip.Refused(f"the private env's autostarts are refused: {', '.join(repr(b) for b in bad)} "
                         "(tasks \\folder\\name; Run values HKCU:\\...|name or HKLM:\\...|name)")
    return tasks, run


def install_body(ip, sums: dict[str, str], root: str, tasks: list[str], run: list[str]) -> str:
    """Install-IemCutover with the modules' sha256 (names and lowercase hex
    only), the user's root and the lists, each value quoted."""
    for name, hexd in sums.items():
        if not re.fullmatch(r"[A-Za-z0-9_.-]+", name) or not re.fullmatch(r"[0-9a-f]{64}", hexd):
            raise ValueError(f"not a module name and a sha256: {name!r} {hexd!r}")
    sha = "; ".join(f"'{n}' = '{h}'" for n, h in sums.items())
    return (f"{INSTALL} -ModuleSha256 @{{ {sha} }} -Root {ip.ps_quote(root)} -Tasks @({quoted(ip, tasks)}) "
            f"-RunValues @({quoted(ip, run)})")


def quoted(ip, items: list[str]) -> str:
    """A PowerShell array's items, each single-quoted."""
    return ", ".join(ip.ps_quote(i) for i in items)


def check_install(ip, r) -> dict:
    """Install's answer: installed, the cutover task named."""
    if not isinstance(r, dict) or r.get("state") != "installed" or r.get("task") != TASK:
        raise ip.StepError(f"{INSTALL} answered {str(r)[:300]!r}, not installed {TASK}")
    return r


def staged_import(ip, ctx, sha: str, rec: dict) -> tuple[list, str, dict[str, str]]:
    """The bundle's three modules (their sums checked again), the statements
    that stage them on the PC in STAGE's order and import the new one from
    its stage copy only, and each one's sha256 by stage name."""
    ep = ip.elevated_ps()
    rel = f"bootstrap/{sha}"
    uploads, mods, sums = [], [], {}
    for member, name in STAGE:
        local, hexd = ip.extract_member(sha, rec, member, nested="/" in member)
        uploads.append((local, name))
        mods.append((ip.ps_quote(ip.pc_join(ctx.env["PC_ROOT"], f"{rel}/{name}")), name, hexd))
        sums[name] = hexd
    return uploads, f"{ep.staged(mods)} ; Import-Module $iemMod -Force ; ", sums


def run(ctx, ip) -> int:
    """`iempc cutover` (dev time, locked)."""
    ip.refuse_open_window("cutover")
    sha = ip.check_sha(ctx.args.sha)
    rec = ip.need_record(sha)
    missing = [member for member, _ in STAGE if member not in (rec.get("sums") or {})]
    if missing:
        raise ip.Refused(f"bundle {sha} has no {', '.join(missing)}: fetch a bundle built with S8's cutover")
    tasks, values = autostarts(ip, ctx.env)
    dry = ["cutover", "--build", sha, "--dry-run"]
    code, reply, raw = ip.iemmode(ctx.env, dry, ip.STATUS_S, ctx.watch(abandon=True))
    if code != 0 or ctx.args.dry_run:
        out = ip.result("iemmode", dry, code, reply, raw)
        out.update({"cutover": "dry-run" if code == 0 else "refused", "sha": sha, "tasks": tasks, "run": values})
        ip.emit(out)
        return code
    env = ctx.env
    mode = ctx.watch(abandon=False)
    uploads, pre, sums = staged_import(ip, ctx, sha, rec)
    rel = f"bootstrap/{sha}"
    ip.pc_mkdir(ctx, rel, mode)
    for local, name in uploads:
        ip.scp(str(local), ip.remote(env, f"{rel}/{name}"), mode)
    body = install_body(ip, sums, env["PC_ROOT"], tasks, values)
    try:
        installed = check_install(ip, ip.run_module(env, body, ip.BOOTSTRAP_S, mode, pre=pre))
    except ip.StepError as e:
        raise ip.StepError(f"{INSTALL} failed: {e}; the guard's cutover did not run") from None
    args = ["cutover", "--build", sha]
    code, reply, raw = ip.iemmode(env, args, ip.SWITCH_S, ctx.watch(abandon=True))
    out = ip.result("iemmode", args, code, reply, raw)
    out.update({"cutover": "done" if code == 0 else "failed", "sha": sha, "install": installed})
    ip.emit(out)
    return code
