"""`iempc gen3-gate hil` (#72 phase H, plan
docs/superpowers/plans/2026-10-28-gen3-phase-h-box-gates.md Tasks 2, 4 and
6): the Gen 3 box's own HIL gate, driven from the dev box over ssh while 3.x
serves the band, its verdict posted as the commit status `hil/gen3` on the
box's build.

Dev time, locked, a box command (iempc's Spec: refused while EVENT-NOW
exists, and while 2.x serves: iempc.WHILE_3X_SERVES, the inverse of
WHILE_2X_SERVES). The box has no guard: the dev box's flag is the event gate,
and "ide event" keeps 3.x running.

**The box's HIL job** (Task 4; shared by the later `live` and `soak` gates:
`begin_job` / `end_job`). The engine's HIL mode is its `--test-signal` flag
(every mix TX written as zero during a burst, the sine only on the site's
`[guard] hil_tx` spare outputs, their D5(b) returns opened). The job is a
runtime drop-in, DROPIN in /run (gone at the next boot), whose `ExecStart=`
is reset and set to the installed unit's own line plus HIL_FLAGS:
- begin: the drop-in written to the admin's stage over ssh's stdin, `sudo -n
  install` into place, read back byte for byte, `daemon-reload`,
  `reset-failed iem-engine`, `restart iem-engine`, then bounded waits for
  `iem-dante-dev health` healthy and for every RX channel that was Receiving
  before the restart to be Receiving again;
- end: `sudo -n rm -f` of the drop-in, `daemon-reload`, and, once the begin
  restarted the engine, `reset-failed` and `restart` again, the same waits,
  and the engine read back without spare outputs (the test flag off).
Every call is one line of scripts/gen3/sudo-lines.txt. An end always runs to
its end ("ignore": a flag never cuts a change of it); its waits are cut by a
flag. `deploy.sh` ends a job a dead run left before its own start; this
command ends one first too (with a restart only when the engine still runs
with its spare outputs).

**The restart budget** (I8; the unit's `StartLimitBurst` starts within
`StartLimitIntervalSec`, read from the installed unit). systemd's counter is
reset (`reset-failed`) before each deliberate start, so a job never meets the
unit's own limit; on top of that, every engine start this command asks for is
kept in iempc's state (`gen3-gate.json`, wall-clock times) and a plan whose
starts, with those of the unit's last interval, would pass the burst is
refused before anything changes (a left job's end counts one, the run two).
The engine's automatic restarts during a run (its `NRestarts`) are added and
fail the verdict.

**Before any change** (each a refusal, nothing changed): `GEN3_HOST` and
`GEN3_DANTE_IP` in the private env; a job left over (`test -e` of the
drop-in); no other drop-in on the engine's unit (`DropInPaths`); the
installed unit's ExecStart of the expected shape and its start limit; the
installed device config's RX and TX counts and the installed site's `[guard]
hil_tx` (read as the service user, numbers kept, nothing of either printed:
Task 2's preflight), every spare output within the device's TX count and its
D5(b) return within the RX count (else `--test-signal` exits 3 and the unit
stays down with an alarm), and `iem-engine check-site` of that site exit 0
(hil_tx against the topology, else exit 2); the box healthy; our device's RX
table and the first spare output's TX name read over ARC by
`GEN3_DANTE_IP` (gen3_arc); the budget.

**The checks** (Task 6), in the job: health healthy and the RX flows back;
the engine's spare outputs open (`supervise status`); versions (the
engine's `--version`, the device's `--version` and the server's
`/api/version` `git_hash` on the box, one SHA); the D5(b) loop (our spare
return self-subscribed to our own first spare output over ARC 0x3010 unless
it already is, waited Receiving); one burst (`supervise test-signal mic1 -30
10`, the flag checked first; `supervise status` polled until the spare
outputs carried it and a later status shows them silent, within
BURST_LIMIT_S): every spare output's loudest peak at -30 dBFS within 0.5 dB,
the last status silent, `loopback_samples` above 0; the return's
subscription put back as it was (or none) and read back; the panic is not
covered (no supervisor fault injection on the box: `iem-engine supervise`
has none, and the engine runs without `--fault-injection`); the alarm path's
count (`iem-server notify --count alarm` as the service user, at least one
device; never a notice) and the engine enabled at boot (the alarm unit's
gate); no automatic restart during the run. Then the job's end.

**The verdict:** one line `{"gen3_gate": {"hil": {...}}}`, numbers and the
fixed words of FAILURES only (no name, address, channel number or build
hash), then the commit status `hil/gen3` on the engine's build SHA through
`gh api repos/<repo>/statuses/<sha>` (success or failure, a fixed
description naming the failed checks' words). Exit 0 only when it passes,
the job ended and the status was posted.

**A flag mid-run:** the burst is stopped (`supervise stop-test-signal`), the
return's subscription put back, the job ended without its waits, and the
event path runs (EventNow: iempc main). No verdict and no status then.

P6: GEN3_HOST, GEN3_DANTE_IP, the device's and its channels' names, the
spare outputs' numbers and the site never reach stdout or stderr.

`iempc gen3-gate live` (Task 5) is iempc_gen3_live.py: it builds on this
module (the preflight without the ARC reads, the job, the budget, the status
post) and `run` passes the call on to it. `iempc gen3-gate soak [--minutes
N]` (Task 7) is iempc_gen3_soak.py: no HIL job (a left one is ended first),
the Box, the status post; `--minutes` is the soak's only.

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import json
import math
import re
import struct
import sys
import time
import tomllib
from dataclasses import dataclass, field

import gen3_arc
import iempc_gen3 as g3

WORDS = g3.Words("gen3-gate", "the HIL gate")
CONTEXT = "hil/gen3"
# The HIL job: a runtime drop-in of the engine's unit (gone at the next boot).
DROPIN = "/run/systemd/system/iem-engine.service.d/iem-hil.conf"
STAGE = ".cache/iem-gen3-gate"
STAGED = f"{STAGE}/iem-hil.conf"
UNIT = "iem-engine.service"
UNIT_FILE = f"/etc/systemd/system/{UNIT}"
DEVICE_CONFIG = "/etc/iemmixer/dante-device.toml"
SITE = "/var/lib/iemmixer/site/iemmixer.toml"
ENGINE_BIN = g3.ENGINE_BIN
SERVER_BIN = "/opt/iemmixer/bin/iem-server"
EXEC_HEAD = f"{ENGINE_BIN} run "
# The engine's HIL flags for the job. Only the test signal: no step drives a
# fault (iem-engine supervise has no injection), and an engine under
# --fault-injection would take one from any supervisor.
HIL_FLAGS = ("--test-signal",)
FLAG_WORDS = ("--test-signal", "--fault-injection")
AS_SERVICE = g3.AS_SERVICE
SUDO = ("sudo", "-n")
SUPERVISE = (*AS_SERVICE, ENGINE_BIN, "supervise")
# The gate's one burst, HIL v1's (hil-v1.ps1): fixed, as the sudo rule names it.
BURST_INPUT, BURST_DBFS, BURST_SECONDS = "mic1", -30, 10
LEVEL_DB = 0.5   # HIL v1's tolerance for the spare outputs' level
CALL_S = 30   # one ssh call: the connect, then the box's own bounded command
RESTART_S = 60   # `systemctl restart`: the engine's bounded stop (10 s), then its start
HEALTH_WAIT_S = 90.0   # healthy after a restart: the clock's 30 s grace and more
RX_WAIT_S = 180.0   # the RX flows again: the FOH card grants one flow about every 9 s
LOOP_WAIT_S = 30.0   # our own self-loop's flow, Receiving
BURST_LIMIT_S = BURST_SECONDS + 30.0   # the burst heard and ended
LEDGER = "gen3-gate.json"
LEDGER_KEEP = 64
VERSION_LINE = re.compile(r"iem-dante-dev \S+ ([0-9a-f]{40})")
HASH = re.compile(r"[0-9a-f]{7,40}")
COUNT = re.compile(r"[0-9]{1,6}")
DESC_PASS = "the Gen 3 box's HIL passed (iempc gen3-gate hil)"
DESC_FAIL = "the Gen 3 box's HIL failed: "
# The failures a verdict names, in its order.
FAILURES = ("health", "rx_not_back", "hil_not_on", "versions", "loop", "not_heard", "level", "not_silent",
            "no_loopback", "loop_not_restored", "alarm_count", "alarm_not_armed", "restarted", "job_end")
# The server's build, read on the box (python3 -I on ssh's stdin, no file left
# there): its `git_hash` only.
VERSION_SCRIPT = """import json, sys, urllib.request
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
with opener.open("http://127.0.0.1/api/version", timeout=5) as reply:
    doc = json.load(reply)
