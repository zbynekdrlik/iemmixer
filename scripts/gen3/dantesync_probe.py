#!/usr/bin/env python3
"""The read back of dantesync on the Gen 3 box (#72 phase B, plan Task 2;
.claude/rules/gen3.md): runs ON the box, fed to `python3 -` over ssh by
install-dantesync.sh (stdlib only, no file left on the box), or by hand:

    ssh <user@host> python3 - --wait 600 < scripts/gen3/dantesync_probe.py

Once a second, until everything holds or --wait seconds passed:

- a "DSYX" request to dantesync's time server (127.0.0.1:31900; the layout in
  dante-stack crates/dante-stack/src/clock/dsyx.rs): a reply, locked, and the date-offset
  extension present (dantesync holds the fleet date offset the device's media
  clock needs);
- its HTTP status (127.0.0.1:8898/status): `date_authority` "follower" (never
  the master, never local), and `gm_source_ip` one of the installed config's
  `system.gm_allowlist` (an address or a CIDR prefix, or a host name entry
  through the status's own `gm_allowlist_resolved`).

Prints one JSON line, numbers only (P6: no address or name, it may be
pasted): waited_s, dsyx, locked, mode, ext (the extension's version, 0 when
absent), freq_ppb, http, follower, gm_allowed. Exit 0 when everything holds,
1 when it did not within --wait (each reason on stderr, values never), 2 for
the command line or an unreadable config."""
from __future__ import annotations

import argparse
import http.client
import ipaddress
import json
import socket
import struct
import sys
import time
import urllib.error
import urllib.request

DSYX_PORT = 31900
HTTP_PORT = 8898
CONFIG = "/etc/dantesync/config.json"
REQUEST_LEN = 64
BASE_LEN = 64
EXT_LEN = 40
REPLY_TIMEOUT_S = 0.2
HTTP_TIMEOUT_S = 1.0


def request(request_id: int) -> bytes:
    """A DSYX request, padded to 64 bytes (dantesync answers no shorter)."""
    return b"DSYX" + struct.pack(">I", request_id) + bytes(REQUEST_LEN - 8)


def parse_reply(reply: bytes, request_id: int) -> dict | None:
    """The base and the extension's version of the reply to `request_id`;
    None for anything else."""
    if len(reply) < BASE_LEN or reply[0:4] != b"DSYR":
        return None
    if struct.unpack(">I", reply[4:8])[0] != request_id:
        return None
    ext = reply[BASE_LEN:]
    return {
        "locked": 1 if reply[41] else 0,
        "mode": reply[40],
        "freq_ppb": struct.unpack(">i", reply[36:40])[0],
        "ext": ext[0] if len(ext) >= EXT_LEN else 0,
    }


def ask_dsyx(port: int, request_id: int) -> dict | None:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.bind(("127.0.0.1", 0))
        s.connect(("127.0.0.1", port))
        s.settimeout(REPLY_TIMEOUT_S)
        try:
            s.send(request(request_id))
            deadline = time.monotonic() + REPLY_TIMEOUT_S
            while True:
                s.settimeout(max(deadline - time.monotonic(), 0.001))
                parsed = parse_reply(s.recv(512), request_id)
                if parsed is not None:
                    return parsed
        except OSError:
            return None


def read_status(port: int) -> dict | None:
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/status", timeout=HTTP_TIMEOUT_S) as r:
            status = json.loads(r.read().decode("utf-8"))
    except (OSError, ValueError, urllib.error.URLError, http.client.HTTPException):
        return None
    return status if isinstance(status, dict) else None


def gm_allowed(status: dict, allowlist: list[str]) -> bool:
    """The grandmaster dantesync follows is one the config allows."""
    try:
        gm = ipaddress.IPv4Address(str(status.get("gm_source_ip")))
    except ValueError:
        return False
    resolved = status.get("gm_allowlist_resolved")
    entries = list(allowlist) + (list(resolved) if isinstance(resolved, list) else [])
    for entry in entries:
        try:
            if gm in ipaddress.ip_network(str(entry).strip(), strict=False):
                return True
        except ValueError:
            continue  # a host name entry: dantesync lists its addresses in gm_allowlist_resolved
    return False


