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
--new-box with the spare's device config and the band's site; the stack stopped;
band_receive.py and the pepper, from the running box or from a backup; the
stack started and healthy; install-cloudflared.sh; verify_box.py, every check 1
and realtime_running 1. Refused on GEN3_HOST: the box that serves the band is
never rebuilt. The new box stays a spare: nothing enables its units, and its
device runs under a name and a state file of its own (`spare_config`): beside
the serving box, the same name would be a second device the band's receivers
can resolve. Taking the band over is a separate step
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
import tempfile
import time
import tomllib
from pathlib import Path

import iempc_gen3 as g3
import iempc_gen3_data as data
import iempc_gen3_deploy as deploy

BACKUP_WORDS = g3.Words("gen3-backup")
DRIFT_WORDS = g3.Words("gen3-drift")
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
SPARE_SUFFIX = "-spare"
SPARE_STATE = "/var/lib/iem-dante/spare.toml"
NAME_MAX = 31   # Dante's device names: 1 to 31 characters
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


# ---- the new box ----

def settings(ip, env: dict[str, str]) -> dict[str, str]:
    missing = [k for k in NEW_BOX_KEYS if not env.get(k)]
    if missing:
        raise ip.Refused(f"gen3-new-box: {', '.join(missing)} missing in the private env: nothing was changed")
    for key in FILE_KEYS:
        if not Path(env[key]).is_file():
            raise ip.Refused(f"gen3-new-box: {key} is not a file: nothing was changed")
    return {k: env[k] for k in NEW_BOX_KEYS}


def same_box(a: str, b: str) -> bool:
    """One machine under two users is still one machine."""
    return a.rsplit("@", 1)[-1].lower() == b.rsplit("@", 1)[-1].lower()


def arguments(ip, args, s: dict[str, str]) -> tuple[str, str, Path]:
    host, sha = args.host, ip.check_sha(args.sha)
    if not HOST.fullmatch(host or "") or host.startswith("-"):
        raise ip.Refused("gen3-new-box: --host must be an ssh destination: nothing was changed")
    if same_box(host, s["GEN3_HOST"]):
        raise ip.Refused("gen3-new-box: --host is GEN3_HOST, the box that serves the band: nothing was changed")
    if not args.bundle or not Path(args.bundle).is_dir():
        raise ip.Refused("gen3-new-box: --bundle is not a folder (the downloaded gen3-bundle-<sha>): "
                         "nothing was changed")
    return host, sha, Path(args.bundle)


def clean_checkout(ip, sha: str) -> None:
    """deploy.sh's own check, made before the first step instead of half way."""
    head = ip.guarded(["git", "-C", str(REPO), "rev-parse", "HEAD"], "", 30, "ignore").strip()
    dirty = ip.guarded(["git", "-C", str(REPO), "status", "--porcelain"], "", 30, "ignore").strip()
    if head != sha or dirty:
        raise ip.Refused("gen3-new-box: this checkout must be exactly --sha with a clean tree: nothing was changed")


def verify(ip, host: str) -> dict:
    """verify_box.py's numbers. Its exit is the verdict's, so only ssh's own counts here."""
    code, out, err = data.box(host, "python3 -", (GEN3 / "verify_box.py").read_bytes(), data.BOX_S)
    lines = out.decode(errors="replace").strip().splitlines()
    try:
        doc = json.loads(lines[-1])["verify"]
    except (ValueError, IndexError, KeyError, TypeError):
        raise ip.StepError(f"verify_box.py gave no answer (exit {code}): {tail(err)}") from None
    return doc


def failing(doc: dict) -> set[str]:
    return {k for k, v in doc.items() if k not in ("pass", "tpm2", "realtime_running") and v != 1}


def stop_stack(ip, host: str) -> None:
    """A fresh box has no such units, so the stop's own exit is not the verdict:
    provision refuses a stack that still runs, and band_onto reads `quiet`."""
    code, _, err = data.box(host, STOP, b"", data.BOX_S)
    if code == SSH_LOST:
        raise ip.StepError(f"ssh to the new box failed: {tail(err)}")


def provision(ctx, ip, s: dict[str, str], host: str) -> None:
    words = ["bash", str(GEN3 / "provision.sh"), "--host", host, "--ntp-from", s["GEN3_DANTESYNC_CONFIG"],
             "--band-roster", s["GEN3_ROSTER_FILE"]]
    try:
        ip.guarded(words, "", PROVISION_S, ctx.watch(abandon=False), env=deploy.deploy_env(ip))
    except ip.StillRunning:
        raise
    except ip.StepError as e:
        if "verify_box.py found" not in str(e):
            raise
        left = failing(verify(ip, host))
        if not left <= BEFORE_DEPLOY:
            raise ip.StepError(f"provision: verify_box.py names {sorted(left)}") from None


