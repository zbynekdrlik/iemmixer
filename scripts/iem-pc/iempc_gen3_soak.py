"""`iempc gen3-gate soak` (#72 phase H, plan
docs/superpowers/plans/2026-10-28-gen3-phase-h-box-gates.md Task 7): the soak
on the Gen 3 box that serves the band. The PC's soak (the ops soak.yml,
.claude/rules/soak.md) driven from the dev box over ssh: `iem-soakclient` on
the box, beside the dev box's reads of the box's own counters, its verdict
posted as the commit status `soak/gen3` on the box's build. Its call site is
iempc_gen3_gate.run.

Dev time, locked, a box command, only while 3.x serves (iempc's Spec and
WHILE_3X_SERVES, as `gen3-gate hil` and `live`): the dev box's flag is the
event gate. `--minutes` is one of MINUTES (10, 30, 60, 120; default 120: the
owner sizes tests to an event of at most 2 h), each one fixed line of the
sudo rule. No HIL job and no engine restart: the soak runs against the
engine as it serves the band, so it spends no restart budget (a HIL job a
dead run left is ended first, the one restart, before any count is read).

**The client reads only** (.claude/rules/soak.md: its only frames are the
listen socket's ListenStart and ListenStop and each socket's Close, never a
mixer command; nothing in it ends a process), so it may run on the box that
serves the band. Its known effects while it runs (soak.md): the member's mix
keeps a solo past its grace, and the one member listen tap is the client's,
so an engineer's Listen to another member's mix answers no_source;
SOAK_MEMBER names a member who is not active.

**Before any call** (Refused): `--minutes` one of MINUTES; GEN3_HOST;
SOAK_MEMBER of the private env an id (the member whose mix the client
listens to; never printed).

**Before any change** (Refused, nothing changed): a HIL job a dead run left
is ended first (iempc_gen3_gate.preflight without its ARC reads, then
end_left, with its budget); the box healthy (`iem-dante-dev health`); the
box's build (`iem-engine --version`, a CI build) named by the box's own
server (`/api/version` read on the box, its git_hash a prefix of 7 hex or
more); a soak unit a dead run left is ended (`systemctl stop`, never
force-ended, or `reset-failed`) and must be gone; the baseline: the device's
status with the engine's pump, the stack's units.

**The run.** The build goes into a 0644 env file in /run (ENV_FILE, staged
over ssh's stdin and installed with `sudo -n install`): the sudo rule's lines
are exact and the build is not, so systemd expands `$IEM_SOAK_BUILD` from
the unit's EnvironmentFile into `--expect-build` (the word goes quoted to
the box's shell). Any summary left is removed, then `systemd-run` starts the
transient unit `iem-soak` as the service user (`--uid`/`--gid iemmixer`,
`--remain-after-exit` so its result stays readable, RuntimeMaxSec the run
plus RUN_MARGIN_S, a STOP_S stop with SendSIGKILL=no: I8):
`iem-soakclient --direct` at its default base `http://127.0.0.1` (the box's
own server on port 80; the band site's lan_url is not the box), `--member`,
`--seconds` the run, `--out` the service user's mint folder,
`--expect-build`, and `--jwt-secret-file` the box server's own key (the
client signs its own engineer token: no PIN, no login). Beside it the dev
box reads `iem-dante-dev ctl status` every STATUS_EVERY_S, and every
HEALTH_EVERY_S `iem-dante-dev health`, the stack's units (`systemctl show`
of iem-engine's and iem-server's NRestarts and ActiveEnterTimestampMonotonic)
and the soak unit's state, until the client ended or the dev box's own
bound passed (the run plus WAIT_MARGIN_S: `not_finished`, the unit
stopped). Then the summary (as the service user), the unit stopped (or
reset-failed), the env file and the summary removed on the box.

**The verdict**, every check run, its words in FAILURES' order:
- not_finished: the client still ran at the bound; client: its unit's
  result not `success` or its exit not 0;
- harness: soak_verdict.box_check over the client's summary (the PC
  verdict's checks 9, 11 and 12 at its bounds: complete, the run's seconds,
  no reconnect, 99 % of the expected listen frames; then a meter frame). The
  listen gaps are counted, not judged (owner on #72, 2026-10-29: the browser
  listen is low priority, #88);
- polls: no status read or no health read during the run, or more than
  POLL_HOLE_S between two good reads of either (`failed_reads` counted);
- health: a health read not healthy (its reasons, known words only);
- restarted: iem-engine's or iem-server's NRestarts rose or its
  ActiveEnterTimestampMonotonic moved (a restart by hand too), or a counter
  of the device's status went back (`counts_back`);
- pump_stalled: a read without the engine's pump on the media clock, or its
  blocks not rising between two reads;
- pump_late, pump_missed, pump_overruns, pump_rebootstraps: the pump's
  counters rose (decision on #72, 2026-10-28: overruns fail it too);
- clock_not_ready: a read whose clock was not ready;
- holdover: a read in holdover, or the clock's holdovers rose;
- re_anchor: the clock's re_anchors rose or its epoch changed;
- rx_late: an RX flow's late packets rose;
- tx_holes: a TX flow's holes rose;
- rx_holes: an RX channel's ring holes rose (owner, 2026-10-31 on #72: zero
  loss, a stage device that stops fails it too);
- rx_off: a read with a subscribed RX channel that does not receive.
Channels in GEN3_OPTIONAL_RX fail neither (gate.optional_rx).
Information only: the longest block since the engine started (`max_process_us`), the worst RX flow lateness of a second in
samples (`lateness_max`). A rise is summed read to read; a count that went
back counts its new value from 0.

One line `{"gen3_gate": {"soak": {...}}}`, numbers and fixed words only (no
token, URL, host, member id or build), then the status `soak/gen3` on the
box's build (iempc_gen3_gate.post), success only when nothing failed. Exit 0
only when it passed and the status was posted.

**A flag** ("ide event"): no read after it; the unit stopped (`systemctl
stop`: the client's SIGTERM, never force-ended), the env file and the
summary removed, the line with conclusion `cancelled` and never a status;
then the event path (EventNow: iempc main).

**Records** in iempc's state, `gen3-soak/<run>/`: polls.jsonl (each good
read's numbers) and soakclient.json (the client's summary); no member,
token, host or build in them.

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import json
import math
import os
import re
import shlex
import shutil
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path

import iempc_gen3 as g3
import iempc_gen3_gate as gate
import soak_verdict

NAME = "soak"   # its refusals: "no soak gate"
CONTEXT = "soak/gen3"
MINUTES = (10, 30, 60, 120)   # each one exact line of scripts/gen3/sudo-lines.txt
DEFAULT_MINUTES = 120   # the owner's event: 2 h at most
ID = re.compile(r"[A-Za-z0-9_-]{1,64}")
UNIT = "iem-soak"
UNIT_SERVICE = f"{UNIT}.service"
ENV_KEY = "IEM_SOAK_BUILD"
ENV_STAGED = f"{gate.STAGE}/iem-soak.env"
ENV_FILE = "/run/iem-soak.env"
SUMMARY = f"{g3.BOX_TOKENS}/soakclient.json"   # the service user's own folder (0700)
RUN_MARGIN_S = 300   # RuntimeMaxSec past the run: the build check, the opens, the closes
STOP_S = 10   # the unit's stop bound (SendSIGKILL=no)
WAIT_MARGIN_S = RUN_MARGIN_S + 60   # the dev box's own bound past the run
STATUS_EVERY_S = 1.0
HEALTH_EVERY_S = 10.0
POLL_HOLE_S = 60.0
RECORDS = "gen3-soak"
UNIT_PROPS = "LoadState,ActiveState,SubState,Result,ExecMainStatus"
STACK = ("iem-engine.service", "iem-server.service")
STACK_PROPS = "Id,NRestarts,ActiveEnterTimestampMonotonic"
RUNNING = ("active", "activating", "deactivating", "reloading")
CLOCK_KEYS = ("ready", "epoch", "re_anchors", "holdovers", "holdover")
ENGINE_KEYS = ("media", "blocks", "late", "missed", "overruns", "rebootstraps", "max_process_us")
PUMP_COUNTS = ("late", "missed", "overruns", "rebootstraps")
HEALTH_STATES = ("healthy", "degraded", "failed", "stopped")
HEALTH_REASONS = frozenset({"unit_failed", "unit_down", "restarted", "no_status", "clock_not_ready", "holdover",
                            "pump_free", "pump_stalled", "pump_rebootstrap", "server_down", "build_mismatch"})
RESULTS = frozenset({"success", "exit-code", "signal", "core-dump", "timeout", "watchdog", "start-limit-hit",
                     "resources", "protocol"})   # systemd's Result words; another reads `other`
# The failures a verdict names, in its order.
FAILURES = ("not_finished", "client", "harness", "polls", "health", "restarted", "pump_stalled", "pump_late",
            "pump_missed", "pump_overruns", "pump_rebootstraps", "clock_not_ready", "holdover", "re_anchor",
            "rx_late", "tx_holes", "rx_holes", "rx_off")
DESC_PASS = "the Gen 3 box's soak passed (iempc gen3-gate soak, {minutes} min)"
DESC_FAIL = "the Gen 3 box's soak failed: "


# ---- pure ----

def client_words(minutes: int, member: str) -> list[str]:
    """The soak client's start after `sudo -n`, as the sudo rule names it
    (`{member}` and the run's length; the build is systemd's variable)."""
    seconds = minutes * 60
    return ["systemd-run", "--quiet", f"--unit={UNIT}", "--remain-after-exit", "--uid=iemmixer", "--gid=iemmixer",
            f"--property=EnvironmentFile={ENV_FILE}", f"--property=RuntimeMaxSec={seconds + RUN_MARGIN_S}",
            f"--property=TimeoutStopSec={STOP_S}", "--property=SendSIGKILL=no", g3.SOAKCLIENT, "--direct",
            "--member", member, "--seconds", str(seconds), "--out", SUMMARY, "--expect-build", f"${ENV_KEY}",
            "--jwt-secret-file", g3.SIGNER_PATH]


def remote(words: list[str]) -> list[str]:
    """The words as ssh hands them to the box's shell: a `$` word quoted, so
    sudo gets it as written and systemd expands it."""
    return [shlex.quote(w) if "$" in w else w for w in words]


def wait_limit(seconds: int) -> float:
    """The dev box's bound on the client's run (tests replace it)."""
    return seconds + WAIT_MARGIN_S


def _count(doc, key: str) -> int | None:
    value = doc.get(key) if isinstance(doc, dict) else None
    return value if type(value) is int and value >= 0 else None


def _by(rows: list[dict], key: str, value: str) -> dict[str, int]:
    out = {}
    for row in rows:
        n, v = _count(row, key), _count(row, value)
        if n is None or v is None:
            raise ValueError(value)
        out[str(n)] = v
    return out


def sample(status, optional: frozenset[int] = frozenset()) -> dict | None:
    """The numbers the soak judges from one `ctl status` (pure); None when it
    is not a status. `engine` is None without the pump's object."""
    clock = status.get("clock") if isinstance(status, dict) else None
    s = {k: _count(clock, k) for k in CLOCK_KEYS}
    if any(v is None for v in s.values()):
        return None
    try:
        rx = g3.rows(status, "rx")
        s["rx_late"] = _by(rx, "flow", "late")
        s["tx_holes"] = _by(g3.rows(status, "tx"), "flow", "holes")
        s["rx_holes"] = _by([c for c in g3.rows(status, "channels") if c.get("rx") not in optional], "rx", "holes")
        s["rx_off"] = len(gate.not_receiving(status, optional))
        lateness = [_count(r, "lateness") for r in rx]
    except ValueError:
        return None
    if any(v is None for v in lateness):
        return None
    s["lateness"] = max(lateness, default=0)
    engine = {k: _count(status.get("engine"), k) for k in ENGINE_KEYS}
    s["engine"] = None if any(v is None for v in engine.values()) else engine
    return {k: s[k] for k in (*CLOCK_KEYS, "engine", "rx_late", "tx_holes", "rx_holes", "rx_off", "lateness")}


