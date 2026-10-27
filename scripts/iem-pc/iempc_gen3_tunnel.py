"""The band's tunnel connector between the IEM PC and the Gen 3 box (#72
phase E Task 6; the design comment on #72 of 2026-10-27). One remotely
managed tunnel sends the band's public host to http://127.0.0.1:80; exactly
one connector of it runs: the PC's Windows service (2.x) or the box's
`iem-tunnel` unit (3.x). Who answers is read from the public URL's
`/api/version` (`git_hash`).

`iempc gen3-tunnel token|to-box|to-pc` (locked; no dev entry, no engine):
- `token`: the PC's connector's token (its service command line) straight
  into the box's credential (`systemd-creds encrypt --name=tunnel`, then
  0600, read back 600 root), never printed, logged or stored in clear here.
- `to-box`: the PC's service stopped and set Disabled (the PC's server's
  tunnel watch then cannot start it again), read back; the box's unit
  started (Type=notify: started means connected); the public URL must name
  the box's build within PUBLIC_S. Any failure after the PC's stop puts the
  PC's connector back (the box's stopped, the PC's Automatic and started).
- `to-pc`: the reverse, the box's unit stopped first; it runs during an
  event too (the owner's way back to 2.x); `token` and `to-box` refuse while
  the "ide event" flag exists.

stdout: one JSON object of fixed words and the two short hashes."""
from __future__ import annotations

import json
import subprocess
import time
import urllib.request

CREDENTIAL = "/etc/credstore.encrypted/iemmixer-tunnel"
ENCRYPT = f"sudo -n systemd-creds encrypt --name=tunnel --with-key=auto - {CREDENTIAL}"
CHMOD = f"sudo -n chmod 0600 {CREDENTIAL}"
STAT = f"sudo -n stat -c %a.%U.%s {CREDENTIAL}"
BOX_START = "sudo -n systemctl start iem-tunnel"
BOX_STOP = "sudo -n systemctl stop iem-tunnel"
BOX_STATE = "systemctl is-active iem-tunnel"
BOX_VERSION = "curl -s --max-time 5 http://127.0.0.1:80/api/version"
SSH = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]
SERVICE = "Cloudflared"
PC_S = 120
BOX_S = 90
PUBLIC_S = 60
POLL_S = 2.0

TOKEN_SCRIPT = (f"$s = Get-CimInstance Win32_Service -Filter \"Name='{SERVICE}'\"; "
                "if ($s -and $s.PathName -match '--token\\s+(\\S+)') { $Matches[1] } else { 'no-token' }")
PC_STATE = f"$s = Get-CimInstance Win32_Service -Filter \"Name='{SERVICE}'\"; $s.State + ' ' + $s.StartMode"
PC_STOP = (f"Stop-Service -Name {SERVICE}; Set-Service -Name {SERVICE} -StartupType Disabled; {PC_STATE}")
PC_START = (f"Set-Service -Name {SERVICE} -StartupType Automatic; Start-Service -Name {SERVICE}; {PC_STATE}")
PC_VERSION = ("try { (Invoke-WebRequest -UseBasicParsing -TimeoutSec 5 http://127.0.0.1:80/api/version).Content } "
              "catch { 'none' }")


def box(host: str, command: str, data: bytes, timeout: float) -> tuple[int, bytes, bytes]:
    """One ssh command on the box, `data` on its stdin (tests replace it)."""
    r = subprocess.run([*SSH, host, command], input=data, capture_output=True, timeout=timeout)
    return r.returncode, r.stdout, r.stderr


def public_hash(url: str) -> str | None:
    """The public URL's /api/version `git_hash`, None when it does not answer
    (tests replace it)."""
    try:
        with urllib.request.urlopen(url.rstrip("/") + "/api/version", timeout=5) as r:
            return json.loads(r.read().decode()).get("git_hash")
    except (OSError, ValueError, AttributeError):
        return None


def hash_of(text: str) -> str | None:
    try:
        value = json.loads(text.strip().splitlines()[-1]).get("git_hash")
    except (ValueError, IndexError, AttributeError):
        return None
    return value if isinstance(value, str) and value else None


def settings(ip, env: dict[str, str]) -> tuple[str, str]:
    host, url = env.get("GEN3_HOST", ""), env.get("GEN3_PUBLIC_URL", "")
    if not host or host.startswith("-"):
        raise ip.Refused("gen3-tunnel: no GEN3_HOST in the private env (the Gen 3 box's ssh destination)")
    if not url.startswith("https://"):
        raise ip.Refused("gen3-tunnel: no GEN3_PUBLIC_URL in the private env (the band's public https:// URL)")
    return host, url


def wait_public(url: str, want: str, bound: float) -> bool:
    deadline = time.monotonic() + bound
    while True:
        if public_hash(url) == want:
            return True
        if time.monotonic() > deadline:
            return False
        time.sleep(POLL_S)


