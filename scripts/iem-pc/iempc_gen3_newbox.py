"""A new Gen 3 box, the box's data off it, and the drift between a box and this
checkout (#72, the design comment "one command for a new box";
.claude/rules/gen3-new-box.md). The cookbook's steps stay in their own scripts:
this module only runs them in order, and a rerun resumes, because each is
idempotent.

**`iempc gen3-backup`** (dev time, locked; reads the box, the stack keeps
running): band_send.py's zip checked here against its manifest, and the pepper
read out of its credential (`systemd-creds decrypt`), written to
`box-backup/<stamp>/` beside iempc's private env (0700; files 0600) as
band.zip, manifest.json and pepper, then read back. It holds the band's
secrets, so it stays there: never in this repo, never in the ops repo.

**`iempc gen3-new-box --host <admin@fresh> --bundle <dir> --sha <40 hex>
[--iface <name>] [--from-backup <stamp>]`** (dev time, locked): a fresh OS to
the band's state, in this order: install-dantesync.sh; the stack stopped;
provision.sh; one reboot when the realtime kernel does not run yet; deploy.sh
--new-box with the usual device config and the band's site; the stack stopped;
band_receive.py and the pepper, from the running box or from a backup; the
stack started and healthy; install-cloudflared.sh; verify_box.py, every check 1
and realtime_running 1. Refused on GEN3_HOST: the box that serves the band is
never rebuilt. The new box stays a spare: nothing enables its units, and our
device keeps the config's usual name. Taking the band over is a separate step
(the cookbook, section 6).

A fresh box's provision exits 1 by design: verify_box.py names `units` and
`napi` until the first deploy. Only those two are let through.

**`iempc gen3-drift`** (dev time, locked; reads only): the box against this
checkout. Files by sha256, the build (a commit after it that changes no crate
is no drift), verify_box.py, the units that start at boot, the device config,
the pinned dantesync and cloudflared. Exit 1 on any difference, each named.

stdout: one JSON object of counts and fixed words; no host, no value (P6)."""
from __future__ import annotations

import base64
import hashlib
import json
import os
import re
import sys
import time
from pathlib import Path

import iempc_gen3 as g3
import iempc_gen3_data as data
import iempc_gen3_deploy as deploy

BACKUP_WORDS = g3.Words("gen3-backup", "the backup")
DRIFT_WORDS = g3.Words("gen3-drift", "the drift check")
HERE = Path(__file__).resolve().parent
GEN3 = HERE.parent / "gen3"
REPO = HERE.parent.parent
SEND = "sudo -n -u iemmixer python3 -I /opt/iemmixer/lib/band_send.py /var/lib/iemmixer/site /var/lib/iemmixer/engine"
DECRYPT = f"sudo -n systemd-creds decrypt --name=pepper {data.CREDENTIAL} -"
STOP = "sudo -n systemctl stop iem-server iem-engine iem-dante"
START = "sudo -n systemctl start iem-engine iem-server"
REBOOT = "sudo -n systemctl reboot"
HEALTH = "/opt/iemmixer/bin/iem-dante-dev health"
BOOT_ID = "cat /proc/sys/kernel/random/boot_id"
ROUTE = "ip -o -4 route show default"
ENABLED = ("iem-engine", "iem-server", "iem-napi", "dantesync")
VERSIONS = (("dantesync", "dantesync --version", "GEN3_DANTESYNC_VERSION"),
            ("cloudflared", "cloudflared --version", "GEN3_CLOUDFLARED_VERSION"))
# What a box holds of this checkout: the path there, the file in scripts/gen3.
CHECKOUT_FILES = {
    "/etc/systemd/system/iem-engine.service": "iem-engine.service",
    "/etc/systemd/system/iem-server.service": "iem-server.service",
    "/etc/systemd/system/iem-dante.service": "iem-dante.service",
    "/etc/systemd/system/iem-alarm@.service": "iem-alarm@.service",
    "/etc/systemd/system/iem-tunnel.service": "iem-tunnel.service",
    "/etc/systemd/system/iem-napi.service": "iem-napi.service",
    "/usr/local/lib/iemmixer/napi-priority": "napi-priority",
    "/opt/iemmixer/lib/band_receive.py": "band_receive.py",
    "/opt/iemmixer/lib/band_send.py": "band_send.py",
    # The template the box's sudoers was rendered from: provision leaves it in its stage.
    ".cache/iem-gen3-provision/sudo-lines.txt": "sudo-lines.txt",
}
DEVICE_CONFIG = "/etc/iemmixer/dante-device.toml"
SEND_S = 300
INSTALL_S = 600
PROVISION_S = 1800
BACK_S = 300
HEALTHY_S = 60
PROBE_S = 3
SSH_LOST = 255
# verify_box.py's checks a box no deploy has reached fails, by design.
BEFORE_DEPLOY = {"units", "napi"}
STAMP = re.compile(r"[0-9]{8}T[0-9]{6}")
HOST = re.compile(r"[A-Za-z0-9_.@-]{1,128}")
NEW_BOX_KEYS = ("GEN3_HOST", "GEN3_DEVICE_CONFIG", "GEN3_SITE_FILE", "GEN3_ROSTER_FILE", "GEN3_DANTESYNC_CONFIG",
                "GEN3_DANTESYNC_VERSION", "GEN3_DANTESYNC_SHA256", "GEN3_CLOUDFLARED_VERSION",
                "GEN3_CLOUDFLARED_SHA256")
