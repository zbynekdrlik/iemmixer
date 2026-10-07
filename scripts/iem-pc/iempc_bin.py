"""The admin-only copies of our programs that iempc's elevated ssh session runs
(#15, the decision of 2026-10-07: an elevated session runs only admin-only copies).

The ssh session to the PC is elevated (the bootstrap, the tuning install and
the trace need it; a non-elevated account was rejected), so a program it runs
from the user's root (PC_BIN, bundles\\<sha>) is one any process of the user
may have replaced. Two rules:

- `iemmode.exe`: `activate` (online and --offline, right after the activation
  put the new bins in place, before the hand-over's reads) and `tuning-install`
  put the fetched, attested bundle's copy (`extract_member`, its sum checked
  again) into %ProgramData%\\iemmixer\\bin: scp into PC_ROOT\\incoming, read once
  on the PC and checked by its sha256, staged admin-only, moved into bin and
  read back (elevated_ps.installed_copy); the answer is the read-back sha256,
  which must be the zip's. This box records the installed build (RECORD,
  removed before every install, written after its read-back). Every iemmode
  call runs that copy while the guard last seen runs the recorded build
  (SEEN, below) and the elevated root, bin and the file read back admin-only
  and the file holds the recorded build (elevated_ps.verified_bin, composed
  into the same ssh call: no extra round trip on the event path), else
  PC_BIN's as before, with one note per command; without a record, or with
  the guard at another or an unknown build, it reads nothing there. The
  first `activate` after this landed installs it. A build activated by
  another path (HIL v1 on the PC) leaves the copy at the recorded build:
  iemmode then runs PC_BIN's, with the note, until the next `activate`.
- The guard's build (#15, the last lane, item 4): every reply of a guard
  names its `guard_build`, and this box keeps the last one (SEEN). It is the
  last known build rather than one read before each call: reading it first
  would take a second iemmode run (a guard round trip) per call, `iemmode
  event` on the event path included, and the reply already carries it.
  `dispatch-hil` forgets it (its run activates the SHA it was given), so the
  older copy never runs after a HIL dispatched from this box; an activation
  this box did not dispatch (an ops `gh run rerun`) is seen at the first
  reply after it, so the older copy runs that one call (an older iemmode
  fails only on a reply over 64 KiB, guard.md).
- `activate --offline` and `install --first` run the bundle's
  iemmixer-guard.exe from the stage: read once from bundles\\<sha> (incoming\\
  for the first bundle), checked by this box's fetch record, staged, checked
  again and run from there (`staged_guard`).

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import json
import os
import sys

NAME = "iemmode.exe"
GUARD = "iemmixer-guard.exe"
# The copy as named to the owner (the elevated root's known folder, no site path).
SHOWN = "%ProgramData%\\iemmixer\\bin\\iemmode.exe"
# The body of the install's ssh call: the installed copy's sha256, read back.
READ_BACK = "(Get-FileHash -LiteralPath $iemDst -Algorithm SHA256).Hash.ToLowerInvariant()"
# This box's record of the build in the admin-only bin ({sha, sha256}), in iempc's state.
RECORD = "elevated-bin.json"
# This box's last view of the running guard's build ({build, at}), in iempc's state.
SEEN = "guard-build.json"
NOTED: list[str] = []   # the note this command printed; main() clears it


def _root(ip, elevated_root: str | None):
    """elevated_ps and the elevated root as PowerShell (another one: the CI self-test)."""
    ep = ip.elevated_ps()
    return ep, ep.ROOT if elevated_root is None else ip.ps_quote(elevated_root)


def pick(ip) -> str:
    """native_script's `then` for an iemmode call: the recorded build's
    admin-only copy (pick_for) while the guard last seen runs that build;
    otherwise PC_BIN's, with the note."""
    rec = ip.read_json(ip.state_dir() / RECORD, None)
    hexd = rec.get("sha256") if isinstance(rec, dict) else None
    if not isinstance(hexd, str) or not ip.HEX64.fullmatch(hexd):
        noted("no admin-only copy is recorded on this box")
        return ""
    try:
        doc = ip.read_json(ip.state_dir() / SEEN, None)
    except ip.StepError as e:
        noted(f"the running guard's build cannot be read on this box ({e})")
        return ""
    build = doc.get("build") if isinstance(doc, dict) else None
    if not isinstance(build, str):
        noted(f"the admin-only copy is build {rec.get('sha')}, and the running guard's build is not known on this box")
        return ""
    if build != rec.get("sha"):
        noted(f"the admin-only copy is build {rec.get('sha')}, the guard last seen runs {build}")
        return ""
    return pick_for(ip, hexd)


