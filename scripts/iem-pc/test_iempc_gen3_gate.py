"""Tests for scripts/iem-pc/iempc_gen3_gate.py, `iempc gen3-gate hil` (#72
phase H, plan docs/superpowers/plans/2026-10-28-gen3-phase-h-box-gates.md
Tasks 2, 4 and 6): the Gen 3 box's HIL job and checks, its verdict and the
`hil/gen3` status. No box and no device exist in CI: a stand-in `ssh` first
on PATH plays the box (its root and the admin's home in a temporary folder,
systemd's view of the engine's unit and drop-in, the engine's supervisor
client with the HIL test signal and its D5(b) probe, the device's status,
health, the builds, the alarm count), and gen3_arc.send reaches a FakeDante
(test_gen3_arc) as our own device, whose RX table the D5(b) loop changes.
FakeGh (iempc_test_support) takes the status post. Every value is synthetic
(P6): the test site's spare outputs 94 and 95, invented names, documentation
addresses."""
from __future__ import annotations

import json
import os
import shutil
import sys
import tomllib
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import gen3_arc  # noqa: E402
import iempc_gen3_gate as gate  # noqa: E402
from iempc_test_support import SHA, Base, ip  # noqa: E402
from test_gen3_arc import FakeDante, Network  # noqa: E402

# The sudo template, read as the deploy's tests read it: only those functions.
sys.path.insert(1, str(HERE.parent / "gen3"))
from test_deploy import expand, sudo_lines, template_lines  # noqa: E402

REPO = HERE.parent.parent
GEN3 = "tester@gen3.test"
DANTE_IP = "192.0.2.20"
OURS = "zyxqw-gen3"
SPARES = (94, 95)   # the test site's [guard] hil_tx
RET = SPARES[0]   # the D5(b) loop's return: our RX 94 hears our TX 94
TRIP = 77   # the self-loop's round trip, samples
PEAK = 10 ** (-30 / 20)   # -30 dBFS, linear
BAND_RX = (101, 102, 103, 104)   # band inputs Receiving
NOW = 1_900_000_000.0
DROPIN = gate.DROPIN
UNIT_LINE = ("/opt/iemmixer/bin/iem-engine run --backend dante --site /var/lib/iemmixer/site/iemmixer.toml "
             "--state-dir /var/lib/iemmixer/engine --pipe /run/iemmixer/engine.sock --dante-config "
             "/etc/iemmixer/dante-device.toml --dante-iface ${IEM_DANTE_IFACE}")

