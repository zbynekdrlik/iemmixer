"""Tests for scripts/gen3/deploy.sh (#72 phase C, plan Task 6; phase D, plan
docs/superpowers/plans/2026-10-26-gen3-phase-d-the-box.md Task 7: two slots,
attested, from one commit): the Gen 3 stack's deploy, run from a copy of this
checkout's script, units and test site in a git repository of its own (the
checkout deploy.sh checks), against a stand-in `ssh`, `scp` and `gh` first on
PATH; ssh and scp play the box in a temporary folder (its home, its root with
real links for the slots, and its units' states). Also the units' contents
(phase C's three; phase D's service user and alarm unit, Task 5) and the sudo
template (sudo-lines.txt). The stand-in ssh records each call's command line
and its stdin apart, so a PIN is seen to travel only on stdin. Every value is
synthetic (P6)."""
from __future__ import annotations

import hashlib
import itertools
import json
import os
import re
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
ALARM = HERE / "iem-alarm@.service"
UNIT_FILES = ("iem-dante.service", "iem-engine.service", "iem-server.service", "iem-alarm@.service")
TEMPLATE = HERE / "sudo-lines.txt"
TEST_SITE = REPO / "config" / "test-site.toml"
TEST_ROSTER = REPO / "config" / "test-roster.toml"
HOST = "tester@gen3.test"
BINARIES = ("iem-engine", "iem-server", "iem-dante-dev", "iem-soakclient")
SLOT_FILES = (*BINARIES, "BUILD", "SHA256SUMS")
STAGE = ".cache/iem-gen3-deploy"
STOP = "sudo -n systemctl stop iem-server iem-engine iem-dante"
CHECK = "sha256sum --check --strict --quiet SHA256SUMS"
STAT = "stat -c '%n %F' /opt/iemmixer/bin /opt/iemmixer/bin.phase-c 2> /dev/null"
HIL_DROPIN = "/run/systemd/system/iem-engine.service.d/iem-hil.conf"
LEFT_JOB = f"test -e {HIL_DROPIN} && echo left || echo none"
LIST_UNITS = ("systemctl list-units --all --plain --no-legend --full iem-dante.service iem-engine.service "
              "iem-server.service")
PIN_RUN = ("sudo -n systemd-run --quiet --pipe --wait --collect --uid=iemmixer --gid=iemmixer "
           "--property=LoadCredentialEncrypted=pepper:/etc/credstore.encrypted/iemmixer-pepper "
           "--setenv=IEMMIXER_CONFIG=/var/lib/iemmixer/site/iemmixer.toml /opt/iemmixer/bin/iem-server pin")
GH_VERIFY = ["attestation", "verify", "<bundle>/SHA256SUMS", "-R", "zbynekdrlik/iemmixer", "--signer-workflow",
             "zbynekdrlik/iemmixer/.github/workflows/ci.yml"]
ENGINEER_PIN = "3917"
MEMBER_PIN = "6284"
ENABLE = "sudo -n systemctl enable iem-engine iem-server"
IS_ENABLED = "systemctl is-enabled iem-engine iem-server"
CARD_NAME = "Zyxqw-Iem-Card"   # the IEM card's usual name (DANTE_IEM_NAME), invented


def holds_pin(text: str | bytes, pin: str) -> bool:
    """Whether `pin` stands in `text` as itself, not inside a run of hex
    digits: a commit SHA or a sha256 digest holds any four digits by chance
    (CI, 2026-10-28: the member PIN inside a staged SHA256SUMS)."""
    data = text.encode() if isinstance(text, str) else text
    return re.search(rb"(?<![0-9a-fA-F])" + pin.encode() + rb"(?![0-9a-fA-F])", data) is not None

# The box: `ssh` answers the deploy's commands from FAKE_BOX (home/, root/ and
# states.json, the units' states), `scp` copies into its home; both log every
# command line as one JSON line in calls.log, and ssh logs its stdin (one JSON
# line per ssh call, in the same order) in stdin.log and its environment in
# env.log. A `set -e` script runs line by line and stops at the first failure.
# The slots' link is a real symbolic link in root/, so `readlink`, `mv -T` and
# a read through /opt/iemmixer/bin behave as on the box; `--version` names the
# BUILD the link leads to.
FAKE_SSH = r'''
import hashlib, json, os, re, shutil, sys
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
STACK = ["iem-server", "iem-engine", "iem-dante"]
PIN = ("systemd-run --quiet --pipe --wait --collect --uid=iemmixer --gid=iemmixer "
       "--property=LoadCredentialEncrypted=pepper:/etc/credstore.encrypted/iemmixer-pepper "
       "--setenv=IEMMIXER_CONFIG=/var/lib/iemmixer/site/iemmixer.toml /opt/iemmixer/bin/iem-server pin ")
CHECK = " && sha256sum --check --strict --quiet SHA256SUMS"

def at(path):
    return os.path.join(root, path.lstrip("/"))

def save():
    json.dump(st, open(states_path, "w"))

def flag_on(when):
    if os.environ.get("FAKE_FLAG_ON") == when:
        open(os.environ["IEMMIXER_EVENT_NOW"], "w").write("now\n")

def failing(env):
    return os.environ.get(env)

def err(text, code=1):
    sys.stderr.write(text + "\n")
    return code

def log(name, text):
    open(os.path.join(box, name), "a").write(text + "\n")

def check_sums(folder):
    sums = os.path.join(folder, "SHA256SUMS")
    if not os.path.isfile(sums):
        return err("sha256sum: SHA256SUMS: No such file or directory")
    bad = 0
    for line in open(sums).read().splitlines():
        want, name = line.split("  ", 1)
        path = os.path.join(folder, name)
        if not os.path.isfile(path) or hashlib.sha256(open(path, "rb").read()).hexdigest() != want:
            print(f"{name}: FAILED")
            bad += 1
    return 1 if bad else 0

def version(binary):
    build = open(at("/opt/iemmixer/bin/BUILD")).read().strip()
    return "iem-engine 2.0.0-dev.45+" + build if binary == "iem-engine" else "iem-dante-dev 2.0.0-dev.45 " + build

def health():
    count = os.path.join(box, "health.count")
    n = int(open(count).read()) + 1 if os.path.exists(count) else 1
    open(count, "w").write(str(n))
    if failing("FAKE_HEALTH"):
        codes = [int(c) for c in os.environ["FAKE_HEALTH"].split(",")]
        code = codes[min(n, len(codes)) - 1]
    else:
        up = [u for u in ("iem-engine", "iem-server", "iem-dante") if st["units"].get(u) == "active"]
        code = 0 if up in (["iem-engine", "iem-server"], ["iem-dante"]) else 3 if not up else 1
    if code == 2:
        return err("iem-dante-dev: unknown argument", 2)
    state = {0: "healthy", 1: "degraded", 3: "stopped"}[code]
    print(json.dumps({"health": {"state": state, "reasons": [], "read": n}}))
    return code

def systemctl(words):
    verb, units = words[0], words[1:]
    if verb == "stop" and units == STACK:
        if st["started"] and failing("FAKE_LATE_STOP_FAILS"):
            return err("sudo: a password is required")
        code = 0
        for unit in units:
            if unit in st["not_loaded"]:
                sys.stderr.write(f"Failed to stop {unit}.service: Unit {unit}.service not loaded.\n")
                code = 5
            elif os.environ.get("FAKE_STILL_RUNS") == unit:
                st["units"][unit] = "deactivating"
            elif st["units"].get(unit) != "failed":   # a stop leaves a failed unit failed
                st["units"][unit] = "inactive"
        return code
    if verb == "daemon-reload" and not units:
        st["not_loaded"] = []
        return 0
    if verb == "disable" and units == ["iem-dante", "iem-engine", "iem-server"]:
        st["enabled"] = []
        return 0
    if verb == "enable" and units == ["iem-engine", "iem-server"]:
        if failing("FAKE_ENABLE_FAILS"):
            return err("Failed to enable unit: Access denied")
        st["enabled"] = sorted(set(st.get("enabled", [])) | set(units))
        return 0
    if verb == "reset-failed":
        for unit in units:
            if st["units"].get(unit, "inactive") == "inactive":
                return err(f"Failed to reset failed state of unit {unit}.service: Unit {unit}.service not loaded.")
        log("reset.log", json.dumps(units))
        return 0
    if verb == "start" and units in (["iem-engine", "iem-server"], ["iem-dante"]):
        for unit in units:
            st["units"][unit] = "active"
        st["started"] = True
        return 0
    return err("unexpected systemctl: " + " ".join(words), 97)

def sudo(words):
    """The command of one `sudo -n <command>`."""
    line = " ".join(words)
    if words[:2] == ["install", "-D"]:
        opts, i = {}, 2
        while words[i] in ("-m", "-o", "-g"):
            opts[words[i]] = words[i + 1]
            i += 2
        src, dst = words[i], words[i + 1]
        if os.environ.get("FAKE_SUDO_FAILS_AT") == dst:
            return err("install: cannot create regular file: Read-only file system")
        target = at(dst)
        os.makedirs(os.path.dirname(target), exist_ok=True)
        if "/slot-" in dst and os.environ.get("FAKE_SLOT_CORRUPT") == os.path.basename(dst):
            open(target, "wb").write(b"changed in the slot")
        else:
            shutil.copyfile(os.path.join(home, src), target)
        log("modes.log", " ".join([opts.get("-o", "root"), opts.get("-g", "root"), opts["-m"], dst]))
        return 0
    if words[:2] == ["install", "-d"]:
        opts = dict(zip(words[2:-1:2], words[3:-1:2]))
        os.makedirs(at(words[-1]), exist_ok=True)
        log("modes.log", " ".join(["dir", opts["-o"], opts["-g"], opts["-m"], words[-1]]))
        return 0
    if words[:2] == ["mv", "-T"] and len(words) == 4:
        src, dst = at(words[2]), at(words[3])
        if not os.path.lexists(src):
            return err(f"mv: cannot stat '{words[2]}': No such file or directory")
        if os.path.isdir(dst) and not os.path.islink(dst):
            if os.path.islink(src) or not os.path.isdir(src):
                return err(f"mv: cannot overwrite directory '{words[3]}' with non-directory")
            if os.listdir(dst):
                return err(f"mv: cannot move '{words[2]}' to '{words[3]}': Directory not empty")
        os.replace(src, dst)
        return 0
    if words[:2] == ["ln", "-sfn"] and len(words) == 4:
        link = at(words[3])
        if os.path.lexists(link):
            os.remove(link)
        os.symlink(words[2], link)
        return 0
    if words[:2] == ["rm", "-f"] and len(words) == 3:
        # gen3-gate's HIL job drop-in (phase H), the one file deploy removes.
        if os.path.lexists(at(words[2])):
            os.remove(at(words[2]))
        log("removed.log", words[2])
        return 0
    if words[0] == "systemctl":
        return systemctl(words[1:])
    if line.startswith(PIN):
        if not os.path.exists(at("/var/lib/iemmixer/site/iemmixer.toml")):
            return err("iem-server: site config: not found")
        if failing("FAKE_PIN_FAILS"):
            return err("iem-server: unknown member", 2)
        log("pins.log", json.dumps([line[len(PIN):], data.decode()]))
        sys.stderr.write("iem-server: stored the PIN hash\n")
        return 0
    return err("unexpected sudo command: " + line, 97)

def run(line):
    words = line.split()
    if words[:2] == ["sudo", "-n"]:
        if words[2] == "-l":
            if words[3:] != ["systemctl", "stop"] + STACK:
                return err("unexpected sudo -l: " + line, 97)
            if failing("FAKE_NO_STOP_RULE"):
                return err("Sorry, user tester is not allowed to execute this command.")
            print("/usr/bin/systemctl stop iem-server iem-engine iem-dante")
            return 0
        return sudo(words[2:])
    if line == "readlink /opt/iemmixer/bin":
        if not os.path.islink(at("/opt/iemmixer/bin")):
            return 1
        print(os.readlink(at("/opt/iemmixer/bin")))
        return 0
    if line == "stat -c '%n %F' /opt/iemmixer/bin /opt/iemmixer/bin.phase-c 2> /dev/null":
        code = 0
        for path in ("/opt/iemmixer/bin", "/opt/iemmixer/bin.phase-c"):
            p = at(path)
            if not os.path.lexists(p):
                code = 1
            else:
                kind = "symbolic link" if os.path.islink(p) else "directory" if os.path.isdir(p) else "regular file"
                print(f"{path} {kind}")
        return code
    m = re.fullmatch(r"cat (/opt/iemmixer/slot-[ab]/bin/(?:BUILD|SHA256SUMS))", line)
    if m:
        if not os.path.isfile(at(m.group(1))):
            return err(f"cat: {m.group(1)}: No such file or directory")
        sys.stdout.write(open(at(m.group(1))).read())
        return 0
    if line == "cd .cache/iem-gen3-deploy" + CHECK:
        return check_sums(os.path.join(home, ".cache/iem-gen3-deploy"))
    m = re.fullmatch(r"cd (/opt/iemmixer/slot-[ab]/bin)" + re.escape(CHECK), line)
    if m:
        return check_sums(at(m.group(1)))
    m = re.fullmatch(r"test -e (\S+) && echo left \|\| echo none", line)
    if m:
        if failing("FAKE_LEFT_FAILS"):
            return err("sh: /run: Input/output error", 7)
        print("left" if os.path.lexists(at(m.group(1))) else "none")
        return 0
    if line == "mkdir -p .cache/iem-gen3-deploy":
        os.makedirs(os.path.join(home, ".cache/iem-gen3-deploy"), exist_ok=True)
        flag_on("mkdir")
        return 0
    if line == "systemctl list-units --all --plain --no-legend --full iem-dante.service iem-engine.service iem-server.service":
        # systemd keeps in memory only a unit that runs, ends or failed
        # lately; one installed and never started (or long inactive) is not
        # listed. Not in the deploy's order, as systemd's own.
        for unit in ("iem-server", "iem-engine", "iem-dante"):
            state = st["units"].get(unit, "inactive")
            if unit not in st["not_loaded"] and state != "inactive":
                print(f"{unit}.service loaded {state} {state} {unit}")
        return 0
    if line == "systemctl is-enabled iem-engine iem-server":
        enabled = [u in st.get("enabled", []) and not failing("FAKE_ENABLE_LOST") for u in words[2:]]
        for on in enabled:
            print("enabled" if on else "disabled")
        return 0 if all(enabled) else 1
    if line.startswith("systemctl is-active "):
        names = line.split()[2:]
        for unit in names:
            print(st["units"].get(unit, "inactive"))
        return 0 if all(st["units"].get(u) == "active" for u in names) else 3
    if words[0] == "sha256sum":
        for path in words[1:]:
            digest = hashlib.sha256(open(at(path), "rb").read()).hexdigest()
            if os.environ.get("FAKE_BAD_INSTALLED") == os.path.basename(path):
                digest = "f" * 64
            print(digest + "  " + path)
        return 0
    if line == "/opt/iemmixer/bin/iem-engine --version":
        print(os.environ.get("FAKE_ENGINE_VERSION") or version("iem-engine"))
        return 0
    if line == "/opt/iemmixer/bin/iem-dante-dev --version":
        print(os.environ.get("FAKE_DEVICE_VERSION") or version("iem-dante-dev"))
        return 0
    if line == "/opt/iemmixer/bin/iem-dante-dev health":
        return health()
    return err("unexpected command: " + line, 97)

if cmd.startswith("set -e\n"):
    lines = cmd.split("\n")[1:]
    kind = ("slot" if "/opt/iemmixer/slot-" in cmd and "install" in cmd else "switch" if "ln -sfn" in cmd
            else "install" if "/etc/systemd/system/" in cmd else "start" if "systemctl start" in cmd else "other")
    for line in lines:
        if failing("FAKE_SUDO_FAILS") and line.startswith("sudo -n "):
            sys.exit(err("sudo: a password is required"))
        code = run(line)
        if code:
            save()
            sys.exit(code)
    save()
    flag_on(kind)
    sys.exit(0)
code = run(cmd)
save()
sys.exit(code)
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

# gh: only `gh attestation verify <file> ...`, logged with the file's sha256
# (gh.log), refused with FAKE_GH_FAILS.
FAKE_GH = r'''
import hashlib, json, os, sys
box = os.environ["FAKE_BOX"]
args = sys.argv[1:]
digest = hashlib.sha256(open(args[2], "rb").read()).hexdigest() if len(args) > 2 and os.path.isfile(args[2]) else None
open(os.path.join(box, "gh.log"), "a").write(json.dumps({"args": args, "digest": digest}) + "\n")
if args[:2] != ["attestation", "verify"]:
    sys.stderr.write("unexpected gh call\n")
    sys.exit(97)
