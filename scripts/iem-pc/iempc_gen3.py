"""What the box's gates and tools share (#72): the box's paths and ssh words,
the device's status over ssh, the build the box runs, and the tokens minted on
the box for a browser run (each only between its mint and its read there, in an
owner-only folder here, never on a command line and never printed)."""
from __future__ import annotations

import json
import os
import re
import shutil
import sys
import time
from dataclasses import dataclass
from pathlib import Path


# The device on the Gen 3 box (scripts/gen3/deploy.sh installs it there).
DANTE_BIN = "/opt/iemmixer/bin/iem-dante-dev"
SSH = ("ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10")
RECEIVING = 0x0101_0009   # ARC 0x3000's Receiving, as the status's `channels` name it
# dstack's SubStatus values that carry audio (the status word's low 16 bits; the high 16 are a receiver detail):
# 9 Subscribed (unicast), 10 Subscribed (multicast, dstack v0.6.54, #72).
RECEIVING_VALUES = (9, 10)


def is_receiving(code: object) -> bool:
    """Whether a channel's status word shows a subscription that receives (pure)."""
    return type(code) is int and (code & 0xFFFF) in RECEIVING_VALUES
CTL_S = 30   # one ctl call over ssh: the connect, then the socket's own 1 s


@dataclass(frozen=True)
class Words:
    """How a command names itself in what the shared steps say."""
    cmd: str    # the command: the head of a failure


def job_run() -> int:
    """The HIL job's run number (tests replace it)."""
    return int(time.time())


def gen3_host(ip, env: dict[str, str], words: Words) -> str:
    host = env.get("GEN3_HOST", "")
    if not host:
        raise ip.Refused(f"{words.cmd}: no GEN3_HOST in the private env (the Gen 3 box's ssh destination)")
    if host.startswith("-"):
        raise ip.Refused(f"{words.cmd}: GEN3_HOST must be an ssh destination, not an option")
    return host


# ---- the Gen 3 device ----

def ctl(ip, host: str, words: list[str], event: str) -> str:
    """`iem-dante-dev ctl <words>` on the Gen 3 box, its stdout. Bounded by
    CTL_S, the flag watched as `event` says (iempc_core.guarded)."""
    return ip.guarded([*SSH, host, DANTE_BIN, "ctl", *words], "", CTL_S, event)


def device_status(ip, ctx, host: str, words: Words) -> dict:
    out = ctl(ip, host, ["status"], ctx.watch(abandon=True))
    lines = [line for line in out.splitlines() if line.strip()]
    try:
        doc = json.loads(lines[-1]) if lines else None
    except ValueError:
        doc = None
    if not isinstance(doc, dict):
        raise ip.StepError(f"{words.cmd}: the Gen 3 device's status is not a JSON object")
    return doc


def rows(status: dict, key: str) -> list[dict]:
    value = status.get(key)
    if not isinstance(value, list) or not all(isinstance(r, dict) for r in value):
        raise ValueError(key)
    return value


# ---- the engine's spare outputs ----


def spare_peaks(engine) -> dict[int, float] | None:
    """The engine's HIL spare outputs and each one's peak since its previous
    Status (`engine.hil`: [{tx, peak}]); None without that list."""
    hil = engine.get("hil") if isinstance(engine, dict) else None
    if not isinstance(hil, list):
        return None
    peaks = {}
    for out in hil:
        tx, peak = (out.get("tx"), out.get("peak")) if isinstance(out, dict) else (None, None)
        if type(tx) is not int or type(peak) not in (int, float):
            return None
        peaks[tx] = float(peak)
    return peaks


# ---- the verdict ----


# ---- the command ----


REPO = Path(__file__).resolve().parent.parent.parent
E2E = REPO / "e2e"
ENGINE_BIN = "/opt/iemmixer/bin/iem-engine"
SOAKCLIENT = "/opt/iemmixer/bin/iem-soakclient"
# The path of the box server's JWT key file, beside its site file.
SIGNER_PATH = "/var/lib/iemmixer/site/secrets/jwt_secret"
# The service user's mint folder on the box (#72 phase D; provision
# makes it, 0700 iemmixer): each token there only between its mint and its read.
BOX_TOKENS = "/var/lib/iemmixer/mint"
# Every call on the box's tokens runs as the service user, the jwt_secret's
# owner: a line of scripts/gen3/sudo-lines.txt each.
AS_SERVICE = ("sudo", "-n", "-u", "iemmixer")
TOKENS = ("engineer", "member")
JWT = re.compile(r"[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+")
BUILD_LINE = re.compile(r"iem-engine \S+\+([0-9a-f]{40})")   # `iem-engine <version>+<GITHUB_SHA>`
# The PIN variables of the mock E2E run: never in a browser run's environment.
NO_PINS = ("E2E_ENGINEER_PIN", "E2E_MEMBER_PIN")


# ---- arguments and the test site ----


# ---- the box ----


