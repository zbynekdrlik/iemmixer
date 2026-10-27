#!/usr/bin/env python3
"""The band's data onto the Gen 3 box (#72 phase E Task 5; plan
docs/superpowers/plans/2026-10-27-gen3-phase-e-band-test.md). Runs on the box
as the service user, with the stack stopped:

  sudo -n -u iemmixer /usr/bin/python3 -I /opt/iemmixer/lib/band_receive.py \
      /var/lib/iemmixer/site /var/lib/iemmixer/engine < band.zip

stdin is one zip (from `iempc gen3-data to-box`) whose members are
`server/<path>` (the server's band files) and `engine/<path>` (the engine's
saved state). Only the names below are taken; anything else (an absolute
path, `..`, a link, a name outside the lists, a repeat) refuses the whole
archive before anything is written.

- server: each top-level item named (a file, or a folder whole) replaces the
  site folder's item of that name: written beside it as `.<name>.incoming`,
  the old one moved to `.<name>.before-<stamp>`, the new one renamed in. The
  site's own files (iemmixer.toml, the roster's status) are never touched.
- engine: the whole state folder is replaced the same way (generations,
  revision markers and the baseline belong together), the old folder kept
  as `<engine>.before-<stamp>`.

Nothing is deleted. Files 0600, folders 0700. stdout: one JSON line
{"written": {"<member>": "<sha256>"}, "kept": ["<the old items' names>"]};
exit 0 written, 1 refused (nothing written; the reason on stderr, never a
file's content), 2 usage, 3 a write or swap that failed part-way (every
replaced item kept beside its target)."""
from __future__ import annotations

import hashlib
import io
import json
import os
import stat
import sys
import time
import zipfile
from pathlib import Path, PurePosixPath

# The server folder's band items: a file by its path, a folder whole.
SERVER_FILES = ("secrets/jwt_secret", "secrets/vapid_private", "secrets/pin_hashes.json", "roster.toml",
                "push_subscriptions.json", "push_subs_v2_migrated", "alarm_subscriptions.json")
SERVER_FOLDERS = ("lineups", "photos", "presets", "snapshots", "customizations", "backups")
# The engine folder's state files (persist.rs): no lock, no temporary or damaged file.
ENGINE_PREFIXES = ("gen-",)
ENGINE_FILES = ("current.json", "baseline.json")
ENGINE_MARKER = "current.json.rev-"

MAX_ARCHIVE = 512 * 1024 * 1024
MAX_MEMBER = 64 * 1024 * 1024
MAX_MEMBERS = 10_000


class Refused(Exception):
    """The archive is not one this script takes: nothing is written."""


def item_of(name: str) -> tuple[str, str]:
    """(`server` or `engine`, the item it replaces: a top-level name, or a
    secret's path) for an allowed member name; Refused otherwise."""
    path = PurePosixPath(name)
    parts = path.parts
    if (name != path.as_posix() or path.is_absolute() or not parts or any(p in ("", ".", "..") for p in parts)
            or "\\" in name or len(parts) < 2):
        raise Refused("a member name is not a plain relative path")
    root, rest = parts[0], PurePosixPath(*parts[1:]).as_posix()
    if root == "server":
        if rest in SERVER_FILES:
            return root, rest
        if parts[1] in SERVER_FOLDERS and len(parts) > 2:
            return root, parts[1]
    elif root == "engine" and len(parts) == 2:
        leaf = parts[1]
        if (leaf in ENGINE_FILES or (leaf.startswith(ENGINE_MARKER) and leaf[len(ENGINE_MARKER):].isdigit())
                or (leaf.startswith(ENGINE_PREFIXES) and leaf.endswith(".json"))):
            return root, ""
    raise Refused("a member is not one of the band's files")


