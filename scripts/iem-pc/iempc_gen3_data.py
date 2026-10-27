"""The band's data from the IEM PC onto the Gen 3 box (#72 phase E Task 5;
plan docs/superpowers/plans/2026-10-27-gen3-phase-e-band-test.md, the design
comment on #72 of 2026-10-27). One direction: the switch to 3.x takes the
PC's data; the way back returns to the PC's state at the switch.

`iempc gen3-data [--pepper] [--dry-run]` (dev time, locked; a PC call that
only reads):
1. The box's stack must be stopped (`systemctl is-active` of its three units).
2. One PowerShell session on the PC (`pc_script`) reads the server's band
   files and the engine's saved state, zips them in memory and prints one
   JSON line: each member's sha256 and the zip as base64. The PC's server
   keeps running: each of its files is written whole by a rename, so a read
   gets one version.
3. The zip is checked here against that manifest, never written to disk,
   and goes on the box's stdin to band_receive.py as the service user
   (`RECEIVE`), whose answer must name the same sha256s.
4. `--pepper`: a second PC session unprotects the server's DPAPI pepper and
   prints it as base64; its 32 bytes go on the box's stdin to `systemd-creds
   encrypt` (`ENCRYPT`), replacing the box's own, made owner-only (`CHMOD`:
   systemd-creds keeps sudo's umask) and read back by `stat` (600, root). Never printed, logged or written in clear on
   the dev box. Without it the box keeps its pepper and the band's PIN
   hashes do not verify there (the members' sessions still do: the same
   `jwt_secret`).
`--dry-run` stops after step 3's check: nothing reaches the box.

stdout: one JSON object of counts and fixed words; no file name, no value."""
from __future__ import annotations

import base64
import binascii
import hashlib
import io
import json
import subprocess
import zipfile

import iempc_core as core

# The same lists as scripts/gen3/band_receive.py (test_iempc_gen3_data compares them).
SERVER_FILES = ("secrets/jwt_secret", "secrets/vapid_private", "secrets/pin_hashes.json", "roster.toml",
                "push_subscriptions.json", "push_subs_v2_migrated", "alarm_subscriptions.json")
SERVER_FOLDERS = ("lineups", "photos", "presets", "snapshots", "customizations", "backups")
UNITS = ("iem-server", "iem-engine", "iem-dante")
RECEIVE = "sudo -n -u iemmixer python3 -I /opt/iemmixer/lib/band_receive.py /var/lib/iemmixer/site /var/lib/iemmixer/engine"
CREDENTIAL = "/etc/credstore.encrypted/iemmixer-pepper"
ENCRYPT = f"sudo -n systemd-creds encrypt --name=pepper --with-key=auto - {CREDENTIAL}"
# systemd-creds keeps sudo's umask (0022): the credential is made owner-only after it.
CHMOD = f"sudo -n chmod 0600 {CREDENTIAL}"
STAT = f"sudo -n stat -c %a.%U.%s {CREDENTIAL}"
SSH = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]
PC_S = 180
BOX_S = 120
PEPPER_LEN = 32


def pc_script(root: str) -> str:
    """The PC's side: the band's files as one zip in memory, one JSON line out
    (`{"manifest": {member: sha256}, "zip": base64}`); single-line
    statements, as `-Command -` reads them."""
    files = ", ".join(f"'{f}'" for f in SERVER_FILES)
    folders = ", ".join(f"'{f}'" for f in SERVER_FOLDERS)
    return "\n".join([
        "$ErrorActionPreference = 'Stop'",
        "Add-Type -AssemblyName System.IO.Compression",
        "$srv = Join-Path $env:LOCALAPPDATA 'iemmixer\\server'; $eng = Join-Path " + core.ps_quote(root) + " 'engine'",
        "$sha = [Security.Cryptography.SHA256]::Create(); $ms = New-Object IO.MemoryStream; "
        "$zip = New-Object IO.Compression.ZipArchive($ms, [IO.Compression.ZipArchiveMode]::Create, $true); "
        "$man = [ordered]@{}",
        "function Add-IemMember($name, $path) { $b = [IO.File]::ReadAllBytes($path); "
        "$s = $zip.CreateEntry($name).Open(); $s.Write($b, 0, $b.Length); $s.Dispose(); "
        "$script:man[$name] = -join ($sha.ComputeHash($b) | ForEach-Object { $_.ToString('x2') }) }",
        f"foreach ($f in @({files})) {{ $p = Join-Path $srv ($f -replace '/', '\\'); "
        "if (Test-Path -LiteralPath $p -PathType Leaf) { Add-IemMember ('server/' + $f) $p } }",
        f"foreach ($d in @({folders})) {{ $p = Join-Path $srv $d; if (Test-Path -LiteralPath $p -PathType Container) "
        "{ Get-ChildItem -LiteralPath $p -Recurse -File -Force | ForEach-Object { "
        "Add-IemMember ('server/' + ($_.FullName.Substring($srv.Length + 1) -replace '\\\\', '/')) $_.FullName } } }",
        "Get-ChildItem -LiteralPath $eng -File -Force | Where-Object { $_.Name -in @('current.json', 'baseline.json') "
        "-or $_.Name -match '^current\\.json\\.rev-[0-9]+$' -or $_.Name -match '^gen-.*\\.json$' } | "
        "ForEach-Object { Add-IemMember ('engine/' + $_.Name) $_.FullName }",
        "$zip.Dispose(); [pscustomobject]@{ manifest = $man; zip = [Convert]::ToBase64String($ms.ToArray()) } | "
        "ConvertTo-Json -Compress -Depth 3",
    ])


