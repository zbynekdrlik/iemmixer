"""`iempc event`, "ide event" on the dev box: the flag and nothing else (#86).
The box serves the band at every event (Gen 2 ended 2026-11-01), so the flag only freezes dev-time work: every command that changes
something refuses while it exists, and every wait sees a new flag within 2 s. A
flag that cannot be written is a warning, never a stop."""
from __future__ import annotations

import sys

import iempc_core as core
from iempc_core import Ctx, emit, event_now


def ensure_flag() -> bool:
    """Writes the "ide event" flag (`date -Iseconds`) unless it exists; True when written."""
    if event_now():
        return False
    core.EVENT_NOW.parent.mkdir(parents=True, exist_ok=True)
    try:
        with open(core.EVENT_NOW, "x", encoding="utf-8") as f:
            f.write(core.now_iso() + "\n")
    except FileExistsError:
        return False
    return True


def write_flag() -> None:
    """`event` writes the flag first; a flag it cannot write (no folder, a
    full or read-only disk) is a warning, never a stop of the event path."""
    try:
        written = ensure_flag()
    except OSError as e:
        print(f"iempc: WARNING: the flag {core.EVENT_NOW} was not written ({e}); write the flag by "
              "hand so no dev-time command runs", file=sys.stderr, flush=True)
        emit({"flag": str(core.EVENT_NOW), "written": False, "error": str(e)})
        return
    if written:
        emit({"flag": str(core.EVENT_NOW), "written": True})


def cmd_event(ctx: Ctx) -> int:
    """The flag; the box is left alone."""
    if not getattr(ctx.args, "dry_run", False):
        write_flag()
    emit({"event": "the flag only: the box keeps serving the band"})
    return 0
