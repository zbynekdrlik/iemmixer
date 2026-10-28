"""`iempc gen3-deploy --bundle <dir> --sha <40 hex> [--dry-run]` (#72,
2026-10-28): the Gen 3 box deployed in place from the dev box, on either side
of the switch, through scripts/gen3/deploy.sh of this checkout.

Since 2026-10-28 the box serves the band by default (`iempc gen3-switch
to-3`; back to 2.x only on the owner's word), and the switch carries no data
back: going to-2 and to-3 again for a deploy would lose what people changed
on 3.x. So the box is deployed while it serves, as the PC is: in dev time
the deploy's own restart is the only interruption.

- **While 3.x serves** (iempc_event.gen3_serves: a side record that does not
  say 2): our device's takeover config is built as `gen3-switch to-3` builds
  it (iempc_gen3_switch.takeover_config: the ops device config
  GEN3_DEVICE_CONFIG, the IEM card's TX labels read now by IP under its spare
  name GEN3_CARD_SPARE, dante_card.py refusing a card that names itself
  otherwise, and the card's usual name DANTE_IEM_NAME), written to a private
  file (mkstemp: 0600, in iempc's state folder, removed whatever happens),
  and deploy.sh runs with it and `--serving` (deploy.sh then takes the side
  record's 3.x instead of refusing it, refuses a config not under the card's
  usual name, disables nothing and enables the engine and the server after
  the start).
- **While 2.x serves:** deploy.sh with the ops device config itself, no
  `--serving` (it disables the units, as every deploy did).

Both: `--host GEN3_HOST --bundle --sha --config --iface --start engine
--site-file GEN3_SITE_FILE --roster-file GEN3_ROSTER_FILE`. The band's site
and roster (the ops site.toml and roster.toml) go on either side:
`gen3-switch to-3` installs no site, so a box deployed with the test site
while 2.x serves would serve the band with it after the next to-3. The
interface is the box's own: IEM_DANTE_IFACE of its /etc/iemmixer/iem-dante.env
(0644, read over ssh, never printed), which an earlier deploy wrote; a box no
deploy has reached is deployed by hand first (`deploy.sh --iface`,
.claude/rules/gen3-new-box.md). deploy.sh runs with iempc's flag, state
folder and private env (IEMMIXER_EVENT_NOW, IEMPC_STATE, PC_ENV), so both
read one side record and one flag; it checks the checkout (exactly --sha, a
clean tree), the bundle, its attestation and everything else itself. Its
stdout goes to stderr here once it succeeded; a failure's error quotes its
stderr.

The engine start the deploy makes counts in the box gates' restart budget
(iempc_gen3_gate's gen3-gate.json), so a gate right after it is refused,
naming the wait: deploy.sh appends a line to the file IEMPC_START_MARK names
(an empty file of iempc's state, removed after) right before it sends the
start; a marked start counts whatever deploy.sh's end, and so does a
deploy.sh left running past its bound. A refusal here, a deploy.sh refused
before its start and the dry run count nothing.

Dev time, locked, a box command (iempc's Spec: refused while EVENT-NOW
exists; on either side, so in neither WHILE_2X_SERVES nor WHILE_3X_SERVES).
A flag during the deploy: deploy.sh's own checks (nothing copied, nothing
installed, the stack not stopped; with --serving a flag never holds down the
stack it stopped), it runs to its end, then the event path (iempc main).

`--dry-run`: the plan (the side, the config's kind, deploy.sh's options with
the host and every path shown only as its kind), no call of any kind.

stdout: one JSON object `{"gen3_deploy": {...}}`: fixed words, the sha and
booleans; no host, address, path, name or label (P6).

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import os
import re
import sys
import tempfile
from pathlib import Path

import dante_card
import iempc_dante
import iempc_gen3_data as data
import iempc_gen3_gate as gate
import iempc_gen3_switch as switch

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
# The private env's keys on either side, and those only the takeover needs.
BOTH = ("GEN3_HOST", "GEN3_DEVICE_CONFIG", "GEN3_SITE_FILE", "GEN3_ROSTER_FILE")
TAKEOVER = ("DANTE_IEM_HOST", "DANTE_IEM_NAME", "GEN3_CARD_SPARE", "NETAUDIO_PYTHON")
FILES = ("GEN3_DEVICE_CONFIG", "GEN3_SITE_FILE", "GEN3_ROSTER_FILE")
# The dry run's words for what deploy.sh would get (P6: no host, no path).
KINDS = {
    "host": "<GEN3_HOST>",
    "bundle": "<--bundle>",
    "takeover": "<the takeover config: GEN3_DEVICE_CONFIG under DANTE_IEM_NAME with the card's TX labels, read "
                "under GEN3_CARD_SPARE; a private file>",
    "usual": "<GEN3_DEVICE_CONFIG>",
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


def settings(ip, env: dict[str, str], serves: bool) -> dict[str, str]:
    """The private env's keys this side needs; each refusal names keys, never values."""
    keys = BOTH + (TAKEOVER if serves else ())
    missing = [k for k in keys if not env.get(k)]
    if missing:
        raise ip.Refused(f"gen3-deploy: {', '.join(missing)} missing in the private env"
                         + (" (3.x serves the band: the takeover config needs the card's keys)" if serves else ""))
    if env["GEN3_HOST"].startswith("-"):
        raise ip.Refused("gen3-deploy: GEN3_HOST must be an ssh destination, not an option")
    for key in FILES:
        if not Path(env[key]).is_file():
            raise ip.Refused(f"gen3-deploy: {key} is not a file")
    if serves:
        if not dante_card.unicast_ipv4(env["DANTE_IEM_HOST"]):
            raise ip.Refused("gen3-deploy: DANTE_IEM_HOST is not a unicast IPv4 address")
        for key in ("GEN3_CARD_SPARE", "DANTE_IEM_NAME"):
            if not dante_card.DEVICE_NAME.fullmatch(env[key]):
                raise ip.Refused(f"gen3-deploy: {key} is not a Dante device name")
        if env["GEN3_CARD_SPARE"].lower() == env["DANTE_IEM_NAME"].lower():
            raise ip.Refused("gen3-deploy: GEN3_CARD_SPARE and DANTE_IEM_NAME must differ (Dante names ignore case)")
    return {k: env[k] for k in keys}