value = doc.get("git_hash") if isinstance(doc, dict) else None
print(value if isinstance(value, str) else "")
"""


def wall() -> float:
    """The wall clock of the restart budget (tests replace it)."""
    return time.time()


# ---- pure: the unit, the drop-in, the counts, the budget ----

def unit_lines(text: str) -> dict[str, list[tuple[str, str, bool]]]:
    """A unit file's (key, value, continued) lines by section."""
    out: dict[str, list[tuple[str, str, bool]]] = {}
    section = ""
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line[0] in "#;":
            continue
        if line.startswith("[") and line.endswith("]"):
            section = line[1:-1]
            continue
        key, sep, value = line.partition("=")
        if sep:
            out.setdefault(section, []).append((key.strip(), value.strip(), raw.rstrip().endswith("\\")))
    return out


def exec_start(text: str) -> str:
    """The installed unit's one ExecStart line, checked for the drop-in to
    repeat it; ValueError names what is wrong (never its words)."""
    lines = [(v, cont) for k, v, cont in unit_lines(text).get("Service", []) if k == "ExecStart"]
    if len(lines) != 1:
        raise ValueError("the installed engine unit has not exactly one ExecStart")
    value, continued = lines[0]
    if continued or not value.startswith(EXEC_HEAD):
        raise ValueError(f"the installed engine unit's ExecStart is not one line '{EXEC_HEAD}...'")
    if any(word in FLAG_WORDS for word in value.split()):
        raise ValueError("the installed engine unit's ExecStart already holds a HIL flag")
    return value