if os.environ.get("FAKE_GH_FAILS"):
    sys.stderr.write("Error: no attestations found for subject\n")
    sys.exit(1)
print("Verification succeeded!")
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


def template_lines() -> list[tuple[str, str]]:
    """sudo-lines.txt as (runas, command), comments and blank lines left out."""
    out = []
    for raw in TEMPLATE.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        runas, sep, command = line.partition(" | ")
        if not sep or runas not in ("root", "iemmixer") or not command or command.startswith("sudo"):
            raise AssertionError(f"sudo-lines.txt: not '<runas> | <command>': {line!r}")
        out.append((runas, command))
    return out


def expand(command: str) -> set[str]:
    """A template command with its placeholders expanded."""
    stack = ("iem-dante", "iem-engine", "iem-server")
    values = {
        "{slot}": ["a", "b"],
        "{member}": roster_people(),
        "{units}": [" ".join(c) for n in range(1, 4) for c in itertools.combinations(stack, n)],
    }
    out = [command]
    for key, choices in values.items():
        out = [text.replace(key, choice) for text in out for choice in choices] if key in command else out
    return set(out)


def sudo_lines(remote: list[str]) -> list[tuple[str, str, bool]]:
    """Every `sudo` a list of remote commands holds, as (runas, command,
    listed): one per line, at the line's start (a sudo elsewhere in a line
    fails the test)."""
    out = []
    for command in remote:
        for line in command.split("\n"):
            if "sudo" not in line.split():
                continue
            words = line.split()
            if words[:2] != ["sudo", "-n"] or words.count("sudo") != 1:
                raise AssertionError(f"a sudo that is not one `sudo -n` at a line's start: {line!r}")
            rest, runas, listed = words[2:], "root", False
            if rest[:1] == ["-l"]:
                rest, listed = rest[1:], True
            if rest[:1] == ["-u"]:
                runas, rest = rest[1], rest[2:]
            out.append((runas, " ".join(rest), listed))
    return out


class DeployTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.box = self.tmp / "box"
        (self.box / "home").mkdir(parents=True)
        (self.box / "root").mkdir()
        bin_dir = self.tmp / "bin"
        bin_dir.mkdir()
        for name, body in (("ssh", FAKE_SSH), ("scp", FAKE_SCP), ("gh", FAKE_GH)):
            p = bin_dir / name
            p.write_text(f"#!{sys.executable}\n{body}", encoding="utf-8")
            p.chmod(0o755)
        self.flag = self.tmp / "config" / "EVENT-NOW"
        # iempc's private env, absent unless a test writes it: the dev box's
        # own is never read.
        self.pc_env = self.tmp / "config" / "iem-pc.env"
        # git without this user's or the system's configuration.
        self.git_env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        self.git_env.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1", GIT_AUTHOR_NAME="tester",
                            GIT_AUTHOR_EMAIL="tester@example.org", GIT_COMMITTER_NAME="tester",
                            GIT_COMMITTER_EMAIL="tester@example.org")
        self.env = {**self.git_env, "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}", "FAKE_BOX": str(self.box),
                    "IEMMIXER_EVENT_NOW": str(self.flag), "GEN3_SERVER_WAIT_S": "3", "PC_ENV": str(self.pc_env),
                    # iempc's state of this test only: the dev box's own side record never reaches a test.
                    "IEMPC_STATE": str(self.tmp / "iempc-state")}
        # The checkout deploy.sh runs from: this one's script, units and test
        # site, committed in a repository of its own.
        self.repo = self.tmp / "checkout"
        (self.repo / "scripts" / "gen3").mkdir(parents=True)
        (self.repo / "config").mkdir()
        for path in (DEPLOY, TEMPLATE, HERE / "band_receive.py", *(HERE / name for name in UNIT_FILES)):
            shutil.copyfile(path, self.repo / "scripts" / "gen3" / path.name)
        for path in (TEST_SITE, TEST_ROSTER):
            shutil.copyfile(path, self.repo / "config" / path.name)
        self.git("init", "-q", "-b", "dev")
        self.sha = self.commit("the first build")
        self.bundle = self.make_bundle(self.sha)
        self.config = self.tmp / "dante-device.toml"
        self.config.write_text('name = "test-device"\n', encoding="utf-8")
        self.set_states({"iem-dante": "active", "iem-engine": "inactive", "iem-server": "inactive"})

    # The checkout and the bundles.

    def git(self, *args: str) -> str:
        return subprocess.run(["git", "-C", str(self.repo), *args], env=self.git_env, check=True,
                              capture_output=True, text=True).stdout.strip()

    def commit(self, note: str) -> str:
        (self.repo / "notes.txt").write_text(note + "\n", encoding="utf-8")
        self.git("add", "-A")
        self.git("commit", "-q", "-m", note)
        return self.git("rev-parse", "HEAD")

    def make_bundle(self, sha: str) -> Path:
        """The CI bundle of `sha` (ci.yml, gen3-bin): four binaries, BUILD,
        SHA256SUMS; every build's binaries differ."""
        bundle = self.tmp / f"gen3-bundle-{sha}"
        bundle.mkdir()
        for name in BINARIES:
            (bundle / name).write_bytes(b"\x7fELF a synthetic " + name.encode() + b" of " + sha.encode())
        (bundle / "BUILD").write_text(sha + "\n", encoding="utf-8")
        self.write_sums(bundle)
        return bundle

    def write_sums(self, bundle: Path, names: tuple[str, ...] = (*BINARIES, "BUILD")) -> None:
        lines = [f"{hashlib.sha256((bundle / n).read_bytes()).hexdigest()}  {n}\n" for n in names]
        (bundle / "SHA256SUMS").write_text("".join(lines), encoding="utf-8")

    def next_build(self, note: str) -> tuple[str, Path]:
        sha = self.commit(note)
        return sha, self.make_bundle(sha)

    # The box.

    def set_states(self, units: dict[str, str], not_loaded: tuple[str, ...] = (),
                   enabled: tuple[str, ...] = ()) -> None:
        (self.box / "states.json").write_text(json.dumps({"units": units, "not_loaded": list(not_loaded),
                                                          "started": False, "enabled": list(enabled)}),
                                              encoding="utf-8")

    def states(self) -> dict[str, str]:
        return json.loads((self.box / "states.json").read_text(encoding="utf-8"))["units"]

    def enabled(self) -> list[str]:
        return json.loads((self.box / "states.json").read_text(encoding="utf-8")).get("enabled", [])

    def phase_c_folder(self, names: tuple[str, ...] = BINARIES) -> dict[str, bytes]:
        """Phase C's real /opt/iemmixer/bin with its binaries."""
        folder = self.box / "root" / "opt" / "iemmixer" / "bin"
        folder.mkdir(parents=True)
        files = {name: b"phase C's " + name.encode() for name in names}
        for name, data in files.items():
            (folder / name).write_bytes(data)
        return files

    def link(self) -> str | None:
        path = self.box / "root" / "opt" / "iemmixer" / "bin"
        return os.readlink(path) if path.is_symlink() else None

    def slot(self, target: str) -> Path:
        return self.box / "root" / "opt" / "iemmixer" / f"slot-{target}" / "bin"

    def assert_slot_holds(self, target: str, bundle: Path) -> None:
        for name in SLOT_FILES:
            self.assertEqual((self.slot(target) / name).read_bytes(), (bundle / name).read_bytes(), (target, name))

    def installed(self, path: str) -> bytes:
        return (self.box / "root" / path.lstrip("/")).read_bytes()

    def exists(self, path: str) -> bool:
        return os.path.lexists(self.box / "root" / path.lstrip("/"))

    # Running it.

    def full_args(self, sha: str | None = None, bundle: Path | None = None) -> list[str]:
        return ["--host", HOST, "--bundle", str(bundle or self.bundle), "--sha", sha or self.sha,
                "--config", str(self.config), "--iface", "eth9"]

    def back_args(self, sha: str) -> list[str]:
        return ["--host", HOST, "--back", "--sha", sha, "--config", str(self.config), "--iface", "eth9"]

    def deploy(self, *extra: str, args: list[str] | None = None, **env: str) -> subprocess.CompletedProcess:
        argv = args if args is not None else [*self.full_args(), *extra]
        return subprocess.run(["bash", str(self.repo / "scripts" / "gen3" / "deploy.sh"), *argv],
                              env={**self.env, **env}, stdin=subprocess.DEVNULL, capture_output=True, text=True,
                              timeout=120)

    def deployed(self, *extra: str, args: list[str] | None = None, **env: str) -> subprocess.CompletedProcess:
        """A deploy that must succeed; the logs start afresh after it."""
        r = self.deploy(*extra, args=args, **env)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.clear_logs()
        return r

    def clear_logs(self) -> None:
        for name in ("calls.log", "stdin.log", "env.log", "modes.log", "health.count", "gh.log", "pins.log",
                     "reset.log", "removed.log"):
            (self.box / name).unlink(missing_ok=True)

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

    def gh_calls(self) -> list[dict]:
        log = self.box / "gh.log"
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

    def sequence(self) -> list[str]:
        """Each call as 'scp' or its remote command."""
        out, remote = [], iter(self.remote())
        for call in self.calls():
            out.append("scp" if call[0] == "scp" else next(remote))
        return out

    def kinds(self) -> list[str]:
        """Each call as 'scp' or its remote command's first line."""
        return [c.split("\n")[0] for c in self.sequence()]

    def scp_names(self) -> list[str]:
        scp = [call for call in self.calls() if call[0] == "scp"]
        self.assertEqual(len(scp), 1)
        self.assertEqual(scp[0][-1], f"{HOST}:{STAGE}/")
        words, args = [], scp[0][1:-1]
        while args:
            if args[0] == "-o":
                args = args[2:]
            elif args[0] == "-q":
                args = args[1:]
            else:
                words.append(os.path.basename(args.pop(0)))
        return sorted(words)

    def health_reads(self) -> int:
        count = self.box / "health.count"
        return int(count.read_text(encoding="utf-8")) if count.exists() else 0

    def write_pc_env(self, text: str) -> None:
        self.pc_env.parent.mkdir(exist_ok=True)
        self.pc_env.write_text("PC_SSH=tester@pc.test\n" + text, encoding="utf-8")

    def write_pins(self) -> None:
        self.write_pc_env(f"GEN3_HOST={HOST}\nGEN3_ENGINEER_PIN=\"{ENGINEER_PIN}\"\n GEN3_MEMBER_PIN = {MEMBER_PIN}\n")

    # What a deploy sends, in order.

    @staticmethod
    def fill(target: str) -> str:
        slot = f"/opt/iemmixer/slot-{target}/bin"
        return "\n".join(["set -e",
                          *(f"sudo -n install -D -m 0755 {STAGE}/{n} {slot}/{n}" for n in BINARIES),
                          f"sudo -n install -D -m 0644 {STAGE}/BUILD {slot}/BUILD",
                          f"sudo -n install -D -m 0644 {STAGE}/SHA256SUMS {slot}/SHA256SUMS"])

    @staticmethod
    def switch(target: str, phase_c: bool = False) -> str:
        return "\n".join(["set -e", *(["sudo -n mv -T /opt/iemmixer/bin /opt/iemmixer/bin.phase-c"] if phase_c else []),
                          f"sudo -n ln -sfn slot-{target}/bin /opt/iemmixer/bin.next",
                          "sudo -n mv -T /opt/iemmixer/bin.next /opt/iemmixer/bin"])

    INSTALL = "\n".join([
        "set -e",
        f"sudo -n install -D -m 0644 {STAGE}/iem-dante.service /etc/systemd/system/iem-dante.service",
        f"sudo -n install -D -m 0644 {STAGE}/iem-engine.service /etc/systemd/system/iem-engine.service",
        f"sudo -n install -D -m 0644 {STAGE}/iem-server.service /etc/systemd/system/iem-server.service",
        f"sudo -n install -D -m 0644 {STAGE}/iem-alarm@.service /etc/systemd/system/iem-alarm@.service",
        f"sudo -n install -D -m 0644 {STAGE}/dante-device.toml /etc/iemmixer/dante-device.toml",
        f"sudo -n install -D -m 0644 {STAGE}/iem-dante.env /etc/iemmixer/iem-dante.env",
        f"sudo -n install -D -m 0755 {STAGE}/band_receive.py /opt/iemmixer/lib/band_receive.py",
        "sudo -n install -d -o iemmixer -g iemmixer -m 0700 /var/lib/iemmixer/site",
        f"sudo -n install -D -o iemmixer -g iemmixer -m 0600 {STAGE}/iemmixer.toml /var/lib/iemmixer/site/iemmixer.toml",
        f"sudo -n install -D -o iemmixer -g iemmixer -m 0600 {STAGE}/roster.toml /var/lib/iemmixer/site/roster.toml"])

    READ_BACK = [
        "sha256sum " + " ".join(f"/opt/iemmixer/bin/{n}" for n in (*BINARIES, "BUILD")),
        "/opt/iemmixer/bin/iem-engine --version", "/opt/iemmixer/bin/iem-dante-dev --version"]

    def expected(self, target: str, *, link: str | None = None, back: bool = False, phase_c: bool = False,
                 start: str = "engine", held: tuple[str, ...] = (), people: tuple[str, ...] | None = None,
                 health: int = 1, left: bool = False, serving: bool = False) -> list[str]:
        slot = f"/opt/iemmixer/slot-{target}/bin"
        out = ["sudo -n -l systemctl stop iem-server iem-engine iem-dante", "readlink /opt/iemmixer/bin"]
        if link is None:
            out.append(STAT)
        if back:
            out += [f"cat {slot}/BUILD", f"cat {slot}/SHA256SUMS", f"cd {slot} && {CHECK}"]
        out += [f"mkdir -p {STAGE}", "scp"]
        if not back:
            out += [f"cd {STAGE} && {CHECK}", self.fill(target), f"cd {slot} && {CHECK}"]
        out += [STOP, "systemctl is-active iem-server iem-engine iem-dante", self.switch(target, phase_c),
                self.INSTALL]
        if people is not None:
            out += [f"{PIN_RUN} set-engineer", *(f"{PIN_RUN} set-member {p}" for p in people)]
        started = "iem-engine iem-server" if start == "engine" else "iem-dante"
        out += [LEFT_JOB, LIST_UNITS,
                "\n".join(["set -e", *([f"sudo -n rm -f {HIL_DROPIN}"] if left else []),
                           "sudo -n systemctl daemon-reload",
                           *([] if serving else ["sudo -n systemctl disable iem-dante iem-engine iem-server"]),
                           *([f"sudo -n systemctl reset-failed {' '.join(held)}"] if held else []),
                           f"sudo -n systemctl start {started}"]),
                *([ENABLE, IS_ENABLED] if serving else []),
                *self.READ_BACK, *["/opt/iemmixer/bin/iem-dante-dev health"] * health]
        return out

    # The whole deploy, none → a, a → b, b → a.

    def test_a_first_deploy_goes_into_slot_a_and_the_stack_runs_from_it(self) -> None:
        r = self.deploy()
        self.assertEqual(r.returncode, 0, r.stderr)
        # The order: the stop allowed; the link read (none) and what lies
        # there; the copy and its sums; slot a filled and its sums, the stack
        # still running; then the stop, nothing left running, the switch, the
        # units and the site, the units systemd holds (read only, for
        # reset-failed), the start, the read back through the link, health.
        self.assertEqual(self.sequence(), self.expected("a"))
        self.assert_slot_holds("a", self.bundle)
        self.assertEqual(self.link(), "slot-a/bin")
        self.assertFalse(self.exists("/opt/iemmixer/slot-b"))
        for name in UNIT_FILES:
            self.assertEqual(self.installed(f"/etc/systemd/system/{name}"), (HERE / name).read_bytes())
        # The units run as the service user of their own: no drop-in.
        self.assertEqual(sorted(os.listdir(self.box / "root" / "etc" / "systemd" / "system")), sorted(UNIT_FILES))
        self.assertEqual(self.installed("/etc/iemmixer/dante-device.toml"), self.config.read_bytes())
        self.assertEqual(self.installed("/etc/iemmixer/iem-dante.env"), b"IEM_DANTE_IFACE=eth9\n")
        self.assertEqual(self.installed("/var/lib/iemmixer/site/iemmixer.toml"), TEST_SITE.read_bytes())
        self.assertEqual(self.installed("/var/lib/iemmixer/site/roster.toml"), TEST_ROSTER.read_bytes())
        # The binaries 0755 (a downloaded artifact loses the executable bit),
        # the site folder and the site and roster the service user's own.
        modes = (self.box / "modes.log").read_text(encoding="utf-8").splitlines()
        self.assertEqual(modes, [
            *(f"root root 0755 /opt/iemmixer/slot-a/bin/{n}" for n in BINARIES),
            "root root 0644 /opt/iemmixer/slot-a/bin/BUILD", "root root 0644 /opt/iemmixer/slot-a/bin/SHA256SUMS",
            *(f"root root 0644 /etc/systemd/system/{n}" for n in UNIT_FILES),
            "root root 0644 /etc/iemmixer/dante-device.toml", "root root 0644 /etc/iemmixer/iem-dante.env",
            "root root 0755 /opt/iemmixer/lib/band_receive.py",
            "dir iemmixer iemmixer 0700 /var/lib/iemmixer/site",
            "iemmixer iemmixer 0600 /var/lib/iemmixer/site/iemmixer.toml",
            "iemmixer iemmixer 0600 /var/lib/iemmixer/site/roster.toml"])
        self.assertFalse(any(" enable " in command for command in self.remote()))
        # The copy: the bundle's six files and the box's own files, one scp.
        self.assertEqual(self.scp_names(), sorted([*SLOT_FILES, *UNIT_FILES, "dante-device.toml", "iem-dante.env",
                                                   "band_receive.py", "iemmixer.toml", "roster.toml"]))
        # The attestation of exactly the bundle's SHA256SUMS, by this
        # repository's CI workflow, before any call.
        self.assertEqual(self.gh_calls(), [{
            "args": [str(self.bundle / "SHA256SUMS") if a == "<bundle>/SHA256SUMS" else a for a in GH_VERIFY],
            "digest": hashlib.sha256((self.bundle / "SHA256SUMS").read_bytes()).hexdigest()}])
        self.assertEqual(self.states(), {"iem-dante": "inactive", "iem-engine": "active", "iem-server": "active"})
        self.assertNotIn("WARNING", r.stderr)
        self.assertTrue(all("BatchMode=yes" in c for c in self.calls()))
        # Without --pins no call carries anything on stdin.
        self.assertEqual(set(self.stdins()), {""})
        self.assertFalse((self.box / "pins.log").exists())
        last = r.stdout.strip().splitlines()[-1]
        self.assertIn("slot-a", last)
        self.assertIn(f"iem-engine 2.0.0-dev.45+{self.sha}", last)
        self.assertIn(f"iem-dante-dev 2.0.0-dev.45 {self.sha}", last)
        self.assertIn('{"health": {"state": "healthy"', last)

    def test_the_next_deploys_alternate_the_slots_and_keep_the_build_before(self) -> None:
        first = self.bundle
        self.deployed()
        # a → b: the stack runs from slot a; the new build goes into b while
        # it runs, and a stays as it was.
        sha2, second = self.next_build("the second build")
        r = self.deployed(args=self.full_args(sha2, second))
        self.assertEqual(self.link(), "slot-b/bin")
        self.assert_slot_holds("b", second)
        self.assert_slot_holds("a", first)
        self.assertIn(f"iem-engine 2.0.0-dev.45+{sha2}", r.stdout)
        # b → a: the third build replaces the first in slot a; b keeps the second.
        sha3, third = self.next_build("the third build")
        r = self.deploy(args=self.full_args(sha3, third))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.sequence(), self.expected("a", link="slot-b/bin"))
        self.assertEqual(self.link(), "slot-a/bin")
        self.assert_slot_holds("a", third)
        self.assert_slot_holds("b", second)
        self.assertIn(f"iem-engine 2.0.0-dev.45+{sha3}", r.stdout)

    def test_a_to_b_sends_exactly_this(self) -> None:
        self.deployed()
        sha2, second = self.next_build("the second build")
        # The engine of slot a stayed down in a crash loop (systemd holds it
        # failed): its start count is reset before the start.
        self.set_states({"iem-dante": "inactive", "iem-engine": "failed", "iem-server": "active"})
        r = self.deploy(args=self.full_args(sha2, second))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.sequence(), self.expected("b", link="slot-a/bin", held=("iem-engine",)))

    def test_the_stack_is_stopped_only_after_the_new_slot_holds_verified_sums(self) -> None:
        self.deployed()
        sha2, second = self.next_build("the second build")
        r = self.deploy(args=self.full_args(sha2, second), FAKE_SLOT_CORRUPT="iem-server")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("slot-b's SHA256SUMS does not hold on the box: the stack was not stopped", r.stderr)
        self.assertEqual(self.sequence()[-1], f"cd /opt/iemmixer/slot-b/bin && {CHECK}")
        self.assertNotIn(STOP, self.remote())
        self.assertEqual(self.states(), {"iem-dante": "inactive", "iem-engine": "active", "iem-server": "active"})
        self.assertEqual(self.link(), "slot-a/bin")
        # And in a good deploy the stop comes after the slot's check.
        self.clear_logs()
        r = self.deploy(args=self.full_args(sha2, second))
        self.assertEqual(r.returncode, 0, r.stderr)
        kinds = self.kinds()
        fill = kinds.index("set -e")
        self.assertTrue(self.sequence()[fill].endswith("/opt/iemmixer/slot-b/bin/SHA256SUMS"))
        self.assertLess(fill, kinds.index(f"cd /opt/iemmixer/slot-b/bin && {CHECK}"))
        self.assertLess(kinds.index(f"cd /opt/iemmixer/slot-b/bin && {CHECK}"), kinds.index(STOP))

    def test_a_refused_sudo_while_filling_the_slot_stops_nothing(self) -> None:
        self.set_states({"iem-dante": "inactive", "iem-engine": "active", "iem-server": "active"})
        r = self.deploy(FAKE_SUDO_FAILS="1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("a password is required", r.stderr)
        self.assertIn("slot-a could not be filled: the stack was not stopped", r.stderr)
        self.assertNotIn(STOP, self.remote())
        self.assertEqual(self.states(), {"iem-dante": "inactive", "iem-engine": "active", "iem-server": "active"})

    # Phase C's folder.

    def test_the_phase_c_folder_is_moved_aside_once_never_deleted_never_twice(self) -> None:
        files = self.phase_c_folder()
        r = self.deploy()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.sequence(), self.expected("a", phase_c=True))
        self.assertEqual(self.link(), "slot-a/bin")
        aside = self.box / "root" / "opt" / "iemmixer" / "bin.phase-c"
        self.assertEqual({p.name: p.read_bytes() for p in aside.iterdir()}, files)
        self.assertIn("bin.phase-c (kept)", r.stdout)
        # The next deploy moves the link only.
        self.clear_logs()
        sha2, second = self.next_build("the second build")
        r = self.deploy(args=self.full_args(sha2, second))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertFalse(any("bin.phase-c" in c for c in self.remote()), self.remote())
        self.assertEqual({p.name: p.read_bytes() for p in aside.iterdir()}, files)
        # A folder at bin again beside the one moved aside: refused before
        # anything is copied or stopped, both left as they are.
        bin_path = self.box / "root" / "opt" / "iemmixer" / "bin"
        bin_path.unlink()
        bin_path.mkdir()
        (bin_path / "iem-engine").write_bytes(b"another folder")
        self.clear_logs()
        self.set_states({"iem-dante": "inactive", "iem-engine": "active", "iem-server": "active"})
        sha3, third = self.next_build("the third build")
        r = self.deploy(args=self.full_args(sha3, third))
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("never twice", r.stderr)
        self.assertIn("nothing was copied", r.stderr)
        self.assertEqual(self.sequence(), self.expected("a")[:3])
        self.assertEqual(self.states()["iem-engine"], "active")
        self.assertEqual({p.name: p.read_bytes() for p in aside.iterdir()}, files)
        self.assertEqual((bin_path / "iem-engine").read_bytes(), b"another folder")

    def test_a_bin_that_is_neither_a_slots_link_nor_a_folder_is_refused_before_any_copy(self) -> None:
        opt = self.box / "root" / "opt" / "iemmixer"
        opt.mkdir(parents=True)
        for make, want in ((lambda p: p.symlink_to("/usr/local/bin"), "to neither slot-a/bin nor slot-b/bin"),
                           (lambda p: p.symlink_to("slot-c/bin"), "to neither slot-a/bin nor slot-b/bin"),
                           (lambda p: p.write_bytes(b"x"), "is a regular file")):
            with self.subTest(want=want):
                bin_path = opt / "bin"
                if os.path.lexists(bin_path):
                    bin_path.unlink()
                make(bin_path)
                self.clear_logs()
                r = self.deploy()
                self.assertEqual(r.returncode, 1, r.stderr)
                self.assertIn(want, r.stderr)
                self.assertIn("nothing was copied", r.stderr)
                self.assertNotIn("scp", self.kinds())
                self.assertNotIn(STOP, self.remote())

    # --back.

    def test_back_goes_to_the_other_slot_after_checking_its_build_and_sums(self) -> None:
        first_sha, first = self.sha, self.bundle
        self.deployed()
        sha2, second = self.next_build("the second build")
        self.deployed(args=self.full_args(sha2, second))
        self.git("checkout", "-q", "--detach", first_sha)
        r = self.deploy(args=self.back_args(first_sha))
        self.assertEqual(r.returncode, 0, r.stderr)
        # Its BUILD and sums read before anything is copied; only the units
        # and the configs copied; no slot filled; no attestation asked.
        self.assertEqual(self.sequence(), self.expected("a", link="slot-b/bin", back=True))
        self.assertEqual(self.scp_names(), sorted([*UNIT_FILES, "dante-device.toml", "iem-dante.env",
                                                   "band_receive.py", "iemmixer.toml", "roster.toml"]))
        self.assertEqual(self.gh_calls(), [])
        self.assertEqual(self.link(), "slot-a/bin")
        self.assert_slot_holds("a", first)
        self.assert_slot_holds("b", second)
        self.assertIn(f"iem-engine 2.0.0-dev.45+{first_sha}", r.stdout)
        self.assertEqual(self.states(), {"iem-dante": "inactive", "iem-engine": "active", "iem-server": "active"})

    def test_back_refuses_a_slot_that_is_not_the_sha_or_whose_sums_fail_with_nothing_stopped(self) -> None:
        first_sha = self.sha
        self.deployed()
        sha2, second = self.next_build("the second build")
        self.deployed(args=self.full_args(sha2, second))
        running = {"iem-dante": "inactive", "iem-engine": "active", "iem-server": "active"}
        # The other slot holds the first build, not the one named.
        r = self.deploy(args=self.back_args(sha2))
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("slot-a's BUILD is not --sha", r.stderr)
        self.assertEqual(self.sequence(), ["sudo -n -l systemctl stop iem-server iem-engine iem-dante",
                                           "readlink /opt/iemmixer/bin", "cat /opt/iemmixer/slot-a/bin/BUILD"])
        self.assertEqual(self.states(), running)
        # The right build, but a file of it changed on the box.
        self.clear_logs()
        self.git("checkout", "-q", "--detach", first_sha)
        (self.slot("a") / "iem-server").write_bytes(b"changed on the box")
        r = self.deploy(args=self.back_args(first_sha))
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("slot-a's SHA256SUMS does not hold on the box", r.stderr)
        self.assertEqual(self.sequence()[-1], f"cd /opt/iemmixer/slot-a/bin && {CHECK}")
        self.assertNotIn("scp", self.kinds())
        self.assertNotIn(STOP, self.remote())
        self.assertEqual(self.states(), running)
        self.assertEqual(self.link(), "slot-b/bin")

    def test_back_with_no_build_in_the_other_slot_is_refused(self) -> None:
        self.deployed()
        r = self.deploy(args=self.back_args(self.sha))
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("slot-b has no BUILD to read", r.stderr)
        self.assertNotIn(STOP, self.remote())
        self.assertEqual(self.link(), "slot-a/bin")

    # One commit.

    def test_a_checkout_that_is_not_the_sha_is_refused_before_any_call(self) -> None:
        sha2 = self.commit("a later commit")
        for args in (self.full_args(), self.back_args(self.sha)):
            with self.subTest(args=args[2]):
                r = self.deploy(args=args)
                self.assertEqual(r.returncode, 1, r.stderr)
                self.assertIn(f"this checkout is at {sha2}, not --sha {self.sha}", r.stderr)
                self.assertIn("nothing was copied", r.stderr)
                self.assertEqual(self.calls(), [])
                self.assertEqual(self.gh_calls(), [])

    def test_a_dirty_checkout_is_refused_before_any_call(self) -> None:
        site = self.repo / "config" / "test-site.toml"
        stray = self.repo / "zyxstray.txt"
        cases = [(lambda: site.write_text(site.read_text(encoding="utf-8") + "# changed\n", encoding="utf-8"),
                  lambda: self.git("checkout", "--", "config/test-site.toml")),
                 (lambda: stray.write_text("x\n", encoding="utf-8"), stray.unlink)]
        for change, undo in cases:
            for args in (self.full_args(), self.back_args(self.sha)):
                with self.subTest(args=args[2]):
                    change()
                    r = self.deploy(args=args)
                    undo()
                    self.assertEqual(r.returncode, 1, r.stderr)
                    self.assertIn("1 changed or untracked paths", r.stderr)
                    self.assertIn("nothing was copied", r.stderr)
                    self.assertNotIn("zyxstray", r.stderr)
                    self.assertNotIn("test-site", r.stderr)
                    self.assertEqual(self.calls(), [])
                    self.assertEqual(self.gh_calls(), [])
        r = self.deploy()
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_an_unattested_bundle_never_leaves_the_dev_box(self) -> None:
        r = self.deploy(FAKE_GH_FAILS="1")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("an unattested bundle never leaves the dev box", r.stderr)
        self.assertIn("nothing was copied", r.stderr)
        self.assertEqual(self.calls(), [])
        self.assertEqual([c["args"] for c in self.gh_calls()],
                         [[str(self.bundle / "SHA256SUMS") if a == "<bundle>/SHA256SUMS" else a for a in GH_VERIFY]])

    # The template.

    def test_every_sudo_line_deploy_sends_is_in_the_template(self) -> None:
        # Every mode and slot: over phase C's folder with a failed engine
        # (reset), with --pins and a HIL job a dead gen3-gate run left; then
        # into b with the standalone device; into a again; back to b; and a
        # late flag's stop.
        self.write_pins()
        self.phase_c_folder()
        dropin = self.box / "root" / HIL_DROPIN.lstrip("/")
        dropin.parent.mkdir(parents=True)
        dropin.write_text("[Service]\n", encoding="utf-8")
        self.set_states({"iem-dante": "active", "iem-engine": "failed", "iem-server": "inactive"})
        sent: list[str] = []
        runs = []
        r = self.deploy("--pins")
        runs.append(r)
        sent += self.remote()
        self.clear_logs()
        sha2, second = self.next_build("the second build")
        runs.append(self.deploy(args=[*self.full_args(sha2, second), "--start", "device"]))
        sent += self.remote()
        self.clear_logs()
        sha3, third = self.next_build("the third build")
        runs.append(self.deploy(args=self.full_args(sha3, third)))
        sent += self.remote()
        self.clear_logs()
        self.git("checkout", "-q", "--detach", sha2)
        runs.append(self.deploy(args=self.back_args(sha2)))
        sent += self.remote()
        self.clear_logs()
        self.flag.parent.mkdir(exist_ok=True)
        late = self.deploy(args=self.back_args(sha2), FAKE_FLAG_ON="start")
        sent += self.remote()
        for run in runs:
            self.assertEqual(run.returncode, 0, run.stderr)
        self.assertEqual(late.returncode, 1, late.stderr)
        allowed = {(runas, text) for runas, command in template_lines() for text in expand(command)}
        lines = sudo_lines(sent)
        for runas, command, _listed in lines:
            self.assertIn((runas, command), allowed, command)
        # The lines that cover each part: both slots, the switch over phase
        # C's folder, every PIN, a reset of what systemd held, both starts.
        seen = {command for _runas, command, _listed in lines}
        for want in ("install -D -m 0755 .cache/iem-gen3-deploy/iem-engine /opt/iemmixer/slot-a/bin/iem-engine",
                     "install -D -m 0755 .cache/iem-gen3-deploy/iem-engine /opt/iemmixer/slot-b/bin/iem-engine",
                     "mv -T /opt/iemmixer/bin /opt/iemmixer/bin.phase-c", "ln -sfn slot-b/bin /opt/iemmixer/bin.next",
                     PIN_RUN[len("sudo -n "):] + " set-engineer",
                     *(PIN_RUN[len("sudo -n "):] + f" set-member {p}" for p in roster_people()),
                     "systemctl reset-failed iem-engine", "systemctl start iem-dante",
                     "systemctl start iem-engine iem-server", "systemctl stop iem-server iem-engine iem-dante",
                     f"rm -f {HIL_DROPIN}"):
            self.assertIn(want, seen)
        self.assertIn(("root", "systemctl stop iem-server iem-engine iem-dante", True), lines)

    def test_the_template_names_only_known_placeholders_and_runas(self) -> None:
        lines = template_lines()
        self.assertGreater(len(lines), 20)
        for _runas, command in lines:
            rest = command
            for key in ("{slot}", "{member}", "{units}"):
                rest = rest.replace(key, "")
            self.assertNotIn("{", rest, command)
            self.assertNotIn("}", rest, command)
        self.assertEqual(len({c for _r, c in lines}), len(lines), "a line twice")
        self.assertEqual(len(expand("systemctl reset-failed {units}")), 7)
        self.assertEqual(expand("ln -sfn slot-{slot}/bin x"), {"ln -sfn slot-a/bin x", "ln -sfn slot-b/bin x"})

    # Phase C's rules, kept.

    def test_the_first_deploy_over_the_standalone_device_stops_what_is_loaded(self) -> None:
        # Phase B's box: only iem-dante is installed (its binary in phase B's
        # folder), so the stack's stop says "not loaded" for the engine and
        # the server (exit 5) while it stops the device; nothing runs, so the
        # deploy goes on.
        self.phase_c_folder(("iem-dante-dev",))
        self.set_states({"iem-dante": "active"}, not_loaded=("iem-engine", "iem-server"))
        r = self.deploy()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("the stop exited 5", r.stderr)
        self.assertNotIn("WARNING", r.stderr)
        self.assertEqual(self.states(), {"iem-dante": "inactive", "iem-engine": "active", "iem-server": "active"})

    def test_a_first_deploy_resets_only_the_units_systemd_holds(self) -> None:
        # Live 1 (#72, 2026-10-26): units installed and never started are not
        # in systemd's memory, and `reset-failed` of one fails ("not
        # loaded"), which stopped the deploy before the start. Only a unit
        # systemd lists is reset; its start counter is the one that matters.
        self.set_states({"iem-dante": "inactive", "iem-engine": "inactive", "iem-server": "inactive"})
        r = self.deploy()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertNotIn("not loaded", r.stderr)
        self.assertEqual(self.states()["iem-engine"], "active")
        self.assertFalse((self.box / "reset.log").exists())

    def test_a_hil_job_a_dead_gen3_gate_run_left_is_ended_before_the_start(self) -> None:
        # Phase H (#72): a dev box that died during `iempc gen3-gate` leaves
        # its runtime drop-in, which would start the engine with the HIL test
        # flag. The deploy removes it right before its daemon-reload, so the
        # engine it starts has the unit's own ExecStart.
        self.deployed()
        sha2, second = self.next_build("the second build")
        dropin = self.box / "root" / HIL_DROPIN.lstrip("/")
        dropin.parent.mkdir(parents=True)
        dropin.write_text("[Service]\nExecStart=\nExecStart=/opt/iemmixer/bin/iem-engine run --test-signal\n",
                          encoding="utf-8")
        self.set_states({"iem-dante": "inactive", "iem-engine": "active", "iem-server": "active"})
        r = self.deploy(args=self.full_args(sha2, second))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.sequence(), self.expected("b", link="slot-a/bin", left=True))
        self.assertFalse(self.exists(HIL_DROPIN))
        self.assertTrue(dropin.parent.is_dir())   # only the file: the folder is systemd's
        self.assertIn("a HIL job a dead iempc gen3-gate run left", r.stdout)
        # Without one, nothing is removed and nothing is said.
        self.clear_logs()
        sha3, third = self.next_build("the third build")
        r = self.deploy(args=self.full_args(sha3, third))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertFalse((self.box / "removed.log").exists())
        self.assertNotIn("HIL job", r.stdout)

    def test_a_look_for_a_left_hil_job_that_fails_starts_nothing(self) -> None:
        self.set_states({"iem-dante": "inactive", "iem-engine": "inactive", "iem-server": "inactive"})
        r = self.deploy(FAKE_LEFT_FAILS="1")
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertIn("the box's HIL job drop-in could not be looked for (exit 7): nothing started", r.stderr)
        self.assertNotIn("systemctl start", "\n".join(self.remote()))
        self.assertEqual(self.states()["iem-engine"], "inactive")

    def test_the_units_systemd_holds_are_reset_in_one_order(self) -> None:
        # systemd lists them in an order of its own; the sudoers rule lists
        # each ordered subset once.
        self.set_states({"iem-dante": "failed", "iem-engine": "inactive", "iem-server": "failed"})
        r = self.deploy()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual((self.box / "reset.log").read_text(encoding="utf-8").splitlines(),
                         ['["iem-dante", "iem-server"]'])

    def test_a_unit_still_running_after_the_stop_switches_nothing(self) -> None:
        self.set_states({"iem-dante": "inactive", "iem-engine": "active", "iem-server": "active"})
        r = self.deploy(FAKE_STILL_RUNS="iem-engine")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("iem-engine is 'deactivating' after the stop", r.stderr)
        self.assertIn("nothing was switched", r.stderr)
        self.assertEqual(self.kinds()[-1], "systemctl is-active iem-server iem-engine iem-dante")
        self.assertIsNone(self.link())
        self.assertFalse(self.exists("/etc/systemd/system"))

    def test_start_device_starts_only_the_standalone_device(self) -> None:
        self.set_states({"iem-dante": "inactive", "iem-engine": "active", "iem-server": "active"})
        r = self.deploy("--start", "device")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.sequence(), self.expected("a", start="device"))
        # The engine and the server were stopped and stay so; every unit and
        # every binary is installed all the same.
        self.assertEqual(self.states(), {"iem-dante": "active", "iem-engine": "inactive", "iem-server": "inactive"})
        self.assertTrue(self.exists("/etc/systemd/system/iem-engine.service"))
        self.assertTrue(self.exists("/opt/iemmixer/bin/iem-server"))
        self.assertIn("iem-dante started", r.stdout.strip().splitlines()[-1])

    # --pins: through systemd-run, only on stdin, never printed.

    def test_pins_go_through_systemd_run_only_on_stdin_and_are_never_printed(self) -> None:
        self.write_pins()
        r = self.deploy("--pins")
        self.assertEqual(r.returncode, 0, r.stderr)
        people = roster_people()
        self.assertGreater(len(people), 1)
        self.assertEqual(self.sequence(), self.expected("a", people=tuple(people)))
        pins = [json.loads(line) for line in (self.box / "pins.log").read_text(encoding="utf-8").splitlines()]
        self.assertEqual(pins, [["set-engineer", ENGINEER_PIN + "\n"],
                                *[[f"set-member {person}", MEMBER_PIN + "\n"] for person in people]])
        # Each PIN only on the stdin of its own call; every other call's stdin
        # empty; no command line (ssh, scp or gh, local or remote) holds one.
        remote, stdins = self.remote(), self.stdins()
        self.assertEqual(len(remote), len(stdins))
        for command, data in zip(remote, stdins):
            if " pin set-" in command:
                self.assertIn(data, (ENGINEER_PIN + "\n", MEMBER_PIN + "\n"))
            else:
                self.assertEqual(data, "", command)
        for call in [*self.calls(), *(c["args"] for c in self.gh_calls())]:
            for word in call:
                for pin in (ENGINEER_PIN, MEMBER_PIN):
                    self.assertFalse(holds_pin(word, pin), word)
        # Not in ssh's environment either (never exported), never printed.
        for line in (self.box / "env.log").read_text(encoding="utf-8").splitlines():
            values = set(json.loads(line).values())
            self.assertNotIn(ENGINEER_PIN, values)
            self.assertNotIn(MEMBER_PIN, values)
        for text in (r.stdout, r.stderr):
            for pin in (ENGINEER_PIN, MEMBER_PIN):
                self.assertFalse(holds_pin(text, pin))
        for name in os.listdir(self.box / "home" / STAGE):
            staged = (self.box / "home" / STAGE / name).read_bytes()
            for pin in (ENGINEER_PIN, MEMBER_PIN):
                self.assertFalse(holds_pin(staged, pin), name)
        # The helper finds a PIN written as itself, never one inside a digest.
        self.assertTrue(holds_pin(f"pin={MEMBER_PIN}\n", MEMBER_PIN))
        self.assertFalse(holds_pin(f"ab{MEMBER_PIN}cd  iem-engine\n", MEMBER_PIN))
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
        for args in (self.full_args(), self.back_args(self.sha)):
            r = self.deploy(args=args)
            self.assertEqual(r.returncode, 1)
            self.assertIn("an event is on", r.stderr)
            self.assertIn("nothing was copied", r.stderr)
            self.assertEqual(self.calls(), [])
            self.assertEqual(self.gh_calls(), [])

    def test_a_flag_that_comes_during_the_copy_fills_no_slot(self) -> None:
        self.flag.parent.mkdir()
        r = self.deploy(FAKE_FLAG_ON="mkdir")
        self.assertEqual(r.returncode, 1)
        self.assertIn("nothing installed", r.stderr)
        self.assertEqual(self.kinds()[-2:], ["scp", f"cd {STAGE} && {CHECK}"])
        self.assertFalse((self.box / "root" / "opt").exists())

    def test_a_flag_that_comes_while_the_slot_is_filled_stops_nothing(self) -> None:
        self.flag.parent.mkdir()
        self.set_states({"iem-dante": "inactive", "iem-engine": "active", "iem-server": "active"})
        r = self.deploy(FAKE_FLAG_ON="slot")
        self.assertEqual(r.returncode, 1)
        self.assertIn("an event is on", r.stderr)
        self.assertIn("the stack was not stopped", r.stderr)
        self.assertEqual(self.kinds()[-1], f"cd /opt/iemmixer/slot-a/bin && {CHECK}")
        self.assertNotIn(STOP, self.remote())
        self.assertIsNone(self.link())

    def test_a_flag_that_comes_during_the_install_starts_nothing(self) -> None:
        self.flag.parent.mkdir()
        r = self.deploy(FAKE_FLAG_ON="install")
        self.assertEqual(r.returncode, 1)
        self.assertIn("installed, nothing started", r.stderr)
        self.assertFalse(any("systemctl start" in c for c in self.remote()))
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
        self.assertFalse(any(c.startswith("sha256sum ") or c.endswith(" health") for c in remote), remote)
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
        for text in ("f" * 40 + "\n", self.sha, self.sha + "\n\n", self.sha + "\nmore\n"):
            with self.subTest(text=text):
                (self.bundle / "BUILD").write_text(text, encoding="utf-8")
                self.write_sums(self.bundle)
                r = self.deploy()
                self.assertEqual(r.returncode, 1, r.stderr)
                self.assertIn("BUILD is not --sha", r.stderr)
                self.assertEqual(self.calls(), [])
                self.assertEqual(self.gh_calls(), [])

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
                self.assertEqual(self.gh_calls(), [])
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
        self.assertEqual(self.kinds()[-1], f"cd {STAGE} && {CHECK}")
        self.assertFalse((self.box / "root" / "opt").exists())

    # Refusals on the box.

    def test_a_box_whose_sudo_does_not_allow_the_stacks_stop_is_refused_before_any_copy(self) -> None:
        # "ide event" and the live guard stop the stack with exactly this
        # command: a stack they could not stop is never installed, and the
        # running device is left alone.
        r = self.deploy(FAKE_NO_STOP_RULE="1")
        self.assertEqual(r.returncode, 1)
        self.assertIn(STOP, r.stderr)
        self.assertIn("sudo-lines.txt", r.stderr)
        self.assertIn("nothing was copied", r.stderr)
        self.assertEqual(self.remote(), ["sudo -n -l systemctl stop iem-server iem-engine iem-dante"])
        self.assertEqual(self.states()["iem-dante"], "active")

    def test_an_install_that_fails_at_a_unit_starts_nothing(self) -> None:
        r = self.deploy(FAKE_SUDO_FAILS_AT="/etc/systemd/system/iem-engine.service")
        self.assertEqual(r.returncode, 1)
        self.assertIn("Read-only file system", r.stderr)
        self.assertIn("nothing started", r.stderr)
        self.assertTrue(self.exists("/etc/systemd/system/iem-dante.service"))
        self.assertFalse(self.exists("/etc/systemd/system/iem-engine.service"))
        self.assertFalse(any("systemctl start" in c for c in self.remote()))

    # The read back.

    def test_an_installed_file_with_another_sha256_fails(self) -> None:
        for name in ("iem-soakclient", "BUILD"):
            with self.subTest(name=name):
                r = self.deploy(FAKE_BAD_INSTALLED=name)
                self.assertEqual(r.returncode, 1)
                self.assertIn(f"the installed /opt/iemmixer/bin/{name}'s sha256 is {'f' * 64}", r.stderr)

    def test_versions_that_do_not_name_the_sha_fail(self) -> None:
        other = "f" * 40
        cases = [({"FAKE_ENGINE_VERSION": f"iem-engine 2.0.0-dev.45+{other}"}, "iem-engine --version said"),
                 ({"FAKE_ENGINE_VERSION": f"iem-engine 2.0.0-dev.45 {self.sha}"}, "iem-engine --version said"),
                 ({"FAKE_DEVICE_VERSION": f"iem-dante-dev 2.0.0-dev.45 {other}"}, "iem-dante-dev --version said"),
                 ({"FAKE_DEVICE_VERSION": "something else"}, "iem-dante-dev --version said")]
        for env, want in cases:
            with self.subTest(env=env):
                r = self.deploy(**env)
                self.assertEqual(r.returncode, 1, r.stderr)
                self.assertIn(want, r.stderr)

    def test_health_is_read_once_a_second_until_healthy(self) -> None:
        r = self.deploy(FAKE_HEALTH="1,3,0")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.health_reads(), 3)
        self.assertEqual(self.sequence(), self.expected("a", health=3))
        self.assertIn('{"health": {"state": "healthy", "reasons": [], "read": 3}}', r.stdout.strip().splitlines()[-1])

    def test_a_stack_never_healthy_fails_after_the_wait_with_its_last_line(self) -> None:
        r = self.deploy(FAKE_HEALTH="1")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertEqual(self.health_reads(), 3)
        self.assertIn("not healthy after 3 tries", r.stderr)
        self.assertIn('{"health": {"state": "degraded", "reasons": [], "read": 3}}', r.stderr)
        # A health that refuses its command line is not asked again.
        self.clear_logs()
        r = self.deploy(FAKE_HEALTH="2")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertEqual(self.health_reads(), 1)
        self.assertIn("refused its command line", r.stderr)

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
        back = self.back_args(self.sha)
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
            (with_value("--sha", self.sha.upper()), "40 lowercase hex"),
            (with_value("--sha", self.sha[:-1]), "40 lowercase hex"),
            (with_value("--iface", "eth 9"), "not an interface name"),
            (with_value("--iface", "a" * 16), "not an interface name"),
            (with_value("--bundle", str(self.tmp / "missing")), "--bundle is not a folder"),
            (with_value("--bundle", str(empty)), "--bundle has no iem-engine"),
            (with_value("--config", str(self.tmp)), "--config is not a file"),
            (full + ["--back"], "--back takes no --bundle"),
            (back + ["--pins"], "--back takes no --pins"),
            (full + ["--site-file", str(self.config)], "--site-file and --roster-file go together"),
            (full + ["--roster-file", str(self.config)], "--site-file and --roster-file go together"),
            (full + ["--site-file", str(self.tmp / "missing"), "--roster-file", str(self.config)],
             "--site-file is not a file"),
            (full + ["--site-file", str(self.config), "--roster-file", str(self.tmp)], "--roster-file is not a file"),
            (full + ["--pins", "--site-file", str(self.config), "--roster-file", str(self.config)],
             "the band's PINs come with iempc gen3-data"),
            ([a for a in back if a not in ("--sha", self.sha)], "--sha is missing"),
        ]
        for argv, want in cases:
            r = self.deploy(args=argv)
            self.assertEqual(r.returncode, 2, (argv, r.stderr))
            self.assertIn(want, r.stderr, argv)
        r = self.deploy(GEN3_SERVER_WAIT_S="0")
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("GEN3_SERVER_WAIT_S", r.stderr)
        self.assertEqual(self.calls(), [])
        self.assertEqual(self.gh_calls(), [])


    def test_a_deploy_while_3x_serves_the_band_is_refused(self) -> None:
        # Phase E (#72): the switch recorded side 3; a deploy would stop the box that plays the band.
        state = self.tmp / "iempc-state"
        state.mkdir()
        (state / "gen3-side.json").write_text('{\n "side": 3,\n "at": "2026-10-27T12:00:00+00:00"\n}', encoding="utf-8")
        r = self.deploy(IEMPC_STATE=str(state))
        self.assertEqual(r.returncode, 1)
        self.assertIn("3.x serves the band", r.stderr)
        self.assertEqual(self.calls(), [])
        (state / "gen3-side.json").write_text('{\n "side": 2,\n "at": "2026-10-27T13:00:00+00:00"\n}', encoding="utf-8")
        self.assertEqual(self.deploy(IEMPC_STATE=str(state)).returncode, 0)

    # Phase E (#72): the box on the band's site and roster from the ops repo.

    def site_files(self, site: str) -> tuple[Path, Path]:
        ops = self.tmp / "ops"
        ops.mkdir(exist_ok=True)
        (ops / "site.toml").write_text(site, encoding="utf-8")
        (ops / "roster.toml").write_text("[[people]]\nid = 'zyxqw'\n", encoding="utf-8")
        return ops / "site.toml", ops / "roster.toml"

    def test_the_bands_site_goes_without_back_to_reaper_and_its_roster_as_given(self) -> None:
        body = "".join(line for line in TEST_SITE.read_text(encoding="utf-8").splitlines(keepends=True)
                       if not line.startswith("back_to_reaper"))
        site, roster = self.site_files('back_to_reaper = ["iemmode.exe", "event"]\n' + body)
        r = self.deploy("--site-file", str(site), "--roster-file", str(roster))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.installed("/var/lib/iemmixer/site/iemmixer.toml").decode(), body)
        self.assertEqual(self.installed("/var/lib/iemmixer/site/roster.toml"), roster.read_bytes())
        self.assertNotIn("pin", " ".join(" ".join(c) for c in self.calls() if c and c[0] == "ssh").replace(
            "pin_changes", ""))

    def test_a_back_to_reaper_on_more_lines_is_refused_before_anything_is_copied(self) -> None:
        site, roster = self.site_files('back_to_reaper = [\n  "iemmode.exe",\n  "event",\n]\n')
        r = self.deploy("--site-file", str(site), "--roster-file", str(roster))
        self.assertEqual(r.returncode, 1)
        self.assertIn("the site's back_to_reaper is not one line", r.stderr)
        self.assertEqual([c for c in self.calls() if c and c[0] == "scp"], [])

    # --serving (#72, 2026-10-28): the box deployed in place while 3.x serves
    # the band (iempc gen3-deploy builds the takeover config and runs it).

    def side(self, side: int | None) -> None:
        """iempc's side record of this test: None is no record."""
        state = self.tmp / "iempc-state"
        state.mkdir(exist_ok=True)
        record = state / "gen3-side.json"
        if side is None:
            record.unlink(missing_ok=True)
        else:
            record.write_text(json.dumps({"side": side, "at": "2026-10-28T12:00:00+00:00"}, indent=1),
                              encoding="utf-8")

    def serving_box(self) -> list[str]:
        """3.x serves the band: the side record says 3, the engine and the
        server run and start at boot, the config is the takeover's (the
        card's usual name). Returns a serving deploy's added arguments: the
        band's site and roster, --serving."""
        self.side(3)
        self.write_pc_env(f"GEN3_HOST={HOST}\nDANTE_IEM_NAME={CARD_NAME}\n")
        self.config.write_text(f'name = "{CARD_NAME}"\nstate_file = "/var/lib/iem-dante/takeover.toml"\n',
                               encoding="utf-8")
        site, roster = self.site_files(TEST_SITE.read_text(encoding="utf-8"))
        self.set_states({"iem-dante": "inactive", "iem-engine": "active", "iem-server": "active"},
                        enabled=("iem-engine", "iem-server"))
        return ["--site-file", str(site), "--roster-file", str(roster), "--serving"]

    def test_a_serving_deploy_disables_nothing_and_enables_the_engine_and_the_server_after_the_start(self) -> None:
        serving = self.serving_box()
        r = self.deploy(*serving)
        self.assertEqual(r.returncode, 0, r.stderr)
        # No side refusal; the start without a disable, then the engine and
        # the server enabled at boot and read back; the read back as always.
        self.assertEqual(self.sequence(), self.expected("a", serving=True))
        self.assertEqual(self.states(), {"iem-dante": "inactive", "iem-engine": "active", "iem-server": "active"})
        self.assertEqual(self.enabled(), ["iem-engine", "iem-server"])
        self.assertIn("iem-engine and iem-server enabled at boot (3.x serves the band; the tunnel unit left as it is)",
                      r.stdout)
        self.assertEqual(self.installed("/etc/iemmixer/dante-device.toml"), self.config.read_bytes())
        # The card's name is compared on the dev box: on no ssh or scp command line, never printed.
        self.assertNotIn(CARD_NAME, json.dumps(self.calls()))
        self.assertNotIn(CARD_NAME, r.stdout + r.stderr)
        # The next one goes into the other slot the same way.
        self.clear_logs()
        sha2, second = self.next_build("the second build")
        r = self.deploy(args=[*self.full_args(sha2, second), *serving])
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.sequence(), self.expected("b", link="slot-a/bin", serving=True))
        self.assertEqual(self.enabled(), ["iem-engine", "iem-server"])

    def test_serving_only_while_3x_serves_and_while_3x_serves_only_serving(self) -> None:
        serving = self.serving_box()
        for side in (None, 2):
            with self.subTest(side=side):
                self.side(side)
                r = self.deploy(*serving)
                self.assertEqual(r.returncode, 1, r.stderr)
                self.assertIn("--serving deploys the box while 3.x serves the band", r.stderr)
                self.assertIn("deploy without --serving: nothing was copied", r.stderr)
                self.assertEqual((self.calls(), self.gh_calls()), ([], []))
        # 3.x serves: the same deploy without --serving is the refusal it was.
        self.side(3)
        r = self.deploy(*serving[:-1])
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("3.x serves the band", r.stderr)
        self.assertIn("`iempc gen3-deploy` deploys the box in place", r.stderr)
        self.assertEqual((self.calls(), self.gh_calls()), ([], []))
        self.assertEqual(self.enabled(), ["iem-engine", "iem-server"])

    def test_serving_refuses_a_config_not_under_the_cards_usual_name(self) -> None:
        serving = self.serving_box()
        cases = [
            ('name = "Zyxqw-Other"\n', "the config's name is not the IEM card's usual name"),
            # Exactly as the takeover writes it: another case is another config.
            ('name = "zyxqw-iem-card"\n', "the config's name is not the IEM card's usual name"),
            ('state_file = "/var/lib/iem-dante/takeover.toml"\n', "the config's name is not the IEM card's usual name"),
            ("name = [\n", "the config could not be read as TOML"),
        ]
        for text, want in cases:
            with self.subTest(want=want, text=text):
                self.config.write_text(text, encoding="utf-8")
                r = self.deploy(*serving)
                self.assertEqual(r.returncode, 1, r.stderr)
                self.assertIn(want, r.stderr)
                self.assertIn("nothing was copied", r.stderr)
                self.assertEqual((self.calls(), self.gh_calls()), ([], []))
                for name in (CARD_NAME, "Zyxqw-Other", "zyxqw-iem-card"):
                    self.assertNotIn(name, r.stdout + r.stderr)
        # The card's name comes from iempc's private env: none there is the command line's fault.
        self.config.write_text(f'name = "{CARD_NAME}"\n', encoding="utf-8")
        for text, want in ((f"GEN3_HOST={HOST}\n", "iempc's private env has no DANTE_IEM_NAME"),
                           (None, "which is not there")):
            with self.subTest(want=want):
                if text is None:
                    self.pc_env.unlink()
                else:
                    self.write_pc_env(text)
                r = self.deploy(*serving)
                self.assertEqual(r.returncode, 2, r.stderr)
                self.assertIn(want, r.stderr)
                self.assertEqual(self.calls(), [])

    def test_serving_needs_the_bands_site_and_the_engine(self) -> None:
        serving = self.serving_box()
        for argv, want in (([*self.full_args(), "--serving"], "--serving needs the band's --site-file"),
                           ([*self.full_args(), *serving, "--start", "device"], "--serving needs --start engine")):
            with self.subTest(want=want):
                r = self.deploy(args=argv)
                self.assertEqual(r.returncode, 2, r.stderr)
                self.assertIn(want, r.stderr)
        self.assertEqual(self.calls(), [])

    def test_a_flag_once_the_serving_stack_is_stopped_never_holds_it_down(self) -> None:
        # "ide event" keeps the box running while 3.x serves: the stack this
        # deploy stopped starts all the same, and a flag after the start
        # stops nothing; the read back goes on.
        serving = self.serving_box()
        self.flag.parent.mkdir(exist_ok=True)
        for when, said in (("install", "starts again at once"), ("start", "so the stack keeps running")):
            with self.subTest(when=when):
                self.flag.unlink(missing_ok=True)
                self.clear_logs()
                r = self.deploy(*serving, FAKE_FLAG_ON=when)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertIn("an event is on; 3.x serves the band", r.stderr)
                self.assertIn(said, r.stderr)
                self.assertEqual(r.stderr.count("an event is on"), 1, r.stderr)
                remote = self.remote()
                starts = [i for i, c in enumerate(remote) if "sudo -n systemctl start iem-engine iem-server" in c]
                self.assertEqual(len(starts), 1, remote)
                self.assertNotIn(STOP, remote[starts[0]:])
                self.assertEqual(remote[starts[0] + 1:starts[0] + 3], [ENABLE, IS_ENABLED])
                self.assertEqual(remote[-1], "/opt/iemmixer/bin/iem-dante-dev health")
                self.assertEqual(self.states(),
                                 {"iem-dante": "inactive", "iem-engine": "active", "iem-server": "active"})
                self.assertEqual(self.enabled(), ["iem-engine", "iem-server"])

    def test_a_flag_before_the_serving_stack_is_stopped_still_refuses(self) -> None:
        serving = self.serving_box()
        self.flag.parent.mkdir(exist_ok=True)
        for when, words in (("mkdir", "nothing installed"), ("slot", "the stack was not stopped")):
            with self.subTest(when=when):
                self.flag.unlink(missing_ok=True)
                self.clear_logs()
                r = self.deploy(*serving, FAKE_FLAG_ON=when)
                self.assertEqual(r.returncode, 1, r.stderr)
                self.assertIn(words, r.stderr)
                self.assertNotIn(STOP, self.remote())
                self.assertEqual(self.states()["iem-engine"], "active")
                self.assertIsNone(self.link())

    def test_an_enable_that_fails_or_does_not_read_back_fails_the_serving_deploy(self) -> None:
        serving = self.serving_box()
        for env, want in (({"FAKE_ENABLE_FAILS": "1"}, "systemctl enable iem-engine iem-server failed on the box"),
                          ({"FAKE_ENABLE_LOST": "1"}, "do not read back enabled at boot (systemctl is-enabled exit 1)")):
            with self.subTest(want=want):
                self.clear_logs()
                r = self.deploy(*serving, **env)
                self.assertEqual(r.returncode, 1, r.stderr)
                self.assertIn(want, r.stderr)
                self.assertIn("the stack runs, but", r.stderr)
                self.assertIn("a reboot would not serve the band", r.stderr)
                self.assertEqual(self.states()["iem-engine"], "active")
                remote = self.remote()
                starts = [i for i, c in enumerate(remote) if "sudo -n systemctl start iem-engine iem-server" in c]
                self.assertEqual(len(starts), 1, remote)
                self.assertNotIn(STOP, remote[starts[0]:])

    def test_every_sudo_line_a_serving_deploy_sends_is_in_the_template(self) -> None:
        # Into a, into b, then --back to a: each with --serving.
        serving = self.serving_box()
        sent: list[str] = []
        r = self.deploy(*serving)
        self.assertEqual(r.returncode, 0, r.stderr)
        sent += self.remote()
        self.clear_logs()
        sha2, second = self.next_build("the second build")
        r = self.deploy(args=[*self.full_args(sha2, second), *serving])
        self.assertEqual(r.returncode, 0, r.stderr)
        sent += self.remote()
        self.clear_logs()
        self.git("checkout", "-q", "--detach", self.sha)
        r = self.deploy(args=[*self.back_args(self.sha), *serving])
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.sequence(), self.expected("a", link="slot-b/bin", back=True, serving=True))
        self.assertEqual(self.enabled(), ["iem-engine", "iem-server"])
        sent += self.remote()
        allowed = {(runas, text) for runas, command in template_lines() for text in expand(command)}
        lines = sudo_lines(sent)
        for runas, command, _listed in lines:
            self.assertIn((runas, command), allowed, command)
        seen = {command for _runas, command, _listed in lines}
        self.assertIn("systemctl enable iem-engine iem-server", seen)
        self.assertNotIn("systemctl disable iem-dante iem-engine iem-server", seen)