# The box. Every call is logged (calls.log: its arguments, one JSON line);
# state.json is systemd's and the engine's view: `loaded` (the drop-in text
# daemon-reload took, or null), `hil` (the engine runs with --test-signal),
# `fault` (and with --fault-injection), `starts` (the restarts asked for),
# `nrestarts`, `pid` (the engine's MainPID, a new one at every start),
# `phases` (the statuses still to come after a test signal), `loopback`,
# `since` (device status reads since the last restart), `alarm_ts` (the alarm
# unit's InactiveExitTimestampMonotonic: it moves when that unit starts) and
# `journal` (the engine's journal as `journalctl -o json` gives it; an
# earlier run's panic, another process's, is in it from the start).
# FAKE_PANIC plays the injected fault: "" the engine's panic, its line in the
# journal and systemd's one restart; `stays` no restart and no line; `twice`
# two restarts; `alarm` the alarm unit started too; `silent` no line.
# loop.json (written by the FakeDante beside) says whether our RX 94 hears our
# own TX 94. FAKE_* change one answer each. For `gen3-gate live`
# (test_iempc_gen3_live): the service user's mint folder (root/var/lib/
# iemmixer/mint: the mint writes a new owner-only file, never over one), the
# live burst (each one a line of bursts.log), the server's journal
# (journal.log), a push subscription left behind (the file push-left) and
# an engine that is not healthy in the HIL job (FAKE_UNHEALTHY_IN_JOB).
# FAKE_SSH_DOWN plays ssh's own failure (exit 255, its stderr naming the
# host as ssh does): `*` for every call, else the one command line it names.
FAKE_SSH = r'''
import json, os, sys, time
box = os.environ["FAKE_GEN3"]
args = sys.argv[1:]
data = sys.stdin.read() if not sys.stdin.isatty() else ""
with open(os.path.join(box, "calls.log"), "a") as f:
    f.write(json.dumps(args) + "\n")
while args and args[0] == "-o":
    args = args[2:]
words = args[1:]
line = " ".join(words)
if os.environ.get("FAKE_SSH_DOWN") in ("*", line):
    sys.stderr.write(f"ssh: connect to host {args[0].rpartition('@')[2]} port 22: No route to host\n")
    sys.exit(255)
root, home = os.path.join(box, "root"), os.path.join(box, "home")
state_path = os.path.join(box, "state.json")
st = json.load(open(state_path))
DROPIN = "/run/systemd/system/iem-engine.service.d/iem-hil.conf"
ENGINE = "/opt/iemmixer/bin/iem-engine"
DANTE = "/opt/iemmixer/bin/iem-dante-dev"
SITE = "/var/lib/iemmixer/site/iemmixer.toml"
SVC = ["sudo", "-n", "-u", "iemmixer"]
PEAK = 10 ** (-30 / 20)

def at(path):
    return os.path.join(root, path.lstrip("/"))

# The state files this call writes (the soak's part adds its own). A file is
# replaced whole (never read half written by the next call), and once this
# call raised the flag it writes nothing more: the gate leaves a read-only call
# to end by itself and goes on, so a later write would undo the next calls'.
SAVES = [lambda: (state_path, st)]
FROZEN = []

def save():
    if FROZEN:
        return
    for path, value in (s() for s in SAVES):
        tmp = f"{path}.{os.getpid()}.tmp"
        with open(tmp, "w") as f:
            json.dump(value, f)
        os.replace(tmp, path)

def done(code=0):
    save()
    sys.exit(code)

def fail(text, code=1):
    sys.stderr.write(text + "\n")
    done(code)

def looped():
    p = os.path.join(box, "loop.json")
    return os.path.exists(p) and json.load(open(p)).get("looped") is True

def flag():
    save()
    FROZEN.append(True)
    path = os.environ["FAKE_FLAG"]
    os.makedirs(os.path.dirname(path), exist_ok=True)
    open(path, "w").write("now\n")

def cat(path):
    try:
        sys.stdout.write(open(at(path)).read())
    except FileNotFoundError:
        fail(f"cat: {path}: No such file or directory")
    done()

def engine_status():
    hil = []
    if st["hil"]:
        phase = st["phases"].pop(0) if st["phases"] else "silent"
        if phase == "sound" and os.environ.get("FAKE_FLAG_ON") == "burst":
            flag()
        if os.environ.get("FAKE_NEVER_SILENT") and phase == "silent" and st["signals"]:
            phase = "sound"
        peak = float(os.environ.get("FAKE_PEAK", PEAK)) if phase == "sound" else 0.0
        hil = [{"tx": 94, "peak": peak}, {"tx": 95, "peak": peak}]
        loopback = st["loopback"] if phase != "stale" else st["old_loopback"]
    else:
        loopback = 0
    return {"callbacks": 9000, "late": 0, "faulted": False, "frames": 32, "hil": hil, "loopback_samples": loopback}

if line == f"test -e {DROPIN} && echo left || echo none":
    print("left" if os.path.exists(at(DROPIN)) else "none")
    done()
if line == "systemctl show --property=DropInPaths iem-engine.service":
    paths = ([DROPIN] if st["loaded"] is not None else []) + os.environ.get("FAKE_DROPINS", "").split()
    print("DropInPaths=" + " ".join(paths))
    done()
if line == "systemctl show --property=NRestarts iem-engine.service":
    if os.environ.get("FAKE_NRESTARTS_FAILS"):
        fail("Failed to get properties: Connection timed out")
    print(f"NRestarts={st['nrestarts']}")
    done()
if line == "systemctl show --property=ActiveState,MainPID,NRestarts iem-engine.service":
    # systemd's own order, not the one asked for.
    print(f"MainPID={st['pid']}\nNRestarts={st['nrestarts']}\nActiveState=active")
    done()
if line == "systemctl show --property=InactiveExitTimestampMonotonic iem-alarm@iem-engine.service.service":
    if os.environ.get("FAKE_FLAG_ON") == "fault":
        flag()
    print(f"InactiveExitTimestampMonotonic={st['alarm_ts']}")
    done()
if line == "systemctl is-enabled iem-engine.service || true":
    print("disabled" if os.environ.get("FAKE_DISABLED") else "enabled")
    done()
if words[:1] == ["cat"] and len(words) == 2:
    cat(words[1])
if line == "mkdir -p .cache/iem-gen3-gate && cat > .cache/iem-gen3-gate/iem-hil.conf":
    os.makedirs(os.path.join(home, ".cache/iem-gen3-gate"), exist_ok=True)
    open(os.path.join(home, ".cache/iem-gen3-gate/iem-hil.conf"), "w").write(data)
    done()
if words[:2] == ["sudo", "-n"] and words[2:3] != ["-u"]:
    rest = words[2:]
    if rest == ["install", "-D", "-m", "0644", ".cache/iem-gen3-gate/iem-hil.conf", DROPIN]:
        os.makedirs(os.path.dirname(at(DROPIN)), exist_ok=True)
        open(at(DROPIN), "w").write(open(os.path.join(home, rest[4])).read())
        done()
    if rest == ["rm", "-f", DROPIN]:
        if os.environ.get("FAKE_RM_FAILS"):
            fail("rm: cannot remove: Read-only file system")
        if os.path.exists(at(DROPIN)):
            os.remove(at(DROPIN))
        done()
    if rest == ["systemctl", "daemon-reload"]:
        if os.environ.get("FAKE_RELOAD_KEEPS") and st["loaded"] is not None:
            done()   # a drop-in systemd still applies (another copy of it, say)
        st["loaded"] = open(at(DROPIN)).read() if os.path.exists(at(DROPIN)) else None
        done()
    if rest == ["systemctl", "reset-failed", "iem-engine"]:
        st["nrestarts"] = 0
        done()
    if rest == ["systemctl", "restart", "iem-engine"]:
        st["starts"] += 1
        st["hil"] = st["loaded"] is not None and "--test-signal" in st["loaded"].split()
        st["fault"] = st["loaded"] is not None and "--fault-injection" in st["loaded"].split()
        st["pid"] += 1
        st["phases"], st["since"], st["signals"] = [], 0, 0
        if st["hil"]:
            st["nrestarts"] += int(os.environ.get("FAKE_CRASHES", "0"))
        done()
    if rest == ["journalctl", "-u", "iem-engine.service", "-o", "json", "--output-fields=_PID,MESSAGE", "-n", "2000",
                "--no-pager", "-q"]:
        for entry in st["journal"]:
            print(json.dumps({"__CURSOR": "s=zyxqw", "__REALTIME_TIMESTAMP": "1", **entry}))
        done()
    if rest == ["journalctl", "-u", "iem-server.service", "-o", "cat", "-n", "20000", "--no-pager", "-q"]:
        if os.environ.get("FAKE_JOURNAL_FAILS"):
            fail("Failed to open the journal: Permission denied")
        journal = os.path.join(box, "journal.log")
        sys.stdout.write(open(journal).read() if os.path.exists(journal) else "")
        done()
    fail("unexpected sudo command: " + line, 97)
if words[:4] == SVC:
    rest = words[4:]
    if rest == ["cat", SITE]:
        cat(SITE)
    if rest == [ENGINE, "check-site", "--site", SITE]:
        if os.environ.get("FAKE_CHECK_SITE_FAILS"):
            fail("iem-engine: site: [guard] hil_tx 94 is the TX of mix zyxqw", 2)
        print(json.dumps({"topology": "ab12", "inputs": 32, "groups": 2, "mixes": 11, "card": False}))
        done()
    if rest == [ENGINE, "supervise", "status"]:
        if os.environ.get("FAKE_STATUS_FAILS"):
            fail("iem-engine supervise: cannot connect (ConnectionRefused)")
        print(json.dumps({"status": engine_status()}))
        done()
    if rest == [ENGINE, "supervise", "test-signal", "mic1", "-30", "10"]:
        if not st["hil"]:
            fail("iem-engine supervise: the engine refused it: forbidden")
        st["old_loopback"], st["loopback"] = st["loopback"], (77 if looped() else 0)
        st["phases"], st["signals"] = ["stale", "sound", "sound", "silent"], st["signals"] + 1
        print(json.dumps({"test_signal": {"rev": 3, "outputs": 2, "dbfs": -30.0, "seconds": 10, "listen": False}}))
        done()
    if rest == [ENGINE, "supervise", "inject-fault"]:
        if not st["fault"]:
            fail("iem-engine supervise: the engine refused it: forbidden")
        panic = os.environ.get("FAKE_PANIC", "")
        if panic not in ("stays", "silent"):
            # The engine's own line, with tracing's colours: journald keeps it as bytes.
            esc = chr(27)
            text = (f"{esc}[2m2026-10-28T10:00:00Z{esc}[0m {esc}[31mERROR{esc}[0m iem_engine::control::stop: "
                    "the RT callback faulted: fault injection: panic on the RT thread")
            st["journal"].append({"_PID": str(st["pid"]), "MESSAGE": list(text.encode())})
        if panic != "stays":
            n = 2 if panic == "twice" else 1
            st["nrestarts"] += n
            st["pid"] += n
            st["phases"], st["since"], st["signals"] = [], 0, 0
        if panic == "alarm":
            st["alarm_ts"] = 7_700_000
        print(json.dumps({"inject_fault": {"rev": 3}}))
        done()
    if rest == [ENGINE, "supervise", "stop-test-signal"]:
        st["phases"], st["signals"] = [], 0
        print(json.dumps({"stop_test_signal": {"rev": 4}}))
        done()
    if rest == ["env", f"IEMMIXER_CONFIG={SITE}", "/opt/iemmixer/bin/iem-server", "notify", "--count", "alarm"]:
        if os.environ.get("FAKE_ALARMS_FAIL"):
            fail("iem-server notify: the push store cannot be read")
        count = os.environ.get("FAKE_ALARMS", "1")
        left = os.path.exists(os.path.join(box, "push-left"))
        print(str(int(count) + 1) if left and count.isdigit() else count)
        done()
    if rest == [ENGINE, "supervise", "test-signal", "mic1", "-20", "30", "--listen"]:
        with open(os.path.join(box, "bursts.log"), "a") as f:
            f.write(f"{time.time()}\n")
        if not st["hil"] or os.environ.get("FAKE_BURST_FAILS"):
            fail("iem-engine supervise: the engine refused it: forbidden")
        st["signals"] += 1
        if os.environ.get("FAKE_CRASH_IN_RUN") and st["signals"] == 1:
            st["nrestarts"] += 1   # the engine restarted by itself during the run, after the begin's checks
        print(json.dumps({"test_signal": {"rev": 3, "outputs": 2, "dbfs": -20.0, "seconds": 30, "listen": True}}))
        done()
    if rest[:2] == ["/opt/iemmixer/bin/iem-soakclient", "token"]:
        out = rest[rest.index("--out") + 1]
        name = os.path.basename(out).split(".")[0]
        if os.environ.get("FAKE_MINT_FAILS") == name:
            fail("secret-unreadable")
        try:
            fd = os.open(at(out), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        except FileExistsError:
            fail("cannot create the token file: File exists")
        os.write(fd, (json.loads(os.environ["FAKE_TOKENS"])[name] + "\n").encode())
        os.close(fd)
        sys.stderr.write("token-written\n")
        done()
    if rest[:1] == ["cat"] and len(rest) == 2 and rest[1].startswith("/var/lib/iemmixer/mint/"):
        cat(rest[1])
    if rest[:2] == ["rm", "-f"] and all(p.startswith("/var/lib/iemmixer/mint/") for p in rest[2:]):
        for path in rest[2:]:
            if os.path.exists(at(path)):
                os.remove(at(path))
        done()
    fail("unexpected command as the service user: " + " ".join(rest), 97)
if line in (f"{DANTE} health", f"{DANTE} health || true"):
    # As health.rs judges: any restart of a unit since its start is the
    # reason `restarted` (degraded), beside whatever else is wrong; exit 1
    # unless healthy (the `|| true` form always 0, its line the same).
    keep = 0 if line.endswith("|| true") else 1
    reasons = []
    if os.environ.get("FAKE_UNHEALTHY") or (os.environ.get("FAKE_UNHEALTHY_IN_JOB") and st["hil"]):
        reasons.append("holdover")
    if st["nrestarts"] > 0:
        reasons.append("restarted")
    if reasons:
        print(json.dumps({"health": {"state": "degraded", "reasons": reasons}}))
        done(keep)
    print(json.dumps({"health": {"state": "healthy", "reasons": []}}))
    done()
if line == f"{DANTE} ctl status":
    st["since"] += 1
    lag = st["since"] <= int(os.environ.get("FAKE_RX_LAG", "1"))
    channels = [{"rx": n, "status": 8 if lag else 0x01010009, "written": 1, "holes": 0, "late": 0, "duplicate": 0,
                 "early": 0} for n in (101, 102, 103, 104)]
    if looped():
        channels.append({"rx": 94, "status": 0x01010009, "written": 1, "holes": 0, "late": 0, "duplicate": 0,
                         "early": 0})
    print(json.dumps({"clock": {"ready": 1, "epoch": 0, "polls": 9, "re_anchors": 0}, "tx": [], "rx": [],
                      "channels": channels, "sources": [], "checks": []}))
    done()
if line == f"{ENGINE} --version":
    print(os.environ.get("FAKE_ENGINE_VERSION", "iem-engine 2.0.0-dev.50+" + os.environ["FAKE_SHA"]))
    done()
if line == f"{DANTE} --version":
    print(os.environ.get("FAKE_DEVICE_VERSION", "iem-dante-dev 2.0.0-dev.50 " + os.environ["FAKE_SHA"]))
    done()
if line == "python3 -I -":
    if "/api/version" not in data:
        fail("not the version script", 97)
    print(os.environ.get("FAKE_SERVER_HASH", os.environ["FAKE_SHA"][:12]))
    done()
fail("unexpected command: " + line, 97)
'''