def start_limit(text: str) -> tuple[int, int]:
    """(StartLimitBurst, StartLimitIntervalSec in s) of the unit's [Unit]
    table; ValueError when either is not a plain number there."""
    keys = {k: v for k, v, _ in unit_lines(text).get("Unit", [])}
    burst, interval = keys.get("StartLimitBurst", ""), keys.get("StartLimitIntervalSec", "")
    if not (burst.isdigit() and interval.isdigit() and int(burst) > 0 and int(interval) > 0):
        raise ValueError("the installed engine unit names no StartLimitBurst and StartLimitIntervalSec in seconds")
    return int(burst), int(interval)


def dropin_text(line: str) -> str:
    """The HIL job's drop-in: ExecStart reset, then the unit's own line and
    the HIL flags."""
    return ("# iempc gen3-gate's HIL job on the Gen 3 box (#72 phase H): the engine's ExecStart with its\n"
            "# HIL test flag for one run. The job's end removes this file; in /run a reboot does too.\n"
            "[Service]\nExecStart=\n" f"ExecStart={line} {' '.join(HIL_FLAGS)}\n")


def device_counts(text: str) -> tuple[int, int]:
    """(RX, TX) channel counts of a device config; ValueError otherwise."""
    doc = tomllib.loads(text)
    rx, tx = doc.get("rx"), doc.get("tx")
    if not (isinstance(rx, list) and isinstance(tx, list) and rx and tx):
        raise ValueError("the installed device config has no rx and tx lists")
    return len(rx), len(tx)


def site_hil_tx(text: str) -> list[int]:
    """The site's `[guard] hil_tx`; ValueError when it is none or not channel numbers."""
    guard = tomllib.loads(text).get("guard")
    hil = guard.get("hil_tx") if isinstance(guard, dict) else None
    if not isinstance(hil, list) or not hil or not all(type(n) is int and 1 <= n <= 65535 for n in hil):
        raise ValueError("the installed site names no [guard] hil_tx spare outputs")
    return hil


def counts_refusal(hil_tx: list[int], rx: int, tx: int) -> str | None:
    """Why the engine could not open the site's spare outputs and their D5(b)
    returns on the box's device (pure); None when it can. No number named."""
    if max(hil_tx) > tx:
        return ("the site's [guard] hil_tx names a spare output the box's device has no TX channel for "
                "(an engine started with --test-signal would exit 3 and stay down)")
    if max(hil_tx) > rx:
        return ("the site's [guard] hil_tx names a spare output whose D5(b) return the box's device has no RX "
                "channel for (an engine started with --test-signal would exit 3 and stay down)")
    return None


def budget_refusal(times: list[float], now: float, interval: int, burst: int, planned: int) -> str | None:
    """Why `planned` more engine starts would pass the unit's `burst` within
    `interval` s with those already asked at `times` (pure); None when not."""
    recent = sorted(t for t in times if now - interval < t <= now)
    over = len(recent) + planned - burst
    if over <= 0:
        return None
    if planned > burst:
        return f"it needs {planned} engine starts, more than the unit's {burst} in {interval} s"
    wait = math.ceil(recent[over - 1] + interval - now)
    return (f"{len(recent)} engine starts in the last {interval} s and {planned} more would pass the unit's "
            f"{burst}: run it again in {wait} s")


def receiving(status: dict) -> set[int]:
    """The RX channels a device status shows Receiving (pure)."""
    try:
        rows = g3.rows(status, "channels")
    except ValueError:
        return set()
    return {c["rx"] for c in rows if type(c.get("rx")) is int and c.get("status") == g3.RECEIVING}


def hil_peaks(status) -> dict[int, float] | None:
    """The engine's spare outputs and their peaks from a supervise status
    (`Status.hil`); None without that list."""
    return g3.spare_peaks(status)


def last_line(text: str) -> str:
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    return lines[-1] if lines else ""


def versions_judged(engine_out: str, device_out: str, server_out: str) -> tuple[str | None, bool]:
    """The engine's build SHA (None when it names none) and whether the
    device and the server are that build too (pure; each answer's last
    line)."""
    m = g3.BUILD_LINE.fullmatch(last_line(engine_out))
    sha = m.group(1) if m else None
    d = VERSION_LINE.fullmatch(last_line(device_out))
    server = last_line(server_out)
    ok = (sha is not None and d is not None and d.group(1) == sha and HASH.fullmatch(server) is not None
          and sha.startswith(server))
    return sha, ok


def db(peak: float) -> float:
    return 20 * math.log10(peak) if peak > 0 else -150.0


@dataclass
class Burst:
    """What one burst showed: refused (the engine took no signal), the
    statuses that carried it, the silent one after, its round trip."""
    refused: bool = False
    during: list[dict[int, float]] = field(default_factory=list)
    after: dict[int, float] | None = None
    loopback: int = 0


