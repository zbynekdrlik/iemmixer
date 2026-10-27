"""Read-only acceptance of the Gen 3 box (#72 phase D, plan
docs/superpowers/plans/2026-10-26-gen3-phase-d-the-box.md Task 9; rules in
.claude/rules/gen3.md): what provision-root.sh made, and the units deploy.sh
installed, checked on the box as its admin user. provision.sh runs it after the
root script; by hand: `ssh <host> python3 - < scripts/gen3/verify_box.py`
(nothing is left on the box). It changes nothing.

Prints one JSON line of numbers, `{"verify": {"pass": 0|1, "<check>": 0|1, ...,
"tpm2": 0|1}}`, names each failing check on stderr with a fixed reason
(`verify_box: <check>: <reason>`), and exits 0 only when every check passes,
1 otherwise. The checks:

- user: the system user iemmixer (no login, no home folder), its own group,
  the admin (the user running this) in that group.
- units: iem-engine, iem-server and iem-dante installed (loaded), each running
  as iemmixer, none with phase C's 10-user.conf drop-in; iem-alarm@.service
  installed as iemmixer. A box no phase D deploy has reached fails here.
- boot: none of the three enabled.
- sudoers: `sudo -n visudo -c` passes and /etc/sudoers.d/iemmixer-gen3 is
  there.
- pepper: `sudo -n stat` of /etc/credstore.encrypted/iemmixer-pepper: 600,
  root, not empty.
- no_test_pepper: `sudo -n find /var/lib/iemmixer -name pepper.test` finds
  none (the server refuses a plain test pepper beside the credential).
- journal: journald persistent, 1 GiB, 30 days; /var/log/journal; no rsyslog.
- apt: no unattended upgrades (20auto-upgrades, the apt timers and the
  service masked); every installed kernel meta package held.
- sleep: sleep, suspend, hibernate and hybrid-sleep masked.
- swap: an active swap area (the owner's rule for every Linux box).
- clock: dantesync enabled and running; systemd-timesyncd, chrony, ntp,
  time-sync-coordinator, ptp4l, phc2sys and statime neither enabled nor
  running.
- napi: iem-napi enabled and active, irqbalance not running; on the Dante
  interface of /etc/iemmixer/iem-dante.env threaded NAPI 1, each of its NAPI
  threads SCHED_FIFO 60 on core 0, each of its interrupts on core 0.

`tpm2` is 1 when `systemd-creds has-tpm2` says the box has a usable TPM2: the
key `--with-key=auto` sealed the pepper with besides the host key. Its output
names no host, user, address, interface or value of a file (P6).
IEM_VERIFY_ROOT prefixes every path it reads, and every command is called by
name: the tests' seams.
"""
from __future__ import annotations

import json
import os
import re
import subprocess
import sys

ROOT = os.environ.get("IEM_VERIFY_ROOT", "")
SERVICE_USER = "iemmixer"
STACK = ("iem-engine.service", "iem-server.service", "iem-dante.service")
TIME_UNITS = ("systemd-timesyncd.service", "chrony.service", "ntp.service", "time-sync-coordinator.service",
              "ptp4l.service", "phc2sys.service", "statime.service")