# The box's commands, as the stand-in logs them (after the host).
LEFT = f"test -e {DROPIN} && echo left || echo none"
DROPINS = "systemctl show --property=DropInPaths iem-engine.service"
CAT_UNIT = "cat /etc/systemd/system/iem-engine.service"
CAT_DEVICE = "cat /etc/iemmixer/dante-device.toml"
CAT_SITE = "sudo -n -u iemmixer cat /var/lib/iemmixer/site/iemmixer.toml"
CHECK_SITE = ("sudo -n -u iemmixer /opt/iemmixer/bin/iem-engine check-site --site "
              "/var/lib/iemmixer/site/iemmixer.toml")
HEALTH = "/opt/iemmixer/bin/iem-dante-dev health"
# Health read for its answer also when it exits 1 (degraded): the panic check's.
HEALTH_KEPT = "/opt/iemmixer/bin/iem-dante-dev health || true"
CTL = "/opt/iemmixer/bin/iem-dante-dev ctl status"
STAGE = "mkdir -p .cache/iem-gen3-gate && cat > .cache/iem-gen3-gate/iem-hil.conf"
INSTALL = f"sudo -n install -D -m 0644 .cache/iem-gen3-gate/iem-hil.conf {DROPIN}"
CAT_DROPIN = f"cat {DROPIN}"
RELOAD = "sudo -n systemctl daemon-reload"
RESET = "sudo -n systemctl reset-failed iem-engine"
RESTART = "sudo -n systemctl restart iem-engine"
RM = f"sudo -n rm -f {DROPIN}"
SUPER = "sudo -n -u iemmixer /opt/iemmixer/bin/iem-engine supervise"
STATUS = f"{SUPER} status"
SIGNAL = f"{SUPER} test-signal mic1 -30 10"
STOP = f"{SUPER} stop-test-signal"
VERSIONS = ["/opt/iemmixer/bin/iem-engine --version", "/opt/iemmixer/bin/iem-dante-dev --version", "python3 -I -"]
ALARM = ("sudo -n -u iemmixer env IEMMIXER_CONFIG=/var/lib/iemmixer/site/iemmixer.toml /opt/iemmixer/bin/iem-server "
         "notify --count alarm")
ENABLED = "systemctl is-enabled iem-engine.service || true"
NRESTARTS = "systemctl show --property=NRestarts iem-engine.service"
ENGINE_UNIT = "systemctl show --property=ActiveState,MainPID,NRestarts iem-engine.service"
ALARM_UNIT = "systemctl show --property=InactiveExitTimestampMonotonic iem-alarm@iem-engine.service.service"
INJECT = f"{SUPER} inject-fault"
JOURNAL = "sudo -n journalctl -u iem-engine.service -o json --output-fields=_PID,MESSAGE -n 2000 --no-pager -q"
PREFLIGHT = [LEFT, DROPINS, CAT_UNIT, CAT_DEVICE, CAT_SITE, CHECK_SITE, HEALTH, CTL]
BEGIN = [STAGE, INSTALL, CAT_DROPIN, RELOAD, RESET, RESTART, HEALTH, CTL, CTL, STATUS]
# The panic: the engine's unit and the alarm unit read, the fault, the engine
# back as a new process, healthy, the RX flows back, then both units again and
# the faulted process's line in the engine's journal.
PANIC = [ENGINE_UNIT, ALARM_UNIT, INJECT, ENGINE_UNIT, HEALTH_KEPT, CTL, CTL, ENGINE_UNIT, ALARM_UNIT, JOURNAL]
END = [RM, RELOAD, RESET, RESTART, HEALTH, CTL, CTL, STATUS]
CHANGES = (STAGE, INSTALL, RELOAD, RESET, RESTART, RM, SIGNAL, INJECT)
# An earlier run's injected panic, another engine process's: never this run's.
OLD_PANIC = {"_PID": "999", "MESSAGE": "the RT callback faulted: fault injection: panic on the RT thread"}


def device_config(rx: int = 136, tx: int = 96) -> str:
    """A device config of the box's shape (synthetic names)."""
    return ('name = "zyxqw-gen3"\nsample_rate = 96000\nencoding = 24\n'
            f"rx = {json.dumps([f'RX {n}' for n in range(1, rx + 1)])}\n"
            f"tx = {json.dumps([f'TX {n}' for n in range(1, tx + 1)])}\n")