def signal_judged(outputs: list[int], b: Burst) -> dict:
    """The spare outputs' level and silence (pure; HIL v1's Test-IemHilSignal):
    each one's loudest peak over the statuses that carried the burst, the
    last status silent."""
    loudest = {tx: max((p.get(tx, 0.0) for p in b.during), default=0.0) for tx in outputs}
    levels = [db(loudest[tx]) for tx in outputs]
    still = sum(1 for tx in outputs if (b.after or {}).get(tx, 1.0) != 0) if b.after is not None else len(outputs)
    return {"heard": 1 if b.during else 0, "outputs": len(outputs),
            "lowest_dbfs": round(min(levels), 2) if levels and b.during else None,
            "highest_dbfs": round(max(levels), 2) if levels and b.during else None,
            "level_ok": bool(levels) and all(abs(v - BURST_DBFS) <= LEVEL_DB for v in levels),
            "still": still}


@dataclass
class Checks:
    """What the job measured."""
    health: bool = False
    rx_back: bool = False
    rx_channels: int = 0
    hil_on: bool = False
    versions: bool = False
    sha: str | None = None
    loop: str = "none"   # kept (already our own loop), set, refused, none
    burst: Burst | None = None
    loop_restored: bool = True
    alarm_devices: int | None = None
    alarm_armed: bool = False
    unit_restarts: int = 0
    starts: int = 0
    end_ok: bool = False
    hil_off: bool = False


def verdict(c: Checks, outputs: list[int], left: bool) -> dict:
    """The gate's verdict (pure): numbers and the fixed words of FAILURES."""
    failed = []
    signal = signal_judged(outputs, c.burst or Burst(refused=True))
    if not c.health:
        failed.append("health")
    if not c.rx_back:
        failed.append("rx_not_back")
    if not c.hil_on:
        failed.append("hil_not_on")
    if not c.versions:
        failed.append("versions")
    if c.loop not in ("kept", "set"):
        failed.append("loop")
    if not signal["heard"]:
        failed.append("not_heard")
    elif not signal["level_ok"]:
        failed.append("level")
    if signal["heard"] and signal["still"]:
        failed.append("not_silent")
    loopback = c.burst.loopback if c.burst else 0
    if loopback <= 0:
        failed.append("no_loopback")
    if not c.loop_restored:
        failed.append("loop_not_restored")
    if c.alarm_devices is None or c.alarm_devices < 1:
        failed.append("alarm_count")
    if not c.alarm_armed:
        failed.append("alarm_not_armed")
    if c.unit_restarts:
        failed.append("restarted")
    if not (c.end_ok and c.hil_off):
        failed.append("job_end")
    return {
        "pass": 0 if failed else 1,
        "health": int(c.health), "rx_back": int(c.rx_back), "rx_channels": c.rx_channels, "hil_on": int(c.hil_on),
        "versions": int(c.versions),
        "outputs": signal["outputs"], "dbfs": BURST_DBFS, "seconds": BURST_SECONDS,
        "lowest_dbfs": signal["lowest_dbfs"], "highest_dbfs": signal["highest_dbfs"], "still": signal["still"],
        "loop": c.loop, "loopback_samples": loopback, "loop_restored": int(c.loop_restored),
        "panic": "not_covered",
        "alarm_devices": c.alarm_devices, "alarm_armed": int(c.alarm_armed),
        "unit_restarts": c.unit_restarts, "starts": c.starts, "left_job": int(left),
        "job_end": int(c.end_ok), "hil_off": int(c.hil_off),
        "failed": failed,
    }


# ---- the box ----

class Box:
    """One ssh call per command on the Gen 3 box, the flag watched as
    iempc_core.guarded watches it: reads `abandon`, changes `finish` unless
    a caller says otherwise. `name` heads its refusals ("no HIL gate", "no
    live gate")."""

    def __init__(self, ip, ctx, host: str, name: str = "HIL") -> None:
        self.ip, self.ctx, self.host, self.name = ip, ctx, host, name

    def call(self, words: list[str], *, change: bool = False, event: str | None = None, stdin: str = "",
             timeout: float = CALL_S) -> str:
        mode = event or self.ctx.watch(abandon=not change)
        return self.ip.guarded([*g3.SSH, self.host, *words], stdin, timeout, mode)

    def last_json(self, out: str) -> dict:
        lines = [line for line in out.splitlines() if line.strip()]
        try:
            doc = json.loads(lines[-1]) if lines else None
        except ValueError:
            doc = None
        if not isinstance(doc, dict):
            raise self.ip.StepError("gen3-gate: a box command's answer is not a JSON object")
        return doc

    def engine_status(self, event: str | None = None) -> dict | None:
        """The engine's Status (`supervise status`), None when it gave none."""
        try:
            doc = self.last_json(self.call([*SUPERVISE, "status"], event=event))
        except self.ip.StepError:
            return None
        status = doc.get("status")
        return status if isinstance(status, dict) else None

    def healthy(self) -> bool:
        try:
            self.call([g3.DANTE_BIN, "health"])
        except self.ip.StepError:
            return False
        return True

    def device_status(self) -> dict | None:
        try:
            return g3.device_status(self.ip, self.ctx, self.host, WORDS)
        except self.ip.StepError:
            return None

    def wait(self, test, limit: float) -> bool:
        """`test()` polled every POLL_S until true, at most `limit` s."""
        deadline = time.monotonic() + limit
        while True:
            if test():
                return True
            if time.monotonic() > deadline:
                return False
            self.ip.pause(self.ctx, self.ip.POLL_S)

    def wait_receiving(self, want: set[int], limit: float) -> bool:
        return self.wait(lambda: want <= receiving(self.device_status() or {}), limit)


