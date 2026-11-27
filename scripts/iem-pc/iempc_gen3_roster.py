"""`iempc gen3-roster` and the deploy's after-checks (#145): a roster that the
box's server refused, or a receiver's subscription to our device that no live
TX flow carries, fails loudly instead of passing as a success.

Seen 26.11.2026: a deploy reported success while the server kept the roster
applied last (a lineup leader on a set without peers, F30), and after the
deploy's engine restart a receiver's flow came up with one subscribed TX
channel left out (an empty slot, dstack#504): a band member heard nothing,
then only one ear, while every view said the patch was fine.

- The roster read: the server writes `roster.status.json` beside its
  roster.toml (iem-server roster_watch: `sha256` of the file it read, `ok`,
  `applied`, `problems`). Wait until the status is of this roster's text (its
  sha256), then require ok and applied; anything else names the problems.
- The flow read: `ctl status`'s TX flows give each live flow's receiver
  and the TX channels it carries; netaudio (as gen3_arc.py relies on it)
  lists the subscriptions those receivers hold, read again until it holds
  every one of them (netaudio leaves out a device that does not answer, and
  exits 1 then); the ones to our device (the `name` of the box's device
  state file) are compared with the flows. The receivers are taken once the live ones held
  SETTLE_S; a subscription whose receiver has no live flow fails too; a
  receiver netaudio cannot list (switched off) is not checked.
  Accepted limit: netaudio leaves out a device that does not answer and the
  network always has some (a clean read is never required, or every deploy
  would fail), so a receiver with NO live flow that netaudio also left out
  is not seen; every receiver of a live flow is read until it is listed.
  dcontroller#163 watches the network-wide case. A
  subscribed TX channel of ours that no live flow to that receiver carries,
  or a TX name our device does not have, is a channel without sound. Flows
  start a few seconds after an engine start (the device's clock), so the
  flows are read again until FLOWS_S.
- `iempc gen3-roster`: GEN3_ROSTER_FILE to the box without a deploy and
  without a restart (as the service user: tee into roster.toml.new, then
  mv -T over roster.toml, so the watch never reads half a file), then both
  reads. It runs in an event too (no restart, no Dante change): a lineup fix
  the owner asks for while the band plays.

Errors name receivers and channels (on stderr, on this PC only); stdout is
counts and booleans (P6). Tests replace `data.box` (ssh) and NETAUDIO (an
external program), never this module's own steps."""
from __future__ import annotations

import hashlib
import json
import subprocess
import sys
import time
import tomllib
from pathlib import Path

import iempc_gen3_data as data

SITE = "/var/lib/iemmixer/site"
STATUS_READ = f"sudo -n -u iemmixer cat {SITE}/roster.status.json"
PUSH_NEW = f"sudo -n -u iemmixer tee {SITE}/roster.toml.new"
PUSH_MOVE = f"sudo -n -u iemmixer mv -T {SITE}/roster.toml.new {SITE}/roster.toml"
STATE_READ = "cat /var/lib/iem-dante/state.toml"
CTL_STATUS = "/opt/iemmixer/bin/iem-dante-dev ctl status"
NETAUDIO = ("netaudio", "-j", "--timeout", "10", "subscription", "list")
NETAUDIO_S = 120
NETAUDIO_TRIES = 3
ROSTER_S = 30.0
FLOWS_S = 60.0
SETTLE_S = 5.0
POLL_S = 1.0


def say(line: str) -> None:
    """A progress line on stderr (stdout is the one JSON object)."""
    sys.stderr.write(line + "\n")
    sys.stderr.flush()