APT_UNITS = ("unattended-upgrades.service", "apt-daily.timer", "apt-daily-upgrade.timer")
SLEEP_UNITS = ("sleep.target", "suspend.target", "hibernate.target", "hybrid-sleep.target")
OTHER_UNITS = ("dantesync.service", "iem-napi.service", "irqbalance.service")
UNITS = (*STACK, *TIME_UNITS, *APT_UNITS, *SLEEP_UNITS, *OTHER_UNITS)
KERNEL_PACKAGES = ("linux-generic*", "linux-image-generic*", "linux-headers-generic*")
CREDENTIAL = "/etc/credstore.encrypted/iemmixer-pepper"
SUDOERS = "/etc/sudoers.d/iemmixer-gen3"
ALARM = "/etc/systemd/system/iem-alarm@.service"
JOURNAL_CONF = "/etc/systemd/journald.conf.d/50-iemmixer.conf"
JOURNAL = {"Storage": "persistent", "SystemMaxUse": "1G", "MaxRetentionSec": "30day"}
AUTO_UPGRADES = "/etc/apt/apt.conf.d/20auto-upgrades"
NO_UPGRADES = ('APT::Periodic::Update-Package-Lists "0";', 'APT::Periodic::Unattended-Upgrade "0";')
ENVFILE = "/etc/iemmixer/iem-dante.env"
ENABLED = {"enabled", "enabled-runtime", "linked", "linked-runtime", "alias", "indirect"}
RUNNING = {"active", "activating", "deactivating", "reloading", "refreshing"}
PRIORITY = 60
CORE = "0"
FIFO = 1
IFACE = re.compile(r"[A-Za-z0-9_.-]{1,15}")
CHECKS = ("user", "units", "boot", "sudoers", "pepper", "no_test_pepper", "journal", "apt", "sleep", "swap",
          "clock", "napi")


class Failed(Exception):
    """A check that does not pass, with its fixed reason."""


def at(path: str) -> str:
    return ROOT + path


def run(*argv: str) -> subprocess.CompletedProcess:
    """A command, its stdin closed (this script itself may be stdin)."""
    try:
        return subprocess.run(argv, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=30)
    except FileNotFoundError:
        raise Failed(f"{argv[0]} is not on the box") from None
    except subprocess.TimeoutExpired:
        raise Failed(f"{argv[0]} did not answer within 30 s") from None


def read(path: str) -> str:
    with open(at(path), encoding="utf-8") as f:
        return f.read()


def parse_show(text: str) -> dict[str, dict[str, str]]:
    """`systemctl show` of several units: a block a unit, named by its Id."""
    units: dict[str, dict[str, str]] = {}
    for block in text.split("\n\n"):
        props = dict(line.split("=", 1) for line in block.splitlines() if "=" in line)
        if props.get("Id"):
            units[props["Id"]] = props
    return units


class Box:
    """What the checks read, each read once."""

    def __init__(self) -> None:
        self._units: dict[str, dict[str, str]] | None = None

    def unit(self, name: str) -> dict[str, str]:
        if self._units is None:
            r = run("systemctl", "show", "--property=Id,LoadState,UnitFileState,ActiveState,User,DropInPaths", *UNITS)
            if r.returncode != 0:
                raise Failed("systemctl show failed")
            self._units = parse_show(r.stdout)
        props = self._units.get(name)
        if props is None:
            raise Failed(f"systemctl show named no {name}")
        return props


def package_status(name: str) -> str:
    """The status word dpkg has for a package, `absent` when it knows none."""
    r = run("dpkg-query", "-W", "-f", "${Status}", name)
    if r.returncode == 1:
        return "absent"
    if r.returncode != 0:
        raise Failed("dpkg-query failed")
    return r.stdout.split()[-1]


def check_user(box: Box) -> None:
    admin = run("id", "-un").stdout.strip()
    pw = run("getent", "passwd", SERVICE_USER)
    if pw.returncode != 0:
        raise Failed("no service user iemmixer")
    _, _, uid, gid, _, home, shell = pw.stdout.strip().split(":")
    if int(uid) >= 1000:
        raise Failed("the service user is no system user")
    if not (shell.endswith("/nologin") or shell in ("/bin/false", "/usr/bin/false")):
        raise Failed("the service user can log in")
    if os.path.lexists(at(home)):
        raise Failed("the service user has a home folder")
    gr = run("getent", "group", SERVICE_USER)
    if gr.returncode != 0:
        raise Failed("no group iemmixer")
    _, _, ggid, members = gr.stdout.strip().split(":")
    if ggid != gid:
        raise Failed("the service user's group is not iemmixer")
    if not admin or admin not in members.split(","):
        raise Failed("the admin is not in the group iemmixer")