FILE_KEYS = ("GEN3_DEVICE_CONFIG", "GEN3_SITE_FILE", "GEN3_ROSTER_FILE", "GEN3_DANTESYNC_CONFIG")


def say(text: str) -> None:
    print(f"iempc: {text}", file=sys.stderr, flush=True)


def tail(err: bytes) -> str:
    return err.decode(errors="replace").strip()[-300:]


# ---- the data ----

def read_band(ip, host: str) -> tuple[bytes, dict[str, str]]:
    """The box's band files, checked against their manifest. The stack may run."""
    code, out, err = data.box(host, SEND, b"", SEND_S)
    if code != 0:
        raise ip.StepError(f"the box's band_send.py failed (exit {code}; a build before it needs a deploy first): "
                           f"{tail(err)}")
    return data.unpack(ip, out.decode(errors="replace"), "the box")


def read_pepper(ip, host: str) -> bytes:
    """The box's pepper, never printed: on an error only its length is named."""
    code, out, err = data.box(host, DECRYPT, b"", data.BOX_S)
    if code != 0:
        raise ip.StepError(f"systemd-creds decrypt failed on the box (exit {code}): {tail(err)}")
    if len(out) != data.PEPPER_LEN:
        raise ip.StepError(f"the box's pepper is {len(out)} bytes, not {data.PEPPER_LEN}")
    return out


def backups(ip) -> Path:
    return ip.env_path().parent / "box-backup"


def write_private(path: Path, blob: bytes) -> None:
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as f:
        f.write(blob)
        f.flush()
        os.fsync(f.fileno())


def load_backup(ip, root: Path, stamp: str) -> tuple[bytes, dict[str, str], bytes]:
    """One backup, checked whole: the zip against its manifest, the pepper's length."""
    if not STAMP.fullmatch(stamp):
        raise ip.Refused("a backup's name is its stamp, <yyyymmdd>T<hhmmss>: nothing was changed")
    folder = root / stamp
    try:
        raw = (folder / "band.zip").read_bytes()
        manifest = json.loads((folder / "manifest.json").read_text(encoding="utf-8"))
        value = (folder / "pepper").read_bytes()
    except (OSError, ValueError):
        raise ip.Refused(f"backup {stamp} is not whole: nothing was changed") from None
    got, sums = data.unpack(ip, json.dumps({"manifest": manifest, "zip": base64.b64encode(raw).decode()}), "the backup")
    if len(value) != data.PEPPER_LEN:
        raise ip.Refused(f"backup {stamp}: its pepper is not {data.PEPPER_LEN} bytes: nothing was changed")
    return got, sums, value


def save_backup(ip, root: Path, stamp: str, raw: bytes, manifest: dict[str, str], value: bytes) -> None:
    """Written whole beside its final name, renamed in, read back."""
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(root, 0o700)
    staging = root / f".{stamp}.incoming"
    staging.mkdir(mode=0o700)
    write_private(staging / "band.zip", raw)
    write_private(staging / "manifest.json", json.dumps(manifest, indent=1, sort_keys=True).encode())
    write_private(staging / "pepper", value)
    dir_fd = os.open(staging, os.O_RDONLY)
    try:
        os.fsync(dir_fd)
    finally:
        os.close(dir_fd)
    os.rename(staging, root / stamp)
    root_fd = os.open(root, os.O_RDONLY)
    try:
        os.fsync(root_fd)
    finally:
        os.close(root_fd)
    load_backup(ip, root, stamp)


def run_backup(ctx, ip) -> int:
    """`iempc gen3-backup`."""
    host = g3.gen3_host(ip, ctx.env, BACKUP_WORDS)
    raw, manifest = read_band(ip, host)
    value = read_pepper(ip, host)
    stamp = time.strftime("%Y%m%dT%H%M%S")
    save_backup(ip, backups(ip), stamp, raw, manifest, value)
    del value
    ip.emit({"gen3_backup": {"stamp": stamp, "members": len(manifest), "bytes": len(raw), "pepper": "kept"}})
    return 0
