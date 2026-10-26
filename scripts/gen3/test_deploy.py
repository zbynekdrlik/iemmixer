"""Tests for scripts/gen3/deploy.sh (#72 phase C, plan Task 6): the Gen 3
stack's deploy, run against a stand-in `ssh` and `scp` first on PATH that play
the box in a temporary folder (its home, its root and its units' states), and
the three units' contents. The stand-in ssh records each call's command line
and its stdin apart, so a PIN is seen to travel only on stdin. Every value is
synthetic (P6)."""
from __future__ import annotations

import hashlib
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
DEPLOY = HERE / "deploy.sh"
UNITS = {name: HERE / f"{name}.service" for name in ("iem-dante", "iem-engine", "iem-server")}
TEST_SITE = REPO / "config" / "test-site.toml"
TEST_ROSTER = REPO / "config" / "test-roster.toml"
HOST = "tester@gen3.test"
COMMIT = "0123456789abcdef0123456789abcdef01234567"
BINARIES = ("iem-engine", "iem-server", "iem-dante-dev", "iem-soakclient")
STAGE = ".cache/iem-gen3-deploy"
STOP = "sudo -n systemctl stop iem-server iem-engine iem-dante"
ENGINEER_PIN = "3917"
MEMBER_PIN = "6284"

# The box: `ssh` answers the deploy's commands from FAKE_BOX (home/, root/ and
# states.json, the units' states), `scp` copies into its home; both log every
# command line as one JSON line in calls.log, and ssh logs its stdin (one JSON
# line per ssh call, in the same order) in stdin.log and its environment in
# env.log.
FAKE_SSH = r'''
import hashlib, json, os, shutil, sys
box = os.environ["FAKE_BOX"]
args = sys.argv[1:]
data = sys.stdin.buffer.read() if not sys.stdin.isatty() else b""
open(os.path.join(box, "calls.log"), "a").write(json.dumps(["ssh"] + args) + "\n")
open(os.path.join(box, "stdin.log"), "a").write(json.dumps(data.decode("utf-8", "replace")) + "\n")
open(os.path.join(box, "env.log"), "a").write(json.dumps(dict(os.environ)) + "\n")
while args and args[0] == "-o":
    args = args[2:]
cmd = " ".join(args[1:])
home, root = os.path.join(box, "home"), os.path.join(box, "root")
states_path = os.path.join(box, "states.json")
st = json.load(open(states_path))

def save():
    json.dump(st, open(states_path, "w"))

def flag_on(when):
    if os.environ.get("FAKE_FLAG_ON") == when:
        open(os.environ["IEMMIXER_EVENT_NOW"], "w").write("now\n")

def failing(env):
    return os.environ.get(env)

STOP = "sudo -n systemctl stop iem-server iem-engine iem-dante"
if cmd == "id -un":
    print(os.environ.get("FAKE_USER", "tester"))
elif cmd == "sudo -n -l systemctl stop iem-server iem-engine iem-dante":
    if failing("FAKE_NO_STOP_RULE"):
        sys.stderr.write("Sorry, user tester is not allowed to execute this command.\n")
        sys.exit(1)
    print("/usr/bin/systemctl stop iem-server iem-engine iem-dante")
elif cmd == STOP:
    if st["started"] and failing("FAKE_LATE_STOP_FAILS"):
        sys.stderr.write("sudo: a password is required\n")
        sys.exit(1)
    code = 0
    for unit in ("iem-server", "iem-engine", "iem-dante"):
        if unit in st["not_loaded"]:
            sys.stderr.write(f"Failed to stop {unit}.service: Unit {unit}.service not loaded.\n")
            code = 5
        elif os.environ.get("FAKE_STILL_RUNS") == unit:
            st["units"][unit] = "deactivating"
        else:
            st["units"][unit] = "inactive"
    save()
    sys.exit(code)
elif cmd.startswith("systemctl is-active "):
    names = cmd.split()[2:]
    for unit in names:
        print(st["units"].get(unit, "inactive"))
    sys.exit(0 if all(st["units"].get(u) == "active" for u in names) else 3)
elif cmd.startswith("mkdir -p "):
    os.makedirs(os.path.join(home, cmd.split()[-1]), exist_ok=True)
    flag_on("mkdir")
elif cmd == "cd .cache/iem-gen3-deploy && sha256sum --check --strict --quiet SHA256SUMS":
    stage = os.path.join(home, ".cache/iem-gen3-deploy")
    bad = 0
    for line in open(os.path.join(stage, "SHA256SUMS")).read().splitlines():
        want, name = line.split("  ", 1)
        if hashlib.sha256(open(os.path.join(stage, name), "rb").read()).hexdigest() != want:
            print(f"{name}: FAILED")
            bad += 1
    sys.exit(1 if bad else 0)
elif cmd.startswith("set -e\n") and "systemctl" not in cmd:
    for line in cmd.splitlines()[1:]:
        if failing("FAKE_SUDO_FAILS") and line.startswith("sudo -n "):
            sys.stderr.write("sudo: a password is required\n")
            sys.exit(1)
        words = line.split()
        modes = open(os.path.join(box, "modes.log"), "a")
        if words[:4] == ["sudo", "-n", "install", "-D"]:
            mode, src, dst = words[5], words[6], words[7]
            if os.environ.get("FAKE_SUDO_FAILS_AT") == dst:
                sys.stderr.write("install: cannot create regular file: Read-only file system\n")
                sys.exit(1)
            target = os.path.join(root, dst.lstrip("/"))
            os.makedirs(os.path.dirname(target), exist_ok=True)
            shutil.copyfile(os.path.join(home, src), target)
            modes.write(f"{mode} {dst}\n")
        elif words[:4] == ["sudo", "-n", "install", "-d"]:
            owner, mode, dst = words[5], words[7], words[8]
            os.makedirs(os.path.join(root, dst.lstrip("/")), exist_ok=True)
            modes.write(f"dir {owner} {mode} {dst}\n")
        elif words[:2] == ["install", "-m"]:
            mode, src, dst = words[2], words[3], words[4]
            target = os.path.join(root, dst.lstrip("/"))
            if not os.path.isdir(os.path.dirname(target)):
                sys.stderr.write("install: cannot create regular file: No such file or directory\n")
                sys.exit(1)
            shutil.copyfile(os.path.join(home, src), target)
            modes.write(f"user {mode} {dst}\n")
        else:
            sys.stderr.write("unexpected install line: " + line + "\n")
            sys.exit(97)
    flag_on("install")
elif cmd.startswith("IEMMIXER_CONFIG=/var/lib/iemmixer/site/iemmixer.toml /opt/iemmixer/bin/iem-server pin "):
    if not os.path.exists(os.path.join(root, "var/lib/iemmixer/site/iemmixer.toml")):
        sys.stderr.write("iem-server: site config: not found\n")
        sys.exit(1)
    if failing("FAKE_PIN_FAILS"):
        sys.stderr.write("iem-server: unknown member\n")
        sys.exit(2)
    what = cmd.split(" pin ", 1)[1]
    open(os.path.join(box, "pins.log"), "a").write(json.dumps([what, data.decode()]) + "\n")
    sys.stderr.write("iem-server: stored the PIN hash\n")
elif cmd.startswith("set -e\n"):
    for line in cmd.splitlines()[1:]:
        words = line.split()
        if words[:3] != ["sudo", "-n", "systemctl"]:
            sys.stderr.write("unexpected start line: " + line + "\n")
            sys.exit(97)
        if words[3] == "daemon-reload":
            st["not_loaded"] = []
        elif words[3] == "start":
            wanted = dict(item.split("=", 1) for item in os.environ.get("FAKE_ACTIVE", "").split(",") if item)
            for unit in words[4:]:
                st["units"][unit] = wanted.get(unit, "active")
            st["started"] = True
    save()
    flag_on("start")
elif cmd.startswith("sha256sum "):
    for path in cmd.split()[1:]:
        digest = hashlib.sha256(open(os.path.join(root, path.lstrip("/")), "rb").read()).hexdigest()
        if os.environ.get("FAKE_BAD_INSTALLED") == os.path.basename(path):
            digest = "f" * 64
        print(digest + "  " + path)
elif cmd == "/opt/iemmixer/bin/iem-engine --version":
    print(os.environ.get("FAKE_ENGINE_VERSION", "iem-engine 2.0.0-dev.45+" + os.environ["FAKE_COMMIT"]))
elif cmd == "/opt/iemmixer/bin/iem-dante-dev --version":
    print(os.environ.get("FAKE_DEVICE_VERSION", "iem-dante-dev 2.0.0-dev.45 " + os.environ["FAKE_COMMIT"]))
elif cmd == "curl -sf --max-time 5 http://127.0.0.1:8080/api/version":
    tries = os.path.join(box, "curl.count")
    n = int(open(tries).read()) + 1 if os.path.exists(tries) else 1
    open(tries, "w").write(str(n))
    if st["units"].get("iem-server") != "active" or failing("FAKE_CURL_NEVER") \
            or n <= int(os.environ.get("FAKE_CURL_FAILS", "0")):
        sys.exit(7)
    git_hash = os.environ.get("FAKE_GIT_HASH", os.environ["FAKE_COMMIT"][:7])
    print(json.dumps({"version": "2.0.0-dev.45", "git_hash": git_hash, "branch": "dev"}))
else:
    sys.stderr.write("unexpected command: " + cmd + "\n")
    sys.exit(97)
'''