class UnitTests(unittest.TestCase):
    """The units (phase C plan Task 6 Interfaces, choice 5; phase D plan Task 5,
    choices 3 and 5: the service user, the alarm unit)."""

    SHELLS = {"sh", "bash", "dash", "zsh", "ksh", "busybox", "env"}
    HARDENING = (("NoNewPrivileges", "yes"), ("ProtectSystem", "strict"), ("ProtectHome", "yes"),
                 ("PrivateTmp", "yes"))
    CREDENTIALS = ("LoadCredential", "LoadCredentialEncrypted", "SetCredential", "SetCredentialEncrypted",
                   "ImportCredential")

    def setUp(self) -> None:
        self.units = {name: parse_unit(path) for name, path in UNITS.items()}

    def alarm(self) -> dict[str, dict[str, list[str]]]:
        return parse_unit(ALARM)

    def all_four(self) -> dict[str, dict[str, dict[str, list[str]]]]:
        return {**self.units, "iem-alarm@": self.alarm()}

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
                for key, value in self.HARDENING:
                    self.assertEqual(one(unit, "Service", key), value, key)
                # [Install] stays so `disable` removes an earlier enable.
                self.assertEqual(one(unit, "Install", "WantedBy"), "multi-user.target")

    def test_every_unit_and_the_alarm_run_as_the_service_user(self) -> None:
        # Phase D choice 5: the system user and group `iemmixer` (not site
        # data) in the units themselves, no drop-in naming the admin user.
        for name, unit in self.all_four().items():
            with self.subTest(unit=name):
                self.assertEqual(one(unit, "Service", "User"), "iemmixer")
                self.assertEqual(one(unit, "Service", "Group"), "iemmixer")
                for section in unit.values():
                    self.assertNotIn("DynamicUser", section)
        for path in [*UNITS.values(), ALARM]:
            self.assertNotIn("10-user.conf", path.read_text(encoding="utf-8"), path.name)

    def test_every_unit_raises_the_alarm_only_when_it_stays_down(self) -> None:
        # OnFailure= runs the alarm once the unit fails for good (a crash
        # loop's start-limit-hit, exit 2 or 3, a stop past its bound);
        # RestartMode=direct keeps a restart from passing through `failed`,
        # so an ordinary restart raises none.
        for name, unit in self.units.items():
            with self.subTest(unit=name):
                self.assertEqual(one(unit, "Unit", "OnFailure"), "iem-alarm@%n.service")
                self.assertNotIn("OnFailure", unit["Service"])
                self.assertNotIn("OnSuccess", unit["Unit"])
                self.assertEqual(one(unit, "Service", "RestartMode"), "direct")

    def test_every_unit_restarts_with_a_backoff_and_stays_down_in_a_crash_loop(self) -> None:
        # Choice 3: 1 s growing to 10 s in four steps; a fourth start within
        # ten minutes is refused (start-limit-hit). The start limit lives in
        # [Unit] (systemd ignores it in [Service]).
        for name, unit in self.units.items():
            with self.subTest(unit=name):
                self.assertEqual(one(unit, "Service", "RestartSec"), "1")
                self.assertEqual(one(unit, "Service", "RestartSteps"), "4")
                self.assertEqual(one(unit, "Service", "RestartMaxDelaySec"), "10")
                self.assertEqual(one(unit, "Unit", "StartLimitIntervalSec"), "600")
                self.assertEqual(one(unit, "Unit", "StartLimitBurst"), "3")
                for key in ("StartLimitIntervalSec", "StartLimitBurst", "StartLimitInterval"):
                    self.assertNotIn(key, unit["Service"])

    def test_only_the_server_holds_a_capability_and_only_to_bind_port_80(self) -> None:
        # dantesync is the box's one PTP owner (owner, 2026-10-26): our
        # device never opens UDP 319/320 and its ports are above 1024. The
        # server listens on port 80, the band's tunnel's origin (phase E, #72),
        # and holds that one capability, nothing else.
        for name, unit in self.all_four().items():
            want = "CAP_NET_BIND_SERVICE" if name == "iem-server" else ""
            with self.subTest(unit=name):
                self.assertEqual(one(unit, "Service", "CapabilityBoundingSet"), want)
                self.assertEqual(one(unit, "Service", "AmbientCapabilities"), want)

    def test_the_pepper_credential_only_on_the_server(self) -> None:
        # Choice 5: systemd decrypts the box's credential into the server's
        # private credentials folder at each start; no other unit gets it.
        server = self.units["iem-server"]
        self.assertEqual(one(server, "Service", "LoadCredentialEncrypted"),
                         "pepper:/etc/credstore.encrypted/iemmixer-pepper")
        for name, unit in self.all_four().items():
            for key in self.CREDENTIALS:
                if name == "iem-server" and key == "LoadCredentialEncrypted":
                    continue
                self.assertNotIn(key, unit["Service"], (name, key))

    def test_the_runtime_folders_open_to_the_service_group_only(self) -> None:
        # 0750: the admin user, in the group `iemmixer`, reaches the device's
        # 0660 control socket; nobody else reaches anything in them.
        engine, device, server = (self.units[n] for n in ("iem-engine", "iem-dante", "iem-server"))
        self.assertEqual(one(engine, "Service", "RuntimeDirectory"), "iemmixer iem-dante")
        self.assertEqual(one(engine, "Service", "RuntimeDirectoryMode"), "0750")
        self.assertEqual(one(device, "Service", "RuntimeDirectory"), "iem-dante")
        self.assertEqual(one(device, "Service", "RuntimeDirectoryMode"), "0750")
        for unit in (server, self.alarm()):
            self.assertNotIn("RuntimeDirectory", unit["Service"])
            self.assertNotIn("RuntimeDirectoryMode", unit["Service"])

    def test_the_state_folders_open_to_the_service_group_only(self) -> None:
        # 0750, as provision makes them: systemd sets a state folder to its
        # StateDirectoryMode (0755 when unset) at every start of a unit that
        # names it, so all three units name the same mode.
        engine, device, server = (self.units[n] for n in ("iem-engine", "iem-dante", "iem-server"))
        for unit, folders in ((engine, "iemmixer iem-dante"), (device, "iem-dante"), (server, "iemmixer")):
            self.assertEqual(one(unit, "Service", "StateDirectory"), folders)
            self.assertEqual(one(unit, "Service", "StateDirectoryMode"), "0750")
        self.assertNotIn("StateDirectory", self.alarm()["Service"])
        self.assertNotIn("StateDirectoryMode", self.alarm()["Service"])

    def test_the_engine_unit_restarts_except_on_a_usage_or_card_refusal(self) -> None:
        engine = self.units["iem-engine"]
        self.assertEqual(one(engine, "Service", "RestartPreventExitStatus"), "2 3")
        self.assertEqual(one(engine, "Service", "LimitRTPRIO"), "80")
        self.assertEqual(one(engine, "Service", "StateDirectory"), "iemmixer iem-dante")
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
                               "PORT": "80", "RUST_LOG": "info"})
        # The site is where deploy.sh copies it, in the folders the units own.
        deploy = DEPLOY.read_text(encoding="utf-8")
        self.assertIn("SITE=$SITEDIR/iemmixer.toml", deploy)
        self.assertIn("SITEDIR=/var/lib/iemmixer/site", deploy)
        self.assertTrue(opts["--site"].startswith("/var/lib/iemmixer/"))
        self.assertTrue(opts["--state-dir"].startswith("/var/lib/iemmixer/"))
        self.assertTrue(opts["--pipe"].startswith("/run/iemmixer/"))
        self.assertEqual(one(server, "Service", "StateDirectory"), "iemmixer")
        # The engine's device reads the standalone device's config, and the
        # device's control socket stays at its phase B path in /run/iem-dante.
        device = shlex.split(one(self.units["iem-dante"], "Service", "ExecStart"))
        self.assertEqual(device[device.index("--config") + 1], opts["--dante-config"])
        self.assertEqual(device[device.index("--control") + 1], "/run/iem-dante/control.sock")
        self.assertNotIn("--dante-control", opts)

    def test_the_standalone_device_keeps_fifo_its_state_and_its_exit_codes(self) -> None:
        device = self.units["iem-dante"]
        for key, value in (("LimitRTPRIO", "80"), ("StateDirectory", "iem-dante"),
                           ("RestartPreventExitStatus", "2")):
            self.assertEqual(one(device, "Service", key), value)
        exec_start = one(device, "Service", "ExecStart")
        self.assertIn("--control /run/iem-dante/control.sock", exec_start)

    def test_the_alarm_unit_says_which_unit_stays_down_and_notifies_the_engineer(self) -> None:
        alarm = self.alarm()
        service = alarm["Service"]
        self.assertEqual(one(alarm, "Service", "Type"), "oneshot")
        self.assertEqual(one(alarm, "Service", "SyslogIdentifier"), "iem-alarm")
        self.assertEqual(one(alarm, "Service", "TimeoutStartSec"), "30")
        # The site the server reads, so the notice reaches its engineer's devices.
        server_env = dict(item.split("=", 1)
                          for item in shlex.split(one(self.units["iem-server"], "Service", "Environment")))
        self.assertEqual(service.get("Environment"), [f"IEMMIXER_CONFIG={server_env['IEMMIXER_CONFIG']}"])
        # The journal line always; the notice only while the box serves the
        # band, i.e. while its engine is enabled at boot (gen3-switch to-3
        # enables it, to-2 disables it). A box that serves nobody, with a
        # copy of the band's data, must not reach the band's devices (#72,
        # the false alarm of 2026-10-27). systemd hands MONITOR_* to
        # ExecStartPre= and ExecStart= only, never to ExecCondition= (the
        # review of PR #84: the line read "(result , exit  )"), so both are
        # ExecStartPre=: an engine not enabled ends the alarm failed at the
        # second, before the notice, which raises nothing more (no OnFailure=).
        self.assertNotIn("ExecCondition", service)
        self.assertEqual(service.get("ExecStartPre"), [
            '/usr/bin/echo "alarm: %i stays down (result ${MONITOR_SERVICE_RESULT}, exit ${MONITOR_EXIT_CODE} '
            '${MONITOR_EXIT_STATUS})"',
            "/usr/bin/systemctl is-enabled --quiet iem-engine.service",
        ])
        for key, lines in service.items():
            if any("${MONITOR_" in line for line in lines):
                self.assertIn(key, ("ExecStartPre", "ExecStart"), "systemd gives MONITOR_* to no other key")
        self.assertEqual(service.get("ExecStart"), [
            '/opt/iemmixer/bin/iem-server notify --to alarm "Gen 3 box" "%i stays down: ${MONITOR_SERVICE_RESULT}"',
        ])
        # The notice is the server unit's own binary: the guard's alarm path.
        notify = shlex.split(service["ExecStart"][0])
        self.assertEqual(notify[0], one(self.units["iem-server"], "Service", "ExecStart"))
        self.assertEqual(notify[1:], ["notify", "--to", "alarm", "Gen 3 box",
                                      "%i stays down: ${MONITOR_SERVICE_RESULT}"])

    def test_the_alarm_unit_never_loops_never_starts_by_itself_and_runs_no_shell(self) -> None:
        alarm = self.alarm()
        # An alarm never raises an alarm, never restarts and is never enabled.
        for section in alarm.values():
            for key in ("OnFailure", "OnSuccess", "Restart"):
                self.assertNotIn(key, section)
        self.assertNotIn("Install", alarm)
        # No shell: every command is a program called with its arguments.
        service = alarm["Service"]
        lines = [line for key in ("ExecStartPre", "ExecStart", "ExecStartPost", "ExecCondition", "ExecReload",
                                  "ExecStop", "ExecStopPost") for line in service.get(key, [])]
        self.assertEqual(len(lines), 3)
        for line in lines:
            argv = shlex.split(line)
            self.assertTrue(argv[0].startswith("/"), line)
            self.assertNotIn(Path(argv[0]).name, self.SHELLS, line)
            self.assertNotIn("-c", argv[1:], line)
        # Its stop is bounded and never a kill (I8); the stack's hardening.
        self.assertEqual(one(alarm, "Service", "TimeoutStopSec"), "10")
        self.assertEqual(one(alarm, "Service", "SendSIGKILL"), "no")
        for key, value in self.HARDENING:
            self.assertEqual(one(alarm, "Service", key), value, key)


if __name__ == "__main__":
    unittest.main()