def growth(values: list[dict]) -> tuple[int, int]:
    """(the rise of cumulative counters read to read, summed over their keys;
    how many went back). A key first read later counts from 0; one that went
    back counts its new value from 0."""
    total = back = 0
    prev = values[0] if values else {}
    for cur in values[1:]:
        for key, value in cur.items():
            before = prev.get(key, 0)
            if value >= before:
                total += value - before
            else:
                total += value
                back += 1
        prev = {**prev, **cur}
    return total, back


def hole(times: list[float], start: float, end: float) -> float:
    """The longest wait between the run's start, its good reads and its end."""
    edges = [start, *times, end]
    return max(b - a for a, b in zip(edges, edges[1:]))


@dataclass(frozen=True)
class Unit:
    """A unit's state as `systemctl show` reads it."""
    load: str
    active: str
    sub: str
    result: str
    status: int | None

    @property
    def loaded(self) -> bool:
        return self.load == "loaded"

    @property
    def running(self) -> bool:
        """Its process may still run (`exited`: it ended, the unit remains)."""
        return self.loaded and self.active in RUNNING and self.sub != "exited"


def _keys(text: str) -> dict[str, str]:
    out = {}
    for line in text.splitlines():
        key, sep, value = line.partition("=")
        if sep:
            out[key.strip()] = value.strip()
    return out