def check_units(box: Box) -> None:
    for name in STACK:
        unit = box.unit(name)
        if unit.get("LoadState") != "loaded":
            raise Failed(f"{name} is not installed")
        if unit.get("User") != SERVICE_USER:
            raise Failed(f"{name} does not run as iemmixer")
        if "10-user.conf" in unit.get("DropInPaths", ""):
            raise Failed(f"{name} keeps phase C's 10-user.conf")
    if not os.path.isfile(at(ALARM)):
        raise Failed("iem-alarm@.service is not installed")
    if f"User={SERVICE_USER}" not in (line.strip() for line in read(ALARM).splitlines()):
        raise Failed("iem-alarm@.service does not run as iemmixer")


def check_boot(box: Box) -> None:
    for name in STACK:
        if box.unit(name).get("UnitFileState") in ENABLED:
            raise Failed(f"{name} is enabled at boot")


def check_sudoers(box: Box) -> None:
    if run("sudo", "-n", "visudo", "-c").returncode != 0:
        raise Failed("sudo -n visudo -c fails")
    if not os.path.isfile(at(SUDOERS)):
        raise Failed("no /etc/sudoers.d/iemmixer-gen3")


def check_pepper(box: Box) -> None:
    r = run("sudo", "-n", "stat", "-c", "%a.%U.%s", CREDENTIAL)
    if r.returncode != 0:
        raise Failed("the pepper's credential is not there (sudo -n stat)")
    mode, owner, size = r.stdout.strip().split(".")
    if (mode, owner) != ("600", "root"):
        raise Failed("the pepper's credential is not 600 root")
    if int(size) <= 0:
        raise Failed("the pepper's credential is empty")


def check_no_test_pepper(box: Box) -> None:
    r = run("sudo", "-n", "find", "/var/lib/iemmixer", "-name", "pepper.test")
    if r.returncode != 0:
        raise Failed("the service user's folder cannot be searched (sudo -n find)")
    if r.stdout.strip():
        raise Failed("a plain test pepper is on the box")


def check_journal(box: Box) -> None:
    section, keys = "", {}
    for raw in read(JOURNAL_CONF).splitlines():
        line = raw.strip()
        if line.startswith("[") and line.endswith("]"):
            section = line[1:-1]
        elif section == "Journal" and "=" in line and not line.startswith(("#", ";")):
            key, _, value = line.partition("=")
            keys[key.strip()] = value.strip()
    if any(keys.get(key) != value for key, value in JOURNAL.items()):
        raise Failed("journald is not persistent, 1G, 30day")
    if not os.path.isdir(at("/var/log/journal")):
        raise Failed("no /var/log/journal")
    if package_status("rsyslog") not in ("absent", "not-installed"):
        raise Failed("rsyslog is installed")


def check_apt(box: Box) -> None:
    lines = {line.strip() for line in read(AUTO_UPGRADES).splitlines()}
    if any(want not in lines for want in NO_UPGRADES):
        raise Failed("20auto-upgrades does not turn unattended upgrades off")
    for pattern in KERNEL_PACKAGES:
        r = run("dpkg-query", "-W", "-f", "${Package} ${Status}\n", pattern)
        if r.returncode > 1:
            raise Failed("dpkg-query failed")
        for line in r.stdout.splitlines():
            words = line.split()
            if len(words) == 4 and words[3] == "installed" and words[1] != "hold":
                raise Failed("an installed kernel package is not held")
    for name in APT_UNITS:
        if box.unit(name).get("UnitFileState") != "masked":
            raise Failed(f"{name} is not masked")


def check_sleep(box: Box) -> None:
    for name in SLEEP_UNITS:
        if box.unit(name).get("UnitFileState") != "masked":
            raise Failed(f"{name} is not masked")


def check_swap(box: Box) -> None:
    if len(read("/proc/swaps").splitlines()) < 2:
        raise Failed("no active swap")