def reboot_once(ctx, ip, host: str) -> bool:
    """The realtime kernel takes one boot; a rerun past that reboots nothing."""
    if verify(ip, host).get("realtime_running") == 1:
        return False
    code, out, err = data.box(host, BOOT_ID, b"", data.BOX_S)
    before = out.decode(errors="replace").strip()
    if code != 0 or not before:
        raise ip.StepError(f"the new box's boot id could not be read: {tail(err)}")
    say("gen3-new-box: reboot into the realtime kernel")
    data.box(host, REBOOT, b"", 30)   # ssh may lose the box before it answers: the new boot id judges it
    end = time.monotonic() + BACK_S
    while time.monotonic() < end:
        ip.pause(ctx, PROBE_S)
        code, out, _ = data.box(host, BOOT_ID, b"", 20)
        now = out.decode(errors="replace").strip()
        if code == 0 and now and now != before:
            if verify(ip, host).get("realtime_running") != 1:
                raise ip.StepError("the new box came back without the realtime kernel")
            return True
    raise ip.StepError(f"the new box did not come back within {BACK_S} s of its reboot")


def interface(ip, args, host: str) -> str:
    """--iface, else the one interface of the box's default route."""
    if args.iface:
        if not deploy.IFACE.fullmatch(args.iface):
            raise ip.Refused("gen3-new-box: --iface is not an interface name: nothing was changed")
        return args.iface
    code, out, _ = data.box(host, ROUTE, b"", data.BOX_S)
    names = {m for m in re.findall(r"\bdev (\S+)", out.decode(errors="replace"))}
    if code != 0 or len(names) != 1:
        raise ip.StepError("gen3-new-box: the new box has no one default route's interface: name it with --iface")
    name = names.pop()
    if not deploy.IFACE.fullmatch(name):
        raise ip.StepError("gen3-new-box: the default route's interface is not an interface name")
    return name


def spare_config(ip, text: str) -> str:
    """The ops device config under the spare's own name and state file; every
    other line kept. Its state file stays out of the way of the serving name's
    (a saved name wins over the config's) when this spare takes the band over."""
    lines = text.splitlines()
    names = [i for i, line in enumerate(lines) if line.startswith("name = ")]
    states = [i for i, line in enumerate(lines) if line.startswith("state_file = ")]
    if len(names) != 1 or len(states) != 1:
        raise ip.Refused("gen3-new-box: GEN3_DEVICE_CONFIG has no single name and state_file line: nothing was changed")
    name = tomllib.loads(text)["name"] + SPARE_SUFFIX
    if len(name) > NAME_MAX:
        raise ip.Refused(f"gen3-new-box: the device's name with {SPARE_SUFFIX} passes Dante's {NAME_MAX} characters: "
                         "nothing was changed")
    lines[names[0]] = f"name = {json.dumps(name)}"
    lines[states[0]] = f"state_file = {json.dumps(SPARE_STATE)}"
    return "\n".join(lines) + "\n"


def deploy_new(ctx, ip, s: dict[str, str], host: str, sha: str, bundle: Path, iface: str) -> None:
    text = spare_config(ip, Path(s["GEN3_DEVICE_CONFIG"]).read_text(encoding="utf-8"))
    fd, name = tempfile.mkstemp(prefix="gen3-new-box-", suffix=".toml", dir=ip.state_dir())
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(text)
        deploy_with(ctx, ip, s, host, sha, bundle, iface, name)
    finally:
        Path(name).unlink(missing_ok=True)


def deploy_with(ctx, ip, s: dict[str, str], host: str, sha: str, bundle: Path, iface: str, config: str) -> None:
    words = [*deploy.DEPLOY, "--host", host, "--bundle", str(bundle), "--sha", sha, "--config",
             config, "--iface", iface, "--start", "engine", "--site-file", s["GEN3_SITE_FILE"],
             "--roster-file", s["GEN3_ROSTER_FILE"], "--new-box"]
    ip.guarded(words, "", deploy.DEPLOY_S, ctx.watch(abandon=False), env=deploy.deploy_env(ip))


def band_onto(ctx, ip, host: str, raw: bytes, manifest: dict[str, str], value: bytes) -> None:
    """The data into a stopped stack, then the stack started and healthy."""
    stop_stack(ip, host)
    if not data.quiet(host):
        raise ip.StepError("the new box's stack did not stop: its data was not sent")
    data.receive(ip, host, raw, manifest)
    data.install_pepper(ip, host, value)
    code, _, err = data.box(host, START, b"", data.BOX_S)
    if code != 0:
        raise ip.StepError(f"the new box's stack did not start (exit {code}): {tail(err)}")
    end = time.monotonic() + HEALTHY_S
    while True:
        code, _, _ = data.box(host, HEALTH, b"", 20)
        if code == 0:
            return
        if time.monotonic() >= end:
            raise ip.StepError(f"the new box is not healthy within {HEALTHY_S} s of its data")
        ip.pause(ctx, PROBE_S)


def install(ctx, ip, script: str, words: list[str]) -> None:
    ip.guarded(["bash", str(GEN3 / script), *words], "", INSTALL_S, ctx.watch(abandon=False),
               env=deploy.deploy_env(ip))


