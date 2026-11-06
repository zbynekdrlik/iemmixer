"""`iempc gen3-deploy --bundle <dir> --sha <40 hex> [--dry-run]` (#72, #86): the
Gen 3 box deployed in place from the dev box, through scripts/gen3/deploy.sh of
this checkout. The box serves the band (Gen 2 ended 2026-11-01), so in dev time
the deploy's own restart is the only interruption.

`--host GEN3_HOST --bundle --sha --config GEN3_DEVICE_CONFIG --iface --start
engine --site-file GEN3_SITE_FILE --roster-file GEN3_ROSTER_FILE`. The ops device
config is the device's whole identity: the name and the TX names the band's
receivers subscribe to. Nothing is built from another device, and nothing
records a side. The interface is the box's own: IEM_DANTE_IFACE of its
/etc/iemmixer/iem-dante.env (0644, read over ssh, never printed), which an
earlier deploy wrote; a box no deploy has reached is deployed by hand first
(`deploy.sh --iface`, .claude/rules/gen3-new-box.md). deploy.sh runs with
iempc's flag and private env (IEMMIXER_EVENT_NOW, PC_ENV); it checks the checkout
(exactly --sha, a clean tree), the bundle, its attestation and everything else
itself. Its stdout goes to stderr here once it succeeded; a failure's error
quotes its stderr.

The engine start the deploy makes counts in the box gates' restart budget
(iempc_gen3_gate's gen3-gate.json), so a gate right after it is refused,
naming the wait: deploy.sh appends a line to the file IEMPC_START_MARK names
(an empty file of iempc's state, removed after) right before it sends the
start; a marked start counts whatever deploy.sh's end, and so does a
deploy.sh left running past its bound. A refusal here, a deploy.sh refused
before its start and the dry run count nothing.

Dev time, locked, a box command (iempc's Spec: refused while EVENT-NOW exists).
A flag during the deploy: deploy.sh's own checks (nothing copied, nothing
installed, the stack not stopped; a flag never holds down the stack it
stopped), it runs to its end, then the event path (iempc main).

`--dry-run`: deploy.sh's options with the host and every path shown only as
its kind, no call of any kind.

stdout: one JSON object `{"gen3_deploy": {...}}`: fixed words, the sha and
booleans; no host, address, path, name or label (P6).

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import os
import re
import sys
import tempfile
from pathlib import Path

import iempc_gen3_data as data
import iempc_gen3_gate as gate

HERE = Path(__file__).resolve().parent
# deploy.sh of this checkout, the program and its script (tests put a stand-in here).
DEPLOY = ("bash", str(HERE.parent / "gen3" / "deploy.sh"))
# One deploy: the bundle's copy, its attestation, the stop's 10 s, the start
# and health's 30 tries. Past it deploy.sh is left to end by itself (I8).
DEPLOY_S = 900
ENV_FILE = "/etc/iemmixer/iem-dante.env"
IFACE_READ = f"cat {ENV_FILE}"
IFACE_KEY = "IEM_DANTE_IFACE="
IFACE = re.compile(r"[A-Za-z0-9_.-]{1,15}")   # deploy.sh's own rule for --iface
KEYS = ("GEN3_HOST", "GEN3_DEVICE_CONFIG", "GEN3_SITE_FILE", "GEN3_ROSTER_FILE")
FILES = ("GEN3_DEVICE_CONFIG", "GEN3_SITE_FILE", "GEN3_ROSTER_FILE")
# The dry run's words for what deploy.sh would get (P6: no host, no path).
KINDS = {
    "host": "<GEN3_HOST>",
    "bundle": "<--bundle>",
    "config": "<GEN3_DEVICE_CONFIG>",
    "iface": f"<the box's {IFACE_KEY[:-1]} ({ENV_FILE})>",
    "site": "<GEN3_SITE_FILE>",
    "roster": "<GEN3_ROSTER_FILE>",
}


def arguments(ip, args) -> tuple[str, Path]:
    """--sha and --bundle, refused before any call."""
    sha = ip.check_sha(args.sha)
    if not args.bundle or not Path(args.bundle).is_dir():
        raise ip.Refused("gen3-deploy: --bundle is not a folder (the downloaded gen3-bundle-<sha>): nothing was called")
    return sha, Path(args.bundle)


def settings(ip, env: dict[str, str]) -> dict[str, str]:
    """The private env's keys; each refusal names keys, never values."""
    missing = [k for k in KEYS if not env.get(k)]
    if missing:
        raise ip.Refused(f"gen3-deploy: {', '.join(missing)} missing in the private env")
    if env["GEN3_HOST"].startswith("-"):
        raise ip.Refused("gen3-deploy: GEN3_HOST must be an ssh destination, not an option")
    for key in FILES:
        if not Path(env[key]).is_file():
            raise ip.Refused(f"gen3-deploy: {key} is not a file")
    return {k: env[k] for k in KEYS}