def check_clock(box: Box) -> None:
    owner = box.unit("dantesync.service")
    if owner.get("UnitFileState") != "enabled" or owner.get("ActiveState") != "active":
        raise Failed("dantesync is not enabled and running")
    for name in TIME_UNITS:
        unit = box.unit(name)
        if unit.get("UnitFileState") in ENABLED or unit.get("ActiveState") in RUNNING:
            raise Failed(f"{name} is enabled or running beside dantesync")


def interface() -> str:
    value = None
    for raw in read(ENVFILE).splitlines():
        line = raw.strip()
        if line.startswith("IEM_DANTE_IFACE="):
            value = line.partition("=")[2].strip().strip('"')
    if value is None or not IFACE.fullmatch(value):
        raise Failed("no Dante interface in /etc/iemmixer/iem-dante.env")
    return value


def napi_threads(iface: str) -> list[str]:
    """The pids of the interface's NAPI threads (napi/<interface>-<id>)."""
    pids = []
    proc = at("/proc")
    for entry in os.listdir(proc):
        if not entry.isdigit():
            continue
        try:
            with open(os.path.join(proc, entry, "comm"), encoding="utf-8") as f:
                comm = f.read().strip()
        except FileNotFoundError:
            continue
        prefix = f"napi/{iface}-"
        if comm.startswith(prefix) and comm[len(prefix):].isdigit():
            pids.append(entry)
    return sorted(pids, key=int)


def interface_irqs(iface: str) -> list[str]:
    """The interrupts a line of /proc/interrupts names the interface on."""
    irqs = []
    for line in read("/proc/interrupts").splitlines():
        words = line.split()
        if not words or not re.fullmatch(r"\d+:", words[0]):
            continue
        if any(w.rstrip(",") == iface or w.rstrip(",").startswith(iface + "-") for w in words[1:]):
            irqs.append(words[0][:-1])
    return irqs


def check_napi(box: Box) -> None:
    unit = box.unit("iem-napi.service")
    if unit.get("UnitFileState") != "enabled" or unit.get("ActiveState") != "active":
        raise Failed("iem-napi is not enabled and active")
    if box.unit("irqbalance.service").get("ActiveState") in RUNNING:
        raise Failed("irqbalance runs")
    iface = interface()
    if read(f"/sys/class/net/{iface}/threaded").strip() != "1":
        raise Failed("threaded NAPI is off on the Dante interface")
    pids = napi_threads(iface)
    if not pids:
        raise Failed("the Dante interface has no NAPI thread")
    for pid in pids:
        fields = read(f"/proc/{pid}/stat").rsplit(") ", 1)[1].split()
        if (int(fields[38]), int(fields[37])) != (FIFO, PRIORITY):
            raise Failed("a NAPI thread is not SCHED_FIFO 60")
        cpus = [line.split("\t", 1)[1].strip() for line in read(f"/proc/{pid}/status").splitlines()
                if line.startswith("Cpus_allowed_list:")]
        if cpus != [CORE]:
            raise Failed("a NAPI thread is not on core 0 alone")
    irqs = interface_irqs(iface)
    if not irqs:
        raise Failed("no interrupt names the Dante interface")
    for irq in irqs:
        if read(f"/proc/irq/{irq}/smp_affinity_list").strip() != CORE:
            raise Failed("an interrupt of the Dante interface is not on core 0 alone")


def tpm2() -> int:
    try:
        return int(run("systemd-creds", "has-tpm2").returncode == 0)
    except Failed:
        return 0


def main() -> int:
    box = Box()
    results: dict[str, int] = {}
    for name in CHECKS:
        try:
            globals()[f"check_{name}"](box)
            results[name] = 1
        except Failed as failed:
            results[name] = 0
            print(f"verify_box: {name}: {failed}", file=sys.stderr)
        except (OSError, ValueError, IndexError) as e:
            results[name] = 0
            print(f"verify_box: {name}: could not be read ({type(e).__name__})", file=sys.stderr)
    passed = int(all(results.values()))
    print(json.dumps({"verify": {"pass": passed, **results, "tpm2": tpm2()}}, separators=(",", ":")))
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