def box_build(ip, ctx, host: str) -> str:
    """The commit the box's engine was built from (`iem-engine --version`);
    StepError when it names none (a local build)."""
    out = ip.guarded([*SSH, host, ENGINE_BIN, "--version"], "", CTL_S, ctx.watch(abandon=True))
    lines = [line.strip() for line in out.splitlines() if line.strip()]
    m = BUILD_LINE.fullmatch(lines[-1]) if lines else None
    if not m:
        raise ip.StepError("the box's iem-engine --version names no CI build (<version>+<40 hex>)")
    return m.group(1)


# ---- the tokens ----

def box_token(name: str) -> str:
    return f"{BOX_TOKENS}/{name}.token"


def mint_tokens(ip, ctx, host: str, member: str, folder: Path, seconds: int, cmd: str) -> Path:
    """An engineer and a member token minted on the box as the service user
    with its server's own JWT secret, valid `seconds`, each read back over ssh's stdout into
    a new owner-only file of `folder` (this box's, owner-only) and removed on
    the box whatever happened; returns their `{engineer, member}` file
    (LIVE_TOKENS), owner-only. A token never travels on a command line and is
    never printed. One ssh call per command (one `sudo -n` each, as the sudo
    template lists it): the removal of any left from an earlier run (the
    mint never writes over a file), the two mints, the two reads. `cmd`
    heads its errors."""
    remove = [*SSH, host, *AS_SERVICE, "rm", "-f", *map(box_token, TOKENS)]
    try:
        ip.guarded(remove, "", CTL_S, ctx.watch(abandon=False))
        for name in TOKENS:
            sub = ["--sub", "engineer", "--engineer"] if name == "engineer" else ["--sub", member]
            ip.guarded([*SSH, host, *AS_SERVICE, SOAKCLIENT, "token", "--jwt-secret-file", SIGNER_PATH, *sub,
                        "--seconds", str(seconds), "--out", box_token(name)], "", CTL_S,
                       ctx.watch(abandon=False))
        for name in TOKENS:
            token = ip.guarded([*SSH, host, *AS_SERVICE, "cat", box_token(name)], "", CTL_S,
                               ctx.watch(abandon=False))
            keep_token(ip, folder / f"{name}.token", token, name, cmd)
    finally:
        remove_box_tokens(ip, host)
    return tokens_file(ip, folder, cmd)


def keep_token(ip, path: Path, token: str, name: str, cmd: str) -> None:
    """A token read from the box into a new owner-only file (never over
    one); StepError naming no token when it cannot be written."""
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(token)
    except OSError as e:
        raise ip.StepError(f"{cmd}: the {name} token read from the box was not kept "
                           f"({e.strerror or type(e).__name__})") from None


# ssh's own failure (exit 255: no connection, a refused key, a lost session):
# its stderr names the box's address, so a warning says these words instead (P6).
SSH_FAILED = "ssh to GEN3_HOST failed (exit 255)"


def ssh_words(e: Exception) -> str:
    """A failed box call as a warning may quote it: ssh's own failure as
    SSH_FAILED, any other as it is (the box's command's own words)."""
    return SSH_FAILED if str(e).startswith(f"{SSH[0]} failed (exit 255)") else str(e)


def remove_box_tokens(ip, host: str) -> None:
    """`rm -f` of the minted tokens on the box as the service user, after any
    outcome (a flag too: a credential is never left there). A failure is a
    warning naming the command to run by hand; it never raises."""
    words = [*AS_SERVICE, "rm", "-f", *map(box_token, TOKENS)]
    try:
        ip.guarded([*SSH, host, *words], "", CTL_S, "ignore")
    except ip.StepError as e:
        print(f"iempc: WARNING: the tokens minted on the Gen 3 box were not removed ({ssh_words(e)}): "
              "remove them by hand "
              f"(ssh <GEN3_HOST> {' '.join(words)})", file=sys.stderr, flush=True)


def tokens_file(ip, folder: Path, cmd: str) -> Path:
    """The copied tokens as `{engineer, member}` in `folder/tokens.json`, a
    new owner-only file; each copy is removed once read. StepError (naming no
    token) when one is not JWT-shaped."""
    tokens = {}
    for name in TOKENS:
        copy = folder / f"{name}.token"
        try:
            text = copy.read_text(encoding="utf-8").strip()
        except (OSError, UnicodeError):
            text = ""
        copy.unlink(missing_ok=True)
        if not JWT.fullmatch(text):
            raise ip.StepError(f"{cmd}: the {name} token copied from the box is not a JWT (it is not shown)")
        tokens[name] = text
    path = folder / "tokens.json"
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(tokens, f)
    return path


def drop_folder(folder: Path) -> None:
    """The tokens' folder on this box goes on every exit; one left is a warning."""
    shutil.rmtree(folder, ignore_errors=True)
    if folder.exists():
        print(f"iempc: WARNING: {folder} (the browser's tokens) was not removed: remove it by hand", file=sys.stderr,
              flush=True)


# ---- the job ----


# ---- the verdict ----


# ---- the command ----