# ---- the restart budget ----

def ledger_times(ip, name: str = "HIL") -> list[float]:
    try:
        doc = ip.read_json(ip.state_dir() / LEDGER, {})
    except ip.StepError:
        raise ip.Refused(f"no {name} gate: {LEDGER} in iempc's state cannot be read: check it by hand (the engine "
                         "starts this command asked for)") from None
    times = doc.get("restarts", [])
    if not isinstance(times, list) or not all(type(t) in (int, float) for t in times):
        raise ip.Refused(f"no {name} gate: {LEDGER} in iempc's state is not a list of times: check it by hand")
    return [float(t) for t in times]


def note_starts(ip, n: int = 1) -> None:
    """Keeps `n` engine starts at now (a sent restart counts, answered or
    not). Never stops a start: a record that cannot be kept is a warning (and
    the next run's read of it refuses)."""
    try:
        times = (ledger_times(ip) + [wall()] * n)[-LEDGER_KEEP:]
        ip.write_json(ip.state_dir() / LEDGER, {"restarts": times})
    except (ip.StepError, OSError) as e:
        warn(f"{LEDGER} in iempc's state was not updated ({type(e).__name__}): the next run cannot count this "
             "engine start")


def refuse_over_budget(ip, facts: "Facts", planned: int, what: str, name: str = "HIL") -> None:
    why = budget_refusal(ledger_times(ip, name), wall(), facts.interval, facts.burst, planned)
    if why:
        raise ip.Refused(f"no {name} gate: the engine's restart budget: {why} ({what})")


# ---- before any change ----

@dataclass
class Facts:
    left: bool
    line: str
    burst: int
    interval: int
    hil_tx: list[int]
    dante: str
    own: str
    ret_before: dict | None   # the return's subscription before the run (gen3_arc.read_rx's row)
    ret_tx_name: str
    receiving: set[int]
    # own, ret_before and ret_tx_name are the HIL gate's ARC reads: "", None and "" for the live gate.


def dante_ip(ip, env: dict[str, str]) -> str:
    value = env.get("GEN3_DANTE_IP", "")
    if not re.fullmatch(r"[0-9]{1,3}(\.[0-9]{1,3}){3}", value):
        raise ip.Refused("gen3-gate: no GEN3_DANTE_IP in the private env (our device's IPv4 address on the Dante "
                         "network: the D5(b) loop's self-subscription goes to it over ARC)")
    return value


def read_box(box: Box, what: str, words: list[str]) -> str:
    try:
        return box.call(words)
    except box.ip.StepError as e:
        raise box.ip.Refused(f"no {box.name} gate: {what} could not be read ({e}) (nothing was changed)") from None


def preflight(box: Box, dante: str | None) -> Facts:
    """Every check before any change; `dante` None reads nothing over ARC
    (the live gate drives no D5(b) loop)."""
    ip, no = box.ip, f"no {box.name} gate"
    left = read_box(box, "the HIL job's drop-in", ["test", "-e", DROPIN, "&&", "echo", "left", "||", "echo",
                                                    "none"]).strip()
    if left not in ("left", "none"):
        raise ip.Refused(f"{no}: the look for a left HIL job answered neither left nor none (nothing was "
                         "changed)")
    shown = read_box(box, "the engine unit's drop-ins", ["systemctl", "show", "--property=DropInPaths", UNIT])
    paths = shown.strip().removeprefix("DropInPaths=").split()
    if any(p != DROPIN for p in paths):
        raise ip.Refused(f"{no}: another drop-in changes the engine's unit (systemctl show "
                         f"--property=DropInPaths {UNIT} on the box): the job's ExecStart would not be the one "
                         "it runs (nothing was changed)")
    unit = read_box(box, "the installed engine unit", ["cat", UNIT_FILE])
    try:
        line = exec_start(unit)
        burst, interval = start_limit(unit)
        rx_count, tx_count = device_counts(read_box(box, "the installed device config", ["cat", DEVICE_CONFIG]))
        hil_tx = site_hil_tx(read_box(box, "the installed site", [*AS_SERVICE, "cat", SITE]))
    except (ValueError, tomllib.TOMLDecodeError) as e:
        why = str(e) if not isinstance(e, tomllib.TOMLDecodeError) else "a file is not TOML"
        raise ip.Refused(f"{no}: {why} (nothing was changed)") from None
    why = counts_refusal(hil_tx, rx_count, tx_count)
    if why:
        raise ip.Refused(f"{no}: {why} (nothing was changed)")
    try:
        box.call([*AS_SERVICE, ENGINE_BIN, "check-site", "--site", SITE])
    except ip.StepError:   # its words may name the site's ids and channels: never quoted
        raise ip.Refused(f"{no}: iem-engine check-site refuses the installed site (its [guard] hil_tx "
                         "against the topology: an engine started with --test-signal would exit 2 and stay down) "
                         "(nothing was changed)") from None
    if not box.healthy():
        raise ip.Refused(f"{no}: the box is not healthy (iem-dante-dev health on the box says why) (nothing "
                         "was changed" + ("; a HIL job a dead run left is still there" if left == "left" else "")
                         + ")")
    before = box.device_status()
    if before is None:
        raise ip.Refused(f"{no}: the Gen 3 device's status could not be read (nothing was changed)")
    facts = Facts(left=left == "left", line=line, burst=burst, interval=interval, hil_tx=hil_tx, dante=dante or "",
                  own="", ret_before=None, ret_tx_name="", receiving=receiving(before))
    if dante is None:
        return facts
    ret = hil_tx[0]
    try:
        table = gen3_arc.read_rx(dante)
        name = gen3_arc.tx_name(dante, ret)
    except (OSError, ValueError, struct.error) as e:
        raise ip.Refused(f"{no}: our device did not answer its ARC reads ({type(e).__name__}) (nothing "
                         "was changed)") from None
    facts.own, facts.ret_before, facts.ret_tx_name = table["name"], table["rx"].get(str(ret)), name
    return facts