def options(s: dict[str, str], sha: str, bundle: str, config: str, iface: str, serving: bool) -> list[str]:
    """deploy.sh's command line (the dry run passes the kinds in place of the values)."""
    out = ["--host", s["GEN3_HOST"], "--bundle", bundle, "--sha", sha, "--config", config, "--iface", iface,
           "--start", "engine", "--site-file", s["GEN3_SITE_FILE"], "--roster-file", s["GEN3_ROSTER_FILE"]]
    return out + (["--serving"] if serving else [])


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


def takeover(ip, s: dict[str, str]) -> str:
    """Our device's config under the card's usual name with its TX labels, as
    `gen3-switch to-3` builds it; the card is read under its spare name, the
    name it holds while 3.x serves (the helper refuses a card naming itself
    otherwise)."""
    card = iempc_dante.Card(s["DANTE_IEM_HOST"], s["GEN3_CARD_SPARE"], s["NETAUDIO_PYTHON"])
    doc = iempc_dante.ask_card(ip, card, "read")
    try:
        labels = {int(n): v for n, v in doc.get("tx", {}).items()}
    except (AttributeError, TypeError, ValueError):
        raise ip.StepError("gen3-deploy: dante_card.py read gave no TX labels: nothing was deployed") from None
    try:
        return switch.takeover_config(Path(s["GEN3_DEVICE_CONFIG"]).read_text(encoding="utf-8"), labels,
                                      s["DANTE_IEM_NAME"])
    except switch.TakeoverError as e:
        raise ip.Refused(f"gen3-deploy: {e}: nothing was deployed") from None


def private_file(ip, text: str) -> Path:
    """`text` in a new file only this user may read (mkstemp: 0600) in iempc's state folder (0700)."""
    fd, name = tempfile.mkstemp(prefix="gen3-deploy-", suffix=".toml", dir=ip.state_dir())
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(text)
    except BaseException:
        Path(name).unlink(missing_ok=True)
        raise
    return Path(name)


def deploy_env(ip) -> dict[str, str]:
    """This process's environment with iempc's flag, state folder and private
    env, so deploy.sh reads the side record and the flag iempc reads."""
    return {**os.environ, "IEMMIXER_EVENT_NOW": str(ip.EVENT_NOW), "IEMPC_STATE": str(ip.STATE_DIR),
            "PC_ENV": str(ip.env_path())}


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
    """`iempc gen3-deploy` (dev time, locked, on either side)."""
    sha, bundle = arguments(ip, ctx.args)
    serves = ip.gen3_serves()
    s = settings(ip, ctx.env, serves)
    side, kind = ("3.x", "takeover") if serves else ("2.x", "usual")
    if ctx.args.dry_run:
        plan = options({"GEN3_HOST": KINDS["host"], "GEN3_SITE_FILE": KINDS["site"],
                        "GEN3_ROSTER_FILE": KINDS["roster"]}, sha, KINDS["bundle"], KINDS[kind], KINDS["iface"],
                       serves)
        ip.emit({"gen3_deploy": {"dry_run": True, "serves": side, "config": kind, "deploy_sh": plan}})
        return 0
    iface = box_iface(ip, s["GEN3_HOST"])
    private: Path | None = None
    mark: Path | None = None
    still_running = False
    try:
        if serves:
            private = private_file(ip, takeover(ip, s))
        config = str(private) if private else s["GEN3_DEVICE_CONFIG"]
        mark = start_mark(ip)
        try:
            out = ip.guarded([*DEPLOY, *options(s, sha, str(bundle), config, iface, serves)], "", DEPLOY_S,
                             ctx.watch(abandon=False), env={**deploy_env(ip), "IEMPC_START_MARK": str(mark)})
        except ip.StillRunning:
            still_running = True
            raise
    finally:
        if private is not None:
            private.unlink(missing_ok=True)
        if mark is not None:
            count_start(ip, mark, still_running)
    sys.stderr.write(out)
    sys.stderr.flush()
    ip.emit({"gen3_deploy": {"serves": side, "config": kind, "serving": serves, "sha": sha, "deployed": True}})
    return 0