def check(archive: zipfile.ZipFile) -> list[zipfile.ZipInfo]:
    """The archive's file members, each allowed, none a link, within the caps."""
    infos = archive.infolist()
    if len(infos) > MAX_MEMBERS:
        raise Refused("too many members")
    names, files = set(), []
    for info in infos:
        if info.is_dir():
            item_of(info.filename.rstrip("/") + "/x")   # a folder only where a file may be
            continue
        if stat.S_ISLNK(info.external_attr >> 16):
            raise Refused("a member is a link")
        if info.filename in names:
            raise Refused("a member is named twice")
        if info.file_size > MAX_MEMBER:
            raise Refused("a member is larger than the cap")
        item_of(info.filename)
        names.add(info.filename)
        files.append(info)
    if "engine/current.json" not in names:
        raise Refused("the archive holds no engine/current.json")
    return files


def write_file(path: Path, data: bytes) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())


def staging(target: Path) -> Path:
    """Where `target`'s new version is written before the swap: beside it."""
    return target.with_name(f".{target.name}.incoming")


def aside(target: Path, stamp: str, hidden: bool) -> Path:
    """A free name beside `target` for its old version: `<name>.before-<stamp>`,
    `.<name>` when `hidden`, with `-<n>` when a run in the same second left one."""
    base = f"{'.' if hidden else ''}{target.name}.before-{stamp}"
    path, n = target.with_name(base), 1
    while path.exists() or path.is_symlink():
        path, n = target.with_name(f"{base}-{n}"), n + 1
    return path


def receive(raw: bytes, site: Path, engine: Path, stamp: str) -> dict:
    if len(raw) > MAX_ARCHIVE:
        raise Refused("the archive is larger than the cap")
    try:
        archive = zipfile.ZipFile(io.BytesIO(raw))
        files = check(archive)
        data = {i.filename: archive.read(i) for i in files}
    except (zipfile.BadZipFile, zipfile.LargeZipFile, OSError, RuntimeError, EOFError) as e:
        raise Refused(f"not a readable zip ({type(e).__name__})") from None
    for d in (site, engine):
        if d.is_symlink() or not d.is_dir():
            raise Refused("a target folder is missing or a link")
    # The targets: the engine folder whole, each server item by its name.
    targets = [engine] + [site / item for item in sorted({item_of(n)[1] for n in data if n.startswith("server/")})]
    if any(staging(t).exists() or staging(t).is_symlink() for t in targets):
        raise Refused("an earlier run's staging is still there")
    # Stage everything first.
    for t in targets:
        if t == engine or t.name in SERVER_FOLDERS:
            staging(t).parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            staging(t).mkdir(mode=0o700)
    for name, blob in data.items():
        root, item = item_of(name)
        rest = PurePosixPath(*PurePosixPath(name).parts[1:])
        if root == "engine":
            write_file(staging(engine) / rest.name, blob)
        elif item in SERVER_FOLDERS:
            write_file(staging(site / item) / PurePosixPath(*rest.parts[1:]), blob)
        else:
            write_file(staging(site / item), blob)
    # Then swap each in: the old one aside under a name of its own, never deleted.
    kept = []
    for t in targets:
        if t.exists() or t.is_symlink():
            before = aside(t, stamp, hidden=t != engine)
            os.rename(t, before)
            kept.append(before.name if t == engine else before.relative_to(site).as_posix())
        os.rename(staging(t), t)
    written = {}
    for name in data:
        rest = PurePosixPath(*PurePosixPath(name).parts[1:])
        path = engine / rest.name if name.startswith("engine/") else site / rest
        written[name] = hashlib.sha256(path.read_bytes()).hexdigest()
    return {"written": dict(sorted(written.items())), "kept": kept}


def main(argv: list[str], stdin) -> int:
    if len(argv) != 2:
        print("usage: band_receive.py <site folder> <engine folder> < band.zip", file=sys.stderr)
        return 2
    os.umask(0o077)
    try:
        out = receive(stdin.read(), Path(argv[0]), Path(argv[1]), time.strftime("%Y%m%dT%H%M%S"))
    except Refused as e:
        print(f"band_receive: {e}: nothing was written", file=sys.stderr)
        return 1
    except OSError as e:   # after the checks: a swap that failed part-way keeps every old item aside
        print(f"band_receive: writing failed ({type(e).__name__}): what was swapped in stays, every item it "
              "replaced is kept beside it as .before-<stamp>", file=sys.stderr)
        return 3
    print(json.dumps(out))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:], sys.stdin.buffer))