def unit_fields(text: str) -> Unit:
    keys = _keys(text)
    status = keys.get("ExecMainStatus", "")
    return Unit(keys.get("LoadState", ""), keys.get("ActiveState", ""), keys.get("SubState", ""),
                keys.get("Result", ""), int(status) if status.isdigit() else None)


def stack_units(text: str) -> dict[str, tuple[int, str]]:
    """iem-engine's and iem-server's (NRestarts, ActiveEnterTimestampMonotonic)
    from `systemctl show`'s blocks (a blank line apart, keys in any order)."""
    out = {}
    for block in re.split(r"\n\s*\n", text):
        keys = _keys(block)
        uid, n = keys.get("Id"), keys.get("NRestarts", "")
        if uid in STACK and n.isdigit():
            out[uid] = (int(n), keys.get("ActiveEnterTimestampMonotonic", ""))
    return out


# ---- what the run read ----

@dataclass
class Watch:
    minutes: int
    left_job: bool
    left_soak: bool
    records: Path
    samples: list[dict] = field(default_factory=list)   # the baseline first
    status_times: list[float] = field(default_factory=list)
    health_times: list[float] = field(default_factory=list)
    failed_reads: int = 0
    not_healthy: int = 0
    reasons: set[str] = field(default_factory=set)
    units_base: dict = field(default_factory=dict)
    units_last: dict = field(default_factory=dict)
    started: float | None = None   # monotonic: the watch's start and end
    ended: float | None = None
    unit: Unit | None = None   # the soak unit's last state read
    finished: bool = False
    summary: object = None

    def record(self, kind: str, doc: dict) -> None:
        t = round(time.monotonic() - self.started, 1) if self.started is not None else 0.0
        with open(self.records / "polls.jsonl", "a", encoding="utf-8") as f:
            f.write(json.dumps({"t": t, "kind": kind, **doc}) + "\n")


