#!/usr/bin/env python3
"""The band's data off the Gen 3 box (#72, the design comment "one command for
a new box"), the reverse of band_receive.py. Since Gen 2 ended the box holds
the only live copy, so `iempc gen3-backup` and `iempc gen3-new-box` read it
here. Runs on the box as the service user; the stack may keep running: each
of its files is written whole by a rename, so a read gets one version:

  sudo -n -u iemmixer /usr/bin/python3 -I /opt/iemmixer/lib/band_send.py \
      /var/lib/iemmixer/site /var/lib/iemmixer/engine

Takes exactly what band_receive.py takes: the same names (test_band_send.py
compares the lists), `server/<path>` and `engine/<name>` members. A generation
the engine prunes while this reads is left out; a link is never followed.

stdout: one JSON line {"manifest": {"<member>": "<sha256>"}, "zip": "<base64>"},
band_receive.py's manifest (iempc_gen3_data.unpack reads it). It carries the band's secrets: only
iempc reads it, and nothing prints it. exit 0 sent, 1 refused (the reason on
stderr, never a file's content), 2 usage."""
from __future__ import annotations

import base64
import hashlib
import io
import json
import os
import re
import stat
import sys
import zipfile
from pathlib import Path

# The same lists as band_receive.py.
SERVER_FILES = ("secrets/jwt_secret", "secrets/vapid_private", "secrets/pin_hashes.json", "secrets/token_floor.json",
                "roster.toml", "push_subscriptions.json", "push_subs_v2_migrated", "alarm_subscriptions.json")
SERVER_FOLDERS = ("lineups", "photos", "presets", "snapshots", "customizations", "backups")
ENGINE_NAME = re.compile(r"current\.json|baseline\.json|current\.json\.rev-[0-9]+|gen-.*\.json")


class Refused(Exception):
    """The box's folders are not ones this script reads."""


def regular(path: Path) -> bool:
    try:
        return stat.S_ISREG(os.lstat(path).st_mode)
    except OSError:
        return False


def members(site: Path, engine: Path) -> list[tuple[str, Path]]:
    """(member name, path) of every band file there is, sorted."""
    out = [(f"server/{name}", site / name) for name in SERVER_FILES if regular(site / name)]
    for folder in SERVER_FOLDERS:
        for root, dirs, files in os.walk(site / folder, followlinks=False):
            dirs.sort()
            for leaf in files:
                path = Path(root) / leaf
                if regular(path):
                    out.append((f"server/{path.relative_to(site).as_posix()}", path))
    out += [(f"engine/{leaf}", engine / leaf) for leaf in os.listdir(engine)
            if ENGINE_NAME.fullmatch(leaf) and regular(engine / leaf)]
    return sorted(out)


def send(site: Path, engine: Path) -> dict:
    for folder in (site, engine):
        if folder.is_symlink() or not folder.is_dir():
            raise Refused("a source folder is missing or a link")
    buffer, manifest = io.BytesIO(), {}
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, path in members(site, engine):
            try:
                blob = path.read_bytes()
            except FileNotFoundError:   # pruned since the listing
                continue
            archive.writestr(name, blob)
            manifest[name] = hashlib.sha256(blob).hexdigest()
    if "engine/current.json" not in manifest:
        raise Refused("the engine folder holds no current.json")
    return {"manifest": manifest, "zip": base64.b64encode(buffer.getvalue()).decode()}


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("usage: band_send.py <site folder> <engine folder>", file=sys.stderr)
        return 2
    try:
        doc = send(Path(argv[0]), Path(argv[1]))
    except (Refused, OSError) as e:
        print(f"band_send: {e if isinstance(e, Refused) else type(e).__name__}: nothing was sent", file=sys.stderr)
        return 1
    print(json.dumps(doc, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