class GateBase(Base):
    ARGS = ("gen3-gate", "hil")

    def setUp(self) -> None:
        super().setUp()
        text = ip.env_path().read_text(encoding="utf-8")
        ip.env_path().write_text(text + f"GEN3_HOST={GEN3}\nGEN3_DANTE_IP={DANTE_IP}\n"
                                 f"DANTE_IEM_NAME={OURS.upper()}\n", encoding="utf-8")
        self.patch(POLL_S=0.01)
        # A health that never comes fails in a second, not in the box's 90.
        bound = mock.patch.object(gate, "HEALTH_WAIT_S", 1.0)
        bound.start()
        self.addCleanup(bound.stop)
        ip.STATE_DIR.mkdir(parents=True, exist_ok=True)
        (ip.STATE_DIR / "gen3-side.json").write_text(json.dumps({"side": 3, "at": "2026-10-28T09:00:00+00:00"}),
                                                    encoding="utf-8")
        self.box = self.tmp / "gen3"
        for folder in ("root/etc/systemd/system", "root/etc/iemmixer", "root/var/lib/iemmixer/site", "home"):
            (self.box / folder).mkdir(parents=True)
        shutil.copyfile(REPO / "scripts" / "gen3" / "iem-engine.service",
                        self.box / "root/etc/systemd/system/iem-engine.service")
        shutil.copyfile(REPO / "config" / "test-site.toml", self.box / "root/var/lib/iemmixer/site/iemmixer.toml")
        self.write_device(device_config())
        (self.box / "calls.log").write_text("", encoding="utf-8")
        self.set_state()
        d = self.tmp / "bin"
        d.mkdir(exist_ok=True)
        p = d / "ssh"
        p.write_text(f"#!{sys.executable}\n{FAKE_SSH}", encoding="utf-8")
        p.chmod(0o755)
        patcher = mock.patch.dict(os.environ, {"PATH": f"{d}{os.pathsep}{os.environ.get('PATH', '')}",
                                               "FAKE_GEN3": str(self.box), "FAKE_FLAG": str(ip.EVENT_NOW),
                                               "FAKE_SHA": SHA})
        patcher.start()
        self.addCleanup(patcher.stop)
        # Our own device on the Dante network: RX 94 subscribed to nothing.
        self.device = FakeDante(OURS, 136, {})
        self.device.after = self.write_loop
        self.write_loop()
        net = mock.patch.object(gen3_arc, "send", Network(**{DANTE_IP.replace(".", "_"): self.device}).send)
        net.start()
        self.addCleanup(net.stop)
        self.now = NOW
        clock = mock.patch.object(gate, "wall", lambda: self.now)
        clock.start()
        self.addCleanup(clock.stop)

    # The box.

    def set_state(self, **kw) -> None:
        st = {"loaded": None, "hil": False, "fault": False, "starts": 0, "nrestarts": 0, "pid": 4100, "phases": [],
              "signals": 0, "loopback": 0, "old_loopback": 0, "since": 99, "alarm_ts": 0, "journal": [OLD_PANIC]}
        st.update(kw)
        (self.box / "state.json").write_text(json.dumps(st), encoding="utf-8")

    def state(self) -> dict:
        return json.loads((self.box / "state.json").read_text(encoding="utf-8"))

    def write_device(self, text: str) -> None:
        (self.box / "root/etc/iemmixer/dante-device.toml").write_text(text, encoding="utf-8")

    def write_loop(self) -> None:
        row = self.device.rows.get(RET)
        looped = row is not None and row[:2] == (self.device.tx_name(RET), OURS)
        (self.box / "loop.json").write_text(json.dumps({"looped": looped}), encoding="utf-8")

    def left_job(self, hil: bool = True) -> None:
        """A job a dead run left: its drop-in in /run, the engine with the flag (or not yet)."""
        path = self.box / "root" / DROPIN.lstrip("/")
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(gate.dropin_text(UNIT_LINE, gate.PANIC_FLAGS), encoding="utf-8")
        self.set_state(loaded=path.read_text(encoding="utf-8") if hil else None, hil=hil, fault=hil)

    def dropin_there(self) -> bool:
        return (self.box / "root" / DROPIN.lstrip("/")).exists()

    def calls(self) -> list[str]:
        out = []
        for raw in (self.box / "calls.log").read_text(encoding="utf-8").splitlines():
            args = json.loads(raw)
            while args and args[0] == "-o":
                args = args[2:]
            self.assertEqual(args[0], GEN3)
            out.append(" ".join(args[1:]))
        return out

    def again(self) -> None:
        """The logs start afresh (the box's calls, the device's packets, gh's calls)."""
        (self.box / "calls.log").write_text("", encoding="utf-8")
        self.device.packets.clear()
        self.gh.calls.clear()

    def ledger(self) -> list[float]:
        path = ip.STATE_DIR / gate.LEDGER
        return json.loads(path.read_text(encoding="utf-8"))["restarts"] if path.exists() else []

    def subscriptions(self) -> list[bytes]:
        return [p for p in self.device.packets if int.from_bytes(p[6:8], "big") == gen3_arc.OPCODE]

    def posts(self) -> list[list[str]]:
        return self.gh.named("api", "-X", "POST")

    def gate_run(self, **env: str) -> tuple[int, list[dict], str]:
        with mock.patch.dict(os.environ, env):
            return self.run_main(*self.ARGS)

    def refused(self, words: str, **env: str) -> str:
        """Refused with nothing changed: exit 1, no output, no change sent, no post."""
        code, docs, err = self.gate_run(**env)
        self.assertEqual((code, docs), (1, []), err)
        self.assertFalse(set(self.calls()) & set(CHANGES), self.calls())
        self.assertEqual((self.subscriptions(), self.posts()), ([], []))
        self.assertIn(words, err)
        self.assertNotIn("Traceback", err)
        self.assert_private(err)
        return err

    def assert_private(self, text: str) -> None:
        """P6: no host, address, device or channel name, spare output's number or build in what is printed."""
        for value in (GEN3, DANTE_IP, OURS, self.device.tx_name(RET), "94", "95", SHA):
            self.assertNotIn(value, text)


class GateTests(GateBase):
    def test_the_gate_passes_with_the_exact_calls_and_posts_its_status(self) -> None:
        code, docs, err = self.gate_run()
        self.assertEqual(code, 0, err)
        self.assertEqual(docs, [{"gen3_gate": {"hil": {
            "pass": 1, "health": 1, "rx_back": 1, "rx_channels": 4, "hil_on": 1, "versions": 1, "outputs": 2,
            "dbfs": -30, "seconds": 10, "lowest_dbfs": -30.0, "highest_dbfs": -30.0, "still": 0, "loop": "set",
            "loopback_samples": TRIP, "loop_restored": 1, "alarm_devices": 1, "alarm_armed": 1, "unit_restarts": 0,
            "panic": 1, "panic_restarts": 1, "starts": 3, "left_job": 0, "job_end": 1, "hil_off": 1,
            "failed": []}}}])
        self.assertEqual(self.calls(), [*PREFLIGHT, *BEGIN, *VERSIONS, CTL, SIGNAL, *[STATUS] * 4, ALARM, ENABLED,
                                        NRESTARTS, *PANIC, *END])
        # The status on the box's build: success, a fixed description.
        self.assertEqual(self.posts(), [["api", "-X", "POST", f"repos/{ip.REPO}/statuses/{SHA}", "-f",
                                         "state=success", "-f", "context=hil/gen3", "-f",
                                         "description=the Gen 3 box's HIL passed (iempc gen3-gate hil)"]])
        # The job's drop-in: ExecStart reset, then the unit's own line and the test flags (the burst's and the
        # panic's); gone again.
        staged = (self.box / "home/.cache/iem-gen3-gate/iem-hil.conf").read_text(encoding="utf-8")
        self.assertEqual(staged.split("\n")[2:], ["[Service]", "ExecStart=",
                                                  f"ExecStart={UNIT_LINE} --test-signal --fault-injection", ""])
        self.assertFalse(self.dropin_there())
        st = self.state()
        # Two restarts asked for (the panic's was systemd's own), the engine without its flags again.
        self.assertEqual((st["loaded"], st["hil"], st["fault"], st["starts"]), (None, False, False, 2))
        # Our RX 94 heard our TX 94 for the run, and is unsubscribed again.
        self.assertEqual(len(self.subscriptions()), 2)
        self.assertEqual(self.device.rows, {})
        # The budget: the job's begin, the panic's restart and the job's end.
        self.assertEqual(self.ledger(), [NOW] * 3)
        self.assert_private(err + json.dumps(docs))
        self.assertEqual(self.pc.calls, [])   # no PC call

    def test_a_loop_already_there_is_kept_and_another_subscription_is_put_back(self) -> None:
        # While 3.x serves, the mirror may already loop our RX 94 to our own TX 94: kept, nothing sent.
        self.device.rows = {RET: (self.device.tx_name(RET), OURS, 9)}
        self.write_loop()
        code, docs, err = self.gate_run()
        self.assertEqual(code, 0, err)
        self.assertEqual((docs[0]["gen3_gate"]["hil"]["loop"], self.subscriptions()), ("kept", []))
        # Subscribed to another device: changed for the run, then exactly that again.
        self.again()
        self.now += 601   # the next run's budget
        self.device.rows = {RET: ("zyxqw spare", "zyxqw-card", 9)}
        self.write_loop()
        code, docs, err = self.gate_run()
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[0]["gen3_gate"]["hil"]["loop"], "set")
        self.assertEqual(self.device.rows, {RET: ("zyxqw spare", "zyxqw-card", 9)})
        self.assertEqual(len(self.subscriptions()), 2)

    def test_failures_are_named_by_fixed_words_and_posted_as_a_failure(self) -> None:
        code, docs, err = self.gate_run(FAKE_PEAK="0.02", FAKE_ALARMS="0", FAKE_DISABLED="1")
        self.assertEqual(code, 1, err)
        doc = docs[0]["gen3_gate"]["hil"]
        self.assertEqual((doc["pass"], doc["failed"]), (0, ["level", "alarm_count", "alarm_not_armed"]))
        self.assertEqual((doc["lowest_dbfs"], doc["alarm_devices"], doc["alarm_armed"]), (-33.98, 0, 0))
        self.assertEqual(self.posts()[0][4:], ["-f", "state=failure", "-f", "context=hil/gen3", "-f",
                                               "description=the Gen 3 box's HIL failed: level, alarm_count, "
                                               "alarm_not_armed"])
        self.assertFalse(self.dropin_there())   # the job ended all the same
        self.assertTrue(set(doc["failed"]) <= set(gate.FAILURES))

    def test_a_burst_that_never_ends_is_stopped_and_fails(self) -> None:
        with mock.patch.object(gate, "BURST_LIMIT_S", 0.2):
            code, docs, err = self.gate_run(FAKE_NEVER_SILENT="1")
        self.assertEqual(code, 1, err)
        doc = docs[0]["gen3_gate"]["hil"]
        self.assertEqual(doc["failed"], ["not_silent", "no_loopback"])
        calls = self.calls()
        self.assertIn(STOP, calls)
        self.assertLess(calls.index(STOP), calls.index(RM))

    def test_an_engine_that_restarts_by_itself_in_the_job_fails_injects_no_fault_and_counts_in_the_budget(
            self) -> None:
        # The review of PR #87: the begin, a crash and the panic's restart would be three starts in the unit's
        # window (systemd's counter reset at the begin), so one more crash before the end's reset-failed would
        # leave the engine down while 3.x serves. No fault then: `panic` reads skipped, the gate fails all the same.
        code, docs, err = self.gate_run(FAKE_CRASHES="1")
        self.assertEqual(code, 1, err)
        doc = docs[0]["gen3_gate"]["hil"]
        # The box's health names the restart too (degraded: restarted).
        self.assertEqual((doc["unit_restarts"], doc["failed"], doc["panic"], doc["panic_restarts"], doc["starts"]),
                         (1, ["health", "restarted"], "skipped", None, 2))
        self.assertNotIn(INJECT, self.calls())
        self.assertIn("the panic check was skipped", err)
        self.assertEqual(self.ledger(), [NOW] * 3)   # the begin, the crash, the end

    def test_a_box_whose_engine_names_no_ci_build_fails_and_posts_nothing(self) -> None:
        code, docs, err = self.gate_run(FAKE_ENGINE_VERSION="iem-engine 2.0.0-dev.50+local")
        self.assertEqual(code, 1, err)
        self.assertEqual(docs[0]["gen3_gate"]["hil"]["failed"], ["versions"])
        self.assertEqual(self.posts(), [])
        self.assertIn("the box's engine names no CI build: no hil/gen3 status was posted", err)

    def test_builds_that_differ_fail_versions_on_the_engine_s_build(self) -> None:
        for env in ({"FAKE_DEVICE_VERSION": "iem-dante-dev 2.0.0-dev.50 " + "f" * 40},
                    {"FAKE_SERVER_HASH": "fffffff"}, {"FAKE_SERVER_HASH": ""}):
            with self.subTest(env=env):
                self.again()
                code, docs, err = self.gate_run(**env)
                self.assertEqual(code, 1, err)
                self.assertEqual(docs[-1]["gen3_gate"]["hil"]["failed"], ["versions"])
                self.assertEqual(self.posts()[0][3], f"repos/{ip.REPO}/statuses/{SHA}")
                self.now += 601   # the next run's budget

    def test_a_refused_status_post_fails_the_command_after_its_verdict(self) -> None:
        def gh(args, timeout=ip.GH_S):
            raise ip.StepError("gh api failed (exit 1): HTTP 403")
        self.patch(gh=gh)
        code, docs, err = self.gate_run()
        self.assertEqual((code, docs[0]["gen3_gate"]["hil"]["pass"]), (1, 1))
        self.assertIn("WARNING: gen3-gate: the status hil/gen3 was not posted", err)