def judged(samples: list[dict]) -> dict:
    """The pump's, the clock's and the flows' numbers over the reads (pure)."""
    engines = [s["engine"] for s in samples if s["engine"] is not None]
    out: dict = {}
    for key in PUMP_COUNTS:
        out[f"pump_{key}"], _ = growth([{key: e[key]} for e in engines])
    blocks = [e["blocks"] for e in engines]
    steps = list(zip(blocks, blocks[1:]))
    out["pump_stalled"] = (sum(1 for s in samples if s["engine"] is None or s["engine"]["media"] != 1)
                           + sum(1 for a, b in steps if b == a))
    out["max_process_us"] = engines[-1]["max_process_us"] if engines else None
    out["clock_not_ready"] = sum(1 for s in samples if s["ready"] != 1)
    out["holdovers"], h_back = growth([{"n": s["holdovers"]} for s in samples])
    out["in_holdover"] = sum(1 for s in samples if s["holdover"])
    anchors, a_back = growth([{"n": s["re_anchors"]} for s in samples])
    epochs = [s["epoch"] for s in samples]
    out["re_anchors"] = anchors + sum(1 for a, b in zip(epochs, epochs[1:]) if a != b)
    back = sum(1 for a, b in steps if b < a) + h_back + a_back
    for key in ("rx_late", "tx_holes", "rx_holes"):
        out[key], b = growth([s[key] for s in samples])
        back += b
    out["rx_off"] = sum(1 for s in samples if s["rx_off"])
    out["lateness_max"] = max((s["lateness"] for s in samples), default=None)
    out["counts_back"] = back
    return out