def token(ip, env: dict[str, str], host: str) -> None:
    """The PC's connector token into the box's credential, read back."""
    out = ip.ssh_ps(env, TOKEN_SCRIPT, PC_S, "abandon")
    lines = [line.strip() for line in out.splitlines() if line.strip()]
    value = lines[-1].encode() if lines and lines[-1] != "no-token" else b""
    if len(value) < 40 or not value.isascii() or b" " in value:
        raise ip.StepError("gen3-tunnel: the PC's connector names no token: the box's credential is unchanged")
    code, _, err = box(host, ENCRYPT, value, BOX_S)
    del value
    if code != 0:
        raise ip.StepError(f"gen3-tunnel: systemd-creds encrypt failed on the box (exit {code}): "
                           f"{err.decode(errors='replace').strip()[-300:]}")
    credential_ok(ip, host)


def credential_ok(ip, host: str, make_owner_only: bool = True) -> None:
    """The box's tunnel credential reads back 600 root; after an encrypt it is
    made so first (systemd-creds keeps sudo's umask)."""
    code = box(host, CHMOD, b"", BOX_S)[0] if make_owner_only else 0
    code2, out, _ = box(host, STAT, b"", BOX_S)
    mode, _, rest = out.decode(errors="replace").strip().partition(".")
    if code != 0 or code2 != 0 or (mode, rest.partition(".")[0]) != ("600", "root"):
        raise ip.StepError("gen3-tunnel: the box's tunnel credential does not read back as 600 root (run "
                           "`iempc gen3-tunnel token` first)")


def pc_state(ip, env: dict[str, str], script: str, want: str, what: str) -> None:
    out = ip.ssh_ps(env, script, PC_S, "ignore")
    lines = [line.strip() for line in out.splitlines() if line.strip()]
    if not lines or lines[-1] != want:
        raise ip.StepError(f"gen3-tunnel: the PC's connector did not {what} (reads '{lines[-1] if lines else ''}')")


def box_hash(ip, host: str) -> str:
    code, out, _ = box(host, BOX_VERSION, b"", BOX_S)
    value = hash_of(out.decode(errors="replace")) if code == 0 else None
    if not value:
        raise ip.StepError("gen3-tunnel: the box's server does not answer /api/version on port 80")
    return value


def pc_hash(ip, env: dict[str, str]) -> str:
    value = hash_of(ip.ssh_ps(env, PC_VERSION, PC_S, "ignore"))
    if not value:
        raise ip.StepError("gen3-tunnel: the PC's server does not answer /api/version on port 80")
    return value


def to_pc(ip, env: dict[str, str], host: str, url: str) -> dict:
    """The box's connector stopped, then the PC's enabled and started; the
    public URL must name the PC's build."""
    code, _, err = box(host, BOX_STOP, b"", BOX_S)
    code2, out, _ = box(host, BOX_STATE, b"", BOX_S)
    if code != 0 or out.decode(errors="replace").strip() != "inactive":
        raise ip.StepError(f"gen3-tunnel: the box's connector did not stop (exit {code}, {code2}): "
                           f"{err.decode(errors='replace').strip()[-300:]}")
    pc_state(ip, env, PC_START, "Running Auto", "start")
    want = pc_hash(ip, env)
    if not wait_public(url, want, PUBLIC_S):
        raise ip.StepError(f"gen3-tunnel: the public URL does not name the PC's build {want} within {PUBLIC_S} s")
    return {"serves": "pc", "build": want}


def to_box(ip, env: dict[str, str], host: str, url: str) -> dict:
    credential_ok(ip, host, make_owner_only=False)   # the token is there (`gen3-tunnel token` once)
    want = box_hash(ip, host)
    pc_state(ip, env, PC_STOP, "Stopped Disabled", "stop and stay disabled")
    try:
        code, _, err = box(host, BOX_START, b"", BOX_S)
        if code != 0:
            raise ip.StepError(f"gen3-tunnel: the box's connector did not start (exit {code}): "
                               f"{err.decode(errors='replace').strip()[-300:]}")
        if not wait_public(url, want, PUBLIC_S):
            raise ip.StepError(f"gen3-tunnel: the public URL does not name the box's build {want} within "
                               f"{PUBLIC_S} s")
    except ip.StepError as e:
        try:
            to_pc(ip, env, host, url)
        except ip.StepError as back:
            raise ip.StepError(f"{e}; putting the PC's connector back failed too: {back}") from None
        raise ip.StepError(f"{e}; the PC's connector serves again") from None
    return {"serves": "box", "build": want}


def run(ctx, ip) -> int:
    """`iempc gen3-tunnel`."""
    verb = ctx.args.verb
    host, url = settings(ip, ctx.env)
    if verb in ("token", "to-box") and ip.event_now():
        raise ip.Refused(f"gen3-tunnel {verb}: the flag exists: an event is on; only to-pc runs then")
    if verb == "token":
        token(ip, ctx.env, host)
        ip.emit({"gen3_tunnel": {"token": "stored"}})
    elif verb == "to-box":
        ip.emit({"gen3_tunnel": to_box(ip, ctx.env, host, url)})
    else:
        ip.emit({"gen3_tunnel": to_pc(ip, ctx.env, host, url)})
    return 0