class JobTests(GateBase):
    def test_a_left_job_is_ended_first_and_the_run_waits_out_its_restart(self) -> None:
        self.left_job()
        code, docs, err = self.gate_run()
        self.assertEqual((code, docs), (1, []), err)
        # The engine read with its spare outputs: removed, reloaded, started again. That start and the run's
        # three would pass the unit's 3 in 600 s: no job begins.
        self.assertEqual(self.calls(), [*PREFLIGHT, STATUS, RM, RELOAD, RESET, RESTART, HEALTH, CTL, CTL, STATUS])
        self.assertIn("a HIL job a dead run left is ended first", err)
        self.assertIn("no HIL gate: the engine's restart budget: 1 engine starts in the last 600 s and 3 more would "
                      "pass the unit's 3: run it again in 600 s (the job's begin, the panic's restart and the job's "
                      "end: nothing was changed; the HIL job a dead run left was ended)", err)
        self.assertEqual((self.state()["starts"], self.ledger()), (1, [NOW]))
        self.assertFalse(self.dropin_there())
        self.assertEqual(self.posts(), [])
        # Once that start left the unit's window the run goes on, no job left.
        self.again()
        self.now = NOW + 600.5
        code, docs, err = self.gate_run()
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[0]["gen3_gate"]["hil"]["left_job"], 0)
        self.assertEqual(self.ledger(), [NOW] + [NOW + 600.5] * 3)

    def test_a_left_job_whose_engine_never_got_the_flag_costs_no_restart(self) -> None:
        self.left_job(hil=False)
        code, docs, err = self.gate_run()
        self.assertEqual(code, 0, err)
        self.assertEqual(self.calls()[len(PREFLIGHT):len(PREFLIGHT) + 6], [STATUS, RM, RELOAD, HEALTH, CTL, STAGE])
        self.assertEqual(self.ledger(), [NOW] * 3)

    def test_the_restart_budget_refuses_a_plan_that_would_pass_the_unit_s_limit(self) -> None:
        # The unit allows 3 starts in 600 s; a run needs 3 (its begin, the panic's restart and its end).
        code, _, err = self.gate_run()
        self.assertEqual(code, 0, err)
        self.again()
        self.now = NOW + 300
        err = self.refused("no HIL gate: the engine's restart budget: 3 engine starts in the last 600 s and 3 more "
                           "would pass the unit's 3: run it again in 300 s (the job's begin, the panic's restart and "
                           "the job's end: nothing was changed)")
        self.again()
        self.now = NOW + 600.5   # all three left the unit's window
        code, _, err = self.gate_run()
        self.assertEqual(code, 0, err)

    def test_the_budget_plans_three_starts(self) -> None:
        # One start 100 s ago: a live job's two more would fit, the HIL gate's three do not.
        (ip.STATE_DIR / gate.LEDGER).write_text(json.dumps({"restarts": [NOW - 100]}), encoding="utf-8")
        self.refused("no HIL gate: the engine's restart budget: 1 engine starts in the last 600 s and 3 more would "
                     "pass the unit's 3: run it again in 500 s (the job's begin, the panic's restart and the job's "
                     "end: nothing was changed)")

    def test_a_left_job_beyond_the_budget_is_refused_before_anything_changes(self) -> None:
        (ip.STATE_DIR / gate.LEDGER).write_text(json.dumps({"restarts": [NOW - 100] * 3}), encoding="utf-8")
        self.left_job()
        self.refused("restart budget: 3 engine starts in the last 600 s and 1 more would pass the unit's 3: run it "
                     "again in 500 s (a HIL job a dead run left is still there: nothing was changed)")
        self.assertTrue(self.dropin_there())

    def test_an_unreadable_ledger_refuses(self) -> None:
        (ip.STATE_DIR / gate.LEDGER).write_text("[1, 2]", encoding="utf-8")
        self.refused("gen3-gate.json in iempc's state cannot be read")

    def test_the_flag_mid_burst_stops_it_ends_the_job_and_runs_the_event_path(self) -> None:
        code, docs, err = self.gate_run(FAKE_FLAG_ON="burst")
        self.assertEqual(code, ip.PREEMPTED, err)
        self.assertEqual(docs[0], {"event": "ide event (flag file)", "action": "iempc event"})
        self.assertNotIn("gen3_gate", json.dumps(docs))
        calls = self.calls()
        after = calls[calls.index(SIGNAL):]
        # The burst stopped, the loop put back, the job ended without its waits; nothing more.
        self.assertEqual([c for c in after if c != STATUS], [SIGNAL, STOP, RM, RELOAD, RESET, RESTART])
        self.assertEqual(self.device.rows, {})
        self.assertFalse(self.dropin_there())
        self.assertFalse(self.state()["hil"])
        self.assertEqual(self.posts(), [])
        self.assertIn(["event", "--signal"], [args for _, args, _ in self.pc.calls])

    def test_a_flag_before_the_begin_changes_nothing_on_the_box(self) -> None:
        original = gate.preflight

        def then_flag(box, dante):
            facts = original(box, dante)
            self.flag()
            return facts
        with mock.patch.object(gate, "preflight", then_flag):
            code, docs, err = self.gate_run()
        self.assertEqual(code, ip.PREEMPTED, err)
        self.assertEqual(self.calls(), PREFLIGHT)

    def test_a_failure_inside_the_job_ends_it_and_fails_the_command(self) -> None:
        def cut(job, c):
            raise ip.StepError("ssh failed (exit 255): Connection reset")
        with mock.patch.object(gate, "read_versions", cut):
            code, docs, err = self.gate_run()
        self.assertEqual((code, docs), (1, []), err)
        self.assertIn("ssh failed (exit 255): Connection reset", err)
        self.assertFalse(self.dropin_there())
        self.assertEqual((self.state()["hil"], self.state()["starts"]), (False, 2))
        self.assertEqual(self.posts(), [])

    def test_an_engine_still_in_hil_mode_after_the_end_fails_the_job_end(self) -> None:
        code, docs, err = self.gate_run(FAKE_RELOAD_KEEPS="1")
        self.assertEqual(code, 1, err)
        doc = docs[0]["gen3_gate"]["hil"]
        self.assertEqual((doc["job_end"], doc["hil_off"], doc["failed"]), (1, 0, ["job_end"]))

    def test_an_end_that_fails_says_how_to_end_it_by_hand_and_fails_the_verdict(self) -> None:
        code, docs, err = self.gate_run(FAKE_RM_FAILS="1")
        self.assertEqual(code, 1, err)
        self.assertIn("job_end", docs[0]["gen3_gate"]["hil"]["failed"])
        self.assertIn("the engine may still run with its HIL test flag; end it by hand: ssh <GEN3_HOST> sudo -n rm "
                      f"-f {DROPIN}", err)