def unit_restarts(base: dict, last: dict) -> tuple[int, int]:
    """(the stack's units' restarts since the baseline, iem-engine's alone):
    each unit's NRestarts rise, at least 1 when its active-enter time moved."""
    total = engine = 0
    for uid, (n0, enter0) in base.items():
        n, enter = last.get(uid, (n0, enter0))
        k = max(n - n0, 1 if enter != enter0 else 0, 0)
        total += k
        if uid == STACK[0]:
            engine = k
    return total, engine


def holes(w: Watch) -> tuple[int, int]:
    if w.started is None:
        return 0, 0
    end = w.ended if w.ended is not None else time.monotonic()
    return (math.ceil(hole(w.status_times, w.started, end)), math.ceil(hole(w.health_times, w.started, end)))


def line(w: Watch, conclusion: str | None = None) -> dict:
    """The gate's line (numbers and fixed words); `conclusion` cancelled for
    a run the flag cut (no failure named, no harness judged)."""
    cut = conclusion == "cancelled"
    if cut:
        problems, numbers = [], {}
    else:
        problems, numbers = soak_verdict.box_check(w.summary, w.minutes * 60)
    j = judged(w.samples)
    restarts, _ = unit_restarts(w.units_base, w.units_last)
    status_hole, health_hole = holes(w)
    failed: list[str] = []
    if not cut:
        if not w.finished:
            failed.append("not_finished")
        elif w.unit is None or w.unit.result != "success" or w.unit.status != 0:
            failed.append("client")
        if problems:
            failed.append("harness")
        if not w.status_times or not w.health_times or max(status_hole, health_hole) > POLL_HOLE_S:
            failed.append("polls")
        if w.not_healthy:
            failed.append("health")
        if restarts or j["counts_back"]:
            failed.append("restarted")
        for word, value in (("pump_stalled", j["pump_stalled"]), ("pump_late", j["pump_late"]),
                            ("pump_missed", j["pump_missed"]), ("pump_overruns", j["pump_overruns"]),
                            ("pump_rebootstraps", j["pump_rebootstraps"]), ("clock_not_ready", j["clock_not_ready"]),
                            ("holdover", j["holdovers"] + j["in_holdover"]), ("re_anchor", j["re_anchors"]),
                            ("rx_late", j["rx_late"]), ("tx_holes", j["tx_holes"]),
                            ("rx_holes", j["rx_holes"]), ("rx_off", j["rx_off"])):
            if value:
                failed.append(word)
    ended = w.finished and w.unit is not None
    return {
        "pass": int(not cut and not failed), "conclusion": conclusion or ("failure" if failed else "success"),
        "minutes": w.minutes,
        "client_exit": w.unit.status if ended else None,
        "client_result": (w.unit.result if w.unit.result in RESULTS else "other") if ended else None,
        "harness": {"problems": problems, **numbers},
        "reads": len(w.samples), "failed_reads": w.failed_reads, "poll_hole_s": status_hole,
        "health_reads": len(w.health_times), "not_healthy": w.not_healthy, "health_reasons": sorted(w.reasons),
        "health_hole_s": health_hole,
        **j, "unit_restarts": restarts, "left_job": int(w.left_job), "left_soak": int(w.left_soak),
        "failed": failed,
    }


# ---- before any change ----

def minutes_of(ip, args) -> int:
    minutes = getattr(args, "minutes", None)
    minutes = DEFAULT_MINUTES if minutes is None else minutes
    if minutes not in MINUTES:
        raise ip.Refused("gen3-gate: --minutes must be 10, 30, 60 or 120 (the sudo rule's fixed run lengths)")
    return minutes


