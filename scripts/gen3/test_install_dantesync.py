"""Tests for scripts/gen3/install-dantesync.sh and dantesync_probe.py (#72
phase B, plan Task 2): the install runs against a stand-in `ssh` and `scp`
first on PATH that play the box in a temporary folder (its home and its
root), the release comes from a local folder through curl's file:// URLs;
the probe runs against stand-in time and status servers on loopback. Every
value is synthetic (P6)."""
from __future__ import annotations

import hashlib
import http.server
import json
import os
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path

from test_deploy import expand, sudo_lines, template_lines

HERE = Path(__file__).resolve().parent
INSTALL = HERE / "install-dantesync.sh"
PROBE = HERE / "dantesync_probe.py"
HOST = "tester@gen3.test"
TAG = "v1.18.0"
GM = "10.0.0.11"
STAGE = ".cache/dantesync-install"
PROBE_OK = '{"waited_s":12,"dsyx":1,"locked":1,"mode":4,"ext":2,"freq_ppb":-750,"http":1,"follower":1,"gm_allowed":1}'

# The box: `ssh` answers the install's commands from FAKE_BOX (home/ and
# root/), `scp` copies into its home; both log every call as one JSON line.
FAKE_SSH = r'''
import hashlib, json, os, shutil, sys
box = os.environ["FAKE_BOX"]
args = sys.argv[1:]
open(os.path.join(box, "calls.log"), "a").write(json.dumps(["ssh"] + args) + "\n")
while args and args[0] == "-o":
    args = args[2:]
cmd = " ".join(args[1:])
home, root = os.path.join(box, "home"), os.path.join(box, "root")
env = os.environ.get
if cmd.startswith(": install-dantesync survey\n"):
    for name in env("FAKE_DAEMONS", "").split():
        print("daemon " + name)
    for unit in env("FAKE_TIME", "systemd-timesyncd").split():
        print("time " + unit)
    if not env("FAKE_NO_PYTHON"):
        print("python3")
elif cmd == "sudo -n ss -Hulnp":
    if env("FAKE_NO_SS_RULE"):
        sys.stderr.write("sudo: a password is required\n")
        sys.exit(1)
    sys.stdout.write(env("FAKE_SS", 'UNCONN 0 0 0.0.0.0:5353 0.0.0.0:* users:(("avahi-daemon",pid=601,fd=12))\n'
                                    'UNCONN 0 0 127.0.0.54:53 0.0.0.0:* users:(("systemd-resolve",pid=520,fd=16))\n'))
elif cmd.startswith("mkdir -p "):
    os.makedirs(os.path.join(home, cmd.split()[-1]), exist_ok=True)
    if env("FAKE_FLAG_ON") == "mkdir":
        open(env("IEMMIXER_EVENT_NOW"), "w").write("now\n")
elif cmd == ".cache/dantesync-install/dantesync --version":
    staged = os.path.join(home, ".cache/dantesync-install/dantesync")
    if not os.access(staged, os.X_OK):
        sys.stderr.write("bash: permission denied\n")
        sys.exit(126)
    print(env("FAKE_STAGED_VERSION", "dantesync 1.18.0"))
elif cmd.startswith("set -e\n"):
    for line in cmd.splitlines()[1:]:
        if env("FAKE_SUDO_FAILS") and line.startswith("sudo -n "):
            sys.stderr.write("sudo: a password is required\n")
            sys.exit(1)
        words = line.split()
        if words[:4] == ["sudo", "-n", "install", "-D"]:
            mode, src, dst = words[5], words[6], words[7]
            target = os.path.join(root, dst.lstrip("/"))
            os.makedirs(os.path.dirname(target), exist_ok=True)
            shutil.copyfile(os.path.join(home, src), target)
            open(os.path.join(box, "modes.log"), "a").write(f"{mode} {dst}\n")
elif cmd.startswith("sha256sum "):
    path = cmd.split()[1]
    data = open(os.path.join(root, path.lstrip("/")), "rb").read()
    print(env("FAKE_SHA", hashlib.sha256(data).hexdigest()) + "  " + path)
elif cmd == "/usr/local/bin/dantesync --version":
    print(env("FAKE_VERSION", "dantesync 1.18.0"))
elif cmd == "systemctl is-active dantesync":
    state = env("FAKE_ACTIVE", "active")
    print(state)
    sys.exit(0 if state == "active" else 3)
elif cmd.startswith("python3 - --wait "):
    open(os.path.join(box, "probe.stdin"), "wb").write(sys.stdin.buffer.read())
    print(env("FAKE_PROBE_OUT", "PROBE_OK"))
    sys.stderr.write(env("FAKE_PROBE_ERR", ""))
    sys.exit(int(env("FAKE_PROBE_RC", "0")))
else:
    sys.stderr.write("unexpected command: " + cmd + "\n")
    sys.exit(97)
'''.replace("PROBE_OK", PROBE_OK.replace('"', '\\"'))

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
    # scp keeps a new file's mode bits, as shutil.copy does.
    shutil.copy(src, os.path.join(folder, os.path.basename(src)))