class ReviewTests(GateBase):
    """The review of PR #87."""

    def test_no_fault_is_injected_into_an_engine_not_healthy_or_without_its_rx_flows(self) -> None:
        # The panic's restart on top of an engine already in trouble: never; the gate fails on that word.
        cases = (({"FAKE_UNHEALTHY_IN_JOB": "1"}, {}, ["health"]),
                 ({"FAKE_RX_LAG": "100000"}, {"since": 1_000_000}, ["rx_not_back", "job_end"]))
        for env, state, failed in cases:
            with self.subTest(failed=failed), mock.patch.object(gate, "RX_WAIT_S", 0.3):
                self.again()
                self.now += 601   # the next run's budget
                self.set_state(**state)
                code, docs, err = self.gate_run(**env)
                self.assertEqual(code, 1, err)
                doc = docs[0]["gen3_gate"]["hil"]
                self.assertEqual((doc["failed"], doc["panic"], doc["unit_restarts"]), (failed, "skipped", 0))
                self.assertNotIn(INJECT, self.calls())
                self.assertFalse(self.dropin_there())

    def test_a_left_job_whose_engine_status_cannot_be_read_is_refused_with_nothing_changed(self) -> None:
        # An unreadable status is no "not in HIL": the drop-in removed without a restart would leave the engine
        # with its test flags and take the evidence away. The drop-in stays for the next gate or deploy.
        self.left_job()
        err = self.refused("no HIL gate: a HIL job a dead run left is still there, and the engine's status could not "
                           "be read", FAKE_STATUS_FAILS="1")
        self.assertIn("supervise status", err)
        self.assertEqual(self.calls(), [*PREFLIGHT, STATUS])
        self.assertTrue(self.dropin_there())
        self.assertEqual((self.state()["starts"], self.ledger()), (0, []))

    def test_a_device_at_gen3_dante_ip_that_is_not_ours_gets_no_subscription(self) -> None:
        # A wrong GEN3_DANTE_IP, or an address that moved, would have the D5(b) loop change another Dante
        # device's RX subscription. Our device answers with DANTE_IEM_NAME while 3.x serves (any case).
        text = ip.env_path().read_text(encoding="utf-8")
        ip.env_path().write_text(text + "DANTE_IEM_NAME=Zyxqw-Card\n", encoding="utf-8")
        err = self.refused("no HIL gate: the device answering at GEN3_DANTE_IP is not our own")
        self.assertNotIn("Zyxqw-Card", err)
        self.assertEqual(self.device.rows, {})
        self.assertEqual(self.calls(), PREFLIGHT)

    def test_an_unreachable_box_is_named_by_fixed_words_never_by_its_address(self) -> None:
        # ssh's own failure (exit 255) names the host in its stderr: never quoted (P6).
        err = self.refused("no HIL gate: the HIL job's drop-in could not be read (ssh to GEN3_HOST failed (exit 255))",
                           FAKE_SSH_DOWN="*")
        self.assertNotIn("gen3.test", err)
        self.assertNotIn("connect to host", err)

    def test_a_box_call_that_fails_mid_run_warns_in_fixed_words(self) -> None:
        code, docs, err = self.gate_run(FAKE_SSH_DOWN=ALARM)
        self.assertEqual(code, 1, err)
        self.assertEqual(docs[0]["gen3_gate"]["hil"]["failed"], ["alarm_count"])
        self.assertIn("WARNING: gen3-gate: the alarm path's count could not be read (ssh to GEN3_HOST failed (exit "
                      "255))", err)
        self.assertNotIn("gen3.test", err)
        self.assert_private(err + json.dumps(docs))
        # The box's command's own failure (any other exit) is still quoted: it names no address.
        self.again()
        self.now += 601
        code, docs, err = self.gate_run(FAKE_ALARMS_FAIL="1")
        self.assertEqual(code, 1, err)
        self.assertIn("the alarm path's count could not be read (ssh failed (exit 1): iem-server notify: the push "
                      "store cannot be read)", err)


class PanicTests(GateBase):
    """The panic check (Task 6 Step 3): the engine's own injected fault, systemd's own restart."""

    def panic_failed(self, **env: str) -> dict:
        """A run whose panic check fails: exit 1, `panic` its one failure, posted as a failure, the job ended."""
        code, docs, err = self.gate_run(**env)
        self.assertEqual(code, 1, err)
        doc = docs[0]["gen3_gate"]["hil"]
        self.assertEqual((doc["pass"], doc["panic"], doc["failed"]), (0, 0, ["panic"]))
        self.assertEqual(self.posts()[0][4:], ["-f", "state=failure", "-f", "context=hil/gen3", "-f",
                                               "description=the Gen 3 box's HIL failed: panic"])
        self.assertEqual(self.calls()[-len(END):], END)
        self.assertFalse(self.dropin_there())
        self.assertIn("WARNING: gen3-gate: the panic check failed: ", err)
        self.assert_private(err + json.dumps(docs))
        return doc

    def test_the_panic_is_the_engine_s_own_fault_and_systemd_s_one_restart(self) -> None:
        code, docs, err = self.gate_run()
        self.assertEqual(code, 0, err)
        doc = docs[0]["gen3_gate"]["hil"]
        self.assertEqual((doc["panic"], doc["panic_restarts"], doc["unit_restarts"]), (1, 1, 0))
        calls = self.calls()
        # After every other check, before the job's end; the flag looked at right before the fault.
        self.assertEqual(calls[calls.index(NRESTARTS) + 1:], [*PANIC, *END])
        # The faulted process wrote it (the earlier run's line under another pid never counts).
        st = self.state()
        self.assertEqual(st["journal"][-1]["_PID"], "4101")
        self.assertEqual(st["nrestarts"], 0)   # the end's reset-failed

    def test_an_unread_restart_count_stops_the_fault_and_fails_the_gate(self) -> None:
        # Without the engine's restart count the job's state is not known:
        # no fault on top of an unknown, and the gate fails on it.
        code, docs, err = self.gate_run(FAKE_NRESTARTS_FAILS="1")
        self.assertEqual(code, 1, err)
        doc = docs[0]["gen3_gate"]["hil"]
        self.assertEqual((doc["panic"], doc["failed"]), ("skipped", ["restarts_unread"]))
        self.assertNotIn(INJECT, self.calls())
        self.assertEqual(self.posts()[0][4:], ["-f", "state=failure", "-f", "context=hil/gen3", "-f",
                                               "description=the Gen 3 box's HIL failed: restarts_unread"])

    def test_no_restart_within_the_bound_fails_the_panic(self) -> None:
        with mock.patch.object(gate, "PANIC_WAIT_S", 0.2):
            doc = self.panic_failed(FAKE_PANIC="stays")
        self.assertEqual(doc["panic_restarts"], 0)
        calls = self.calls()
        after = calls[calls.index(INJECT) + 1:calls.index(RM)]
        # Polled until the bound, no health or RX wait for an engine that never went, then the reads.
        self.assertGreater(after.count(ENGINE_UNIT), 2)
        self.assertNotIn(HEALTH, after)
        self.assertEqual(after[-3:], [ENGINE_UNIT, ALARM_UNIT, JOURNAL])
        # The fault was sent: its restart counted, answered or not.
        self.assertEqual(self.ledger(), [NOW] * 3)

    def test_two_restarts_fail_the_panic_and_count_in_the_budget(self) -> None:
        doc = self.panic_failed(FAKE_PANIC="twice")
        self.assertEqual(doc["panic_restarts"], 2)
        self.assertEqual(self.ledger(), [NOW] * 4)

    def test_an_alarm_unit_that_started_fails_the_panic(self) -> None:
        doc = self.panic_failed(FAKE_PANIC="alarm")
        self.assertEqual(doc["panic_restarts"], 1)

    def test_no_panic_of_the_faulted_process_in_the_journal_fails_it(self) -> None:
        # The engine restarted once, but the journal holds only an earlier run's panic (another process).
        doc = self.panic_failed(FAKE_PANIC="silent")
        self.assertEqual(doc["panic_restarts"], 1)
        self.assertEqual(self.state()["journal"], [OLD_PANIC])

    def test_an_engine_without_the_fault_injection_flag_refuses_the_fault(self) -> None:
        with mock.patch.object(gate, "PANIC_FLAGS", gate.TEST_FLAGS):
            doc = self.panic_failed()
        self.assertIsNone(doc["panic_restarts"])
        calls = self.calls()
        self.assertEqual(calls[calls.index(INJECT) + 1], RM)   # nothing waited for or read after the refusal

    def test_the_flag_before_the_fault_injects_nothing_and_ends_the_job(self) -> None:
        code, docs, err = self.gate_run(FAKE_FLAG_ON="fault")
        self.assertEqual(code, ip.PREEMPTED, err)
        self.assertNotIn("gen3_gate", json.dumps(docs))
        calls = self.calls()
        self.assertNotIn(INJECT, calls)
        # The job ended without its waits; nothing more.
        self.assertEqual(calls[calls.index(ALARM_UNIT) + 1:], [RM, RELOAD, RESET, RESTART])
        self.assertFalse(self.dropin_there())
        self.assertFalse(self.state()["fault"])
        self.assertEqual(self.posts(), [])
        self.assertEqual(self.ledger(), [NOW] * 2)   # the begin and the end, no fault
        self.assertIn(["event", "--signal"], [args for _, args, _ in self.pc.calls])