def member_of(ip, env: dict[str, str]) -> str:
    member = env.get("SOAK_MEMBER", "")
    if not ID.fullmatch(member):
        raise ip.Refused("gen3-gate: SOAK_MEMBER of the private env is not an id (1 to 64 of A-Z a-z 0-9 _ -): the "
                         "member whose mix the soak client listens to, one not active")
    return member


def end_left_job(box: gate.Box) -> bool:
    """A HIL job a dead run left: ended first (its engine's restart in the budget)."""
    left = gate.read_box(box, "the HIL job's drop-in",
                         ["test", "-e", gate.DROPIN, "&&", "echo", "left", "||", "echo", "none"]).strip()
    if left == "none":
        return False
    if left != "left":
        raise box.ip.Refused(f"no {NAME} gate: the look for a left HIL job answered neither left nor none (nothing "
                             "was changed)")
    gate.end_left(box, gate.preflight(box, None))
    return True


def box_build(box: gate.Box) -> str:
    """The box's build: its engine's CI build, named by its own server."""
    ip, no = box.ip, f"no {NAME} gate"
    try:
        sha = g3.box_build(ip, box.ctx, box.host)
    except ip.StepError as e:
        raise ip.Refused(f"{no}: {gate.box_error(ip, e)} (nothing was changed)") from None
    try:
        served = gate.last_line(box.call(["python3", "-I", "-"], stdin=gate.VERSION_SCRIPT))
    except ip.StepError as e:
        raise ip.Refused(f"{no}: the box's server did not answer /api/version ({e}) (nothing was changed)") from None
    if not (gate.HASH.fullmatch(served) and sha.startswith(served)):
        raise ip.Refused(f"{no}: the box's server does not name the engine's build in /api/version (nothing was "
                         "changed)")
    return sha


def unit_state(box: gate.Box, event: str | None = None) -> Unit:
    return unit_fields(box.call(["systemctl", "show", f"--property={UNIT_PROPS}", UNIT_SERVICE], event=event))


def end_unit(box: gate.Box, state: Unit, event: str) -> None:
    """A loaded soak unit ended: a failed one reset, any other stopped (its
    client's SIGTERM, never force-ended; an exited one only unloads)."""
    verb = "reset-failed" if state.active == "failed" else "stop"
    box.call([*gate.SUDO, "systemctl", verb, UNIT], event=event)


def clear_unit(box: gate.Box) -> bool:
    """A soak unit a dead run left, ended first; Refused when it stays."""
    ip, no = box.ip, f"no {NAME} gate"
    try:
        state = unit_state(box)
    except ip.StepError as e:
        raise ip.Refused(f"{no}: the soak's unit could not be read ({e}) (nothing was changed)") from None
    if not state.loaded:
        return False
    print(f"iempc: gen3-gate: a soak client a dead run left is ended first (systemctl stop {UNIT}, never "
          "force-ended)", file=sys.stderr, flush=True)
    change = box.ctx.watch(abandon=False)
    end_unit(box, state, change)
    state = unit_state(box)
    if state.loaded and state.active == "failed":
        end_unit(box, state, change)
        state = unit_state(box)
    if state.loaded:
        raise ip.Refused(f"{no}: the soak's unit a dead run left is still there (systemctl status {UNIT} on the box; "
                         f"sudo -n systemctl stop {UNIT}, then sudo -n systemctl reset-failed {UNIT})")
    return True


def read_stack(box: gate.Box) -> dict | None:
    try:
        units = stack_units(box.call(["systemctl", "show", f"--property={STACK_PROPS}", *STACK]))
    except box.ip.StepError:
        return None
    return units if set(units) == set(STACK) else None


