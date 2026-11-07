"""`iempc gen3-gate reboot` and `iempc gen3-gate reset` (#72, owner 2026-11-01:
the end-to-end tests include a reboot of the box, and the box survives a
sudden power loss without data damage): the box restarted for real, and proven
from outside to serve again with no manual step.

`reboot` is `systemctl reboot`. `reset` is sudden loss as near as software
comes: the kernel's magic SysRq `b` (no sync, no unmount) while the engine
saves, its writes kept up by `Writes`: the engineer's level over the band's
WebSocket, 20 times a second, moved by LOAD_DB and back. `reset` puts the
exact level back afterwards. A real power cut stays a physical step (#72).

Dev time, locked, a box command, (iempc's Spec, as the
other gates). No owner question: the owner approved reboots and hard kills of
the box for robustness (#72, 2026-11-01).

**Before any change** (Refused): `GEN3_HOST`, for `reset` also `LIVE_MEMBER`
(the mint's member); no HIL job left; healthy; a CI build its server names;
every required RX channel Receiving; the boot id; the engine's rev; the sums of
the files the band depends on (FILES).

**The verdict**, every check run, words in FAILURES' order:
- not_back: the box did not answer as a new boot within BACK_S;
- not_ready, not_healthy, rx_off: the clock, `health`, or a required RX
  channel not there within READY_S of the restart;
- units: a unit failed, or a unit of UNITS not active;
- no_shaping: no qdisc from the boot's shaping;
- phase: dantesync's phase error at its first lock missing or past PHASE_US.
  The stack goes ready on that lock, so a wide error is TX stamped wrong for
  minutes, late at a receiver at a low latency (#72, 2026-11-02: the FOH
  console's flow at 250 us, every packet late from about -150 us on);
- unlocked: dantesync lost its lock after it;
- fs_errors: the kernel's log since the boot names a filesystem or disk error;
- state: current.json unreadable, or a rev below the one before, or below the
  rev the engine says it loaded;
- files: a file of FILES changed;
- build: another build answers;
- level: `reset` only, the engineer's level did not come back.

Numbers only in the line `{"gen3_gate": {"reboot": {...}}}`, then the status
`reboot/gen3` or `reset/gen3` on the box's build. Exit 0 only when it passed
and the status was posted.

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import base64
import json
import math
import os
import re
import socket
import struct
import tempfile
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path

import iempc_gen3 as g3
import iempc_gen3_gate as gate
import iempc_gen3_soak as soak

BACK_S = 180.0   # the restart to ssh on a new boot
READY_S = 300.0   # the restart to the clock, health and every required RX channel
SETTLE_S = 20.0   # the writes run this long before the reset, so saves are under way
PHASE_US = 150.0   # past this a receiver at 250 us counted every packet late
LOAD_DB = 0.1   # the writes' move: no one hears it
WRITE_EVERY_S = 0.05
TOKEN_S = 3600
MIX = "engineer"
STATE = "/var/lib/iemmixer/engine/current.json"
FILES = ("/var/lib/iemmixer/site/iemmixer.toml", "/var/lib/iemmixer/site/roster.toml",
         "/var/lib/iem-dante/state.toml")
UNITS = ("iem-engine", "iem-server", "iem-tunnel", "iem-napi", "dantesync")
BOOT_ID = ("cat", "/proc/sys/kernel/random/boot_id")
REBOOT = (*gate.SUDO, "systemctl", "reboot")
SYSRQ_ON = (*gate.SUDO, "sysctl", "kernel.sysrq=1")
SYSRQ = (*gate.SUDO, "tee", "/proc/sysrq-trigger")
READ_STATE = (*g3.AS_SERVICE, "cat", STATE)
SUMS = (*g3.AS_SERVICE, "sha256sum", *FILES)
DANTESYNC_LOG = (*gate.SUDO, "journalctl", "-u", "dantesync.service", "-b", "-o", "cat", "-n", "3000", "--no-pager",
                 "-q")
KERNEL_LOG = (*gate.SUDO, "journalctl", "-k", "-b", "-o", "cat", "-n", "5000", "--no-pager", "-q")
FAILURES = ("not_back", "not_ready", "not_healthy", "rx_off", "units", "no_shaping", "phase", "unlocked", "fs_errors",
            "state", "files", "build", "level")
ANSI = re.compile(r"\x1b\[[0-9;]*m")
LOCK = re.compile(r"\[PHASE-LOCK\] engaged:.*\be=([-+]?[0-9.]+)us")
FS_ERROR = re.compile(r"EXT4-fs error|EXT4-fs warning|I/O error|Remounting filesystem read-only|Buffer I/O")
LOADED = re.compile(r"state rev (\d+) loaded")


# ---- pure ----

def first_lock(log: str) -> tuple[float | None, int]:
    """(the phase error at dantesync's first lock of this boot, us; how many
    times it lost the lock after it)."""
    error, unlocks = None, 0
    for line in ANSI.sub("", log).splitlines():
        m = LOCK.search(line)
        if m and error is None:
            error = float(m.group(1))
        elif error is not None and "=== UNLOCKED" in line:
            unlocks += 1
    return error, unlocks


def fs_errors(log: str) -> int:
    return sum(1 for line in log.splitlines() if FS_ERROR.search(line))


def state_of(text: str) -> tuple[int, float] | None:
    """(rev, the engineer's level) of the engine's state file; None when it
    does not read as one."""
    try:
        payload = json.loads(text)["payload"]
        rev, level = payload["rev"], payload["state"]["mixes"][MIX]["out"]["volume_db"]
    except (ValueError, KeyError, TypeError):
        return None
    if type(rev) is not int or not isinstance(level, (int, float)) or not math.isfinite(level):
        return None
    return rev, float(level)


def loaded_rev(journal: str) -> int | None:
    """The last rev the engine says it loaded (`journalctl -o json` lines).
    journald gives a message with colour codes as a list of bytes."""
    rev = None
    for line in journal.splitlines():
        try:
            message = json.loads(line).get("MESSAGE")
        except (ValueError, AttributeError):
            continue
        if isinstance(message, list):
            message = bytes(b & 255 for b in message if isinstance(b, int)).decode("utf-8", "replace")
        m = LOADED.search(ANSI.sub("", message)) if isinstance(message, str) else None
        if m:
            rev = int(m.group(1))
    return rev


@dataclass
class Seen:
    """What the restart left, read from outside."""
    back_s: float | None = None
    ready_s: float | None = None
    healthy_s: float | None = None
    rx_s: float | None = None
    units_ok: bool = False
    shaped: bool = False
    phase_us: float | None = None
    unlocks: int = 0
    fs_errors: int | None = None
    rev: int | None = None
    loaded: int | None = None
    sums_same: bool = False
    build_same: bool = False
    level_ok: bool | None = None   # None: the gate wrote no level
    state_read: dict = field(default_factory=dict)


def failed(s: Seen, rev_before: int) -> list[str]:
    """Every check's word, FAILURES' order (pure)."""
    out = []
    if s.back_s is None:
        return ["not_back"]
    for word, bad in (("not_ready", s.ready_s is None), ("not_healthy", s.healthy_s is None),
                      ("rx_off", s.rx_s is None), ("units", not s.units_ok), ("no_shaping", not s.shaped),
                      ("phase", s.phase_us is None or abs(s.phase_us) > PHASE_US), ("unlocked", s.unlocks > 0),
                      ("fs_errors", s.fs_errors is None or s.fs_errors > 0),
                      ("state", s.rev is None or s.rev < rev_before or (s.loaded is not None and s.rev < s.loaded)),
                      ("files", not s.sums_same), ("build", not s.build_same), ("level", s.level_ok is False)):
        if bad:
            out.append(word)
    return out


def verdict(kind: str, s: Seen, rev_before: int) -> dict:
    words = failed(s, rev_before)
    return {"pass": int(not words), "kind": kind, "back_s": s.back_s, "ready_s": s.ready_s, "healthy_s": s.healthy_s,
            "rx_s": s.rx_s, "phase_us": s.phase_us, "unlocks": s.unlocks, "fs_errors": s.fs_errors,
            "rev_before": rev_before, "rev": s.rev, "rev_loaded": s.loaded, "failed": words}


# ---- the writes ----

def ws_frame(text: str) -> bytes:
    data, mask = text.encode(), os.urandom(4)
    size = bytes([0x80 | len(data)]) if len(data) < 126 else bytes([0xFE]) + struct.pack(">H", len(data))
    return bytes([0x81]) + size + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(data))


def level_command(level: float) -> str:
    return json.dumps({"cmd": "SetGlobalLevel", "level_db": level})


def open_mix(host: str, token: str) -> socket.socket:
    """The engineer's mixer socket; OSError when the server does not take it.
    Its answers are drained, so the server's frames never pile up."""
    s = socket.create_connection((host, 80), timeout=5)
    key = base64.b64encode(os.urandom(16)).decode()
    s.sendall((f"GET /ws/{MIX}?token={token}&proto=3 HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\n"
               f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
    if b" 101 " not in s.recv(4096).split(b"\r\n", 1)[0]:
        s.close()
        raise OSError("the mixer socket was refused")

    def drain() -> None:
        try:
            while s.recv(65536):
                pass
        except OSError:
            pass
    threading.Thread(target=drain, daemon=True).start()
    return s


class Writes(threading.Thread):
    """The engineer's level moved and back until the box goes away or `stop`."""

    def __init__(self, host: str, token: str, level: float) -> None:
        super().__init__(daemon=True)
        self.host, self.token, self.level = host, token, level
        self.done = threading.Event()
        self.sent = 0

    def run(self) -> None:
        try:
            s = open_mix(self.host, self.token)
            while not self.done.wait(WRITE_EVERY_S):
                s.sendall(ws_frame(level_command(self.level - LOAD_DB * (self.sent % 2))))
                self.sent += 1
        except OSError:
            pass


def put_back(host: str, token: str, level: float) -> bool:
    try:
        s = open_mix(host, token)
        s.sendall(ws_frame(level_command(level)))
        time.sleep(1.0)
        s.close()
    except OSError:
        return False
    return True


# ---- the box ----

def boot_id(box: gate.Box) -> str | None:
    try:
        return box.call(list(BOOT_ID), event="ignore", timeout=15).strip() or None
    except box.ip.StepError:
        return None


def read_state(box: gate.Box) -> tuple[int, float] | None:
    try:
        return state_of(box.call(list(READ_STATE), event="ignore", timeout=15))
    except box.ip.StepError:
        return None


def receives(box: gate.Box) -> bool:
    status = box.device_status()
    return status is not None and bool(status.get("channels")) and not gate.not_receiving(status, box.optional)


def clock_ready(box: gate.Box) -> bool:
    status = box.device_status()
    clock = status.get("clock") if status else None
    return isinstance(clock, dict) and clock.get("ready") == 1


def timed(box: gate.Box, t0: float, test, limit: float) -> float | None:
    """Seconds since `t0` until `test()`; None past `limit` since `t0`."""
    if box.wait(test, max(0.0, limit - (time.monotonic() - t0))):
        return round(time.monotonic() - t0, 1)
    return None


def read(box: gate.Box, words) -> str | None:
    try:
        return box.call(list(words), event="ignore", timeout=30)
    except box.ip.StepError:
        return None


def prove(box: gate.Box, s: Seen, t0: float, sums: str, sha: str) -> None:
    """Everything read back once the box answers on its new boot."""
    s.ready_s = timed(box, t0, lambda: clock_ready(box), READY_S)
    s.healthy_s = timed(box, t0, box.healthy, READY_S)
    s.rx_s = timed(box, t0, lambda: receives(box), READY_S)
    active = read(box, ["systemctl", "is-active", *UNITS, "||", "true"])
    bad = read(box, ["systemctl", "--failed", "--no-legend"])
    s.units_ok = bad == "" and active is not None and active.split() == ["active"] * len(UNITS)
    s.shaped = "tbf" in (read(box, ["tc", "qdisc", "show"]) or "")
    log = read(box, DANTESYNC_LOG)
    if log is not None:
        s.phase_us, s.unlocks = first_lock(log)
    kernel = read(box, KERNEL_LOG)
    s.fs_errors = None if kernel is None else fs_errors(kernel)
    state = read_state(box)
    s.rev = state[0] if state else None
    journal = read(box, gate.ENGINE_JOURNAL)
    s.loaded = None if journal is None else loaded_rev(journal)
    s.sums_same = read(box, SUMS) == sums
    try:
        s.build_same = soak.box_build(box) == sha
    except box.ip.Refused:
        s.build_same = False


def restart(box: gate.Box, kind: str) -> float:
    """The restart sent; its time. A reset's call never answers."""
    if kind == "reboot":
        try:
            box.call(list(REBOOT), change=True, timeout=30)
        except box.ip.StepError:
            pass   # ssh may lose the box before it answers: the new boot id judges it
        return time.monotonic()
    box.call(list(SYSRQ_ON), change=True, timeout=30)
    t0 = time.monotonic()
    try:
        box.call(list(SYSRQ), change=True, event="ignore", stdin="b", timeout=8)
    except box.ip.StepError:
        pass
    return t0


# ---- the command ----

def run(ctx, ip, kind: str) -> int:
    """`iempc gen3-gate reboot|reset` (dev time, locked)."""
    host = g3.gen3_host(ip, ctx.env, gate.WORDS)
    member = ctx.env.get("LIVE_MEMBER", "")
    if kind == "reset" and not soak.ID.fullmatch(member):
        raise ip.Refused("gen3-gate: LIVE_MEMBER of the private env is not an id (the reset gate's token mint)")
    box = gate.Box(ip, ctx, host, kind)
    if soak.end_left_job(box) or not box.healthy():
        raise ip.Refused(f"no {kind} gate: the box is not healthy or a HIL job was just ended (nothing was changed)")
    sha = soak.box_build(box)
    if not receives(box):
        raise ip.Refused(f"no {kind} gate: a required RX channel does not receive (nothing was changed)")
    before, state, sums = boot_id(box), read_state(box), read(box, SUMS)
    if before is None or state is None or sums is None:
        raise ip.Refused(f"no {kind} gate: the boot id, the engine's state or the files could not be read "
                         "(nothing was changed)")
    rev0, level0 = state
    folder = Path(tempfile.mkdtemp(prefix="gen3-reset-", dir=ip.state_dir()))
    try:
        s, sent = Seen(), 0
        lan = host.rsplit("@", 1)[-1]
        token = ""
        if kind == "reset":
            path = g3.mint_tokens(ip, ctx, host, member, folder, TOKEN_S, "gen3-gate")
            token = json.loads(path.read_text(encoding="utf-8"))["engineer"]
            writes = Writes(lan, token, level0)
            writes.start()
            try:
                ip.pause(ctx, SETTLE_S)
            except ip.EventNow:   # no restart after a flag: the exact level back, then the event path
                writes.done.set()
                put_back(lan, token, level0)
                raise
            sent = writes.sent
            if not sent:
                writes.done.set()
                raise ip.Refused("no reset gate: the writes did not reach the box (nothing was changed)")
        t0 = restart(box, kind)
        if kind == "reset":
            writes.done.set()
        s.back_s = timed(box, t0, lambda: boot_id(box) not in (None, before), BACK_S)
        if s.back_s is not None:
            prove(box, s, t0, sums, sha)
        if kind == "reset" and s.back_s is not None:
            put_back(lan, token, level0)
            box.wait(lambda: abs((read_state(box) or (0, math.inf))[1] - level0) < 0.01, 15.0)
            now = read_state(box)
            s.level_ok = now is not None and abs(now[1] - level0) < 0.01
    finally:
        g3.drop_folder(folder)
    doc = verdict(kind, s, rev0)
    doc["writes"] = sent
    ip.emit({"gen3_gate": {kind: doc}})
    passed = doc["pass"] == 1
    description = (f"the Gen 3 box's {kind} passed (iempc gen3-gate {kind})" if passed
                   else f"the Gen 3 box's {kind} failed: " + ", ".join(doc["failed"]))
    posted = gate.post(ip, sha, passed, description, f"{kind}/gen3")
    return 0 if passed and posted else 1