# ---- the job ----

@dataclass
class Job:
    box: Box
    facts: Facts
    installed: bool = False   # the drop-in's install was sent
    restarted: bool = False   # the begin's restart was sent
    burst_sent: bool = False
    loop_set: bool = False


def warn(text: str) -> None:
    print(f"iempc: WARNING: gen3-gate: {text}", file=sys.stderr, flush=True)


BY_HAND = (f"end it by hand: ssh <GEN3_HOST> sudo -n rm -f {DROPIN}, then sudo -n systemctl daemon-reload, "
           f"sudo -n systemctl reset-failed iem-engine, sudo -n systemctl restart iem-engine (or run iempc "
           "gen3-gate hil again: it ends a left job first)")


def restart_engine(job: Job, event: str) -> None:
    """`reset-failed`, then `restart` of the engine, counted in the budget first."""
    note_starts(job.box.ip)
    job.box.call([*SUDO, "systemctl", "reset-failed", "iem-engine"], event=event)
    job.box.call([*SUDO, "systemctl", "restart", "iem-engine"], event=event, timeout=RESTART_S)


def begin_job(job: Job, c: Checks) -> None:
    """The drop-in in place and the engine started with the HIL flags, then
    the waits. Changes run "finish": a flag ends the run after the call."""
    box, text = job.box, dropin_text(job.facts.line)
    box.ip.pause(box.ctx, 0)
    box.call(["mkdir", "-p", STAGE, "&&", "cat", ">", STAGED], change=True, stdin=text)
    job.installed = True
    box.call([*SUDO, "install", "-D", "-m", "0644", STAGED, DROPIN], change=True)
    if box.call(["cat", DROPIN]) != text:
        raise box.ip.StepError("gen3-gate: the HIL job's drop-in did not read back as written")
    box.call([*SUDO, "systemctl", "daemon-reload"], change=True)
    job.restarted = True
    c.starts += 1
    restart_engine(job, box.ctx.watch(abandon=False))
    c.health = box.wait(box.healthy, HEALTH_WAIT_S)
    c.rx_back = box.wait_receiving(job.facts.receiving, RX_WAIT_S)
    peaks = hil_peaks(box.engine_status())
    c.hil_on = peaks is not None and sorted(peaks) == sorted(job.facts.hil_tx)


def end_job(job: Job, c: Checks | None, wait: bool) -> bool:
    """The drop-in removed and, once the begin restarted the engine, the
    engine started without it; with `wait`, healthy, the RX flows back and
    its spare outputs gone. Every change "ignore" (an end always completes);
    a flag cuts only the waits. Never raises a StepError: False and a
    warning instead."""
    box, restarted = job.box, job.restarted
    if not (job.installed or job.restarted):
        return True   # nothing of the job reached the box, or it ended already
    try:
        box.call([*SUDO, "rm", "-f", DROPIN], event="ignore")
        box.call([*SUDO, "systemctl", "daemon-reload"], event="ignore")
        if restarted:
            if c is not None:
                c.starts += 1
            restart_engine(job, "ignore")
    except box.ip.StepError as e:
        warn(f"the HIL job did not end ({e}): the engine may still run with its HIL test flag; {BY_HAND}")
        return False
    job.installed = job.restarted = False   # ended: another end (a flag in these waits) changes nothing
    if not wait:
        return True
    healthy = box.wait(box.healthy, HEALTH_WAIT_S)
    back = box.wait_receiving(job.facts.receiving, RX_WAIT_S)
    peaks = hil_peaks(box.engine_status()) if restarted else {}
    if c is not None:
        c.hil_off = peaks == {}
    if not healthy:
        warn("the engine is not healthy after the HIL job's end (iem-dante-dev health on the box says why)")
    return healthy and back