def sha256(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


# ---- pure ----


def roster_verdict(status: object, want_sha: str) -> list[str] | None:
    """None while the status is not of the roster `want_sha` (not read yet);
    [] when it is applied; else its problems (at least one line)."""
    if not isinstance(status, dict) or status.get("sha256") != want_sha:
        return None
    if status.get("ok") is True and status.get("applied") is True:
        return []
    problems = [p for p in status.get("problems") or [] if isinstance(p, str)]
    return problems or ["the server did not apply it and named no problem"]


def our_device(state_text: str) -> tuple[str, list[str]]:
    """The device's name and its TX names in channel order (TX n = index n-1)."""
    doc = tomllib.loads(state_text)
    name, tx = doc.get("name"), doc.get("tx")
    if not isinstance(name, str) or not name or not isinstance(tx, list) or not all(isinstance(t, str) for t in tx):
        raise ValueError("the device state names no device or TX list")
    return name, tx


def subscriptions_to(subs: object, device: str) -> list[tuple[str, str, str]]:
    """netaudio's subscriptions whose TX device is `device`: (receiver, its RX
    channel, our TX channel name)."""
    if not isinstance(subs, list):
        raise ValueError("netaudio's subscription list is not a list")
    out = []
    for s in subs:
        if isinstance(s, dict) and s.get("tx_device") == device and isinstance(s.get("tx_channel"), str):
            out.append((str(s.get("rx_device")), str(s.get("rx_channel")), s["tx_channel"]))
    return out


def carried(status: object) -> dict[str, set[int]]:
    """Each receiver's TX channels that a live flow of ours carries."""
    flows = status.get("tx") if isinstance(status, dict) else None
    if not isinstance(flows, list):
        raise ValueError("the device status has no TX flow list")
    out: dict[str, set[int]] = {}
    for f in flows:
        if not isinstance(f, dict) or f.get("live") != 1:
            continue
        channels = {c for c in f.get("tx_channels") or [] if type(c) is int}
        out.setdefault(str(f.get("receiver")), set()).update(channels)
    return out


def silent(subs: list[tuple[str, str, str]], tx_names: list[str], flows: dict[str, set[int]]) -> list[str]:
    """Each subscription no live flow carries, as a line naming it."""
    out = []
    for receiver, rx, tx in subs:
        if tx not in tx_names:
            out.append(f"{receiver} {rx!r} subscribes {tx!r}, a TX name our device does not have")
        elif tx_names.index(tx) + 1 not in flows.get(receiver, set()):
            out.append(f"{receiver} {rx!r} subscribes {tx!r} (our TX {tx_names.index(tx) + 1}), "
                       "but no live flow to it carries that channel")
    return out


# ---- the box ----


def _box(ip, host: str, command: str, what: str, stdin: bytes = b"") -> str:
    code, out, err = data.box(host, command, stdin, data.BOX_S)
    if code != 0:
        raise ip.StepError(f"{what}: the box's answer failed (ssh exit {code}): {err.decode(errors='replace').strip()}")
    return out.decode(errors="replace")


def await_roster(ip, host: str, text: str, cmd: str, clock=None) -> None:
    """Until the box's server has read this roster: StepError naming the
    problems when it refused it, or when it read none of it by ROSTER_S."""
    clock = clock or time
    want, end = sha256(text), clock.monotonic() + ROSTER_S
    while True:
        try:
            status = json.loads(_box(ip, host, STATUS_READ, cmd))
        except ValueError:
            status = None
        verdict = roster_verdict(status, want)
        if verdict == []:
            say(f"{cmd}: the box's server applied the roster (sha256 {want[:12]})")
            return
        if verdict is not None:
            raise ip.StepError(f"{cmd}: the box's server REFUSED the roster and kept the one applied last: "
                               + "; ".join(verdict))
        if clock.monotonic() >= end:
            raise ip.StepError(f"{cmd}: the box's server did not read this roster within {ROSTER_S:g} s "
                               "(roster.status.json is of another file)")
        clock.sleep(POLL_S)


def netaudio_once(cmd: str) -> object:
    """One netaudio read; None when it gave no JSON (it exits 1 with a
    partial list when one device of the network does not answer, so its
    stdout is read whatever its exit)."""
    try:
        proc = subprocess.run(NETAUDIO, capture_output=True, timeout=NETAUDIO_S, check=False)
    except (OSError, subprocess.TimeoutExpired) as e:
        say(f"{cmd}: netaudio could not run: {e}")
        return None
    try:
        return json.loads(proc.stdout)
    except ValueError:
        say(f"{cmd}: netaudio gave no JSON (exit {proc.returncode})")
        return None


def subscriptions_of(ip, cmd: str, device: str, receivers: set[str]) -> list[tuple[str, str, str]]:
    """The subscriptions to `device`, read until netaudio's list holds every
    one of `receivers` (the receivers of our live flows): netaudio leaves out
    a device that did not answer, which must never read as "no subscription"."""
    for attempt in range(1, NETAUDIO_TRIES + 1):
        subs = netaudio_once(cmd)
        if isinstance(subs, list):
            heard = {str(s.get("rx_device")) for s in subs if isinstance(s, dict)}
            unread = receivers - heard
            if not unread:
                return subscriptions_to(subs, device)
            say(f"{cmd}: netaudio read {attempt}/{NETAUDIO_TRIES} left out {len(unread)} receiver(s) of our flows: "
                + ", ".join(sorted(unread)))
        else:
            unread = receivers
    raise ip.StepError(f"{cmd}: the subscriptions of {', '.join(sorted(unread))} could not be read in "
                       f"{NETAUDIO_TRIES} netaudio reads: the flows are NOT checked")


def await_flows(ip, host: str, cmd: str, clock=None) -> int:
    """Until every subscription to our device has a live flow carrying it;
    StepError naming the silent ones at FLOWS_S. Returns their count.

    Right after an engine start the flows come up one by one: the receivers
    are taken once the set of live receivers held for SETTLE_S (none by
    FLOWS_S fails: the box always serves a receiver), and a receiver that
    turns up later has its subscriptions read too. Every subscription to our
    device that netaudio lists counts, also one whose receiver has no live
    flow (switched on, subscribed, silent); a receiver netaudio cannot list
    (switched off) is not checked."""
    clock = clock or time
    def flows() -> dict[str, set[int]]:
        try:
            lines = _box(ip, host, CTL_STATUS, cmd).splitlines()
            return carried(json.loads(lines[-1]) if lines else None)
        except ValueError as e:
            raise ip.StepError(f"{cmd}: {e}") from None

    try:
        device, tx_names = our_device(_box(ip, host, STATE_READ, cmd))
    except ValueError as e:
        raise ip.StepError(f"{cmd}: {e}") from None
    end = clock.monotonic() + FLOWS_S
    now, seen, since = flows(), None, clock.monotonic()
    while True:
        receivers = set(now)
        if receivers != seen:
            seen, since = receivers, clock.monotonic()
        elif receivers and clock.monotonic() - since >= SETTLE_S:
            break
        if clock.monotonic() >= end:
            if receivers:
                break
            raise ip.StepError(f"{cmd}: no live TX flow of our device within {FLOWS_S:g} s: no receiver plays it")
        clock.sleep(POLL_S)
        now = flows()
    say(f"{cmd}: our live TX flows go to {len(seen)} receiver(s): {', '.join(sorted(seen))}")
    subs = subscriptions_of(ip, cmd, device, seen)
    if not subs:
        raise ip.StepError(f"{cmd}: netaudio lists no subscription to our device ({device}) although "
                           f"{len(seen)} receiver(s) play its flows: the flows are NOT checked")
    say(f"{cmd}: netaudio lists {len(subs)} subscriptions to our device")
    while True:
        new = set(now) - seen
        if new:
            say(f"{cmd}: a live flow to {', '.join(sorted(new))} came up later: its subscriptions are read too")
            seen |= new
            subs = subscriptions_of(ip, cmd, device, seen)
        missing = silent(subs, tx_names, now)
        if not missing:
            say(f"{cmd}: {len(subs)} subscriptions to our device, each carried by a live flow")
            return len(subs)
        if clock.monotonic() >= end:
            raise ip.StepError(f"{cmd}: {len(missing)} subscribed channel(s) of our device get NO SOUND: "
                               + "; ".join(missing)
                               + " (an unpatch and re-patch of that receiver channel in Dante Controller fills it; "
                               "dstack#504)")
        say(f"{cmd}: {len(missing)} subscribed channel(s) not carried yet, read again in {POLL_S:g} s")
        clock.sleep(POLL_S)
        now = flows()


def after_start(ip, host: str, roster_text: str, cmd: str, clock=None) -> dict:
    """Both reads, the roster's first."""
    clock = clock or time
    await_roster(ip, host, roster_text, cmd, clock)
    count = await_flows(ip, host, cmd, clock)
    return {"roster_applied": True, "subscriptions_carried": count}


def run(ctx, ip) -> int:
    """`iempc gen3-roster`: GEN3_ROSTER_FILE to the box live, then both reads."""
    host, path = ctx.env.get("GEN3_HOST"), ctx.env.get("GEN3_ROSTER_FILE")
    if not host or not path:
        raise ip.Refused("gen3-roster: GEN3_HOST or GEN3_ROSTER_FILE missing in the private env")
    if host.startswith("-"):
        raise ip.Refused("gen3-roster: GEN3_HOST must be an ssh destination, not an option")
    try:
        text = Path(path).read_text(encoding="utf-8")
        tomllib.loads(text)
    except (OSError, ValueError) as e:
        raise ip.Refused(f"gen3-roster: GEN3_ROSTER_FILE is not a readable TOML file ({type(e).__name__}): "
                         "nothing was sent") from None
    _box(ip, host, PUSH_NEW, "gen3-roster", text.encode("utf-8"))
    _box(ip, host, PUSH_MOVE, "gen3-roster")
    ip.emit({"gen3_roster": {"sent": True, **after_start(ip, host, text, "gen3-roster")}})
    return 0