def check(dsyx: dict | None, status: dict | None, allowlist: list[str], waited_s: int) -> tuple[dict, list[str]]:
    """The line and the reasons it is not yet a locked follower of an
    allowed grandmaster."""
    line = {
        "waited_s": waited_s,
        "dsyx": 0 if dsyx is None else 1,
        "locked": None if dsyx is None else dsyx["locked"],
        "mode": None if dsyx is None else dsyx["mode"],
        "ext": None if dsyx is None else dsyx["ext"],
        "freq_ppb": None if dsyx is None else dsyx["freq_ppb"],
        "http": 0 if status is None else 1,
        "follower": None if status is None else int(status.get("date_authority") == "follower"),
        "gm_allowed": None if status is None else int(gm_allowed(status, allowlist)),
    }
    reasons = []
    if dsyx is None:
        reasons.append("no DSYX reply from dantesync's time server (is dantesync running?)")
    else:
        if not dsyx["locked"]:
            reasons.append("dantesync is not locked to a grandmaster")
        if not dsyx["ext"]:
            reasons.append("no date-offset extension in the DSYX reply: dantesync holds no fleet date offset "
                           "(is ntp_server the fleet's NTP master, does it answer, is clock_discipline ptp_phase_lock?)")
    if status is None:
        reasons.append("no HTTP status from dantesync (is http_status enabled on 8898?)")
    else:
        if not line["follower"]:
            authority = status.get("date_authority")
            word = authority if isinstance(authority, str) and authority.isalpha() and len(authority) <= 16 else "?"
            reasons.append(f"date_authority is '{word}', not follower")
        if not line["gm_allowed"]:
            reasons.append("the grandmaster dantesync follows is not one of the config's gm_allowlist")
    return line, reasons


def load_allowlist(path: str) -> list[str]:
    with open(path, encoding="utf-8") as f:
        config = json.load(f)
    system = config.get("system") if isinstance(config, dict) else None
    allow = system.get("gm_allowlist") if isinstance(system, dict) else None
    if not isinstance(allow, list) or not allow:
        raise ValueError("no system.gm_allowlist")
    return [str(a) for a in allow]


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(prog="dantesync_probe.py", description=__doc__.split("\n\n")[0])
    ap.add_argument("--wait", type=int, default=300, help="seconds to wait for everything (default 300)")
    ap.add_argument("--config", default=CONFIG)
    ap.add_argument("--dsyx-port", type=int, default=DSYX_PORT)
    ap.add_argument("--http-port", type=int, default=HTTP_PORT)
    ap.add_argument("--interval", type=float, default=1.0, help=argparse.SUPPRESS)
    args = ap.parse_args(argv)
    if args.wait < 0:
        ap.error("--wait must be 0 or more")
    try:
        allowlist = load_allowlist(args.config)
    except (OSError, ValueError) as e:
        print(f"dantesync_probe: {args.config}: {type(e).__name__}: cannot read its gm_allowlist", file=sys.stderr)
        return 2
    start = time.monotonic()
    request_id = int(start * 1000) & 0xFFFFFFFF
    while True:
        request_id = (request_id + 1) & 0xFFFFFFFF
        dsyx = ask_dsyx(args.dsyx_port, request_id)
        status = read_status(args.http_port)
        elapsed = time.monotonic() - start
        waited_s = int(elapsed)
        line, reasons = check(dsyx, status, allowlist, waited_s)
        if not reasons or elapsed >= args.wait:
            break
        time.sleep(args.interval)
    print(json.dumps(line, separators=(",", ":")))
    for reason in reasons:
        print(f"dantesync_probe: after {waited_s} s: {reason}", file=sys.stderr)
    return 0 if not reasons else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