def end_left(box: Box, facts: Facts) -> None:
    """A job a dead run left: removed, and the engine started again only
    while it still runs with its spare outputs (the budget counts that)."""
    status = box.engine_status()
    in_hil = bool(hil_peaks(status))
    if in_hil:
        refuse_over_budget(box.ip, facts, 1, "a HIL job a dead run left is still there: nothing was changed",
                           box.name)
    job = Job(box, facts, installed=True, restarted=in_hil)
    print("iempc: gen3-gate: a HIL job a dead run left is ended first", file=sys.stderr, flush=True)
    if not end_job(job, None, wait=True):
        raise box.ip.StepError(f"gen3-gate: the HIL job a dead run left did not end cleanly: no {box.name} gate "
                               "this time")


# ---- the D5(b) loop over ARC (our own device only) ----

def set_loop(job: Job, c: Checks) -> None:
    """Our first spare output's return subscribed to that output of our own
    device, unless it already is."""
    f = job.facts
    before = f.ret_before
    if before and (before.get("tx_device") or "").casefold() == f.own.casefold() \
            and (before.get("tx_channel") or "").casefold() == f.ret_tx_name.casefold():
        c.loop = "kept"
        return
    job.box.ip.pause(job.box.ctx, 0)
    job.loop_set = True
    try:
        word = gen3_arc.status(gen3_arc.send(f.dante, gen3_arc.subscription(f.hil_tx[0], f.ret_tx_name, f.own,
                                                                             0x4801)), 0x4801)
    except (OSError, ValueError, struct.error) as e:
        warn(f"the D5(b) loop's self-subscription was not answered ({type(e).__name__})")
        c.loop = "refused"
        return
    if word != gen3_arc.DONE:
        warn(f"our device answered the D5(b) loop's self-subscription with {word:#06x}")
        c.loop = "refused"
        return
    c.loop = "set"
    if not job.box.wait_receiving({f.hil_tx[0]}, LOOP_WAIT_S):
        warn("the D5(b) loop's own flow did not read Receiving in time")


def restore_loop(job: Job, c: Checks | None) -> None:
    """The return's subscription put back as it was (or none) and read back;
    never raises."""
    if not job.loop_set:
        return
    f = job.facts
    before = f.ret_before
    try:
        packet = gen3_arc.subscription(f.hil_tx[0], before.get("tx_channel") if before else None,
                                       before.get("tx_device") if before else None, 0x4802)
        word = gen3_arc.status(gen3_arc.send(f.dante, packet), 0x4802)
        now = gen3_arc.read_rx(f.dante)["rx"].get(str(f.hil_tx[0]))
    except (OSError, ValueError, struct.error) as e:
        word, now = None, {"unread": type(e).__name__}
    same = (now is None and before is None) or (
        now is not None and before is not None and now.get("tx_channel") == before.get("tx_channel")
        and (now.get("tx_device") or "").casefold() == (before.get("tx_device") or "").casefold())
    if word == gen3_arc.DONE and same:
        job.loop_set = False
        return
    if c is not None:
        c.loop_restored = False
    warn("the spare return's subscription did not read back as before the run: put our device's RX table back "
         "(iempc gen3-mirror, while 3.x serves)")


# ---- the burst ----

def burst(job: Job) -> Burst:
    """One test-signal burst: sent once the flag is checked, then `supervise
    status` until the spare outputs carried it and a later status shows them
    silent (BURST_LIMIT_S)."""
    box = job.box
    box.ip.pause(box.ctx, 0)
    job.burst_sent = True
    try:
        box.call([*SUPERVISE, "test-signal", BURST_INPUT, str(BURST_DBFS), str(BURST_SECONDS)], change=True)
    except box.ip.StepError as e:
        job.burst_sent = False
        warn(f"the engine took no test signal ({e})")
        return Burst(refused=True)
    b = Burst()
    deadline = time.monotonic() + BURST_LIMIT_S
    while True:
        status = box.engine_status()
        peaks = hil_peaks(status)
        if peaks:
            if any(p > 0 for p in peaks.values()):
                b.during.append(peaks)
            elif b.during:
                b.after = peaks
                value = status.get("loopback_samples")
                b.loopback = value if type(value) is int and value > 0 else 0
                job.burst_sent = False
                return b
        if time.monotonic() > deadline:
            return b
        box.ip.pause(box.ctx, box.ip.POLL_S)


def stop_burst(job: Job, seconds: int = BURST_SECONDS) -> None:
    """`supervise stop-test-signal` of a burst (one of `seconds`) that may
    still sound; never raises."""
    if not job.burst_sent:
        return
    try:
        job.box.call([*SUPERVISE, "stop-test-signal"], event="ignore")
        job.burst_sent = False
    except job.box.ip.StepError as e:
        warn(f"the burst's stop was not answered ({e}): it ends by itself within {seconds} s")


# ---- the other checks ----

