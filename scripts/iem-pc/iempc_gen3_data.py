"""The band's data on the Gen 3 box (#72): the box's calls that `iempc gen3-new-box`
and `gen3-backup` share. `box` is one ssh command on the box, never ended by
force; `unpack` checks band_send.py's zip against its manifest; `quiet` reads
whether the stack is stopped; `receive` puts a checked zip through
band_receive.py and reads it back; `install_pepper` writes the pepper into the
box's credential and reads it back (its value only on ssh's stdin)."""
from __future__ import annotations

import base64
import binascii
import hashlib
import io
import json
import subprocess
import zipfile


UNITS = ("iem-server", "iem-engine", "iem-dante")
RECEIVE = "sudo -n -u iemmixer python3 -I /opt/iemmixer/lib/band_receive.py /var/lib/iemmixer/site /var/lib/iemmixer/engine"
CREDENTIAL = "/etc/credstore.encrypted/iemmixer-pepper"
ENCRYPT = f"sudo -n systemd-creds encrypt --name=pepper --with-key=auto - {CREDENTIAL}"
# systemd-creds keeps sudo's umask (0022): the credential is made owner-only after it.
CHMOD = f"sudo -n chmod 0600 {CREDENTIAL}"
STAT = f"sudo -n stat -c %a.%U.%s {CREDENTIAL}"
SSH = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]
BOX_S = 120
PEPPER_LEN = 32


def unpack(ip, out: str, source: str) -> tuple[bytes, dict[str, str]]:
    """`source`'s one JSON line as its zip and manifest, every member's sha256
    checked here (the PC's script or the box's band_send.py)."""
    lines = [line for line in out.splitlines() if line.strip()]
    try:
        doc = json.loads(lines[-1]) if lines else None
        raw = base64.b64decode(doc["zip"], validate=True)
        manifest = doc["manifest"]
        with zipfile.ZipFile(io.BytesIO(raw)) as z:
            got = {i.filename: hashlib.sha256(z.read(i)).hexdigest() for i in z.infolist()}
    except (ValueError, TypeError, KeyError, binascii.Error, zipfile.BadZipFile):
        raise ip.StepError(f"gen3-data: {source}'s answer is not the band's zip and its manifest") from None
    if not isinstance(manifest, dict) or got != manifest:
        raise ip.StepError(f"gen3-data: {source}'s zip does not match its own manifest")
    if "engine/current.json" not in got:
        raise ip.StepError(f"gen3-data: {source}'s zip holds no engine/current.json")
    return raw, manifest


STILL_RUNNING = 124   # box()'s exit for a command past its bound, left running (never ended by force)


def box(host: str, command: str, data: bytes, timeout: float) -> tuple[int, bytes, bytes]:
    """One ssh command on the box, `data` on its stdin (tests replace it).
    Past `timeout` it is left to end by itself, never ended by force (I8):
    exit STILL_RUNNING and that said on stderr."""
    proc = subprocess.Popen([*SSH, host, command], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE)
    try:
        out, err = proc.communicate(data, timeout=timeout)
    except subprocess.TimeoutExpired:
        return STILL_RUNNING, b"", f"still running after {timeout:g} s, left to end by itself".encode()
    return proc.returncode, out, err


STATES = "systemctl is-active " + " ".join(UNITS)
# A stop past its bound (SendSIGKILL=no, I8) can leave a unit `failed`, or
# `inactive` after reset-failed, with its process alive: stopped is also an
# empty process group.
TASKS = "systemctl show -p TasksCurrent --value " + " ".join(UNITS)


def quiet(host: str) -> bool:
    """None of the stack's units runs: each `inactive` or `failed`, and no
    process left in any of their groups."""
    code, out, _ = box(host, STATES, b"", BOX_S)
    states = out.decode(errors="replace").split()
    if len(states) != len(UNITS) or any(s not in ("inactive", "failed") for s in states):
        return False
    code, out, _ = box(host, TASKS, b"", BOX_S)
    # One value per unit, a blank line between units (as systemd prints them).
    tasks = [line.strip() for line in out.decode(errors="replace").splitlines() if line.strip()]
    return code == 0 and len(tasks) == len(UNITS) and all(t in ("0", "[not set]") for t in tasks)


def install_pepper(ip, host: str, value: bytes) -> None:
    """32 bytes into the box's credential, made owner-only, read back. The
    value only on ssh's stdin (iempc_gen3_newbox has it from another box)."""
    code, _, err = box(host, ENCRYPT, value, BOX_S)
    if code != 0:
        raise ip.StepError(f"gen3-data: systemd-creds encrypt failed on the box (exit {code}): "
                           f"{err.decode(errors='replace').strip()[-300:]}")
    code, _, err = box(host, CHMOD, b"", BOX_S)
    if code != 0:
        raise ip.StepError(f"gen3-data: the box's pepper credential could not be made owner-only (exit {code}): "
                           f"{err.decode(errors='replace').strip()[-300:]}")
    code, out_b, _ = box(host, STAT, b"", BOX_S)
    mode, _, rest = out_b.decode(errors="replace").strip().partition(".")
    owner = rest.partition(".")[0]
    if code != 0 or (mode, owner) != ("600", "root"):
        raise ip.StepError("gen3-data: the box's pepper credential does not read back as 600 root")


def receive(ip, host: str, raw: bytes, manifest: dict[str, str]) -> dict[str, str]:
    """A checked zip through the box's receiver, what it wrote read back."""
    code, out, err = box(host, RECEIVE, raw, BOX_S)
    if code != 0:
        raise ip.StepError(f"gen3-data: the box's receiver failed (exit {code}): "
                           f"{err.decode(errors='replace').strip()[-300:]}")
    try:
        written = json.loads(out.decode().strip().splitlines()[-1])["written"]
    except (ValueError, IndexError, KeyError, TypeError):
        raise ip.StepError("gen3-data: the box's receiver gave no answer to read back") from None
    if written != manifest:
        raise ip.StepError("gen3-data: what the box wrote does not read back as the sent files")
    return written