class PreflightTests(GateBase):
    def test_hil_tx_beyond_the_device_s_tx_or_rx_count_is_refused_before_any_restart(self) -> None:
        self.write_device(device_config(tx=94))
        self.refused("the site's [guard] hil_tx names a spare output the box's device has no TX channel for (an "
                     "engine started with --test-signal would exit 3 and stay down)")
        self.again()
        self.write_device(device_config(rx=94))
        self.refused("whose D5(b) return the box's device has no RX channel for")
        self.assertEqual(self.state()["starts"], 0)

    def test_every_other_preflight_refusal_changes_nothing(self) -> None:
        unit = self.box / "root/etc/systemd/system/iem-engine.service"
        text = unit.read_text(encoding="utf-8")
        cases = (
            ({"FAKE_DROPINS": "/etc/systemd/system/iem-engine.service.d/zz.conf"}, None,
             "another drop-in changes the engine's unit"),
            ({"FAKE_CHECK_SITE_FAILS": "1"}, None, "iem-engine check-site refuses the installed site"),
            ({"FAKE_UNHEALTHY": "1"}, None, "the box is not healthy"),
            ({}, text.replace("--dante-iface ${IEM_DANTE_IFACE}", "--dante-iface ${IEM_DANTE_IFACE} --test-signal"),
             "the installed engine unit's ExecStart already holds a HIL flag"),
            ({}, text.replace("StartLimitBurst=3\n", ""), "names no StartLimitBurst and StartLimitIntervalSec"),
            ({}, text.replace("ExecStart=/opt/iemmixer/bin/iem-engine", "ExecStart=-/opt/iemmixer/bin/iem-engine"),
             "the installed engine unit's ExecStart is not one line"))
        for env, unit_text, words in cases:
            with self.subTest(words=words):
                self.again()
                unit.write_text(unit_text or text, encoding="utf-8")
                self.refused(words, **env)
        unit.write_text(text, encoding="utf-8")
        self.again()
        site = self.box / "root/var/lib/iemmixer/site/iemmixer.toml"
        site.write_text(site.read_text(encoding="utf-8").replace("hil_tx = [94, 95]", "hil_tx = []"),
                        encoding="utf-8")
        self.refused("the installed site names no [guard] hil_tx spare outputs")

    def test_a_device_that_does_not_answer_its_arc_reads_refuses(self) -> None:
        with mock.patch.object(gen3_arc, "send", Network().send):
            self.refused("our device did not answer its ARC reads (TimeoutError)")

    def test_missing_keys_refuse_before_any_call(self) -> None:
        text = ip.env_path().read_text(encoding="utf-8")
        for drop, words in ((f"GEN3_DANTE_IP={DANTE_IP}\n", "no GEN3_DANTE_IP in the private env"),
                            (f"DANTE_IEM_NAME={OURS.upper()}\n", "no DANTE_IEM_NAME in the private env"),
                            (f"GEN3_HOST={GEN3}\n", "no GEN3_HOST in the private env")):
            with self.subTest(words=words):
                ip.env_path().write_text(text.replace(drop, ""), encoding="utf-8")
                code, docs, err = self.gate_run()
                self.assertEqual((code, docs, self.calls()), (1, [], []), err)
                self.assertIn(words, err)

    def test_the_command_is_dev_time_locked_a_box_command_and_runs_only_while_3x_serves(self) -> None:
        spec = ip.COMMANDS["gen3-gate"]
        self.assertEqual((spec.pc, spec.dev_time, spec.locked), (False, True, True))
        self.assertIn("gen3-gate", ip.WHILE_3X_SERVES)
        self.assertNotIn("gen3-gate", ip.WHILE_2X_SERVES)
        side = ip.STATE_DIR / "gen3-side.json"
        for record in (None, {"side": 2}):
            with self.subTest(record=record):
                if record is None:
                    side.unlink()
                else:
                    side.write_text(json.dumps(record), encoding="utf-8")
                code, docs, err = self.gate_run()
                self.assertEqual((code, docs, self.calls()), (1, [], []), err)
                self.assertIn("2.x serves the band (gen3-side.json): 'gen3-gate' runs only while 3.x serves", err)
        side.write_text(json.dumps({"side": 3}), encoding="utf-8")
        self.flag()
        code, docs, err = self.gate_run()
        self.assertEqual((code, docs, self.calls()), (1, [], []), err)
        self.assertIn("'gen3-gate' runs only in dev time", err)


class SudoTests(GateBase):
    def test_every_sudo_line_the_gate_sends_is_in_the_template(self) -> None:
        # A whole run, a run that ends a left job first, and one the flag ends mid-burst.
        sent: list[str] = []
        code, _, err = self.gate_run()
        self.assertEqual(code, 0, err)
        sent += self.calls()
        self.again()
        self.now += 1200
        self.left_job()
        code, _, err = self.gate_run()
        self.assertEqual(code, 1, err)   # the left job ended, then the budget's wait
        self.assertIn(RESTART, self.calls())
        sent += self.calls()
        self.again()
        self.now += 1200
        code, _, err = self.gate_run(FAKE_FLAG_ON="burst")
        self.assertEqual(code, ip.PREEMPTED, err)
        sent += self.calls()
        allowed = {(runas, text) for runas, command in template_lines() for text in expand(command)}
        lines = sudo_lines(sent)
        for runas, command, listed in lines:
            self.assertIn((runas, command), allowed, command)
            self.assertFalse(listed, command)
        seen = {(runas, command) for runas, command, _listed in lines}
        for want in (INSTALL, RM, RELOAD, RESET, RESTART):
            self.assertIn(("root", want[len("sudo -n "):]), seen)
        for want in (CAT_SITE, CHECK_SITE, STATUS, SIGNAL, STOP, ALARM, INJECT):
            self.assertIn(("iemmixer", want[len("sudo -n -u iemmixer "):]), seen)
        self.assertIn(("root", JOURNAL[len("sudo -n "):]), seen)