def baseline(box: gate.Box, w: Watch) -> None:
    """The counts the run's rises start from: the device's status with the
    pump, the stack's units."""
    no = f"no {NAME} gate"
    status = box.device_status()
    s = sample(status, box.optional) if status is not None else None
    if s is None:
        raise box.ip.Refused(f"{no}: the Gen 3 device's status could not be read (nothing was changed)")
    if s["engine"] is None:
        raise box.ip.Refused(f"{no}: the Gen 3 device's status shows no engine (nothing was changed)")
    units = read_stack(box)
    if units is None:
        raise box.ip.Refused(f"{no}: the stack's units could not be read (systemctl show of {', '.join(STACK)}) "
                             "(nothing was changed)")
    w.samples.append(s)
    w.record("status", s)
    w.units_base = w.units_last = units


def records_folder(ip, number: int) -> Path:
    """The run's records in iempc's state, owner-only; one of the same
    number (two runs in one second: only a test makes them) is replaced."""
    folder = ip.state_dir() / RECORDS / str(number)
    folder.parent.mkdir(mode=0o700, exist_ok=True)
    if folder.exists():
        shutil.rmtree(folder)
    folder.mkdir(mode=0o700)
    os.chmod(folder, 0o700)
    return folder


# ---- the run ----

@dataclass
class Job:
    box: gate.Box
    env_installed: bool = False   # the env file's install was sent
    summary_cleared: bool = False   # the summary's removal was sent: one may exist from here on
    started: bool = False   # the client's start was sent


def start(job: Job, sha: str, member: str, minutes: int) -> None:
    """The build into the unit's env file, any summary left removed, the
    client started. Changes run "finish": a flag ends the run after the call."""
    box = job.box
    box.ip.pause(box.ctx, 0)
    box.call(["mkdir", "-p", gate.STAGE, "&&", "cat", ">", ENV_STAGED], change=True, stdin=f"{ENV_KEY}={sha}\n")
    job.env_installed = True
    box.call([*gate.SUDO, "install", "-D", "-m", "0644", ENV_STAGED, ENV_FILE], change=True)
    job.summary_cleared = True
    box.call([*g3.AS_SERVICE, "rm", "-f", SUMMARY], change=True)
    job.started = True
    box.call([*gate.SUDO, *remote(client_words(minutes, member))], change=True)


def read_sample(box: gate.Box, w: Watch) -> None:
    status = box.device_status()
    s = sample(status, box.optional) if status is not None else None
    if s is None:
        w.failed_reads += 1
        return
    w.samples.append(s)
    w.status_times.append(time.monotonic())
    w.record("status", s)


def read_health(box: gate.Box, w: Watch) -> None:
    """`health || true`: its JSON line whatever its exit; a read that gives
    none is a failed read."""
    try:
        doc = box.last_json(box.call([g3.DANTE_BIN, "health", "||", "true"]))
    except box.ip.StepError:
        w.failed_reads += 1
        return
    h = doc.get("health") if isinstance(doc.get("health"), dict) else {}
    state, reasons = h.get("state"), h.get("reasons")
    if state not in HEALTH_STATES:
        w.failed_reads += 1
        return
    w.health_times.append(time.monotonic())
    known = sorted(r for r in reasons if r in HEALTH_REASONS) if isinstance(reasons, list) else []
    if state != "healthy":
        w.not_healthy += 1
        w.reasons.update(known)
    w.record("health", {"state": state, "reasons": known})


def read_units(box: gate.Box, w: Watch) -> None:
    units = read_stack(box)
    if units is None:
        w.failed_reads += 1
        return
    w.units_last = units


def watch(job: Job, w: Watch) -> bool:
    """The reads beside the client until it ended (True) or the dev box's
    bound passed (False). A flag in a read or a pause raises EventNow."""
    box = job.box
    w.started = time.monotonic()
    limit = wait_limit(w.minutes * 60)
    next_health = w.started
    while True:
        now = time.monotonic()
        read_sample(box, w)
        if now >= next_health:
            next_health = now + HEALTH_EVERY_S
            read_health(box, w)
            read_units(box, w)
            try:
                w.unit = unit_state(box)
            except box.ip.StepError:
                w.failed_reads += 1
            if w.unit is not None and not w.unit.running:
                w.ended = time.monotonic()
                return True
        if time.monotonic() - w.started > limit:
            w.ended = time.monotonic()
            return False
        box.ip.pause(box.ctx, max(0.0, now + STATUS_EVERY_S - time.monotonic()))