'''

UNIT = """[Unit]
Description=DanteSync PTP Time Sync Service v1.18.0
After=network-online.target
Wants=network-online.target

[Service]
# Run as root for port 319, adjtimex, and RTC ioctl access
User=root
Group=root
ExecStart=/usr/local/bin/dantesync
Restart=always
RestartSec=5
# High priority for timestamping accuracy
CPUSchedulingPolicy=fifo
CPUSchedulingPriority=50

[Install]
WantedBy=multi-user.target
"""


def follower_config(**changes) -> dict:
    """A synthetic follower config (dantesync's own layout), changed by
    `changes` (a value of None removes the key; `system` is merged)."""
    config = {
        "ntp_server": "ntp-master.test",
        "ntp_server_mode": {"enabled": False, "max_step_us": 100000, "port": 123, "stratum": 3},
        "http_status": {"enabled": True, "port": 8898},
        "system": {"gm_allowlist": [GM], "clock_discipline": "ptp_phase_lock"},
    }
    system = changes.pop("system", None)
    if system is not None:
        config["system"].update(system)
        config["system"] = {k: v for k, v in config["system"].items() if v is not None}
    config.update(changes)
    return {k: v for k, v in config.items() if v is not None}


class InstallTests(unittest.TestCase):
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
        # The release as GitHub serves it: the asset and its .sha256.
        releases = self.tmp / "releases"
        asset = releases / TAG / "dantesync-linux-amd64"
        asset.parent.mkdir(parents=True)
        self.binary = b"\x7fELF a synthetic dantesync"
        asset.write_bytes(self.binary)
        self.sha = hashlib.sha256(self.binary).hexdigest()
        (releases / TAG / "dantesync-linux-amd64.sha256").write_text(
            f"{self.sha}  dantesync-linux-amd64\n", encoding="utf-8")
        self.env = {**os.environ, "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}", "FAKE_BOX": str(self.box),
                    "IEMMIXER_EVENT_NOW": str(self.flag), "GEN3_SETTLE_S": "0",
                    "DANTESYNC_RELEASE_BASE": releases.as_uri()}
        self.config = self.tmp / "gen3-dantesync.json"
        self.write_config(follower_config())

    def write_config(self, config: dict | str) -> None:
        text = config if isinstance(config, str) else json.dumps(config, indent=2) + "\n"
        self.config.write_text(text, encoding="utf-8")

    def install(self, *extra: str, args: list[str] | None = None, **env: str) -> subprocess.CompletedProcess:
        argv = args if args is not None else ["--host", HOST, "--version", TAG, "--sha256", self.sha,
                                              "--config", str(self.config), *extra]
        return subprocess.run(["bash", str(INSTALL), *argv], env={**self.env, **env}, capture_output=True,
                              text=True, timeout=60)

    def calls(self) -> list[list[str]]:
        log = self.box / "calls.log"
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

    def installed(self, path: str) -> bytes:
        return (self.box / "root" / path.lstrip("/")).read_bytes()

    def assert_nothing_installed(self) -> None:
        self.assertFalse(any(c.startswith("set -e\n") for c in self.remote()), self.remote())
        self.assertEqual(list((self.box / "root").iterdir()), [])

    def assert_nothing_copied(self) -> None:
        self.assertNotIn("scp", [c[0] for c in self.calls()])
        self.assert_nothing_installed()

    def test_an_install_puts_dantesync_in_place_and_reads_it_back(self) -> None:
        r = self.install(FAKE_TIME="systemd-timesyncd chrony")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.installed("/usr/local/bin/dantesync"), self.binary)
        self.assertEqual(self.installed("/etc/dantesync/config.json"), self.config.read_bytes())
        self.assertEqual(self.installed("/etc/systemd/system/dantesync.service").decode(), UNIT)
        modes = (self.box / "modes.log").read_text(encoding="utf-8").splitlines()
        self.assertEqual(modes, ["0755 /usr/local/bin/dantesync", "0644 /etc/dantesync/config.json",
                                 "0644 /etc/systemd/system/dantesync.service"])
        remote = self.remote()
        self.assertTrue(remote[0].startswith(": install-dantesync survey\n"), remote[0])
        self.assertEqual(remote[1:4], ["sudo -n ss -Hulnp", f"mkdir -p {STAGE}", f"{STAGE}/dantesync --version"])
        install = remote[4].splitlines()
        self.assertEqual(install[0], "set -e")
        self.assertEqual(install[4:], [
            "sudo -n systemctl daemon-reload",
            "sudo -n systemctl disable --now systemd-timesyncd",
            "sudo -n systemctl disable --now chrony",
            "sudo -n timedatectl set-ntp false",
            "sudo -n systemctl enable dantesync",
            "sudo -n systemctl restart dantesync",
        ])
        self.assertEqual(remote[5:], ["sha256sum /usr/local/bin/dantesync", "/usr/local/bin/dantesync --version",
                                      "systemctl is-active dantesync", "python3 - --wait 300"])
        # The probe goes to the box's python3 on stdin, nothing left there.
        self.assertEqual((self.box / "probe.stdin").read_bytes(), PROBE.read_bytes())
        self.assertTrue(all("BatchMode=yes" in c for c in self.calls()))
        last = r.stdout.strip().splitlines()[-1]
        self.assertIn("dantesync 1.18.0", last)
        self.assertIn(f"sha256 {self.sha}", last)
        self.assertIn("dantesync active", last)
        self.assertIn(PROBE_OK, last)
        # No site value of the config is ever printed (P6).
        for value in (GM, "ntp-master.test"):
            self.assertNotIn(value, r.stdout + r.stderr)

    def test_every_sudo_line_the_install_sends_is_in_the_template(self) -> None:
        # Every time service the box may have, so each disable line is sent.
        r = self.install(FAKE_TIME="systemd-timesyncd chrony ntp time-sync-coordinator")
        self.assertEqual(r.returncode, 0, r.stderr)
        allowed = {(runas, text) for runas, command in template_lines() for text in expand(command)}
        lines = sudo_lines(self.remote())
        # ss, three installs, daemon-reload, four disables, set-ntp, enable, restart.
        self.assertEqual(len(lines), 12, lines)
        for runas, command, _listed in lines:
            self.assertIn((runas, command), allowed, command)

    def test_only_the_time_services_on_the_box_are_disabled(self) -> None:
        for present, want in (("", []), ("ntp time-sync-coordinator", ["ntp", "time-sync-coordinator"])):
            with self.subTest(present=present):
                (self.box / "calls.log").unlink(missing_ok=True)
                r = self.install(FAKE_TIME=present)
                self.assertEqual(r.returncode, 0, r.stderr)
                install = next(c for c in self.remote() if c.startswith("set -e\n")).splitlines()
                disabled = [line.split()[-1] for line in install if "disable --now" in line]
                self.assertEqual(disabled, want)
                self.assertIn("sudo -n timedatectl set-ntp false", install)

    def test_the_wait_for_the_read_back_is_bounded_by_the_env(self) -> None:
        r = self.install(GEN3_DANTESYNC_WAIT_S="45")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.remote()[-1], "python3 - --wait 45")

    def test_the_event_flag_refuses_before_anything(self) -> None:
        self.flag.parent.mkdir()
        self.flag.write_text("now\n", encoding="utf-8")
        r = self.install()
        self.assertEqual(r.returncode, 1)
        self.assertIn("an event is on", r.stderr)
        self.assertIn("nothing was downloaded or changed", r.stderr)
        self.assertEqual(self.calls(), [])

    def test_a_flag_that_comes_during_the_copy_refuses_the_install(self) -> None:
        self.flag.parent.mkdir()
        r = self.install(FAKE_FLAG_ON="mkdir")
        self.assertEqual(r.returncode, 1)
        self.assertIn("an event is on", r.stderr)
        self.assertIn("nothing installed", r.stderr)
        self.assertEqual(self.remote()[-1], f"{STAGE}/dantesync --version")
        self.assert_nothing_installed()

    def test_a_busy_319_or_320_refuses_before_any_copy(self) -> None:
        cases = [
            ('UNCONN 0 0 0.0.0.0:319 0.0.0.0:* users:(("ptp4l",pid=900,fd=5))\n', "UDP 319 by ptp4l"),
            ('UNCONN 0 0 0.0.0.0%eth9:320 0.0.0.0:*\n', "UDP 320 by an unknown process"),
            ('UNCONN 0 0 [::]:319 [::]:* users:(("statime",pid=7,fd=3),("dantesync",pid=8,fd=4))\n'
             'UNCONN 0 0 224.0.1.129:320 0.0.0.0:* users:(("statime",pid=7,fd=9))\n',
             "UDP 319 by statime, UDP 320 by statime"),
        ]
        for sockets, want in cases:
            with self.subTest(want=want):
                (self.box / "calls.log").unlink(missing_ok=True)
                r = self.install(FAKE_SS=sockets)
                self.assertEqual(r.returncode, 1, r.stderr)
                self.assertIn("UDP 319/320 is already held on the box", r.stderr)
                self.assertIn(want, r.stderr)
                self.assertIn("nothing was copied", r.stderr)
                self.assertEqual(self.remote()[-1], "sudo -n ss -Hulnp")
                self.assert_nothing_copied()

    def test_dantesync_s_own_sockets_are_an_update(self) -> None:
        sockets = ('UNCONN 0 0 0.0.0.0:319 0.0.0.0:* users:(("dantesync",pid=812,fd=6))\n'
                   'UNCONN 0 0 0.0.0.0:320 0.0.0.0:* users:(("dantesync",pid=812,fd=7))\n'
                   'UNCONN 0 0 0.0.0.0:31900 0.0.0.0:* users:(("dantesync",pid=812,fd=8))\n'
                   'UNCONN 0 0 0.0.0.0:3190 0.0.0.0:* users:(("other",pid=9,fd=1))\n')
        r = self.install(FAKE_SS=sockets)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("dantesync already holds UDP 319/320: updating it", r.stdout)
        self.assertEqual(self.installed("/usr/local/bin/dantesync"), self.binary)

    def test_a_ptp_daemon_on_the_box_refuses_before_any_copy(self) -> None:
        for daemons in ("ptp4l", "phc2sys", "statime", "ptp4l.service", "phc2sys statime.service"):
            with self.subTest(daemons=daemons):
                (self.box / "calls.log").unlink(missing_ok=True)
                r = self.install(FAKE_DAEMONS=daemons)
                self.assertEqual(r.returncode, 1, r.stderr)
                self.assertIn(f"a PTP daemon is installed on the box ({daemons.replace(' ', ', ')})", r.stderr)
                self.assertIn("nothing was copied", r.stderr)
                self.assertEqual(len(self.remote()), 1)
                self.assert_nothing_copied()

    def test_a_box_without_python3_is_refused_before_any_copy(self) -> None:
        r = self.install(FAKE_NO_PYTHON="1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("no python3", r.stderr)
        self.assert_nothing_copied()

    def test_a_box_whose_sudo_does_not_allow_ss_is_refused_before_any_copy(self) -> None:
        r = self.install(FAKE_NO_SS_RULE="1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("'sudo -n ss -Hulnp'", r.stderr)
        self.assertIn("nothing was copied", r.stderr)
        self.assert_nothing_copied()

    def test_a_wrong_sha256_refuses_before_any_call(self) -> None:
        r = self.install(args=["--host", HOST, "--version", TAG, "--sha256", "0" * 64, "--config", str(self.config)])
        self.assertEqual(r.returncode, 1)
        self.assertIn("names '" + self.sha + "', not --sha256 " + "0" * 64, r.stderr)
        self.assertIn("nothing was copied", r.stderr)
        self.assertEqual(self.calls(), [])
        # The release's .sha256 agrees with --sha256, the binary does not
        # (a corrupt download): refused too.
        asset = self.tmp / "releases" / TAG / "dantesync-linux-amd64"
        asset.write_bytes(self.binary + b"!")
        r = self.install()
        self.assertEqual(r.returncode, 1)
        self.assertIn(f"sha256 is {hashlib.sha256(self.binary + b'!').hexdigest()}, not {self.sha}", r.stderr)
        self.assertEqual(self.calls(), [])
        # A tag without the asset: the download fails.
        r = self.install(args=["--host", HOST, "--version", "v9.9.9", "--sha256", self.sha,
                               "--config", str(self.config)])
        self.assertEqual(r.returncode, 1)
        self.assertIn("the download of dantesync-linux-amd64 v9.9.9 failed", r.stderr)
        self.assertEqual(self.calls(), [])

    def test_a_staged_binary_of_another_version_is_never_installed(self) -> None:
        r = self.install(FAKE_STAGED_VERSION="dantesync 1.17.2")
        self.assertEqual(r.returncode, 1)
        self.assertIn("says 'dantesync 1.17.2', not 1.18.0: nothing installed", r.stderr)
        self.assert_nothing_installed()

    def test_a_missing_extension_fails_the_read_back_and_nothing_follows(self) -> None:
        line = PROBE_OK.replace('"ext":2', '"ext":0').replace('"waited_s":12', '"waited_s":300')
        r = self.install(FAKE_PROBE_OUT=line, FAKE_PROBE_RC="1",
                         FAKE_PROBE_ERR="dantesync_probe: after 300 s: no date-offset extension in the DSYX reply\n")
        self.assertEqual(r.returncode, 1)
        self.assertIn("no date-offset extension", r.stderr)
        self.assertIn("not yet a locked follower of an allowed grandmaster within 300 s", r.stderr)
        self.assertIn(line, r.stderr)
        # The probe was the last thing done: nothing changed after it.
        self.assertEqual(self.remote()[-1], "python3 - --wait 300")

    def test_a_dantesync_that_is_not_active_fails_before_the_probe(self) -> None:
        r = self.install(FAKE_ACTIVE="activating")
        self.assertEqual(r.returncode, 1)
        self.assertIn("dantesync is 'activating'", r.stderr)
        self.assertEqual(self.remote()[-1], "systemctl is-active dantesync")

    def test_an_installed_binary_or_version_that_is_not_the_release_fails(self) -> None:
        r = self.install(FAKE_SHA="f" * 64)
        self.assertEqual(r.returncode, 1)
        self.assertIn(f"sha256 is {'f' * 64}, not {self.sha}", r.stderr)
        r = self.install(FAKE_VERSION="dantesync 1.18.1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("said 'dantesync 1.18.1', not 1.18.0", r.stderr)

    def test_a_refused_sudo_stops_before_the_read_back(self) -> None:
        r = self.install(FAKE_SUDO_FAILS="1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("a password is required", r.stderr)
        self.assertTrue(self.remote()[-1].startswith("set -e\n"))

    def test_a_bad_command_line_is_exit_2_and_calls_nothing(self) -> None:
        full = ["--host", HOST, "--version", TAG, "--sha256", self.sha, "--config", str(self.config)]

        def without(flag: str) -> list[str]:
            i = full.index(flag)
            return full[:i] + full[i + 2:]

        def with_value(flag: str, value: str) -> list[str]:
            i = full.index(flag)
            return full[:i + 1] + [value] + full[i + 2:]

        cases = [
            (without("--host"), "--host is missing"),
            (without("--version"), "--version is missing"),
            (without("--sha256"), "--sha256 is missing"),
            (without("--config"), "--config is missing"),
            (full + ["--iface", "eth9"], "unknown argument '--iface'"),
            (full + ["--host"], "--host needs a value"),
            (with_value("--host", "-oProxyCommand=true"), "not an option"),
            (with_value("--version", "latest"), "not a release tag"),
            (with_value("--version", "1.18.0"), "not a release tag"),
            (with_value("--sha256", self.sha.upper()), "64 lowercase hex"),
            (with_value("--config", str(self.tmp / "missing.json")), "--config is not a file"),
        ]
        for argv, want in cases:
            r = self.install(args=argv)
            self.assertEqual(r.returncode, 2, (argv, r.stderr))
            self.assertIn(want, r.stderr, argv)
        self.assertEqual(self.calls(), [])

    def test_a_config_that_is_not_a_follower_s_is_exit_2_and_calls_nothing(self) -> None:
        cases = [
            ("{not json", "is not JSON"),
            ("[]", "is not a JSON object"),
            (follower_config(ntp_server=None), "has no ntp_server"),
            (follower_config(ntp_server=" "), "has no ntp_server"),
            (follower_config(ntp_server_mode={"enabled": True, "port": 123}), "ntp_server_mode.enabled must be false"),
            (follower_config(ntp_server_mode=None), "ntp_server_mode.enabled must be false"),
            (follower_config(http_status={"enabled": False, "port": 8898}), "http_status enabled on port 8898"),
            (follower_config(http_status={"enabled": True, "port": 8899}), "http_status enabled on port 8898"),
            (follower_config(system={"gm_allowlist": []}), "has no system.gm_allowlist"),
            (follower_config(system={"gm_allowlist": None}), "has no system.gm_allowlist"),
            (follower_config(system={"gm_allowlist": [""]}), "has no system.gm_allowlist"),
            (follower_config(system={"clock_discipline": "legacy"}), "clock_discipline ptp_phase_lock"),
        ]
        for config, want in cases:
            with self.subTest(want=want, config=config):
                self.write_config(config)
                r = self.install()
                self.assertEqual(r.returncode, 2, r.stderr)
                self.assertIn(want, r.stderr)
                for value in (GM, "ntp-master.test"):
                    self.assertNotIn(value, r.stderr)
        self.assertEqual(self.calls(), [])
        # The default discipline (the key absent) is the phase lock.
        self.write_config(follower_config(system={"clock_discipline": None}))
        self.assertEqual(self.install().returncode, 0)


def dsyx_reply(request_id: int, locked: bool = True, mode: int = 4, freq_ppb: int = -750, ext: int = 2) -> bytes:
    """A reply laid out as dantesync's build_response and encode_extension
    write it (crates/iem-dante/src/clock/dsyx.rs); ext 0: the 64-byte base
    alone."""
    base = bytearray(64)
    base[0:4] = b"DSYR"
    base[4:8] = struct.pack(">I", request_id)
    base[8:16] = struct.pack(">q", 1_790_000_100_000_000_000)
    base[36:40] = struct.pack(">i", freq_ppb)
    base[40] = mode
    base[41] = int(locked)
    base[62] = 0x02
    if not ext:
        return bytes(base)
    extension = bytearray(48)
    extension[0] = ext
    extension[1] = 0x01
    extension[32:40] = struct.pack(">q", 100_000_000_000)
    return bytes(base + extension)


class StandIns:
    """dantesync's time server (UDP) and HTTP status on loopback, answering
    from `self.dsyx` (a function of the request id and the poll's number,
    returning the datagrams to send) and `self.status` (None: HTTP 500)."""

    def __init__(self, test: unittest.TestCase) -> None:
        self.dsyx = lambda request_id, n: [dsyx_reply(request_id)]
        self.status: dict | None = {"date_authority": "follower", "gm_source_ip": GM, "gm_allowlist_resolved": []}
        self.requests = 0
        self.udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.udp.bind(("127.0.0.1", 0))
        self.udp.settimeout(0.05)
        self.stop = threading.Event()
        udp_thread = threading.Thread(target=self.serve_udp, daemon=True)
        udp_thread.start()
        stand_ins = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self) -> None:  # http.server's name for a GET
                body = json.dumps(stand_ins.status).encode() if stand_ins.status is not None else b"{}"
                self.send_response(200 if stand_ins.status is not None else 500)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args) -> None:
                pass

        self.http = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        http_thread = threading.Thread(target=self.http.serve_forever, daemon=True)
        http_thread.start()

        def close() -> None:
            self.stop.set()
            self.http.shutdown()
            self.http.server_close()
            udp_thread.join(5)
            self.udp.close()

        test.addCleanup(close)

    def serve_udp(self) -> None:
        while not self.stop.is_set():
            try:
                data, peer = self.udp.recvfrom(256)
            except OSError:
                continue
            if len(data) != 64 or data[0:4] != b"DSYX":
                continue
            self.requests += 1
            request_id = struct.unpack(">I", data[4:8])[0]
            for reply in self.dsyx(request_id, self.requests):
                self.udp.sendto(reply, peer)

    def ports(self) -> list[str]:
        return ["--dsyx-port", str(self.udp.getsockname()[1]), "--http-port", str(self.http.server_address[1])]


class ProbeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.config = self.tmp / "config.json"
        self.write_allowlist([GM])
        self.stand_ins = StandIns(self)

    def write_allowlist(self, allow: list[str]) -> None:
        self.config.write_text(json.dumps(follower_config(system={"gm_allowlist": allow})), encoding="utf-8")

    def probe(self, *extra: str, stdin: bool = False, ports: list[str] | None = None) -> subprocess.CompletedProcess:
        args = ["--config", str(self.config), "--interval", "0.05",
                *(ports if ports is not None else self.stand_ins.ports()), *extra]
        if stdin:
            return subprocess.run([sys.executable, "-", *args], input=PROBE.read_text(encoding="utf-8"),
                                  capture_output=True, text=True, timeout=30)
        return subprocess.run([sys.executable, str(PROBE), *args], capture_output=True, text=True, timeout=30)

    def line(self, r: subprocess.CompletedProcess) -> dict:
        lines = r.stdout.strip().splitlines()
        self.assertEqual(len(lines), 1, r.stdout)
        return json.loads(lines[0])

    def test_a_locked_follower_of_an_allowed_grandmaster_passes(self) -> None:
        r = self.probe("--wait", "0")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.line(r), {"waited_s": 0, "dsyx": 1, "locked": 1, "mode": 4, "ext": 2,
                                        "freq_ppb": -750, "http": 1, "follower": 1, "gm_allowed": 1})
        self.assertEqual(r.stderr, "")

    def test_the_probe_runs_from_stdin_as_install_feeds_it(self) -> None:
        r = self.probe("--wait", "0", stdin=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.line(r)["gm_allowed"], 1)

    def test_each_missing_piece_is_its_reason_and_no_value_is_printed(self) -> None:
        def unlocked(request_id: int, n: int) -> list[bytes]:
            return [dsyx_reply(request_id, locked=False, mode=1)]

        def no_ext(request_id: int, n: int) -> list[bytes]:
            return [dsyx_reply(request_id, ext=0)]

        def silent(request_id: int, n: int) -> list[bytes]:
            return []

        def other_id(request_id: int, n: int) -> list[bytes]:
            return [dsyx_reply((request_id + 1) & 0xFFFFFFFF)]

        follower = {"date_authority": "follower", "gm_source_ip": GM}
        cases = [
            (no_ext, follower, "no date-offset extension in the DSYX reply", {"ext": 0, "locked": 1}),
            (unlocked, follower, "dantesync is not locked", {"locked": 0, "mode": 1}),
            (silent, follower, "no DSYX reply", {"dsyx": 0, "locked": None, "ext": None}),
            (other_id, follower, "no DSYX reply", {"dsyx": 0}),
            (None, {"date_authority": "local", "gm_source_ip": GM}, "date_authority is 'local', not follower",
             {"follower": 0, "gm_allowed": 1}),
            (None, {"date_authority": "master", "gm_source_ip": GM}, "date_authority is 'master', not follower",
             {"follower": 0}),
            (None, {"date_authority": "follower", "gm_source_ip": "10.0.0.99"},
             "the grandmaster dantesync follows is not one of the config's gm_allowlist",
             {"follower": 1, "gm_allowed": 0}),
            (None, {"date_authority": "follower", "gm_source_ip": None},
             "the grandmaster dantesync follows is not one of the config's gm_allowlist", {"gm_allowed": 0}),
            (None, None, "no HTTP status", {"http": 0, "follower": None, "gm_allowed": None}),
        ]
        for dsyx, status, reason, fields in cases:
            with self.subTest(reason=reason, status=status):
                if dsyx is not None:
                    self.stand_ins.dsyx = dsyx
                self.stand_ins.status = status
                r = self.probe("--wait", "0")
                self.assertEqual(r.returncode, 1, r.stderr)
                self.assertIn(f"dantesync_probe: after 0 s: {reason}", r.stderr)
                line = self.line(r)
                for key, value in fields.items():
                    self.assertEqual(line[key], value, key)
                self.assertTrue(all(v is None or isinstance(v, int) for v in line.values()), line)
                for value in (GM, "10.0.0.99"):
                    self.assertNotIn(value, r.stdout + r.stderr)
                self.stand_ins.dsyx = lambda request_id, n: [dsyx_reply(request_id)]

    def test_a_stale_reply_before_ours_is_skipped(self) -> None:
        self.stand_ins.dsyx = lambda request_id, n: [dsyx_reply((request_id - 1) & 0xFFFFFFFF, locked=False),
                                                     dsyx_reply(request_id)]
        r = self.probe("--wait", "0")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.line(r)["locked"], 1)

    def test_a_prefix_or_a_resolved_host_allows_the_grandmaster(self) -> None:
        self.write_allowlist(["10.0.0.0/24"])
        self.assertEqual(self.probe("--wait", "0").returncode, 0)
        self.write_allowlist(["gm.test"])
        self.stand_ins.status = {"date_authority": "follower", "gm_source_ip": GM, "gm_allowlist_resolved": [GM]}
        self.assertEqual(self.probe("--wait", "0").returncode, 0)
        self.stand_ins.status = {"date_authority": "follower", "gm_source_ip": GM, "gm_allowlist_resolved": []}
        self.assertEqual(self.probe("--wait", "0").returncode, 1)
        self.write_allowlist(["10.0.1.0/24", "10.0.0.12"])
        self.assertEqual(self.probe("--wait", "0").returncode, 1)

    def test_it_polls_until_dantesync_is_ready_within_the_wait(self) -> None:
        # Not locked for the first three polls, then a locked follower.
        self.stand_ins.dsyx = lambda request_id, n: [dsyx_reply(request_id, locked=n > 3)]
        r = self.probe("--wait", "10")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertGreaterEqual(self.stand_ins.requests, 4)
        self.assertEqual(self.line(r)["locked"], 1)
        # Never ready: it gives up after the wait, with its reason.
        self.stand_ins.dsyx = lambda request_id, n: [dsyx_reply(request_id, ext=0)]
        r = self.probe("--wait", "1")
        self.assertEqual(r.returncode, 1)
        self.assertEqual(self.line(r)["waited_s"], 1)
        self.assertIn("after 1 s: no date-offset extension", r.stderr)

    def test_an_unreadable_config_is_exit_2(self) -> None:
        for text in ("{not json", json.dumps(follower_config(system={"gm_allowlist": []})), "[]"):
            self.config.write_text(text, encoding="utf-8")
            r = self.probe("--wait", "0")
            self.assertEqual(r.returncode, 2, text)
            self.assertIn("cannot read its gm_allowlist", r.stderr)
        r = self.probe("--wait", "0", "--config", str(self.tmp / "missing.json"))
        self.assertEqual(r.returncode, 2)


if __name__ == "__main__":
    unittest.main()