def options(s: dict[str, str], sha: str, bundle: str, config: str, iface: str) -> list[str]:
    """deploy.sh's command line (the dry run passes the kinds in place of the values)."""
    return ["--host", s["GEN3_HOST"], "--bundle", bundle, "--sha", sha, "--config", config, "--iface", iface,
            "--start", "engine", "--site-file", s["GEN3_SITE_FILE"], "--roster-file", s["GEN3_ROSTER_FILE"]]


def box_iface(ip, host: str) -> str:
    """The Dante interface the box's stack runs on: the one line
    IEM_DANTE_IFACE=<name> of its env file (a deploy writes it, 0644)."""
    code, out, _ = data.box(host, IFACE_READ, b"", data.BOX_S)
    if code != 0:
        raise ip.StepError(f"gen3-deploy: the box's {ENV_FILE} could not be read (ssh exit {code}): a box no deploy "
                           "has reached is deployed by hand first (scripts/gen3/deploy.sh --iface): nothing was "
                           "deployed")
    values = [line[len(IFACE_KEY):].strip() for line in out.decode(errors="replace").splitlines()
              if line.startswith(IFACE_KEY)]
    if len(values) != 1 or not IFACE.fullmatch(values[0]):
        raise ip.StepError(f"gen3-deploy: the box's {ENV_FILE} names no one Dante interface ({IFACE_KEY}<name>): "
                           "nothing was deployed")
    return values[0]


def deploy_env(ip) -> dict[str, str]:
    """This process's environment with iempc's flag and private env."""
    return {**os.environ, "IEMMIXER_EVENT_NOW": str(ip.EVENT_NOW), "PC_ENV": str(ip.env_path())}


def start_mark(ip) -> Path:
    """An empty file of iempc's state folder deploy.sh marks right before it
    sends the engine's start (IEMPC_START_MARK)."""
    fd, name = tempfile.mkstemp(prefix="gen3-deploy-start-", dir=ip.state_dir())
    os.close(fd)
    return Path(name)


def count_start(ip, mark: Path, still_running: bool) -> None:
    """deploy.sh's engine start into the box gates' restart budget
    (gen3-gate.json) once deploy.sh marked it, whatever its end, or while
    it may still reach it (left running past its bound); the mark removed.
    An unreadable mark counts (an over-count only makes the next gate wait)."""
    try:
        started = still_running or mark.read_text(encoding="utf-8").strip() != ""
    except OSError:
        started = True
    mark.unlink(missing_ok=True)
    if started:
        gate.note_starts(ip)


def run(ctx, ip) -> int:
    """`iempc gen3-deploy` (dev time, locked)."""
    sha, bundle = arguments(ip, ctx.args)
    s = settings(ip, ctx.env)
    if ctx.args.dry_run:
        plan = options({"GEN3_HOST": KINDS["host"], "GEN3_SITE_FILE": KINDS["site"],
                        "GEN3_ROSTER_FILE": KINDS["roster"]}, sha, KINDS["bundle"], KINDS["config"], KINDS["iface"])
        ip.emit({"gen3_deploy": {"dry_run": True, "deploy_sh": plan}})
        return 0
    iface = box_iface(ip, s["GEN3_HOST"])
    mark = start_mark(ip)
    still_running = False
    try:
        out = ip.guarded([*DEPLOY, *options(s, sha, str(bundle), s["GEN3_DEVICE_CONFIG"], iface)], "", DEPLOY_S,
                         ctx.watch(abandon=False), env={**deploy_env(ip), "IEMPC_START_MARK": str(mark)})
    except ip.StillRunning:
        still_running = True
        raise
    finally:
        count_start(ip, mark, still_running)
    sys.stderr.write(out)
    sys.stderr.flush()
    ip.emit({"gen3_deploy": {"sha": sha, "deployed": True}})
    return 0
