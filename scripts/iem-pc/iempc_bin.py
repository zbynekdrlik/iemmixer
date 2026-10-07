"""The admin-only copies of our programs that iempc's elevated ssh session runs
(#15, the decision of 2026-10-07: an elevated session runs only admin-only copies).

The ssh session to the PC is elevated (the bootstrap, the tuning install and
the trace need it; a non-elevated account was rejected), so a program it runs
from the user's root (PC_BIN, bundles\\<sha>) is one any process of the user
may have replaced. Two rules:

- `iemmode.exe`: `activate` (online and --offline, after the verified
  hand-over) and `tuning-install` put the fetched, attested bundle's copy
  (`extract_member`, its sum checked again) into %ProgramData%\\iemmixer\\bin:
  scp into PC_ROOT\\incoming, read once on the PC and checked by its sha256,
  staged admin-only, moved into bin and read back (elevated_ps.installed_copy);
  the answer is the read-back sha256, which must be the zip's. Every iemmode
  call runs that copy while the elevated root, bin and the file read back
  admin-only (elevated_ps.verified_bin, composed into the same ssh call: no
  extra round trip on the event path), else PC_BIN's as before, with one note
  per command; the first `activate` after this landed installs the copy.
- `activate --offline` runs the bundle's iemmixer-guard.exe from the stage:
  read once from bundles\\<sha>, checked by this box's fetch record, staged,
  checked again and run from there (`offline_guard`).

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import sys

NAME = "iemmode.exe"
GUARD = "iemmixer-guard.exe"
# The copy as named to the owner (the elevated root's known folder, no site path).
SHOWN = "%ProgramData%\\iemmixer\\bin\\iemmode.exe"
# The body of the install's ssh call: the installed copy's sha256, read back.
READ_BACK = "(Get-FileHash -LiteralPath $iemDst -Algorithm SHA256).Hash.ToLowerInvariant()"
NOTED: list[str] = []   # the note this command printed; main() clears it


def _root(ip, elevated_root: str | None):
    """elevated_ps and the elevated root as PowerShell (another one: the CI self-test)."""
    ep = ip.elevated_ps()
    return ep, ep.ROOT if elevated_root is None else ip.ps_quote(elevated_root)


def pick(ip, elevated_root: str | None = None) -> str:
    """native_script's `then` for an iemmode call: $x becomes the admin-only
    copy when it reads back, else stays PC_BIN's and the reply's note says why."""
    ep, root = _root(ip, elevated_root)
    return f"{ep.verified_bin(NAME, root)} ; if ($iemUse) {{ $x = $iemUse }} ; "


def noted(note: str) -> None:
    """The one note per command that an iemmode call ran from PC_BIN."""
    if NOTED:
        return
    NOTED.append(note)
    print(f"iempc: NOTE: iemmode ran from PC_BIN: the admin-only copy ({SHOWN}) is not there or did not read back "
          f"({note[-300:]}); 'iempc activate --sha <build>' installs it", file=sys.stderr, flush=True)


def _install_pre(ip, src: str, hexd: str, elevated_root: str | None) -> str:
    ep, root = _root(ip, elevated_root)
    return ep.installed_copy(ip.ps_quote(src), NAME, hexd, root) + " ; "


def install_script(ip, src: str, hexd: str, elevated_root: str | None = None) -> str:
    """The install's ssh script as sent (Test-IemStage.ps1 runs it on the runner)."""
    return ip.module_script(READ_BACK, pre=_install_pre(ip, src, hexd, elevated_root))


def install(ctx, ip, sha: str) -> dict:
    """The fetched bundle's iemmode.exe into the admin-only bin. Every step
    changes the PC: a new flag lets each finish (then the event path)."""
    rec = ip.need_record(sha)
    local, hexd = ip.extract_member(sha, rec, NAME)
    mode = ctx.watch(abandon=False)
    rel = f"incoming/iemmode-{sha}.exe"
    ip.pc_mkdir(ctx, "incoming", mode)
    ip.scp(str(local), ip.remote(ctx.env, rel), mode)
    got = ip.run_module(ctx.env, READ_BACK, ip.INSTALL_S, mode,
                        pre=_install_pre(ip, ip.pc_join(ctx.env["PC_ROOT"], rel), hexd, None))
    if not isinstance(got, str) or not ip.HEX64.fullmatch(got):
        raise ip.StepError(f"the PC read back {NAME} {got!r}, not a sha256")
    if got != hexd:
        raise ip.StepError(f"the PC read back {NAME} {got}, this box sent {hexd}")
    return {"path": SHOWN, "sha256": got}


def install_after_activate(ctx, ip, sha: str) -> None:
    """After activate's verified hand-over: the new bundle's iemmode.exe into
    the admin-only bin. A failure is reported and never raised, so the
    activation counts; a failure after a new flag goes on to main, which runs
    the event path, and a new flag itself (EventNow) always does."""
    try:
        ip.emit({"elevated_bin": sha, **install(ctx, ip, sha)})
    except ip.StepError as e:   # StillRunning too: the install is left to finish, never force-ended
        if ip.event_now() and not ctx.flag_at_start:
            raise
        print(f"iempc: WARNING: the admin-only iemmode.exe was not installed from {sha} ({e}); the activation is done; "
              f"iemmode runs from PC_BIN until 'iempc activate --sha {sha}' installs it", file=sys.stderr, flush=True)
        ip.emit({"elevated_bin": "failed", "sha": sha, "error": str(e)[-800:]})


def offline_guard(ip, exe: str, want: str) -> tuple[tuple[str, ...], str]:
    """native_script's `checks` and `then` for activate --offline's guard:
    bundles\\<sha>\\iemmixer-guard.exe read once, checked by `want` (this box's
    fetch record), staged admin-only and read back; the stage copy runs."""
    ep = ip.elevated_ps()
    stage = (f"$ErrorActionPreference = 'Stop' ; {ep.staged([(ip.ps_quote(exe), GUARD, want)])} ; "
             "$ErrorActionPreference = 'Continue'")
    return (stage,), "$x = $iemMod ; "