PEPPER_SCRIPT = "\n".join([
    "$ErrorActionPreference = 'Stop'",
    "Add-Type -AssemblyName System.Security",
    "$u = [Security.Cryptography.ProtectedData]::Unprotect([IO.File]::ReadAllBytes((Join-Path $env:LOCALAPPDATA "
    "'iemmixer\\server\\secrets\\pepper.dpapi')), $null, 'CurrentUser'); [Convert]::ToBase64String($u); "
    "[Array]::Clear($u, 0, $u.Length)",
])


def band_zip(ip, env: dict[str, str], event: str) -> tuple[bytes, dict[str, str]]:
    """The PC's zip and its manifest, every member's sha256 checked here."""
    out = ip.ssh_ps(env, pc_script(env["PC_ROOT"]), PC_S, event)
    lines = [line for line in out.splitlines() if line.strip()]
    try:
        doc = json.loads(lines[-1]) if lines else None
        raw = base64.b64decode(doc["zip"], validate=True)
        manifest = doc["manifest"]
        with zipfile.ZipFile(io.BytesIO(raw)) as z:
            got = {i.filename: hashlib.sha256(z.read(i)).hexdigest() for i in z.infolist()}
    except (ValueError, TypeError, KeyError, binascii.Error, zipfile.BadZipFile):
        raise ip.StepError("gen3-data: the PC's answer is not the band's zip and its manifest") from None
    if not isinstance(manifest, dict) or got != manifest:
        raise ip.StepError("gen3-data: the PC's zip does not match its own manifest")
    if "engine/current.json" not in got:
        raise ip.StepError("gen3-data: the PC's zip holds no engine/current.json")
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


def stopped(ip, host: str) -> None:
    if not quiet(host):
        raise ip.Refused("gen3-data: the box's stack is not stopped (a unit runs, or a process is left in one): "
                         "nothing was sent")


def pepper(ip, env: dict[str, str], host: str, event: str) -> None:
    """The PC's pepper into the box's credential, read back."""
    out = ip.ssh_ps(env, PEPPER_SCRIPT, PC_S, event)
    lines = [line.strip() for line in out.splitlines() if line.strip()]
    try:
        value = base64.b64decode(lines[-1], validate=True) if lines else b""
    except (ValueError, binascii.Error):
        value = b""
    if len(value) != PEPPER_LEN:
        raise ip.StepError("gen3-data: the PC's pepper is not 32 bytes: the box's credential is unchanged")
    code, _, err = box(host, ENCRYPT, value, BOX_S)
    del value
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


def send_band(ip, env: dict[str, str], host: str, event: str) -> dict:
    """The PC's band files onto the box (stopped) and read back: the counts."""
    stopped(ip, host)
    raw, manifest = band_zip(ip, env, event)
    if ip.event_now():
        raise ip.Refused("gen3-data: the flag appeared: nothing was sent to the box")
    code, out, err = box(host, RECEIVE, raw, BOX_S)
    if code != 0:
        raise ip.StepError(f"gen3-data: the box's receiver failed (exit {code}): "
                           f"{err.decode(errors='replace').strip()[-300:]}")
    try:
        written = json.loads(out.decode().strip().splitlines()[-1])["written"]
    except (ValueError, IndexError, KeyError, TypeError):
        raise ip.StepError("gen3-data: the box's receiver gave no answer to read back") from None
    if written != manifest:
        raise ip.StepError("gen3-data: what the box wrote does not read back as the PC's files")
    return {"members": len(manifest), "bytes": len(raw), "written": len(written), "manifest": manifest}


def host_of(ip, env: dict[str, str], cmd: str) -> str:
    host = env.get("GEN3_HOST", "")
    if not host or host.startswith("-"):
        raise ip.Refused(f"{cmd}: no GEN3_HOST in the private env (the Gen 3 box's ssh destination)")
    return host


def run(ctx, ip) -> int:
    """`iempc gen3-data` (dev time, locked)."""
    args, env = ctx.args, ctx.env
    host = host_of(ip, env, "gen3-data")
    if args.dry_run:
        stopped(ip, host)
        raw, manifest = band_zip(ip, env, ctx.watch(abandon=True))
        ip.emit({"gen3_data": {"members": len(manifest), "bytes": len(raw), "dry_run": True}})
        return 0
    report = {**send_band(ip, env, host, ctx.watch(abandon=True)), "dry_run": False}
    report = {k: report[k] for k in ("members", "bytes", "dry_run", "written")}
    if args.pepper:
        pepper(ip, env, host, ctx.watch(abandon=False))
        report["pepper"] = "replaced"
    ip.emit({"gen3_data": report})
    return 0