class PureTests(unittest.TestCase):
    UNIT = (REPO / "scripts" / "gen3" / "iem-engine.service").read_text(encoding="utf-8")

    def test_the_unit_s_exec_start_and_start_limit_are_read(self) -> None:
        self.assertEqual(gate.exec_start(self.UNIT), UNIT_LINE)
        self.assertEqual(gate.start_limit(self.UNIT), (3, 600))
        two = self.UNIT.replace("Restart=on-failure", f"ExecStart={UNIT_LINE}\nRestart=on-failure")
        cont = self.UNIT.replace(UNIT_LINE, UNIT_LINE[:30] + " \\\n  " + UNIT_LINE[30:])
        for bad in (two, cont, self.UNIT.replace(UNIT_LINE, UNIT_LINE + " --fault-injection")):
            with self.assertRaises(ValueError):
                gate.exec_start(bad)
        for bad in (self.UNIT.replace("StartLimitIntervalSec=600", "StartLimitIntervalSec=10min"),
                    self.UNIT.replace("StartLimitBurst=3", "StartLimitBurst=0")):
            with self.assertRaises(ValueError):
                gate.start_limit(bad)

    def test_the_drop_in_resets_exec_start_then_adds_the_job_s_flags(self) -> None:
        # A job's default, the live gate's: the test signal only.
        text = gate.dropin_text(UNIT_LINE)
        lines = [line for line in text.splitlines() if not line.startswith("#")]
        self.assertEqual(lines, ["[Service]", "ExecStart=", f"ExecStart={UNIT_LINE} --test-signal"])
        self.assertNotIn("--fault-injection", text)
        self.assertEqual(gate.Job(None, None).flags, gate.TEST_FLAGS)
        # The HIL gate's: the fault injection beside it, for the panic check.
        text = gate.dropin_text(UNIT_LINE, gate.PANIC_FLAGS)
        lines = [line for line in text.splitlines() if not line.startswith("#")]
        self.assertEqual(lines, ["[Service]", "ExecStart=", f"ExecStart={UNIT_LINE} --test-signal --fault-injection"])
        self.assertTrue(set(gate.PANIC_FLAGS) <= set(gate.FLAG_WORDS))

    def test_the_engine_s_unit_is_read_whatever_systemd_s_order(self) -> None:
        self.assertEqual(gate.engine_unit("NRestarts=2\nActiveState=active\nMainPID=4321\n"),
                         gate.EngineUnit(active=True, pid=4321, restarts=2))
        self.assertEqual(gate.engine_unit("ActiveState=activating\nMainPID=0\nNRestarts=1"),
                         gate.EngineUnit(active=False, pid=0, restarts=1))
        for bad in ("", "MainPID=4321\nActiveState=active", "MainPID=x\nNRestarts=1\nActiveState=active",
                    "MainPID=4321\nNRestarts=-1\nActiveState=active"):
            self.assertIsNone(gate.engine_unit(bad), bad)

    def test_the_engine_is_back_only_as_a_new_active_process_after_a_restart(self) -> None:
        before = gate.EngineUnit(active=True, pid=4100, restarts=0)
        self.assertTrue(gate.back(gate.EngineUnit(active=True, pid=4101, restarts=1), before))
        for now in (None, gate.EngineUnit(active=False, pid=4101, restarts=1),   # still coming up
                    gate.EngineUnit(active=True, pid=4100, restarts=1),           # the same process
                    gate.EngineUnit(active=True, pid=4101, restarts=0)):          # no restart by systemd
            self.assertFalse(gate.back(now, before), now)

    def test_the_injected_panic_is_the_faulted_process_s_own_line(self) -> None:
        esc = chr(27)
        coloured = f"{esc}[31mERROR{esc}[0m the RT callback faulted: {gate.PANIC_TEXT}"
        hook = f"thread 'dante-engine' panicked at crates/iem-engine/src/rt.rs:577:9:\n{gate.PANIC_TEXT}"
        lines = "\n".join([
            "not json", json.dumps([1, 2]), json.dumps({"_PID": "4100", "MESSAGE": None}),
            json.dumps({"_PID": "999", "MESSAGE": gate.PANIC_TEXT}),             # another process's
            json.dumps({"_PID": "4100", "MESSAGE": "the RT callback faulted: unknown"}),
            json.dumps({"_PID": "4100", "MESSAGE": [300, 1]}),                    # no bytes
        ])
        self.assertFalse(gate.panic_logged(lines, 4100))
        # journald's bytes (a line with a control character) and its plain text both count.
        for message in (list(coloured.encode()), hook, gate.PANIC_TEXT):
            entry = json.dumps({"__CURSOR": "s=zyxqw", "_PID": "4100", "MESSAGE": message})
            self.assertTrue(gate.panic_logged(lines + "\n" + entry, 4100), message)
            self.assertFalse(gate.panic_logged(lines + "\n" + entry, 4101), message)
        # The engine's panic message, rt.rs's inject_fault.
        rt = (REPO / "crates" / "iem-engine" / "src" / "rt.rs").read_text(encoding="utf-8")
        self.assertIn(f'panic!("{gate.PANIC_TEXT}")', rt)

    def test_the_budget_counts_the_unit_s_window_only(self) -> None:
        self.assertIsNone(gate.budget_refusal([], NOW, 600, 3, 2))
        self.assertIsNone(gate.budget_refusal([NOW - 600, NOW - 700, NOW - 10], NOW, 600, 3, 2))
        self.assertEqual(gate.budget_refusal([NOW - 100, NOW - 50], NOW, 600, 3, 2),
                         "2 engine starts in the last 600 s and 2 more would pass the unit's 3: run it again in "
                         "500 s")
        self.assertIn("run it again in 550 s", gate.budget_refusal([NOW - 100, NOW - 50, NOW - 10], NOW, 600, 3, 2))
        self.assertEqual(gate.budget_refusal([], NOW, 600, 1, 2), "it needs 2 engine starts, more than the unit's 1 "
                                                                   "in 600 s")

    def test_the_counts_cover_the_spare_outputs_and_their_returns(self) -> None:
        self.assertIsNone(gate.counts_refusal([94, 95], 95, 95))
        self.assertIn("no TX channel", gate.counts_refusal([94, 95], 136, 94))
        self.assertIn("no RX channel", gate.counts_refusal([94, 95], 94, 96))
        self.assertEqual(gate.device_counts(device_config(3, 2)), (3, 2))
        with self.assertRaises(ValueError):
            gate.device_counts('name = "x"\n')
        self.assertEqual(gate.site_hil_tx("[guard]\nhil_tx = [94, 95]\n"), [94, 95])
        for bad in ("", "[guard]\nhil_tx = []\n", "[guard]\nhil_tx = [0]\n", "[guard]\nhil_tx = ['94']\n"):
            with self.assertRaises((ValueError, tomllib.TOMLDecodeError)):
                gate.site_hil_tx(bad)

    def test_the_signal_is_judged_as_hil_v1_judges_it(self) -> None:
        b = gate.Burst(during=[{94: PEAK, 95: 0.0}, {94: 0.001, 95: PEAK}], after={94: 0.0, 95: 0.0}, loopback=5)
        got = gate.signal_judged([94, 95], b)
        self.assertEqual((got["heard"], got["lowest_dbfs"], got["highest_dbfs"], got["level_ok"], got["still"]),
                         (1, -30.0, -30.0, True, 0))
        low = gate.signal_judged([94, 95], gate.Burst(during=[{94: PEAK, 95: PEAK * 0.94}], after={94: 0.0, 95: 0.1}))
        self.assertEqual((low["level_ok"], low["lowest_dbfs"], low["still"]), (False, -30.54, 1))
        none = gate.signal_judged([94, 95], gate.Burst(refused=True))
        self.assertEqual((none["heard"], none["lowest_dbfs"], none["still"]), (0, None, 2))

    def test_versions_are_one_build(self) -> None:
        engine, device = f"noise\niem-engine 2.0.0-dev.50+{SHA}\n", f"iem-dante-dev 2.0.0-dev.50 {SHA}\n"
        self.assertEqual(gate.versions_judged(engine, device, SHA[:12] + "\n"), (SHA, True))
        self.assertEqual(gate.versions_judged(engine, device, SHA[:6]), (SHA, False))   # too short a hash
        self.assertEqual(gate.versions_judged(engine, device.replace(SHA, "0" * 40), SHA[:12]), (SHA, False))
        self.assertEqual(gate.versions_judged("iem-engine 2.0.0-dev.50+local", device, SHA[:12]), (None, False))

    def test_the_verdict_names_only_fixed_words(self) -> None:
        doc = gate.verdict(gate.Checks(), [94, 95], left=True)
        self.assertEqual(doc["pass"], 0)
        self.assertEqual(doc["failed"], ["health", "rx_not_back", "hil_not_on", "versions", "loop", "not_heard",
                                         "no_loopback", "alarm_count", "alarm_not_armed", "panic", "job_end"])
        self.assertEqual((doc["panic"], doc["panic_restarts"]), (0, None))
        self.assertEqual(doc["failed"], [w for w in gate.FAILURES if w in doc["failed"]])   # in FAILURES' order
        self.assertNotIn("94", json.dumps(doc))


if __name__ == "__main__":
    unittest.main()