FAKE_SCP = r'''
import json, os, shutil, sys
box = os.environ["FAKE_BOX"]
args = sys.argv[1:]
open(os.path.join(box, "calls.log"), "a").write(json.dumps(["scp"] + args) + "\n")
words = []
while args:
    if args[0] == "-o":
        args = args[2:]
    elif args[0] == "-q":
        args = args[1:]
    else:
        words.append(args.pop(0))
*sources, dest = words
folder = os.path.join(box, "home", dest.split(":", 1)[1])
for src in sources:
    target = os.path.join(folder, os.path.basename(src))
    if os.environ.get("FAKE_SCP_CORRUPT") == os.path.basename(src):
        open(target, "wb").write(b"changed on the way")
    else:
        shutil.copyfile(src, target)
'''


def parse_unit(path: Path) -> dict[str, dict[str, list[str]]]:
    """A unit file as {section: {key: [values]}}, comments left out."""
    sections: dict[str, dict[str, list[str]]] = {}
    current: dict[str, list[str]] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            current = sections.setdefault(line[1:-1], {})
            continue
        key, _, value = line.partition("=")
        current.setdefault(key, []).append(value)
    return sections


def one(unit: dict[str, dict[str, list[str]]], section: str, key: str) -> str:
    values = unit[section].get(key, [])
    if len(values) != 1:
        raise AssertionError(f"[{section}] {key}: {values}")
    return values[0]


def roster_people() -> list[str]:
    with open(TEST_ROSTER, "rb") as f:
        return [person["id"] for person in tomllib.load(f)["people"]]


class DeployTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.box = self.tmp / "box"
        (self.box / "home").mkdir(parents=True)
        (self.box / "root").mkdir()
        bin_dir = self.tmp / "bin"
        bin_dir.mkdir()
        for name, body in (("ssh", FAKE_SSH), ("scp", FAKE_SCP)):
            p = bin_dir / name
            p.write_text(f"#!{sys.executable}\n{body}", encoding="utf-8")
            p.chmod(0o755)
        self.flag = self.tmp / "config" / "EVENT-NOW"
        # iempc's private env, absent unless a test writes it: the dev box's
        # own is never read.
        self.pc_env = self.tmp / "config" / "iem-pc.env"
        self.env = {**os.environ, "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}", "FAKE_BOX": str(self.box),
                    "FAKE_COMMIT": COMMIT, "IEMMIXER_EVENT_NOW": str(self.flag), "GEN3_SETTLE_S": "0",
                    "GEN3_SERVER_WAIT_S": "3", "PC_ENV": str(self.pc_env)}
        # The CI bundle (ci.yml, gen3-bin): four binaries, BUILD, SHA256SUMS.
        self.bundle = self.tmp / "gen3-bundle"
        self.bundle.mkdir()
        for name in BINARIES:
            (self.bundle / name).write_bytes(b"\x7fELF a synthetic " + name.encode())
        (self.bundle / "BUILD").write_text(COMMIT + "\n", encoding="utf-8")
        self.write_sums()
        self.config = self.tmp / "dante-device.toml"
        self.config.write_text('name = "test-device"\n', encoding="utf-8")
        self.set_states({"iem-dante": "active", "iem-engine": "inactive", "iem-server": "inactive"})

    def set_states(self, units: dict[str, str], not_loaded: tuple[str, ...] = ()) -> None:
        (self.box / "states.json").write_text(json.dumps({"units": units, "not_loaded": list(not_loaded),
                                                          "started": False}), encoding="utf-8")

    def states(self) -> dict[str, str]:
        return json.loads((self.box / "states.json").read_text(encoding="utf-8"))["units"]

    def write_sums(self, names: tuple[str, ...] = (*BINARIES, "BUILD")) -> None:
        lines = [f"{hashlib.sha256((self.bundle / n).read_bytes()).hexdigest()}  {n}\n" for n in names]
        (self.bundle / "SHA256SUMS").write_text("".join(lines), encoding="utf-8")

    def full_args(self) -> list[str]:
        return ["--host", HOST, "--bundle", str(self.bundle), "--sha", COMMIT, "--config", str(self.config),
                "--iface", "eth9"]

    def deploy(self, *extra: str, args: list[str] | None = None, **env: str) -> subprocess.CompletedProcess:
        argv = args if args is not None else [*self.full_args(), *extra]
        return subprocess.run(["bash", str(DEPLOY), *argv], env={**self.env, **env}, stdin=subprocess.DEVNULL,
                              capture_output=True, text=True, timeout=120)

    def calls(self) -> list[list[str]]:
        log = self.box / "calls.log"
        if not log.exists():
            return []
        return [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]

    def stdins(self) -> list[str]:
        log = self.box / "stdin.log"
        if not log.exists():
            return []
        return [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]

    def remote(self) -> list[str]:
        """The remote command of every ssh call, in order."""
        out = []
        for call in self.calls():
            if call[0] == "ssh":
                args = call[1:]
                while args and args[0] == "-o":
                    args = args[2:]
                self.assertEqual(args[0], HOST)
                out.append(" ".join(args[1:]))
        return out

    def kinds(self) -> list[str]:
        """Each call as 'scp' or its remote command's first line."""
        out, remote = [], iter(self.remote())
        for call in self.calls():
            out.append("scp" if call[0] == "scp" else next(remote).splitlines()[0])
        return out

    def installed(self, path: str) -> bytes:
        return (self.box / "root" / path.lstrip("/")).read_bytes()

    def exists(self, path: str) -> bool:
        return (self.box / "root" / path.lstrip("/")).exists()

    def write_pc_env(self, text: str) -> None:
        self.pc_env.parent.mkdir(exist_ok=True)
        self.pc_env.write_text("PC_SSH=tester@pc.test\n" + text, encoding="utf-8")

    def write_pins(self) -> None:
        self.write_pc_env(f"GEN3_HOST={HOST}\nGEN3_ENGINEER_PIN=\"{ENGINEER_PIN}\"\n GEN3_MEMBER_PIN = {MEMBER_PIN}\n")

    # The whole deploy.

    def test_a_deploy_stops_copies_installs_starts_and_reads_back(self) -> None:
        r = self.deploy()
        self.assertEqual(r.returncode, 0, r.stderr)
        for name in BINARIES:
            self.assertEqual(self.installed(f"/opt/iemmixer/bin/{name}"), (self.bundle / name).read_bytes())
        for name, path in UNITS.items():
            self.assertEqual(self.installed(f"/etc/systemd/system/{name}.service"), path.read_bytes())
            self.assertEqual(self.installed(f"/etc/systemd/system/{name}.service.d/10-user.conf"),
                             b"[Service]\nUser=tester\n")
        self.assertEqual(self.installed("/etc/iemmixer/dante-device.toml"), self.config.read_bytes())
        self.assertEqual(self.installed("/etc/iemmixer/iem-dante.env"), b"IEM_DANTE_IFACE=eth9\n")
        self.assertEqual(self.installed("/var/lib/iemmixer/site/iemmixer.toml"), TEST_SITE.read_bytes())
        self.assertEqual(self.installed("/var/lib/iemmixer/site/roster.toml"), TEST_ROSTER.read_bytes())
        modes = (self.box / "modes.log").read_text(encoding="utf-8").splitlines()
        # The binaries 0755 (a downloaded artifact loses the executable bit),
        # every drop-in (User=) before the units, the site folder the user's
        # own and the site and roster copied by the user, not by sudo.
        self.assertEqual(modes, [
            "0755 /opt/iemmixer/bin/iem-engine", "0755 /opt/iemmixer/bin/iem-server",
            "0755 /opt/iemmixer/bin/iem-dante-dev", "0755 /opt/iemmixer/bin/iem-soakclient",
            "0644 /etc/systemd/system/iem-dante.service.d/10-user.conf",
            "0644 /etc/systemd/system/iem-engine.service.d/10-user.conf",
            "0644 /etc/systemd/system/iem-server.service.d/10-user.conf",
            "0644 /etc/systemd/system/iem-dante.service", "0644 /etc/systemd/system/iem-engine.service",
            "0644 /etc/systemd/system/iem-server.service",
            "0644 /etc/iemmixer/dante-device.toml", "0644 /etc/iemmixer/iem-dante.env",
            "dir tester 0700 /var/lib/iemmixer/site",
            "user 0600 /var/lib/iemmixer/site/iemmixer.toml", "user 0600 /var/lib/iemmixer/site/roster.toml"])
        # The order: the stack's stop allowed, stopped, nothing left running,
        # the copy, the sums again on the box, the install, the start, the
        # read back.
        self.assertEqual(self.kinds(), [
            "id -un", f"sudo -n -l systemctl stop iem-server iem-engine iem-dante", STOP,
            "systemctl is-active iem-server iem-engine iem-dante", f"mkdir -p {STAGE}", "scp",
            f"cd {STAGE} && sha256sum --check --strict --quiet SHA256SUMS", "set -e", "set -e",
            "sha256sum " + " ".join(f"/opt/iemmixer/bin/{n}" for n in BINARIES),
            "/opt/iemmixer/bin/iem-engine --version", "/opt/iemmixer/bin/iem-dante-dev --version",
            "systemctl is-active iem-engine iem-server", "curl -sf --max-time 5 http://127.0.0.1:8080/api/version"])
        remote = self.remote()
        install = remote[6].splitlines()
        self.assertEqual(install[1:], [
            f"sudo -n install -D -m 0755 {STAGE}/iem-engine /opt/iemmixer/bin/iem-engine",
            f"sudo -n install -D -m 0755 {STAGE}/iem-server /opt/iemmixer/bin/iem-server",
            f"sudo -n install -D -m 0755 {STAGE}/iem-dante-dev /opt/iemmixer/bin/iem-dante-dev",
            f"sudo -n install -D -m 0755 {STAGE}/iem-soakclient /opt/iemmixer/bin/iem-soakclient",
            f"sudo -n install -D -m 0644 {STAGE}/10-user.conf /etc/systemd/system/iem-dante.service.d/10-user.conf",
            f"sudo -n install -D -m 0644 {STAGE}/10-user.conf /etc/systemd/system/iem-engine.service.d/10-user.conf",
            f"sudo -n install -D -m 0644 {STAGE}/10-user.conf /etc/systemd/system/iem-server.service.d/10-user.conf",
            f"sudo -n install -D -m 0644 {STAGE}/iem-dante.service /etc/systemd/system/iem-dante.service",
            f"sudo -n install -D -m 0644 {STAGE}/iem-engine.service /etc/systemd/system/iem-engine.service",
            f"sudo -n install -D -m 0644 {STAGE}/iem-server.service /etc/systemd/system/iem-server.service",
            f"sudo -n install -D -m 0644 {STAGE}/dante-device.toml /etc/iemmixer/dante-device.toml",
            f"sudo -n install -D -m 0644 {STAGE}/iem-dante.env /etc/iemmixer/iem-dante.env",
            "sudo -n install -d -o tester -m 0700 /var/lib/iemmixer/site",
            f"install -m 0600 {STAGE}/iemmixer.toml /var/lib/iemmixer/site/iemmixer.toml",
            f"install -m 0600 {STAGE}/roster.toml /var/lib/iemmixer/site/roster.toml"])
        # Disabled, the start count reset, then started: nothing enables any
        # unit, so a reboot of the box never starts the stack.
        self.assertEqual(remote[7].splitlines()[1:], [
            "sudo -n systemctl daemon-reload", "sudo -n systemctl disable iem-dante iem-engine iem-server",
            "sudo -n systemctl reset-failed iem-dante iem-engine iem-server",
            "sudo -n systemctl start iem-engine iem-server"])
        self.assertFalse(any(" enable " in command for command in remote), remote)
        # The copy: the bundle's six files and the box's own files, one scp.
        scp = next(call for call in self.calls() if call[0] == "scp")
        self.assertEqual(scp[-1], f"{HOST}:{STAGE}/")
        self.assertEqual(sorted(os.path.basename(src) for src in scp[1:-1] if not src.startswith("-")
                                and src not in ("BatchMode=yes", "ConnectTimeout=10")),
                         sorted([*BINARIES, "BUILD", "SHA256SUMS", "iem-dante.service", "iem-engine.service",
                                 "iem-server.service", "10-user.conf", "dante-device.toml", "iem-dante.env",
                                 "iemmixer.toml", "roster.toml"]))
        self.assertEqual(self.states(), {"iem-dante": "inactive", "iem-engine": "active", "iem-server": "active"})
        self.assertNotIn("WARNING", r.stderr)
        self.assertTrue(all("BatchMode=yes" in c for c in self.calls()))
        # Without --pins no call carries anything on stdin.
        self.assertEqual(set(self.stdins()), {""})
        self.assertFalse((self.box / "pins.log").exists())
        last = r.stdout.strip().splitlines()[-1]
        self.assertIn(f"iem-engine 2.0.0-dev.45+{COMMIT}", last)
        self.assertIn(f"iem-dante-dev 2.0.0-dev.45 {COMMIT}", last)
        self.assertIn("iem-engine and iem-server active", last)
        self.assertIn(f"/api/version names {COMMIT[:7]}", last)

    def test_the_first_deploy_over_the_standalone_device_stops_what_is_loaded(self) -> None:
        # Phase B's box: only iem-dante is installed, so the stack's stop says
        # "not loaded" for the engine and the server (exit 5) while it stops
        # the device; nothing runs, so the deploy goes on.
        self.set_states({"iem-dante": "active"}, not_loaded=("iem-engine", "iem-server"))
        r = self.deploy()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("the stop exited 5", r.stderr)
        self.assertNotIn("WARNING", r.stderr)
        self.assertEqual(self.states(), {"iem-dante": "inactive", "iem-engine": "active", "iem-server": "active"})

    def test_a_unit_still_running_after_the_stop_refuses_the_copy(self) -> None:
        self.set_states({"iem-dante": "inactive", "iem-engine": "active", "iem-server": "active"})
        r = self.deploy(FAKE_STILL_RUNS="iem-engine")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("iem-engine is 'deactivating' after the stop", r.stderr)
        self.assertIn("nothing was copied", r.stderr)
        self.assertEqual(self.kinds()[-1], "systemctl is-active iem-server iem-engine iem-dante")
        self.assertNotIn("scp", self.kinds())

    def test_start_device_starts_only_the_standalone_device(self) -> None:
        self.set_states({"iem-dante": "inactive", "iem-engine": "active", "iem-server": "active"})
        r = self.deploy("--start", "device")
        self.assertEqual(r.returncode, 0, r.stderr)
        remote = self.remote()
        start = next(c for c in remote if c.startswith("set -e\n") and "systemctl" in c).splitlines()
        self.assertEqual(start[-1], "sudo -n systemctl start iem-dante")
        self.assertIn("systemctl is-active iem-dante", remote)
        self.assertFalse(any(c.startswith("curl ") for c in remote), remote)
        # The engine and the server were stopped and stay so; all three units
        # and all four binaries are installed all the same.
        self.assertEqual(self.states(), {"iem-dante": "active", "iem-engine": "inactive", "iem-server": "inactive"})
        self.assertTrue(self.exists("/etc/systemd/system/iem-engine.service"))
        self.assertTrue(self.exists("/opt/iemmixer/bin/iem-server"))
        self.assertIn("iem-dante active", r.stdout.strip().splitlines()[-1])

    # --pins: only on stdin, never printed.

    def test_pins_go_only_on_stdin_and_are_never_printed(self) -> None:
        self.write_pins()
        r = self.deploy("--pins")
        self.assertEqual(r.returncode, 0, r.stderr)
        pins = [json.loads(line) for line in (self.box / "pins.log").read_text(encoding="utf-8").splitlines()]
        people = roster_people()
        self.assertGreater(len(people), 1)
        self.assertEqual(pins, [["set-engineer", ENGINEER_PIN + "\n"],
                                *[[f"set-member {person}", MEMBER_PIN + "\n"] for person in people]])
        # Each PIN only on the stdin of its own call; every other call's stdin
        # empty; no command line (ssh or scp, local or remote) holds one.
        remote, stdins = self.remote(), self.stdins()
        self.assertEqual(len(remote), len(stdins))
        for command, data in zip(remote, stdins):
            if " pin set-" in command:
                self.assertIn(data, (ENGINEER_PIN + "\n", MEMBER_PIN + "\n"))
            else:
                self.assertEqual(data, "", command)
        for call in self.calls():
            for word in call:
                self.assertNotIn(ENGINEER_PIN, word)
                self.assertNotIn(MEMBER_PIN, word)
        # Not in ssh's environment either (never exported), never printed.
        for line in (self.box / "env.log").read_text(encoding="utf-8").splitlines():
            values = set(json.loads(line).values())
            self.assertNotIn(ENGINEER_PIN, values)
            self.assertNotIn(MEMBER_PIN, values)
        for text in (r.stdout, r.stderr):
            self.assertNotIn(ENGINEER_PIN, text)
            self.assertNotIn(MEMBER_PIN, text)
        for name in ("10-user.conf", "iem-dante.env"):
            staged = (self.box / "home" / STAGE / name).read_text(encoding="utf-8")
            self.assertNotIn(ENGINEER_PIN, staged)
            self.assertNotIn(MEMBER_PIN, staged)
        # The PINs are set while the server is stopped: after the site is in
        # place, before the start.
        kinds = self.kinds()
        first_pin = next(i for i, c in enumerate(remote) if " pin set-" in c)
        start = next(i for i, c in enumerate(remote) if c.startswith("set -e\n") and "systemctl" in c)
        install = next(i for i, c in enumerate(remote) if c.startswith("set -e\n") and "install -D" in c)
        self.assertLess(install, first_pin)
        self.assertLess(first_pin + len(people), start)
        self.assertEqual(kinds.count("scp"), 1)
        self.assertIn(f"the engineer and {len(people)} roster people", r.stdout)

    def test_pins_need_both_pins_of_four_digits_in_the_private_env(self) -> None:
        cases = [
            (None, "not there"),
            (f"GEN3_ENGINEER_PIN={ENGINEER_PIN}\n", "no GEN3_MEMBER_PIN"),
            (f"GEN3_MEMBER_PIN={MEMBER_PIN}\n", "no GEN3_ENGINEER_PIN"),
            (f"GEN3_ENGINEER_PIN=39170\nGEN3_MEMBER_PIN={MEMBER_PIN}\n", "GEN3_ENGINEER_PIN of iempc's private env is not 4 digits"),
            (f"GEN3_ENGINEER_PIN={ENGINEER_PIN}\nGEN3_MEMBER_PIN=62a4\n", "GEN3_MEMBER_PIN of iempc's private env is not 4 digits"),
        ]
        for text, want in cases:
            with self.subTest(want=want):
                if text is None:
                    self.pc_env.unlink(missing_ok=True)
                else:
                    self.write_pc_env(f"GEN3_HOST={HOST}\n" + text)
                r = self.deploy("--pins")
                self.assertEqual(r.returncode, 2, r.stderr)
                self.assertIn(want, r.stderr)
                for value in (ENGINEER_PIN, MEMBER_PIN, "39170", "62a4"):
                    self.assertNotIn(value, r.stderr + r.stdout)
                self.assertEqual(self.calls(), [])

    def test_a_pin_the_box_refuses_starts_nothing(self) -> None:
        self.write_pins()
        r = self.deploy("--pins", FAKE_PIN_FAILS="1")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("did not store the PIN (pin set-engineer): nothing started", r.stderr)
        self.assertNotIn(ENGINEER_PIN, r.stderr + r.stdout)
        self.assertFalse(any("systemctl start" in c for c in self.remote()))

    # The event flag.

    def test_the_event_flag_refuses_before_any_call(self) -> None:
        self.flag.parent.mkdir()
        self.flag.write_text("now\n", encoding="utf-8")
        r = self.deploy()
        self.assertEqual(r.returncode, 1)
        self.assertIn("an event is on", r.stderr)
        self.assertIn("nothing was copied", r.stderr)
        self.assertEqual(self.calls(), [])

    def test_a_flag_that_comes_during_the_copy_refuses_the_install(self) -> None:
        self.flag.parent.mkdir()
        r = self.deploy(FAKE_FLAG_ON="mkdir")
        self.assertEqual(r.returncode, 1)
        self.assertIn("nothing installed", r.stderr)
        self.assertEqual(self.kinds()[-2:], ["scp", f"cd {STAGE} && sha256sum --check --strict --quiet SHA256SUMS"])
        self.assertFalse((self.box / "root" / "opt").exists())

    def test_a_flag_that_comes_during_the_install_starts_nothing(self) -> None:
        self.flag.parent.mkdir()
        r = self.deploy(FAKE_FLAG_ON="install")
        self.assertEqual(r.returncode, 1)
        self.assertIn("installed, nothing started", r.stderr)
        self.assertFalse(any("systemctl" in c for c in self.remote() if c.startswith("set -e\n")))
        self.assertEqual(self.states(), {"iem-dante": "inactive", "iem-engine": "inactive", "iem-server": "inactive"})

    def test_a_flag_that_comes_during_the_start_stops_all_three_again(self) -> None:
        # The race: the flag is checked, "ide event" arrives and stops the
        # stack, then the deploy's start brings it up again. The flag is read
        # again after the start: the stack is stopped, exit 1.
        self.flag.parent.mkdir()
        r = self.deploy(FAKE_FLAG_ON="start")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("an event is on", r.stderr)
        self.assertIn("the stack stopped again", r.stderr)
        remote = self.remote()
        self.assertTrue(remote[-2].startswith("set -e\n") and "systemctl start" in remote[-2], remote)
        self.assertEqual(remote[-1], STOP)
        self.assertFalse(any(c.startswith("sha256sum ") for c in remote), remote)
        self.assertEqual(self.states(), {"iem-dante": "inactive", "iem-engine": "inactive", "iem-server": "inactive"})

    def test_a_stop_after_a_late_flag_that_fails_says_so(self) -> None:
        self.flag.parent.mkdir()
        r = self.deploy(FAKE_FLAG_ON="start", FAKE_LATE_STOP_FAILS="1")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("an event is on", r.stderr)
        self.assertIn("may still run", r.stderr)
        self.assertEqual(self.remote()[-1], STOP)

    # The bundle.

    def test_a_build_that_is_not_the_sha_is_refused_before_any_call(self) -> None:
        for text in ("f" * 40 + "\n", COMMIT, COMMIT + "\n\n", COMMIT + "\nmore\n"):
            with self.subTest(text=text):
                (self.bundle / "BUILD").write_text(text, encoding="utf-8")
                self.write_sums()
                r = self.deploy()
                self.assertEqual(r.returncode, 1, r.stderr)
                self.assertIn("BUILD is not --sha", r.stderr)
                self.assertEqual(self.calls(), [])

    def test_sums_that_do_not_hold_on_the_dev_box_are_refused_before_any_call(self) -> None:
        good = (self.bundle / "SHA256SUMS").read_text(encoding="utf-8")
        lines = good.splitlines(keepends=True)
        cases = [
            (good.replace(lines[1], "0" * 64 + "  iem-server\n"), "iem-server of the bundle does not have its"),
            ("".join(lines[:-1]), "no line for BUILD"),
            (good + lines[0], "names iem-engine twice"),
            (good + "0" * 64 + "  ../escape\n", "not '<sha256>  <name>'"),
            (good + "0" * 64 + "  extra\n", "names extra, which is not in a gen3 bundle"),
            (good.replace("  iem-engine", " *iem-engine"), "not '<sha256>  <name>'"),
        ]
        for text, want in cases:
            with self.subTest(want=want):
                (self.bundle / "SHA256SUMS").write_text(text, encoding="utf-8")
                r = self.deploy()
                self.assertEqual(r.returncode, 1, r.stderr)
                self.assertIn(want, r.stderr)
                self.assertIn("nothing was copied", r.stderr)
                self.assertEqual(self.calls(), [])
        (self.bundle / "SHA256SUMS").write_text(good, encoding="utf-8")
        (self.bundle / "iem-engine").write_bytes(b"\x7fELF another build")
        r = self.deploy()
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("iem-engine of the bundle does not have its SHA256SUMS sum on the dev box", r.stderr)
        self.assertEqual(self.calls(), [])

    def test_sums_that_do_not_hold_on_the_box_install_nothing(self) -> None:
        r = self.deploy(FAKE_SCP_CORRUPT="iem-server")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("SHA256SUMS does not hold on the box: nothing installed", r.stderr)
        self.assertEqual(self.kinds()[-1], f"cd {STAGE} && sha256sum --check --strict --quiet SHA256SUMS")
        self.assertFalse((self.box / "root" / "opt").exists())

    # Refusals on the box.

    def test_the_services_never_run_as_root(self) -> None:
        r = self.deploy(FAKE_USER="root")
        self.assertEqual(r.returncode, 1)
        self.assertIn("not root", r.stderr)
        self.assertEqual(self.remote(), ["id -un"])

    def test_a_box_whose_sudo_does_not_allow_the_stacks_stop_is_refused_before_any_copy(self) -> None:
        # "ide event" and the live guard stop the stack with exactly this
        # command: a stack they could not stop is never installed, and the
        # running device is left alone.
        r = self.deploy(FAKE_NO_STOP_RULE="1")
        self.assertEqual(r.returncode, 1)
        self.assertIn(STOP, r.stderr)
        self.assertIn("sudoers", r.stderr)
        self.assertIn("nothing was copied", r.stderr)
        self.assertEqual(self.remote(), ["id -un", "sudo -n -l systemctl stop iem-server iem-engine iem-dante"])
        self.assertEqual(self.states()["iem-dante"], "active")

    def test_a_refused_sudo_stops_before_the_start(self) -> None:
        r = self.deploy(FAKE_SUDO_FAILS="1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("a password is required", r.stderr)
        self.assertTrue(self.remote()[-1].startswith("set -e\n"))
        self.assertFalse(any("systemctl start" in c for c in self.remote()))

    def test_an_install_that_fails_at_a_unit_never_leaves_a_unit_without_its_user(self) -> None:
        # A fresh box: the install stops at the engine's unit. Every drop-in
        # naming the user is in place already, so no unit file runs as root.
        r = self.deploy(FAKE_SUDO_FAILS_AT="/etc/systemd/system/iem-engine.service")
        self.assertEqual(r.returncode, 1)
        self.assertIn("Read-only file system", r.stderr)
        for name in UNITS:
            self.assertEqual(self.installed(f"/etc/systemd/system/{name}.service.d/10-user.conf"),
                             b"[Service]\nUser=tester\n")
        self.assertFalse(self.exists("/etc/systemd/system/iem-engine.service"))
        self.assertFalse(any("systemctl start" in c for c in self.remote()))

    # The read back.

    def test_an_installed_binary_with_another_sha256_fails(self) -> None:
        r = self.deploy(FAKE_BAD_INSTALLED="iem-soakclient")
        self.assertEqual(r.returncode, 1)
        self.assertIn(f"the installed /opt/iemmixer/bin/iem-soakclient's sha256 is {'f' * 64}", r.stderr)

    def test_versions_that_do_not_name_the_sha_fail(self) -> None:
        other = "f" * 40
        cases = [({"FAKE_ENGINE_VERSION": f"iem-engine 2.0.0-dev.45+{other}"}, "iem-engine --version said"),
                 ({"FAKE_ENGINE_VERSION": f"iem-engine 2.0.0-dev.45 {COMMIT}"}, "iem-engine --version said"),
                 ({"FAKE_DEVICE_VERSION": f"iem-dante-dev 2.0.0-dev.45 {other}"}, "iem-dante-dev --version said"),
                 ({"FAKE_DEVICE_VERSION": "something else"}, "iem-dante-dev --version said")]
        for env, want in cases:
            with self.subTest(env=env):
                r = self.deploy(**env)
                self.assertEqual(r.returncode, 1, r.stderr)
                self.assertIn(want, r.stderr)

    def test_a_unit_that_is_not_active_after_the_start_fails(self) -> None:
        for active, want in (("iem-server=failed", "iem-server is 'failed'"),
                             ("iem-engine=activating", "iem-engine is 'activating'")):
            with self.subTest(active=active):
                r = self.deploy(FAKE_ACTIVE=active)
                self.assertEqual(r.returncode, 1, r.stderr)
                self.assertIn(want, r.stderr)
                self.assertFalse(any(c.startswith("curl ") for c in self.remote()))
                (self.box / "calls.log").unlink()

    def test_the_server_is_asked_until_it_answers_and_must_name_the_sha(self) -> None:
        r = self.deploy(FAKE_CURL_FAILS="2")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(sum(c.startswith("curl ") for c in self.remote()), 3)
        (self.box / "calls.log").unlink()
        (self.box / "curl.count").unlink()
        r = self.deploy(FAKE_CURL_NEVER="1")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("did not answer /api/version on port 8080 within 3 tries", r.stderr)
        self.assertEqual(sum(c.startswith("curl ") for c in self.remote()), 3)
        r = self.deploy(FAKE_GIT_HASH="abcdef0")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn(f"names abcdef0, not a prefix of {COMMIT}", r.stderr)

    # iempc's private env (GEN3_HOST).

    def gen3_warnings(self, r: subprocess.CompletedProcess) -> list[str]:
        return [line for line in r.stderr.splitlines() if "WARNING" in line]

    def test_a_deploy_to_the_host_ide_event_stops_warns_nothing(self) -> None:
        # Read as iempc reads the env: spaces around the key and the value,
        # quotes around it, the last line of a key winning, comments skipped.
        for text in (f"GEN3_HOST={HOST}\n", f"  GEN3_HOST = \"{HOST}\"  \n",
                     f"GEN3_HOST=other@gen3.test\n# GEN3_HOST=x@y\nGEN3_HOST={HOST}\n"):
            with self.subTest(text=text):
                self.write_pc_env(text)
                r = self.deploy()
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual(self.gen3_warnings(r), [], r.stderr)

    def test_a_deploy_to_another_host_than_gen3_host_warns(self) -> None:
        self.write_pc_env("GEN3_HOST=other@gen3.test\n")
        r = self.deploy()
        self.assertEqual(r.returncode, 0, r.stderr)
        warnings = self.gen3_warnings(r)
        self.assertEqual(len(warnings), 1, r.stderr)
        self.assertIn("GEN3_HOST", warnings[0])
        self.assertIn("ide event", warnings[0])
        # The values stay out of the line (P6: it may be pasted).
        self.assertNotIn("other@gen3.test", r.stderr)

    def test_an_env_without_gen3_host_warns_and_no_env_is_no_warning(self) -> None:
        self.write_pc_env("PC_ROOT=X:\\root\n")
        r = self.deploy()
        self.assertEqual(r.returncode, 0, r.stderr)
        warnings = self.gen3_warnings(r)
        self.assertEqual(len(warnings), 1, r.stderr)
        self.assertIn("no GEN3_HOST", warnings[0])
        self.pc_env.unlink()
        r = self.deploy()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.gen3_warnings(r), [], r.stderr)

    def test_a_bad_command_line_is_exit_2_and_calls_nothing(self) -> None:
        full = self.full_args()

        def without(flag: str) -> list[str]:
            i = full.index(flag)
            return full[:i] + full[i + 2:]

        def with_value(flag: str, value: str) -> list[str]:
            i = full.index(flag)
            return full[:i + 1] + [value] + full[i + 2:]

        empty = self.tmp / "empty-bundle"
        empty.mkdir()
        cases = [
            (without("--host"), "--host is missing"),
            (without("--bundle"), "--bundle is missing"),
            (without("--sha"), "--sha is missing"),
            (without("--iface"), "--iface is missing"),
            (full + ["--artifact", "x"], "unknown argument '--artifact'"),
            (full + ["--sha256", "x"], "unknown argument '--sha256'"),
            (full + ["--host"], "--host needs a value"),
            (full + ["--start", "both"], "--start is engine or device"),
            (with_value("--host", "-oProxyCommand=true"), "not an option"),
            (with_value("--sha", COMMIT.upper()), "40 lowercase hex"),
            (with_value("--sha", COMMIT[:-1]), "40 lowercase hex"),
            (with_value("--iface", "eth 9"), "not an interface name"),
            (with_value("--iface", "a" * 16), "not an interface name"),
            (with_value("--bundle", str(self.tmp / "missing")), "--bundle is not a folder"),
            (with_value("--bundle", str(empty)), "--bundle has no iem-engine"),
            (with_value("--config", str(self.tmp)), "--config is not a file"),
        ]
        for argv, want in cases:
            r = self.deploy(args=argv)
            self.assertEqual(r.returncode, 2, (argv, r.stderr))
            self.assertIn(want, r.stderr, argv)
        for env, want in (({"GEN3_SETTLE_S": "x"}, "GEN3_SETTLE_S"), ({"GEN3_SERVER_WAIT_S": "0"}, "GEN3_SERVER_WAIT_S")):
            r = self.deploy(**env)
            self.assertEqual(r.returncode, 2, r.stderr)
            self.assertIn(want, r.stderr)
        self.assertEqual(self.calls(), [])