def seen(ip, reply) -> None:
    """Keeps the guard_build an iemmode reply names (every reply of a guard
    does; iemmode's own --direct reply does not and leaves what was seen),
    written only when it changed, through a temp file of this process's own:
    the event path runs next to other commands. A failed write is a warning;
    the next pick then reads the build before, and the next reply writes again."""
    build = reply.get("guard_build") if isinstance(reply, dict) else None
    if not isinstance(build, str) or not 0 < len(build) <= 100:
        return
    path = ip.state_dir() / SEEN
    try:
        old = ip.read_json(path, None)
    except ip.StepError:
        old = None   # unreadable: written again below
    if isinstance(old, dict) and old.get("build") == build:
        return
    tmp = path.with_name(f"{path.name}.{os.getpid()}.tmp")
    try:
        tmp.write_text(json.dumps({"build": build, "at": ip.now_iso()}), encoding="utf-8")
        os.chmod(tmp, 0o600)
        tmp.replace(path)
    except OSError as e:
        print(f"iempc: WARNING: the guard's build {build!r} was not recorded on this box ({e}); the next reply records it",
              file=sys.stderr, flush=True)


def forget(ip) -> None:
    """dispatch-hil: its HIL run activates the SHA it was given, so the
    guard's build is not known until a reply names it (PC_BIN's iemmode
    meanwhile, with the note)."""
    (ip.state_dir() / SEEN).unlink(missing_ok=True)


def pick_for(ip, hexd: str, elevated_root: str | None = None) -> str:
    """$x becomes the admin-only copy when it reads back and holds `hexd`, else
    stays PC_BIN's and the reply's note says why (Test-IemStage.ps1 runs it)."""
    ep, root = _root(ip, elevated_root)
    return f"{ep.verified_bin(NAME, hexd, root)} ; if ($iemUse) {{ $x = $iemUse }} ; "


def noted(note: str) -> None:
    """The one note per command that an iemmode call ran from PC_BIN."""
    if NOTED:
        return
    NOTED.append(note)
    print(f"iempc: NOTE: iemmode ran from PC_BIN: the admin-only copy ({SHOWN}) was not used "
          f"({note[-300:]}); 'iempc activate --sha <build>' installs it", file=sys.stderr, flush=True)


def _install_pre(ip, src: str, hexd: str, elevated_root: str | None) -> str:
    ep, root = _root(ip, elevated_root)
    return ep.installed_copy(ip.ps_quote(src), NAME, hexd, root) + " ; "


def install_script(ip, src: str, hexd: str, elevated_root: str | None = None) -> str:
    """The install's ssh script as sent (Test-IemStage.ps1 runs it on the runner)."""
    return ip.module_script(READ_BACK, pre=_install_pre(ip, src, hexd, elevated_root))


def install(ctx, ip, sha: str) -> dict:
    """The fetched bundle's iemmode.exe into the admin-only bin, then this
    box's record of it. Every step changes the PC: a new flag lets each
    finish (then the event path)."""
    record = ip.state_dir() / RECORD
    record.unlink(missing_ok=True)   # until the read-back, no copy is trusted: iemmode runs PC_BIN's
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
    ip.write_json(record, {"sha": sha, "sha256": got})
    return {"path": SHOWN, "sha256": got}


def install_after_activate(ctx, ip, sha: str) -> None:
    """Right after the activation put the new bins in place (before the
    hand-over's reads): the new bundle's iemmode.exe into the admin-only bin.
    A failure is reported and never raised, so the activation counts; a
    failure after a new flag goes on to main, which runs the event path, and
    a new flag itself (EventNow) always does."""
    try:
        ip.emit({"elevated_bin": sha, **install(ctx, ip, sha)})
    except ip.StillRunning as e:
        if ip.event_now() and not ctx.flag_at_start:
            raise
        print(f"iempc: WARNING: the install may still run on the PC ({e}); it is left to finish, never force-ended; the "
              f"activation is done; iemmode runs from PC_BIN until 'iempc activate --sha {sha}' records the copy",
              file=sys.stderr, flush=True)
        ip.emit({"elevated_bin": "still-running", "sha": sha, "error": str(e)[-800:]})
    except ip.StepError as e:
        if ip.event_now() and not ctx.flag_at_start:
            raise
        print(f"iempc: WARNING: the admin-only iemmode.exe was not installed from {sha} ({e}); the activation is done; "
              f"iemmode runs from PC_BIN until 'iempc activate --sha {sha}' installs it", file=sys.stderr, flush=True)
        ip.emit({"elevated_bin": "failed", "sha": sha, "error": str(e)[-800:]})


def staged_guard(ip, exe: str, want: str) -> tuple[tuple[str, ...], str]:
    """native_script's `checks` and `then` for a bundle's own guard (activate
    --offline, install --first): `exe` read once, checked by `want` (this box's
    fetch record), staged admin-only and read back; the stage copy runs."""
    ep = ip.elevated_ps()
    stage = (f"$ErrorActionPreference = 'Stop' ; {ep.staged([(ip.ps_quote(exe), GUARD, want)])} ; "
             "$ErrorActionPreference = 'Continue'")
    return (stage,), "$x = $iemMod ; "