def run_new_box(ctx, ip) -> int:
    """`iempc gen3-new-box`."""
    args = ctx.args
    s = settings(ip, ctx.env)
    host, sha, bundle = arguments(ip, args, s)
    clean_checkout(ip, sha)
    stored = load_backup(ip, backups(ip), args.from_backup) if args.from_backup else None
    say("gen3-new-box: dantesync")
    install(ctx, ip, "install-dantesync.sh", ["--host", host, "--version", s["GEN3_DANTESYNC_VERSION"], "--sha256",
                                              s["GEN3_DANTESYNC_SHA256"], "--config", s["GEN3_DANTESYNC_CONFIG"]])
    say("gen3-new-box: provision")
    stop_stack(ip, host)
    provision(ctx, ip, s, host)
    rebooted = reboot_once(ctx, ip, host)
    say("gen3-new-box: deploy")
    deploy_new(ctx, ip, s, host, sha, bundle, interface(ip, args, host))
    say("gen3-new-box: the band's data")
    if stored:
        raw, manifest, value = stored
    else:
        raw, manifest = read_band(ip, s["GEN3_HOST"])
        value = read_pepper(ip, s["GEN3_HOST"])
    band_onto(ctx, ip, host, raw, manifest, value)
    del value
    say("gen3-new-box: cloudflared")
    install(ctx, ip, "install-cloudflared.sh", ["--host", host, "--version", s["GEN3_CLOUDFLARED_VERSION"], "--sha256",
                                                s["GEN3_CLOUDFLARED_SHA256"]])
    doc = verify(ip, host)
    passed = doc.get("pass") == 1 and doc.get("realtime_running") == 1
    ip.emit({"gen3_new_box": {"pass": int(passed), "sha": sha, "rebooted": rebooted, "members": len(manifest),
                              "data": "backup" if stored else "running box", "failed": sorted(failing(doc))}})
    return 0 if passed else 1


# ---- the drift ----

def sums_on_box(host: str, paths: list[str]) -> dict[str, str]:
    """{path: sha256} of what the box can read; a missing file is left out (sha256sum's exit is not the verdict)."""
    _, out, _ = data.box(host, "sha256sum " + " ".join(paths), b"", data.BOX_S)
    found = {}
    for line in out.decode(errors="replace").splitlines():
        digest, _, name = line.partition("  ")
        if name:
            found[name] = digest
    return found


def build_drift(ip, ctx, host: str) -> bool:
    """A commit after the box's build that changes no crate is no drift."""
    sha = g3.box_build(ip, ctx, host)
    try:
        changed = ip.guarded(["git", "-C", str(REPO), "diff", "--name-only", sha, "HEAD", "--", "crates", "Cargo.toml",
                              "Cargo.lock"], "", 30, "ignore")
    except ip.StepError:
        return True   # the box runs a commit this checkout does not hold
    return bool(changed.strip())


def expected_config(ip, env: dict[str, str]) -> str:
    return Path(deploy.settings(ip, env)["GEN3_DEVICE_CONFIG"]).read_text(encoding="utf-8")


def differences(ctx, ip, host: str) -> list[str]:
    """Every word that differs between the box and this checkout, sorted."""
    env, out = ctx.env, []
    want = {path: hashlib.sha256((GEN3 / name).read_bytes()).hexdigest() for path, name in CHECKOUT_FILES.items()}
    want[DEVICE_CONFIG] = hashlib.sha256(expected_config(ip, env).encode()).hexdigest()
    got = sums_on_box(host, sorted(want))
    out += [f"file:{Path(path).name}" for path, digest in want.items() if got.get(path) != digest]
    if build_drift(ip, ctx, host):
        out.append("build")
    doc = verify(ip, host)
    out += [f"verify:{name}" for name in failing(doc)]
    if doc.get("realtime_running") != 1:
        out.append("realtime_running")
    _, answer, _ = data.box(host, "systemctl is-enabled " + " ".join(ENABLED), b"", data.BOX_S)
    states = answer.decode(errors="replace").split()
    if len(states) != len(ENABLED):
        out.append("boot")
    else:
        out += [f"boot:{unit}" for unit, state in zip(ENABLED, states) if state != "enabled"]
    for name, command, key in VERSIONS:
        _, answer, _ = data.box(host, command, b"", data.BOX_S)
        if env[key].removeprefix("v") not in answer.decode(errors="replace"):
            out.append(f"version:{name}")
    return sorted(out)


def run_drift(ctx, ip) -> int:
    """`iempc gen3-drift`."""
    host = g3.gen3_host(ip, ctx.env, DRIFT_WORDS)
    missing = [key for _, _, key in VERSIONS if not ctx.env.get(key)]
    if missing:
        raise ip.Refused(f"gen3-drift: {', '.join(missing)} missing in the private env")
    found = differences(ctx, ip, host)
    ip.emit({"gen3_drift": {"pass": int(not found), "differ": found}})
    return 0 if not found else 1