def end_client(job: Job, state: Unit | None, event: str) -> None:
    """The soak unit ended (`state` its last read, or read here); never
    raises: a warning names how to end it by hand."""
    if not job.started:
        return
    box = job.box
    try:
        if state is None:
            state = unit_state(box, event)
        if state.loaded:
            end_unit(box, state, event)
    except box.ip.StepError as e:
        gate.warn(f"the soak client's unit was not ended ({e}): end it by hand: ssh <GEN3_HOST> sudo -n systemctl stop "
                  f"{UNIT}, then sudo -n systemctl reset-failed {UNIT} (the next soak gate ends it first)")
        return
    job.started = False


def end_files(job: Job) -> None:
    """The env file and the summary removed from the box; never raises."""
    box = job.box
    for flag, words, what in (("env_installed", [*gate.SUDO, "rm", "-f", ENV_FILE], "the soak's env file"),
                              ("summary_cleared", [*g3.AS_SERVICE, "rm", "-f", SUMMARY], "the soak's summary")):
        if not getattr(job, flag):
            continue
        try:
            box.call(words, event="ignore")
            setattr(job, flag, False)
        except box.ip.StepError as e:
            gate.warn(f"{what} was not removed ({e}): remove it by hand (ssh <GEN3_HOST> {' '.join(words)})")


def read_summary(box: gate.Box, w: Watch):
    """The client's summary (as the service user), kept in the records; None
    when it cannot be read (the harness check names it)."""
    try:
        text = box.call([*g3.AS_SERVICE, "cat", SUMMARY], event="ignore")
    except box.ip.StepError:
        return None
    try:
        doc = json.loads(text)
    except ValueError:
        return soak_verdict.UNREADABLE
    (w.records / "soakclient.json").write_text(json.dumps(doc) + "\n", encoding="utf-8")
    return doc


def soak_run(job: Job, w: Watch, sha: str, member: str) -> None:
    start(job, sha, member, w.minutes)
    w.finished = watch(job, w)
    if not w.finished:   # still running at the bound: stopped before its summary is read
        end_client(job, w.unit, "ignore")
    w.summary = read_summary(job.box, w)
    end_client(job, w.unit, "ignore")
    end_files(job)


# ---- the command ----

def run(ctx, ip) -> int:
    """`iempc gen3-gate soak [--minutes N]` (dev time, locked, while 3.x serves)."""
    minutes = minutes_of(ip, ctx.args)
    host = g3.gen3_host(ip, ctx.env, gate.WORDS)
    member = member_of(ip, ctx.env)
    box = gate.Box(ip, ctx, host, NAME)
    left_job = end_left_job(box)
    if not box.healthy():
        raise ip.Refused(f"no {NAME} gate: the box is not healthy (iem-dante-dev health on the box says why) "
                         "(nothing was changed)")
    sha = box_build(box)
    left_soak = clear_unit(box)
    number = g3.job_run()
    w = Watch(minutes, left_job, left_soak, records_folder(ip, number))
    baseline(box, w)
    job = Job(box)
    try:
        soak_run(job, w, sha, member)
    except ip.EventNow:
        end_client(job, None, "ignore")
        end_files(job)
        w.ended = time.monotonic()
        ip.emit({"gen3_gate": {"soak": line(w, "cancelled")}})
        raise
    except BaseException:
        end_client(job, None, "ignore")
        end_files(job)
        raise
    print(f"iempc: gen3-gate: the soak's records are {RECORDS}/{number} in iempc's state", file=sys.stderr,
          flush=True)
    doc = line(w)
    _, engine_restarts = unit_restarts(w.units_base, w.units_last)
    if engine_restarts:
        gate.note_starts(ip, engine_restarts)   # the unit's start limit counts them: the HIL gate's budget too
    ip.emit({"gen3_gate": {"soak": doc}})
    passed = doc["pass"] == 1
    description = DESC_PASS.format(minutes=minutes) if passed else DESC_FAIL + ", ".join(doc["failed"])
    posted = gate.post(ip, sha, passed, description, CONTEXT)
    return 0 if passed and posted else 1