class UnitTests(unittest.TestCase):
    """The three units (plan Task 6 Interfaces, choice 5)."""

    def setUp(self) -> None:
        self.units = {name: parse_unit(path) for name, path in UNITS.items()}

    def test_the_engine_and_the_standalone_device_conflict_both_ways(self) -> None:
        self.assertEqual(self.units["iem-engine"]["Unit"].get("Conflicts"), ["iem-dante.service"])
        self.assertEqual(self.units["iem-dante"]["Unit"].get("Conflicts"), ["iem-engine.service"])
        self.assertNotIn("Conflicts", self.units["iem-server"]["Unit"])

    def test_no_ordering_or_dependency_between_the_engine_and_the_server(self) -> None:
        # The server reconnects to the engine by itself: the two start and stop
        # in parallel, and stopping one never stops the other.
        keys = ("After", "Before", "Requires", "Wants", "BindsTo", "PartOf", "Requisite", "Upholds", "PropagatesStopTo",
                "StopPropagatedFrom")
        for name, other in (("iem-server", "iem-engine"), ("iem-engine", "iem-server")):
            for key in keys:
                for value in self.units[name]["Unit"].get(key, []):
                    self.assertNotIn(other, value, (name, key))

    def test_every_unit_stops_within_its_bound_never_killed_and_is_never_enabled_by_itself(self) -> None:
        for name, unit in self.units.items():
            with self.subTest(unit=name):
                self.assertEqual(one(unit, "Service", "TimeoutStopSec"), "10")
                self.assertEqual(one(unit, "Service", "SendSIGKILL"), "no")
                self.assertEqual(one(unit, "Service", "Restart"), "on-failure")
                self.assertEqual(one(unit, "Service", "NoNewPrivileges"), "yes")
                self.assertEqual(one(unit, "Service", "CapabilityBoundingSet"), "")
                self.assertEqual(one(unit, "Service", "AmbientCapabilities"), "")
                # The user comes from deploy.sh's drop-in (P6), never the unit.
                self.assertNotIn("User", unit["Service"])
                # [Install] stays so `disable` removes an earlier enable.
                self.assertEqual(one(unit, "Install", "WantedBy"), "multi-user.target")

    def test_the_engine_unit_restarts_except_on_a_usage_or_card_refusal_and_stays_down_in_a_crash_loop(self) -> None:
        engine = self.units["iem-engine"]
        self.assertEqual(one(engine, "Service", "RestartSec"), "2")
        self.assertEqual(one(engine, "Service", "RestartPreventExitStatus"), "2 3")
        # The start limit lives in [Unit] (systemd ignores it in [Service]).
        self.assertEqual(one(engine, "Unit", "StartLimitIntervalSec"), "600")
        self.assertEqual(one(engine, "Unit", "StartLimitBurst"), "3")
        self.assertEqual(one(engine, "Service", "LimitRTPRIO"), "80")
        self.assertEqual(one(engine, "Service", "StateDirectory"), "iemmixer iem-dante")
        self.assertEqual(one(engine, "Service", "RuntimeDirectory"), "iemmixer iem-dante")
        self.assertEqual(one(engine, "Service", "RuntimeDirectoryMode"), "0700")
        self.assertEqual(one(engine, "Service", "EnvironmentFile"), "/etc/iemmixer/iem-dante.env")

    def test_the_engine_runs_the_dante_backend_on_the_servers_site_and_pipe(self) -> None:
        engine = shlex.split(one(self.units["iem-engine"], "Service", "ExecStart"))
        self.assertEqual(engine[:4], ["/opt/iemmixer/bin/iem-engine", "run", "--backend", "dante"])
        opts = dict(zip(engine[4::2], engine[5::2]))
        self.assertEqual(len(engine[4:]), 2 * len(opts))
        self.assertEqual(opts, {"--site": "/var/lib/iemmixer/site/iemmixer.toml",
                                "--state-dir": "/var/lib/iemmixer/engine",
                                "--pipe": "/run/iemmixer/engine.sock",
                                "--dante-config": "/etc/iemmixer/dante-device.toml",
                                "--dante-iface": "${IEM_DANTE_IFACE}"})
        server = self.units["iem-server"]
        self.assertEqual(one(server, "Service", "ExecStart"), "/opt/iemmixer/bin/iem-server")
        env = dict(item.split("=", 1) for item in shlex.split(one(server, "Service", "Environment")))
        self.assertEqual(env, {"IEMMIXER_CONFIG": opts["--site"], "IEMMIXER_ENGINE_PIPE": opts["--pipe"],
                               "PORT": "8080", "RUST_LOG": "info"})
        # The site is where deploy.sh copies it, in the folders the units own.
        deploy = DEPLOY.read_text(encoding="utf-8")
        self.assertIn("SITE=$SITEDIR/iemmixer.toml", deploy)
        self.assertIn("SITEDIR=/var/lib/iemmixer/site", deploy)
        self.assertTrue(opts["--site"].startswith("/var/lib/iemmixer/"))
        self.assertTrue(opts["--state-dir"].startswith("/var/lib/iemmixer/"))
        self.assertTrue(opts["--pipe"].startswith("/run/iemmixer/"))
        self.assertEqual(one(server, "Service", "StateDirectory"), "iemmixer")
        self.assertNotIn("RuntimeDirectory", server["Service"])
        # The engine's device reads the standalone device's config, and the
        # device's control socket stays at its phase B path in /run/iem-dante.
        device = shlex.split(one(self.units["iem-dante"], "Service", "ExecStart"))
        self.assertEqual(device[device.index("--config") + 1], opts["--dante-config"])
        self.assertEqual(device[device.index("--control") + 1], "/run/iem-dante/control.sock")
        self.assertNotIn("--dante-control", opts)

    def test_the_standalone_device_keeps_fifo_its_private_runtime_folder_and_its_stop(self) -> None:
        device = self.units["iem-dante"]
        for key, value in (("LimitRTPRIO", "80"), ("RuntimeDirectory", "iem-dante"), ("RuntimeDirectoryMode", "0700"),
                           ("StateDirectory", "iem-dante"), ("RestartPreventExitStatus", "2")):
            self.assertEqual(one(device, "Service", key), value)
        exec_start = one(device, "Service", "ExecStart")
        self.assertIn("--control /run/iem-dante/control.sock", exec_start)


if __name__ == "__main__":
    unittest.main()