def read_versions(box: Box, c: Checks) -> None:
    """The engine's, the device's and the server's builds on the box."""
    answers = []
    for words, stdin in (([ENGINE_BIN, "--version"], ""), ([g3.DANTE_BIN, "--version"], ""),
                         (["python3", "-I", "-"], VERSION_SCRIPT)):
        try:
            answers.append(box.call(words, stdin=stdin))
        except box.ip.StepError as e:
            warn(f"a build could not be read ({e})")
            answers.append("")
    c.sha, c.versions = versions_judged(*answers)


def alarm_count(box: Box) -> int | None:
    """How many devices an alarm would reach, the engineer's push
    subscriptions (`iem-server notify --count alarm` as the service user,
    never a notice); None (a warning) when it cannot be read."""
    try:
        out = box.call([*AS_SERVICE, "env", f"IEMMIXER_CONFIG={SITE}", SERVER_BIN, "notify", "--count", "alarm"])
    except box.ip.StepError as e:
        warn(f"the alarm path's count could not be read ({e})")
        return None
    return int(out.strip()) if COUNT.fullmatch(out.strip()) else None


def read_alarm(box: Box, c: Checks) -> None:
    c.alarm_devices = alarm_count(box)
    try:
        c.alarm_armed = box.call(["systemctl", "is-enabled", UNIT, "||", "true"]).strip() == "enabled"
    except box.ip.StepError:
        c.alarm_armed = False


def read_restarts(box: Box, c: Checks) -> None:
    """The engine's automatic restarts since the begin's reset-failed, kept in the budget."""
    try:
        out = box.call(["systemctl", "show", "--property=NRestarts", UNIT]).strip()
    except box.ip.StepError as e:
        warn(f"the engine's restarts could not be read ({e})")
        return
    value = out.removeprefix("NRestarts=")
    c.unit_restarts = int(value) if value.isdigit() else 0
    if c.unit_restarts:
        note_starts(box.ip, c.unit_restarts)


def in_job(job: Job, c: Checks) -> None:
    begin_job(job, c)
    read_versions(job.box, c)
    if c.hil_on:
        set_loop(job, c)
        c.burst = burst(job)
        stop_burst(job)   # one the limit cut short
        restore_loop(job, c)
    read_alarm(job.box, c)
    read_restarts(job.box, c)


# ---- the status ----

def post(ip, sha: str, passed: bool, description: str, context: str) -> bool:
    """The commit status `context` on the box's build, success or failure,
    its description cut to 140 characters (a user token posts a status);
    False (a warning) when gh refused it."""
    state = "success" if passed else "failure"
    try:
        ip.gh(["api", "-X", "POST", f"repos/{ip.REPO}/statuses/{sha}", "-f", f"state={state}",
               "-f", f"context={context}", "-f", f"description={description[:140]}"])
    except ip.StepError as e:
        warn(f"the status {context} was not posted ({e})")
        return False
    return True


def post_status(ip, sha: str, doc: dict) -> bool:
    """`hil/gen3` on the box's build; False (a warning) when gh refused it."""
    passed = doc["pass"] == 1
    return post(ip, sha, passed, DESC_PASS if passed else DESC_FAIL + ", ".join(doc["failed"]), CONTEXT)


# ---- the command ----

def run(ctx, ip) -> int:
    """`iempc gen3-gate hil|live|soak` (dev time, locked, while 3.x serves)."""
    if ctx.args.gate != "soak" and getattr(ctx.args, "minutes", None) is not None:
        raise ip.Refused("gen3-gate: --minutes is the soak gate's only (nothing was called)")
    if ctx.args.gate == "live":
        import iempc_gen3_live   # it imports this module: imported here, never at the top
        return iempc_gen3_live.run(ctx, ip)
    if ctx.args.gate == "soak":
        import iempc_gen3_soak   # it imports this module: imported here, never at the top
        return iempc_gen3_soak.run(ctx, ip)
    if ctx.args.gate != "hil":
        raise ip.Refused(f"gen3-gate: {ctx.args.gate!r} is not a gate of this build (hil, live, soak)")
    host = g3.gen3_host(ip, ctx.env, WORDS)
    dante = dante_ip(ip, ctx.env)
    box = Box(ip, ctx, host)
    facts = preflight(box, dante)
    if facts.left:
        end_left(box, facts)
    refuse_over_budget(ip, facts, 2, "the job's begin and end: nothing was changed"
                       + ("; the HIL job a dead run left was ended" if facts.left else ""))
    job, c = Job(box, facts), Checks(rx_channels=len(facts.receiving))
    try:
        in_job(job, c)
    except ip.EventNow:
        stop_burst(job)
        restore_loop(job, None)
        end_job(job, None, wait=False)
        raise
    except BaseException:
        stop_burst(job)
        restore_loop(job, None)
        end_job(job, None, wait=True)
        raise
    c.end_ok = end_job(job, c, wait=True)
    doc = verdict(c, facts.hil_tx, facts.left)
    ip.emit({"gen3_gate": {"hil": doc}})
    if c.sha is None:
        warn(f"the box's engine names no CI build: no {CONTEXT} status was posted")
        return 1
    posted = post_status(ip, c.sha, doc)
    return 0 if doc["pass"] == 1 and posted else 1
