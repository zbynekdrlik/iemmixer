"""Tests for the Gen 3 box's provisioning (#72 phase D, plan
docs/superpowers/plans/2026-10-26-gen3-phase-d-the-box.md Task 9):
provision-root.sh on a temporary root (IEM_PROVISION_ROOT) with stand-in
commands first on PATH that play the box's users, units, packages, owners and
the TPM (`systemd-creds`), every call logged; napi-priority on stand-in /sys
and /proc trees with stand-in `chrt` and `taskset`; verify_box.py on what the
root script made (IEM_VERIFY_ROOT, the same stand-ins and a stand-in `sudo`);
provision.sh against a stand-in `ssh` and `scp`. The rendered sudoers rule
goes through the runner's real `visudo -cf` and `cvtsudoers`. Every value is
synthetic (P6)."""
from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from test_deploy import expand, parse_unit, roster_people, sudo_lines, template_lines

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
ROOT_SCRIPT = HERE / "provision-root.sh"
PROVISION = HERE / "provision.sh"
VERIFY = HERE / "verify_box.py"
NAPI = HERE / "napi-priority"
NAPI_UNIT = HERE / "iem-napi.service"
TEMPLATE = HERE / "sudo-lines.txt"
STAGE_FILES = ("provision-root.sh", "sudo-lines.txt", "iem-napi.service", "napi-priority")
ADMIN = "tester"
NTP = "ntp-master.test"
IFACE = "ethz9"
# Another interface whose name starts with the Dante one's: never touched.
OTHER = "ethz90"
NAPI_THREADS = {"4101": f"napi/{IFACE}-8193", "4102": f"napi/{IFACE}-8194"}
OTHER_THREADS = {"1": "systemd", "4201": f"napi/{OTHER}-8195"}
IRQS = ("131", "132")
OTHER_IRQS = ("1", "141")
INTERRUPTS = f"""            CPU0       CPU1       CPU2       CPU3
   1:          0          0          0          9  IR-IO-APIC    1-edge      i8042
 131:       1200          0          0          0  IR-PCI-MSIX-0000:01:00.0    1-edge      {IFACE}-TxRx-0
 132:          0       1300          0          0  IR-PCI-MSIX-0000:01:00.0    2-edge      {IFACE}-TxRx-1
 141:          5          0          0          0  IR-PCI-MSI-0000:02:00.0    0-edge      {OTHER}
 NMI:          0          0          0          0   Non-maskable interrupts
"""
SWAPS_HEADER = "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n"
STEPS = ("user", "folders", "state", "pepper", "phase-c", "journal", "realtime", "apt", "sleep", "swap", "clock",
         "napi", "display", "stack", "sudoers")
CHECKS = ("user", "units", "boot", "sudoers", "pepper", "no_test_pepper", "state", "journal", "realtime", "apt",
          "sleep", "swap", "clock", "napi", "display")
# verify's information beside the checks, never part of pass.
INFO = ("tpm2", "realtime_running")
# The realtime step on a 4-CPU box (#72 phase F): the kernel the stand-in
# apt-get installs, grub's entry for it, the command line isolating CPUs 2-3,
# the workqueue mask of CPUs 0-1.
REALTIME = "7.0.0-38-realtime"
RT_ENTRY = f"Advanced options for Ubuntu>Ubuntu, with Linux {REALTIME}"
RT_CMDLINE = "isolcpus=domain,managed_irq,2-3 nohz_full=2-3 rcu_nocbs=2-3 irqaffinity=0-1"
GRUB = "/etc/default/grub"
GRUB_BEFORE = """# If you change this file, run 'update-grub' afterwards to update
# /boot/grub/grub.cfg.

GRUB_DEFAULT=0
GRUB_TIMEOUT_STYLE=hidden
GRUB_TIMEOUT=0
GRUB_CMDLINE_LINUX_DEFAULT="quiet splash"
GRUB_CMDLINE_LINUX=""

# Uncomment to disable graphical terminal
#GRUB_TERMINAL=console
"""
GRUB_AFTER = GRUB_BEFORE.replace("GRUB_DEFAULT=0\n", "GRUB_DEFAULT=saved\n").replace(
    'GRUB_CMDLINE_LINUX_DEFAULT="quiet splash"', f'GRUB_CMDLINE_LINUX_DEFAULT="{RT_CMDLINE}"')
WORKQUEUE_CONF = "/etc/tmpfiles.d/iemmixer-workqueue.conf"
WORKQUEUE_LINE = "w /sys/devices/virtual/workqueue/cpumask - - - - 3\n"
AFTER_REBOOT = "realtime: the box runs it after its next reboot"
# A file the root script renders in its temporary folder, installed 0644 root.
TMP_INSTALL = ["install", "-D", "-m", "0644", "-o", "root", "-g", "root", "--", "<tmp>"]
# The display drivers' output polling (the kernel's drm_kms_helper.poll, root
# only), and the tmpfiles.d file that turns it off at every boot.
POLL = "sys/module/drm_kms_helper/parameters/poll"
DISPLAY_CONF = "/etc/tmpfiles.d/iemmixer-display.conf"
POLL_OFF = "w /sys/module/drm_kms_helper/parameters/poll - - - - N\n"
STACK = ("iem-engine.service", "iem-server.service", "iem-dante.service")
# The stack's state folders (the units' StateDirectory=), and find's question
# about them: the first path not owned by iemmixer:iemmixer.
STATE_DIRS = ("var/lib/iemmixer", "var/lib/iem-dante")
FOREIGN = ["-not", "-user", "iemmixer", "-print", "-quit", "-o", "-not", "-group", "iemmixer", "-print", "-quit"]
FIND_STATE = ["find", "-P", "/var/lib/iemmixer", "/var/lib/iem-dante", *FOREIGN]
CHOWN_STATE = ["chown", "-R", "-h", "iemmixer:iemmixer", "--", "/var/lib/iemmixer", "/var/lib/iem-dante"]

# One stand-in for every command the box's scripts call by name, dispatched
# on its own name. The box's state is FAKE_WORLD/db.json; each call is one
# JSON line of calls.log, marked a write unless it only reads.
FAKE = r'''
import fnmatch, hashlib, json, os, subprocess, sys

WORLD = os.environ["FAKE_WORLD"]
ROOT = os.environ["FAKE_ROOT"]
name = os.path.basename(sys.argv[0])
args = sys.argv[1:]
DB = os.path.join(WORLD, "db.json")
READS = {"id", "getent", "stat", "dpkg-query", "visudo", "sudo", "find", "nproc", "uname"}
UNIT_DEFAULT = {"LoadState": "not-found", "UnitFileState": "", "ActiveState": "inactive", "User": "",
                "DropInPaths": ""}
REAL_VISUDO = next((p for p in ("/usr/sbin/visudo", "/usr/bin/visudo", "/sbin/visudo") if os.access(p, os.X_OK)), None)


def load():
    with open(DB, encoding="utf-8") as f:
        return json.load(f)


def save(db):
    with open(DB, "w", encoding="utf-8") as f:
        json.dump(db, f, indent=1, sort_keys=True)


def is_write():
    if name in READS:
        return False
    if name in ("systemctl", "timedatectl"):
        return args[:1] != ["show"]
    if name == "systemd-creds":
        return args[:1] == ["encrypt"]
    if name == "grub-editenv":
        return args != ["list"]
    return True


with open(os.path.join(WORLD, "calls.log"), "a", encoding="utf-8") as f:
    f.write(json.dumps({"argv": [name] + args, "write": is_write()}) + "\n")


def fail(text, code=1):
    sys.stderr.write(f"{name}: {text}\n")
    sys.exit(code)


def unit_of(db, unit):
    return {**UNIT_DEFAULT, **db["units"].get(unit, {}), "Id": unit}


def stat_text(fmt, path):
    st = os.stat(path)
    owner, group = load()["owners"].get(os.path.realpath(path), "root root").split()
    out = fmt.replace("%a", format(st.st_mode & 0o7777, "o")).replace("%U", owner).replace("%G", group)
    return out.replace("%s", str(st.st_size)).replace("%n", path)


def reload(db):
    unit_dir = os.path.join(ROOT, "etc/systemd/system")
    for entry in sorted(os.listdir(unit_dir)):
        path = os.path.join(unit_dir, entry)
        if "@" in entry or not entry.endswith((".service", ".timer", ".target")) or not os.path.isfile(path):
            continue
        p = db["units"].setdefault(entry, dict(UNIT_DEFAULT))
        if p["LoadState"] == "not-found":
            p.update(LoadState="loaded", UnitFileState="disabled", ActiveState="inactive")
        user, dropins = "", []
        for line in open(path, encoding="utf-8"):
            if line.startswith("User="):
                user = line.split("=", 1)[1].strip()
        if os.path.isdir(path + ".d"):
            for conf in sorted(os.listdir(path + ".d")):
                dropins.append(f"/etc/systemd/system/{entry}.d/{conf}")
                for line in open(os.path.join(path + ".d", conf), encoding="utf-8"):
                    if line.startswith("User="):
                        user = line.split("=", 1)[1].strip()
        p["User"] = user if entry.endswith(".service") else ""
        p["DropInPaths"] = " ".join(dropins)


def run_napi():
    """systemd running iem-napi.service's ExecStart; its output is the journal's."""
    with open(os.path.join(ROOT, "etc/iemmixer/iem-dante.env"), encoding="utf-8") as f:
        iface = [line.split("=", 1)[1].strip() for line in f if line.startswith("IEM_DANTE_IFACE=")][-1]
    env = {**os.environ, "IEM_NAPI_SYS": os.path.join(ROOT, "sys"), "IEM_NAPI_PROC": os.path.join(ROOT, "proc"),
           "IEM_NAPI_IFACE_WAIT": "2", "IEM_NAPI_THREAD_WAIT": "2"}
    with open(os.path.join(WORLD, "journal.log"), "a", encoding="utf-8") as journal:
        return subprocess.run([os.path.join(ROOT, "usr/local/lib/iemmixer/napi-priority"), iface], env=env,
                              stdout=journal, stderr=journal).returncode


def proc_file(pid, leaf):
    return os.path.join(os.environ["IEM_NAPI_PROC"], pid, leaf)


# find's question about the stack's state (provision-root.sh, and verify_box.py
# through sudo): the first path not owned by iemmixer:iemmixer.
FOREIGN = ["-not", "-user", "iemmixer", "-print", "-quit", "-o", "-not", "-group", "iemmixer", "-print", "-quit"]


def owner_key(path):
    """The owners' key of the path itself: a link's own, never its target's."""
    return os.path.join(os.path.realpath(os.path.dirname(path)), os.path.basename(path))


def walk(top):
    """top and everything under it, depth first, never through a link."""
    yield top
    if os.path.isdir(top) and not os.path.islink(top):
        for entry in sorted(os.listdir(top)):
            yield from walk(os.path.join(top, entry))


def find_foreign(tops, shown):
    """find -P <tops> FOREIGN: the first path that is not iemmixer's, shown
    as the caller sees it; a top that is not there is an error, the others
    still searched."""
    owners, rc = load()["owners"], 0
    for top in tops:
        if not os.path.lexists(top):
            sys.stderr.write(f"find: '{shown(top)}': No such file or directory\n")
            rc = 1
            continue
        for path in walk(top):
            if owners.get(owner_key(path), "root root") != "iemmixer iemmixer":
                print(shown(path))
                sys.exit(rc)
    sys.exit(rc)


if name == "id":
    db = load()
    if args == ["-u"]:
        print(db["uid"])
    elif args == ["-un"]:
        print(db["user"])
    else:
        fail("unexpected id call", 97)
elif name == "getent":
    table, key = args
    line = load()[table].get(key)
    if line is None:
        sys.exit(2)
    print(line)
elif name == "groupadd":
    db = load()
    assert args[0] == "--system", args
    group = args[-1]
    if group in db["group"]:
        fail(f"group '{group}' already exists", 9)
    db["group"][group] = f"{group}:x:{db['next_id']}:"
    db["next_id"] += 1
    save(db)
elif name == "useradd":
    db = load()
    opts, user = args[:-1], args[-1]
    want = ["--system", "--gid", "iemmixer", "--no-create-home", "--home-dir", "/nonexistent", "--shell",
            "/usr/sbin/nologin"]
    if opts != want:
        fail(f"unexpected options {opts}", 97)
    gid = db["group"]["iemmixer"].split(":")[2]
    db["passwd"][user] = f"{user}:x:{db['next_id']}:{gid}::/nonexistent:/usr/sbin/nologin"
    db["next_id"] += 1
    save(db)
elif name == "usermod":
    db = load()
    if args[:3] != ["--append", "--groups", "iemmixer"]:
        fail("unexpected usermod", 97)
    gname, pw, gid, members = db["group"]["iemmixer"].split(":")
    members = [m for m in members.split(",") if m] + [args[3]]
    db["group"]["iemmixer"] = ":".join([gname, pw, gid, ",".join(members)])
    save(db)
elif name == "install":
    real, owner, group, positional, dirs, rest = [], None, None, [], False, list(args)
    while rest:
        a = rest.pop(0)
        if a == "--":
            positional += rest
            break
        if a in ("-o", "-g"):
            value = rest.pop(0)
            owner, group = (value, group) if a == "-o" else (owner, value)
        elif a == "-m":
            real += [a, rest.pop(0)]
        elif a.startswith("-"):
            real.append(a)
            dirs = dirs or a == "-d"
        else:
            positional.append(a)
    rc = subprocess.run(["/usr/bin/install", *real, "--", *positional]).returncode
    if rc:
        sys.exit(rc)
    db = load()
    for target in (positional if dirs else positional[-1:]):
        db["owners"][os.path.realpath(target)] = f"{owner or 'root'} {group or 'root'}"
    save(db)
elif name == "find":
    if args[:1] != ["-P"] or args[-len(FOREIGN):] != FOREIGN or len(args) <= 1 + len(FOREIGN):
        fail("unexpected find", 97)
    find_foreign(args[1:-len(FOREIGN)], lambda p: p)
elif name == "chown":
    if args[:4] != ["-R", "-h", "iemmixer:iemmixer", "--"] or len(args) < 5:
        fail("unexpected chown", 97)
    db = load()
    for top in args[4:]:
        if not os.path.lexists(top):
            fail(f"cannot access '{top}': No such file or directory")
        for path in walk(top):
            db["owners"][owner_key(path)] = "iemmixer iemmixer"
    save(db)
elif name == "stat":
    rest = [a for a in args if a != "--"]
    assert rest[0] == "-c", rest
    if not os.path.exists(rest[2]):
        fail(f"cannot statx '{rest[2]}': No such file or directory")
    print(stat_text(rest[1], rest[2]))
elif name == "systemctl":
    db = load()
    verb, rest = args[0], args[1:]
    if verb == "show":
        props, value_only, units = [], False, []
        for a in rest:
            if a.startswith("--property="):
                props = a.split("=", 1)[1].split(",")
            elif a == "--value":
                value_only = True
            else:
                units.append(a)
        blocks = []
        for unit in units:
            p = unit_of(db, unit)
            blocks.append("\n".join(p[k] if value_only else f"{k}={p[k]}" for k in props))
        print(("\n" if value_only else "\n\n").join(blocks))
        sys.exit(0)
    if verb == "daemon-reload":
        reload(db)
        save(db)
        sys.exit(0)
    now = "--now" in rest
    for unit in (a for a in rest if not a.startswith("--")):
        p = db["units"].setdefault(unit, dict(UNIT_DEFAULT))
        if verb == "mask":
            p.update(LoadState="masked", UnitFileState="masked")
            if now:
                p["ActiveState"] = "inactive"
        elif verb in ("enable", "disable"):
            if p["LoadState"] != "loaded":
                fail(f"Unit file {unit} does not exist.")
            p["UnitFileState"] = "enabled" if verb == "enable" else "disabled"
            if now:
                p["ActiveState"] = "active" if verb == "enable" else "inactive"
        elif verb in ("start", "restart"):
            if p["LoadState"] != "loaded":
                fail(f"Unit {unit} not found.", 5)
            if unit == "iem-napi.service":
                rc = run_napi()
                p["ActiveState"] = "active" if rc == 0 else "failed"
                if rc:
                    save(db)
                    fail(f"Job for {unit} failed.")
            else:
                p["ActiveState"] = "active"
        elif verb == "stop":
            p["ActiveState"] = "inactive"
        else:
            fail(f"unexpected systemctl {verb}", 97)
    save(db)
elif name == "timedatectl":
    db = load()
    if args == ["show", "--property=NTP", "--value"]:
        print(db["ntp"])
    elif args == ["set-ntp", "false"]:
        db["ntp"] = "no"
        save(db)
    else:
        fail("unexpected timedatectl", 97)
elif name == "dpkg-query":
    fmt, patterns, rest = "${Package}\n", [], list(args)
    while rest:
        a = rest.pop(0)
        if a == "-W":
            continue
        if a == "-f":
            fmt = rest.pop(0)
        else:
            patterns.append(a)
    fmt = fmt.replace("\\n", "\n")
    db, found, out = load(), True, []
    for pattern in patterns:
        hits = sorted(p for p in db["packages"] if fnmatch.fnmatchcase(p, pattern))
        if not hits:
            found = False
            sys.stderr.write(f"dpkg-query: no packages found matching {pattern}\n")
        out += [fmt.replace("${Package}", p).replace("${Status}", db["packages"][p]) for p in hits]
    sys.stdout.write("".join(out))
    sys.exit(0 if found else 1)
elif name == "apt-mark":
    db = load()
    assert args[0] == "hold", args
    for package in args[1:]:
        db["packages"][package] = "hold ok installed"
        print(f"{package} set on hold.")
    save(db)
elif name == "apt-get":
    db = load()
    if os.environ.get("DEBIAN_FRONTEND") != "noninteractive":
        fail("unexpected apt-get", 97)
    if args[:2] == ["purge", "-y"]:
        for package in args[2:]:
            db["packages"].pop(package, None)
    elif args == ["install", "-y", "--no-install-recommends", "linux-image-realtime"]:
        # The meta package, the kernel it depends on, and that kernel's image.
        kernel = db["realtime_kernel"]
        db["packages"]["linux-image-realtime"] = "install ok installed"
        db["packages"][f"linux-image-{kernel}"] = "install ok installed"
        with open(os.path.join(ROOT, "boot", f"vmlinuz-{kernel}"), "w", encoding="utf-8") as f:
            f.write("synthetic realtime kernel\n")
    else:
        fail("unexpected apt-get", 97)
    save(db)
elif name == "systemd-creds":
    if args[:1] == ["has-tpm2"]:
        sys.exit(int(os.environ.get("FAKE_TPM2_RC", "1")))
    if args[:4] != ["encrypt", "--name=pepper", "--with-key=auto", "-"] or len(args) != 5:
        fail("unexpected systemd-creds", 97)
    data = sys.stdin.buffer.read()
    with open(os.path.join(WORLD, "creds-input.hex"), "w", encoding="utf-8") as f:
        f.write(data.hex())
    fd = os.open(args[4], os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o666)
    os.write(fd, b"FAKE-SEALED " + hashlib.sha256(data).hexdigest().encode() + b"\n")
    os.close(fd)
elif name == "nproc":
    if args != ["--all"]:
        fail("unexpected nproc", 97)
    print(load()["nproc"])
elif name == "uname":
    if args != ["-r"]:
        fail("unexpected uname", 97)
    print(load()["uname"])
elif name == "update-grub":
    if args:
        fail("unexpected update-grub", 97)
    sys.exit(int(os.environ.get("FAKE_UPDATE_GRUB_RC", "0")))
elif name == "grub-editenv":
    if args != ["list"]:
        fail("unexpected grub-editenv", 97)
    for key, value in load()["grubenv"].items():
        print(f"{key}={value}")
elif name == "grub-set-default":
    if len(args) != 1:
        fail("unexpected grub-set-default", 97)
    if not os.environ.get("FAKE_GRUB_SET_IGNORED"):
        db = load()
        db["grubenv"].pop("prev_saved_entry", None)
        db["grubenv"]["saved_entry"] = args[0]
        save(db)
elif name == "visudo":
    if args[:1] != ["-cf"] or REAL_VISUDO is None:
        fail("unexpected visudo call, or no visudo on this machine", 127)
    sys.exit(subprocess.run([REAL_VISUDO, *args]).returncode)
elif name == "fallocate":
    path = args[-1]
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o666)
    os.write(fd, bytes(4096))
    os.close(fd)
elif name == "mkswap":
    with open(args[-1], "r+b") as f:
        f.seek(4086)
        f.write(b"SWAPSPACE2")
elif name == "swapon":
    path = args[-1]
    with open(os.path.join(ROOT, "proc/swaps"), "a", encoding="utf-8") as f:
        f.write(f"{path[len(ROOT):]}\tfile\t\t4\t\t0\t\t-2\n")
elif name == "chrt":
    assert args[:2] == ["-f", "-p"], args
    priority, pid = args[2], args[3]
    if not os.environ.get("FAKE_CHRT_IGNORED"):
        with open(proc_file(pid, "stat"), encoding="utf-8") as f:
            head, _, tail = f.read().rpartition(") ")
        fields = tail.split()
        fields[37], fields[38] = priority, "1"
        with open(proc_file(pid, "stat"), "w", encoding="utf-8") as f:
            f.write(head + ") " + " ".join(fields) + "\n")
elif name == "systemd-tmpfiles":
    # Its `w` lines: the argument written to a file that exists.
    assert args[:1] == ["--create"] and len(args) == 2, args
    rc = int(os.environ.get("FAKE_TMPFILES_RC", "0"))
    if rc:
        fail(f"Failed to write to {args[1]}: Permission denied", rc)
    if not os.environ.get("FAKE_TMPFILES_IGNORED"):
        with open(args[1], encoding="utf-8") as f:
            for line in f:
                words = line.split()
                if len(words) == 7 and words[0] == "w" and os.path.exists(ROOT + words[1]):
                    with open(ROOT + words[1], "w", encoding="utf-8") as out:
                        out.write(words[6] + "\n")
elif name == "taskset":
    assert args[:2] == ["-p", "-c"], args
    cores, pid = args[2], args[3]
    print(f"pid {pid}'s current affinity list: 0-3")
    print(f"pid {pid}'s new affinity list: {cores}")
    with open(proc_file(pid, "status"), "w", encoding="utf-8") as f:
        f.write(f"Name:\tnapi\nCpus_allowed_list:\t{cores}\n")
elif name == "sudo":
    rest = args[1:] if args[:1] == ["-n"] else fail("a sudo without -n", 97)
    if rest == ["visudo", "-c"]:
        path = os.path.join(ROOT, "etc/sudoers.d/iemmixer-gen3")
        if not os.path.exists(path) or REAL_VISUDO is None:
            fail("visudo: unable to open the rule")
        sys.exit(subprocess.run([REAL_VISUDO, "-cf", path], stdout=subprocess.DEVNULL).returncode)
    if rest[:2] == ["stat", "-c"] and len(rest) == 4:
        path = ROOT + rest[3]
        if not os.path.exists(path):
            fail("stat: No such file or directory")
        print(stat_text(rest[2], path))
        sys.exit(0)
    if rest[:2] == ["find", "-P"] and rest[-len(FOREIGN):] == FOREIGN and len(rest) > 2 + len(FOREIGN):
        find_foreign([ROOT + p for p in rest[2:-len(FOREIGN)]], lambda p: p[len(ROOT):])
    if rest[:1] == ["cat"] and len(rest) == 2:
        if os.environ.get("FAKE_SUDO_CAT_REFUSED"):
            fail("a password is required")
        path = ROOT + rest[1]
        if not os.path.exists(path):
            fail(f"cat: {rest[1]}: No such file or directory")
        with open(path, encoding="utf-8") as f:
            sys.stdout.write(f.read())
        sys.exit(0)
    if rest[:1] == ["find"] and rest[2:] == ["-name", "pepper.test"]:
        base = ROOT + rest[1]
        if not os.path.isdir(base):
            fail(f"find: '{rest[1]}': No such file or directory")
        for folder, _, files in os.walk(base):
            for leaf in files:
                if leaf == "pepper.test":
                    print(os.path.join(folder, leaf)[len(ROOT):])
        sys.exit(0)
    fail("a password is required")
else:
    fail("no stand-in for this name", 97)
'''

FAKE_NAMES = ("id", "getent", "groupadd", "useradd", "usermod", "install", "stat", "systemctl", "timedatectl",
              "dpkg-query", "apt-mark", "apt-get", "systemd-creds", "visudo", "fallocate", "mkswap", "swapon",
              "chrt", "taskset", "sudo", "find", "chown", "systemd-tmpfiles", "nproc", "uname", "update-grub",
              "grub-editenv", "grub-set-default")


def stat_line(pid: str, comm: str, policy: int, priority: int) -> str:
    """A /proc/<pid>/stat line: fields 3 to 52, rt_priority field 40, policy 41."""
    fields = ["S"] + ["0"] * 49
    fields[40 - 3], fields[41 - 3] = str(priority), str(policy)
    return f"{pid} ({comm}) " + " ".join(fields) + "\n"


def write(path: Path, text: str | bytes, mode: int | None = None) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(text, bytes):
        path.write_bytes(text)
    else:
        path.write_text(text, encoding="utf-8")
    if mode is not None:
        path.chmod(mode)


def kernel_trees(root: Path, applied: bool) -> None:
    """/sys and /proc as the kernel leaves them (or as napi-priority sets
    them): the Dante interface and another one whose name starts with it."""
    for iface in (IFACE, OTHER):
        write(root / "sys/class/net" / iface / "threaded", "1\n" if applied and iface == IFACE else "0\n")
    for pid, comm in {**NAPI_THREADS, **OTHER_THREADS}.items():
        mine = applied and pid in NAPI_THREADS
        write(root / "proc" / pid / "comm", comm + "\n")
        write(root / "proc" / pid / "stat", stat_line(pid, comm, 1 if mine else 0, 60 if mine else 0))
        write(root / "proc" / pid / "status", f"Name:\tx\nCpus_allowed_list:\t{'0' if mine else '0-3'}\n")
    write(root / "proc/interrupts", INTERRUPTS)
    for irq in (*IRQS, *OTHER_IRQS):
        write(root / "proc/irq" / irq / "smp_affinity_list", "0\n" if applied and irq in IRQS else "0-3\n")


class World(unittest.TestCase):
    """A phase C box: the stack's units with their 10-user.conf drop-ins, the
    stack's state the admin's (phase C ran the stack as the admin) with the
    synthetic band's test pepper, dantesync installed (not yet enabled),
    systemd-timesyncd, rsyslog, irqbalance, the apt timers, no swap, the
    kernel trees as the kernel leaves them."""

    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.root = self.tmp / "root"
        self.world = self.tmp / "world"
        self.stage = self.tmp / "home" / ".cache" / "iem-gen3-provision"
        self.worktmp = self.tmp / "tmp"
        for folder in (self.root, self.world, self.stage, self.worktmp):
            folder.mkdir(parents=True)
        self.bin = self.tmp / "bin"
        self.bin.mkdir()
        for name in FAKE_NAMES:
            p = self.bin / name
            p.write_text(f"#!{sys.executable}\n{FAKE}", encoding="utf-8")
            p.chmod(0o755)
        for name in STAGE_FILES:
            shutil.copyfile(HERE / name, self.stage / name)
        write(self.stage / "ntp-server", NTP + "\n")
        write(self.stage / "roster-ids", "".join(p + "\n" for p in roster_people()))
        self.env = {**os.environ, "PATH": f"{self.bin}{os.pathsep}{os.environ['PATH']}",
                    "FAKE_WORLD": str(self.world), "FAKE_ROOT": str(self.root),
                    "IEM_PROVISION_ROOT": str(self.root), "IEM_VERIFY_ROOT": str(self.root),
                    "TMPDIR": str(self.worktmp)}
        self.build_box()

    def build_box(self) -> None:
        r = self.root
        # The programs sudo-lines.txt names, where sudo's secure_path finds them.
        for program in self.template_programs():
            write(r / ("usr/sbin" if program == "visudo" else "usr/bin") / program, "#!/bin/sh\n", 0o755)
        write(r / "usr/local/bin/dantesync", "#!/bin/sh\n", 0o755)
        write(r / "etc/systemd/system/dantesync.service", "[Service]\nUser=root\nExecStart=/usr/local/bin/dantesync\n")
        write(r / "etc/dantesync/config.json", json.dumps({"ntp_server": NTP, "ntp_server_mode": {"enabled": False}}))
        write(r / "etc/iemmixer/iem-dante.env", f"IEM_DANTE_IFACE={IFACE}\n")
        (r / "etc/iemmixer").chmod(0o755)
        for unit in STACK:
            write(r / "etc/systemd/system" / unit, "[Service]\nExecStart=/opt/iemmixer/bin/iem-engine run\n")
            write(r / "etc/systemd/system" / f"{unit}.d" / "10-user.conf", f"[Service]\nUser={ADMIN}\n")
        # Phase C's state, all the admin's: the server's secrets (0600 in a
        # 0775 folder, the admin's umask), its site, roster status, push
        # subscriptions and lineups, the engine's persistence, the device's
        # state file and its phase B copy.
        secrets = r / "var/lib/iemmixer/site/secrets"
        for leaf in ("pepper.test", "pin_hashes.json", "jwt_secret", "vapid_private"):
            write(secrets / leaf, f"synthetic {leaf}\n", 0o600)
        for rel, mode in (("site/iemmixer.toml", 0o644), ("site/roster.toml", 0o644),
                          ("site/roster.status.json", 0o644), ("site/push_subscriptions.json", 0o600),
                          ("site/push_subs_v2_migrated", 0o644), ("site/lineups/active.json", 0o644),
                          ("site/lineups/lineup1/customizations/member1.json", 0o644),
                          ("engine/current.json", 0o644), ("engine/baseline.json", 0o644)):
            write(r / "var/lib/iemmixer" / rel, f"synthetic {rel}\n", mode)
        for leaf in ("state.toml", "state.toml.phase-b-32x16"):
            write(r / "var/lib/iem-dante" / leaf, f"synthetic {leaf}\n", 0o644)
        for rel, mode in (("var/lib/iemmixer", 0o755), ("var/lib/iem-dante", 0o755), ("var/lib/iemmixer/site", 0o755),
                          ("var/lib/iemmixer/site/secrets", 0o775)):
            (r / rel).chmod(mode)
        owners = {str(path.resolve()): f"{ADMIN} {ADMIN}"
                  for top in STATE_DIRS for path in (r / top, *(r / top).rglob("*"))}
        (r / "etc/sudoers.d").mkdir(parents=True)
        (r / "etc/apt/apt.conf.d").mkdir(parents=True)
        write(r / "etc/fstab", "# /etc/fstab: static file system information.\nUUID=0000 / ext4 defaults 0 1\n")
        write(r / "proc/meminfo", "MemTotal:       16123456 kB\nMemFree:         9000000 kB\n")
        write(r / "proc/swaps", SWAPS_HEADER)
        kernel_trees(r, applied=False)
        # The i915 driver polls a box's display outputs every 10 s by default.
        write(r / POLL, "Y\n", 0o600)
        # Ubuntu's generic kernel and grub's settings as the installer leaves them.
        write(r / "boot/vmlinuz-7.0.0-1-generic", "synthetic generic kernel\n", 0o600)
        write(r / GRUB.lstrip("/"), GRUB_BEFORE, 0o644)
        units = {
            **{u: {"LoadState": "loaded", "UnitFileState": "disabled", "ActiveState": "inactive", "User": ADMIN,
                   "DropInPaths": f"/etc/systemd/system/{u}.d/10-user.conf"} for u in STACK},
            "dantesync.service": {"LoadState": "loaded", "UnitFileState": "disabled", "ActiveState": "inactive",
                                  "User": "root"},
            "systemd-journald.service": {"LoadState": "loaded", "UnitFileState": "static", "ActiveState": "active"},
            "systemd-timesyncd.service": {"LoadState": "loaded", "UnitFileState": "enabled", "ActiveState": "active"},
            "irqbalance.service": {"LoadState": "loaded", "UnitFileState": "enabled", "ActiveState": "active"},
            "unattended-upgrades.service": {"LoadState": "loaded", "UnitFileState": "enabled",
                                            "ActiveState": "active"},
            "apt-daily.timer": {"LoadState": "loaded", "UnitFileState": "enabled", "ActiveState": "active"},
            "apt-daily-upgrade.timer": {"LoadState": "loaded", "UnitFileState": "enabled", "ActiveState": "active"},
            **{t: {"LoadState": "loaded", "UnitFileState": "static", "ActiveState": "inactive"}
               for t in ("sleep.target", "suspend.target", "hibernate.target", "hybrid-sleep.target")},
        }
        # Phase C's deploy left the standalone device enabled on this box.
        units["iem-dante.service"]["UnitFileState"] = "enabled"
        self.save_db({
            "uid": "0", "user": ADMIN, "next_id": 990, "ntp": "yes", "owners": owners,
            "nproc": 4, "uname": "7.0.0-1-generic", "grubenv": {}, "realtime_kernel": REALTIME,
            "passwd": {ADMIN: f"{ADMIN}:x:1000:1000::/home/{ADMIN}:/bin/bash", "root": "root:x:0:0::/root:/bin/bash"},
            "group": {ADMIN: f"{ADMIN}:x:1000:", "systemd-journal": "systemd-journal:x:999:"},
            "units": units,
            "packages": {"rsyslog": "install ok installed", "linux-generic": "install ok installed",
                         "linux-image-generic": "install ok installed",
                         "linux-headers-generic": "install ok installed",
                         "linux-image-7.0.0-1-generic": "install ok installed",
                         "linux-firmware": "install ok installed"},
        })

    @staticmethod
    def template_programs() -> set[str]:
        return {command.split()[0] for _runas, command in template_lines() if not command.startswith("/")}

    # The world.

    def db(self) -> dict:
        return json.loads((self.world / "db.json").read_text(encoding="utf-8"))

    def save_db(self, db: dict) -> None:
        (self.world / "db.json").write_text(json.dumps(db, indent=1, sort_keys=True), encoding="utf-8")

    def set_unit(self, unit: str, **props: str) -> None:
        db = self.db()
        db["units"].setdefault(unit, {"LoadState": "loaded", "UnitFileState": "disabled",
                                      "ActiveState": "inactive"}).update(props)
        self.save_db(db)

    def set_owner(self, rel: str, owner: str) -> None:
        db = self.db()
        db["owners"][str((self.root / rel).resolve())] = owner
        self.save_db(db)

    def state(self) -> dict[str, tuple[int, str]]:
        """Every path of the stack's state folders, the folders included:
        (mode, 'owner group')."""
        owners, out = self.db()["owners"], {}
        for top in STATE_DIRS:
            for path in (self.root / top, *sorted((self.root / top).rglob("*"))):
                out[str(path.relative_to(self.root))] = (path.lstat().st_mode & 0o7777,
                                                         owners.get(str(path.resolve()), "root root"))
        return out

    def calls(self) -> list[dict]:
        log = self.world / "calls.log"
        if not log.exists():
            return []
        return [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]

    def clear_calls(self) -> None:
        (self.world / "calls.log").unlink(missing_ok=True)

    def norm(self, arg: str) -> str:
        """A path in a logged call as the box sees it."""
        for prefix, shown in ((str(self.stage), "<stage>"), (str(self.worktmp), "<tmp>"), (str(self.root), "")):
            if arg.startswith(prefix + "/"):
                rest = arg[len(prefix):]
                return f"{shown}{rest}" if shown != "<tmp>" else "<tmp>"
        return arg

    def writes(self) -> list[list[str]]:
        return [[self.norm(a) for a in c["argv"]] for c in self.calls() if c["write"]]

    def tree(self) -> dict[str, tuple[int, bytes]]:
        """Every file under the root: (mode, bytes)."""
        out = {}
        for path in sorted(self.root.rglob("*")):
            if path.is_file():
                out[str(path.relative_to(self.root))] = (path.stat().st_mode, path.read_bytes())
        return out

    def snapshot(self) -> Path:
        saved = self.tmp / f"saved-{len(list(self.tmp.glob('saved-*')))}"
        shutil.copytree(self.root, saved / "root", symlinks=True)
        shutil.copyfile(self.world / "db.json", saved / "db.json")
        return saved

    def restore(self, saved: Path) -> None:
        shutil.rmtree(self.root)
        shutil.copytree(saved / "root", self.root, symlinks=True)
        shutil.copyfile(saved / "db.json", self.world / "db.json")
        self.clear_calls()

    # The scripts.

    def provision(self, *argv: str, **env: str) -> subprocess.CompletedProcess:
        args = list(argv) or ["--admin", ADMIN, "--ntp-file", str(self.stage / "ntp-server"),
                              "--roster-file", str(self.stage / "roster-ids")]
        return subprocess.run(["bash", str(self.stage / "provision-root.sh"), *args], env={**self.env, **env},
                              stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=120)

    def provisioned(self) -> subprocess.CompletedProcess:
        r = self.provision()
        self.assertEqual(r.returncode, 0, r.stderr)
        return r

    @staticmethod
    def steps(r: subprocess.CompletedProcess) -> dict[str, str]:
        out = {}
        for line in r.stdout.splitlines():
            m = re.fullmatch(r"([a-z-]+) (changed|unchanged)", line)
            if m:
                out[m.group(1)] = m.group(2)
        return out

    def deploy_units(self) -> None:
        """What deploy.sh installs: phase D's units, then daemon-reload."""
        for name in (*STACK, "iem-alarm@.service"):
            shutil.copyfile(HERE / name, self.root / "etc/systemd/system" / name)
        self.run_fake("systemctl", "daemon-reload")

    def run_fake(self, name: str, *args: str) -> None:
        subprocess.run([str(self.bin / name), *args], env=self.env, check=True, capture_output=True)

    def verify(self, **env: str) -> subprocess.CompletedProcess:
        with open(VERIFY, "rb") as script:
            return subprocess.run([sys.executable, "-"], stdin=script, env={**self.env, **env},
                                  capture_output=True, text=True, timeout=120)

    def verify_line(self, r: subprocess.CompletedProcess) -> dict[str, int]:
        lines = r.stdout.strip().splitlines()
        self.assertEqual(len(lines), 1, r.stdout)
        return json.loads(lines[0])["verify"]


class ProvisionRootTests(World):
    def test_the_first_run_changes_every_step_in_order_and_starts_nothing_of_the_stack(self) -> None:
        r = self.provisioned()
        self.assertEqual(self.steps(r), {step: "changed" for step in STEPS})
        self.assertEqual([line.split()[0] for line in r.stdout.splitlines() if line.endswith("changed")], list(STEPS))
        tmp_install = ["install", "-D", "-m", "0644", "-o", "root", "-g", "root", "--", "<tmp>"]
        napi_calls = []
        for pid in sorted(NAPI_THREADS):
            napi_calls += [["chrt", "-f", "-p", "60", pid], ["taskset", "-p", "-c", "0", pid]]
        self.assertEqual(self.writes(), [
            ["groupadd", "--system", "iemmixer"],
            ["useradd", "--system", "--gid", "iemmixer", "--no-create-home", "--home-dir", "/nonexistent",
             "--shell", "/usr/sbin/nologin", "iemmixer"],
            ["usermod", "--append", "--groups", "iemmixer", ADMIN],
            ["install", "-d", "-m", "0700", "-o", "root", "-g", "root", "--", "/etc/credstore.encrypted"],
            ["install", "-d", "-m", "0700", "-o", "iemmixer", "-g", "iemmixer", "--", "/var/lib/iemmixer/mint"],
            ["install", "-d", "-m", "0750", "-o", "iemmixer", "-g", "iemmixer", "--", "/var/lib/iemmixer"],
            ["install", "-d", "-m", "0750", "-o", "iemmixer", "-g", "iemmixer", "--", "/var/lib/iem-dante"],
            CHOWN_STATE,
            ["systemd-creds", "encrypt", "--name=pepper", "--with-key=auto", "-",
             "/etc/credstore.encrypted/iemmixer-pepper"],
            ["systemctl", "daemon-reload"],
            [*tmp_install, "/etc/systemd/journald.conf.d/50-iemmixer.conf"],
            ["install", "-d", "-m", "2755", "-o", "root", "-g", "systemd-journal", "--", "/var/log/journal"],
            ["systemctl", "restart", "systemd-journald.service"],
            ["apt-get", "purge", "-y", "rsyslog"],
            ["apt-get", "install", "-y", "--no-install-recommends", "linux-image-realtime"],
            [*tmp_install, GRUB],
            ["update-grub"],
            ["grub-set-default", RT_ENTRY],
            [*tmp_install, WORKQUEUE_CONF],
            [*tmp_install, "/etc/apt/apt.conf.d/20auto-upgrades"],
            ["apt-mark", "hold", "linux-generic", "linux-image-generic", "linux-headers-generic",
             "linux-image-realtime"],
            ["systemctl", "mask", "unattended-upgrades.service", "apt-daily.timer", "apt-daily-upgrade.timer"],
            ["systemctl", "mask", "sleep.target", "suspend.target", "hibernate.target", "hybrid-sleep.target"],
            ["fallocate", "-l", "8G", "--", "/swapfile"],
            ["mkswap", "--", "/swapfile"],
            ["swapon", "--", "/swapfile"],
            ["timedatectl", "set-ntp", "false"],
            ["systemctl", "disable", "--now", "systemd-timesyncd.service"],
            ["systemctl", "enable", "--now", "dantesync.service"],
            ["install", "-D", "-m", "0755", "-o", "root", "-g", "root", "--", "<stage>/napi-priority",
             "/usr/local/lib/iemmixer/napi-priority"],
            ["install", "-D", "-m", "0644", "-o", "root", "-g", "root", "--", "<stage>/iem-napi.service",
             "/etc/systemd/system/iem-napi.service"],
            ["systemctl", "daemon-reload"],
            ["systemctl", "mask", "--now", "irqbalance.service"],
            ["systemctl", "enable", "iem-napi.service"],
            ["systemctl", "restart", "iem-napi.service"],
            *napi_calls,
            [*tmp_install, DISPLAY_CONF],
            ["systemd-tmpfiles", "--create", DISPLAY_CONF],
            ["systemctl", "disable", "iem-dante.service"],
            ["install", "-D", "-m", "0440", "-o", "root", "-g", "root", "--", "<tmp>",
             "/etc/sudoers.d/.iemmixer-gen3.new"],
        ])
        # Nothing of the stack started, stopped or restarted.
        for call in self.writes():
            if call[0] == "systemctl" and call[1] in ("start", "restart", "stop", "enable"):
                self.assertFalse(set(call) & set(STACK), call)
        # The files, byte for byte, with their modes and owners.
        r = self.root
        journald = (r / "etc/systemd/journald.conf.d/50-iemmixer.conf").read_text(encoding="utf-8")
        self.assertTrue(journald.endswith("[Journal]\nStorage=persistent\nSystemMaxUse=1G\nMaxRetentionSec=30day\n"),
                        journald)
        upgrades = (r / "etc/apt/apt.conf.d/20auto-upgrades").read_text(encoding="utf-8")
        self.assertTrue(upgrades.endswith('APT::Periodic::Update-Package-Lists "0";\n'
                                          'APT::Periodic::Unattended-Upgrade "0";\n'), upgrades)
        self.assertEqual((r / "usr/local/lib/iemmixer/napi-priority").read_bytes(), NAPI.read_bytes())
        self.assertEqual((r / "usr/local/lib/iemmixer/napi-priority").stat().st_mode & 0o777, 0o755)
        self.assertEqual((r / "etc/systemd/system/iem-napi.service").read_bytes(), NAPI_UNIT.read_bytes())
        display = (r / DISPLAY_CONF.lstrip("/")).read_text(encoding="utf-8")
        self.assertTrue(display.endswith(POLL_OFF), display)
        self.assertEqual((r / DISPLAY_CONF.lstrip("/")).stat().st_mode & 0o777, 0o644)
        self.assertEqual((r / POLL).read_text(encoding="utf-8"), "N\n")
        self.assertEqual((r / "etc/sudoers.d/iemmixer-gen3").stat().st_mode & 0o777, 0o440)
        self.assertFalse((r / "etc/sudoers.d/.iemmixer-gen3.new").exists())
        self.assertEqual((r / "etc/credstore.encrypted/iemmixer-pepper").stat().st_mode & 0o777, 0o600)
        self.assertEqual((r / "etc/credstore.encrypted").stat().st_mode & 0o777, 0o700)
        self.assertEqual((r / "var/lib/iemmixer/mint").stat().st_mode & 0o777, 0o700)
        self.assertEqual(self.db()["owners"][str((r / "var/lib/iemmixer/mint").resolve())], "iemmixer iemmixer")
        self.assertEqual((r / "swapfile").stat().st_mode & 0o777, 0o600)
        self.assertIn("/swapfile none swap sw 0 0\n", (r / "etc/fstab").read_text(encoding="utf-8"))
        # Phase C's leftovers are gone; the band's other secrets stay.
        for unit in STACK:
            self.assertFalse((r / "etc/systemd/system" / f"{unit}.d").exists(), unit)
        secrets = r / "var/lib/iemmixer/site/secrets"
        self.assertEqual(sorted(p.name for p in secrets.iterdir()), ["jwt_secret", "vapid_private"])
        # The box's state.
        db = self.db()
        self.assertEqual(db["passwd"]["iemmixer"], "iemmixer:x:991:990::/nonexistent:/usr/sbin/nologin")
        self.assertEqual(db["group"]["iemmixer"], f"iemmixer:x:990:{ADMIN}")
        self.assertEqual(db["ntp"], "no")
        self.assertNotIn("rsyslog", db["packages"])
        for package in ("linux-generic", "linux-image-generic", "linux-headers-generic", "linux-image-realtime"):
            self.assertEqual(db["packages"][package], "hold ok installed")
        for package in ("linux-image-7.0.0-1-generic", f"linux-image-{REALTIME}"):
            self.assertEqual(db["packages"][package], "install ok installed")
        u = db["units"]
        self.assertEqual((u["dantesync.service"]["UnitFileState"], u["dantesync.service"]["ActiveState"]),
                         ("enabled", "active"))
        self.assertEqual((u["iem-napi.service"]["UnitFileState"], u["iem-napi.service"]["ActiveState"]),
                         ("enabled", "active"))
        self.assertEqual(u["systemd-timesyncd.service"]["ActiveState"], "inactive")
        for unit in STACK:
            self.assertEqual((u[unit]["UnitFileState"], u[unit]["ActiveState"], u[unit]["DropInPaths"]),
                             ("disabled", "inactive", ""), unit)
        # The kernel trees: the Dante interface's set, the other one's not.
        proc = r / "proc"
        self.assertEqual((r / f"sys/class/net/{IFACE}/threaded").read_text().strip(), "1")
        self.assertEqual((r / f"sys/class/net/{OTHER}/threaded").read_text().strip(), "0")
        for irq in IRQS:
            self.assertEqual((proc / "irq" / irq / "smp_affinity_list").read_text().strip(), "0")
        for irq in OTHER_IRQS:
            self.assertEqual((proc / "irq" / irq / "smp_affinity_list").read_text().strip(), "0-3")

    def test_the_second_run_changes_nothing(self) -> None:
        self.provisioned()
        before = self.tree()
        self.clear_calls()
        r = self.provisioned()
        self.assertEqual(self.steps(r), {step: "unchanged" for step in STEPS})
        self.assertEqual(self.writes(), [])
        self.assertEqual(self.tree(), before)

    def test_phase_c_s_state_becomes_the_service_user_s(self) -> None:
        # Live on the box (#72 phase D): phase C ran the stack as the admin, so
        # its state was the admin's, and deploy --pins' `pin set-engineer` as
        # iemmixer could not write site/secrets (os error 13). systemd's
        # StateDirectory= takes a tree over only at a unit's start, and the
        # PINs are given before any.
        before = self.state()
        self.assertEqual({owner for _mode, owner in before.values()}, {f"{ADMIN} {ADMIN}"})
        r = self.provisioned()
        self.assertEqual(self.steps(r).get("state"), "changed")
        after = self.state()
        # Nothing deleted but phase C's test pepper and its PIN hashes (the
        # phase-c step); mint is the folders step's.
        gone = {f"var/lib/iemmixer/site/secrets/{leaf}" for leaf in ("pepper.test", "pin_hashes.json")}
        self.assertEqual(set(after), (set(before) - gone) | {"var/lib/iemmixer/mint"})
        self.assertEqual({owner for _mode, owner in after.values()}, {"iemmixer iemmixer"})
        # The two folders 0750 (the stack shares the group), every other mode
        # as it was: the secrets stay 0600.
        for path, (mode, _owner) in after.items():
            want = {"var/lib/iemmixer": 0o750, "var/lib/iem-dante": 0o750, "var/lib/iemmixer/mint": 0o700}
            self.assertEqual(mode, want.get(path, before.get(path, (None,))[0]), path)
        for leaf in ("jwt_secret", "vapid_private"):
            self.assertEqual(after[f"var/lib/iemmixer/site/secrets/{leaf}"][0], 0o600)
        # One question before the chown and the same one after it, never
        # through a link.
        state_calls = [[self.norm(a) for a in c["argv"]] for c in self.calls() if c["argv"][0] in ("find", "chown")]
        self.assertEqual(state_calls, [FIND_STATE, CHOWN_STATE, FIND_STATE])
        # A second run asks once and sends no chown.
        self.clear_calls()
        r = self.provisioned()
        self.assertEqual(self.steps(r).get("state"), "unchanged")
        self.assertEqual([c["argv"][0] for c in self.calls() if c["argv"][0] in ("find", "chown")], ["find"])
        self.assertEqual(self.writes(), [])
        self.assertEqual(self.state(), after)
        # A file of another user, or of the service user in another group, is
        # taken over again; nothing else is written.
        for owner in ("root root", "iemmixer root"):
            with self.subTest(owner=owner):
                self.set_owner("var/lib/iem-dante/state.toml", owner)
                self.clear_calls()
                r = self.provisioned()
                self.assertEqual(self.steps(r).get("state"), "changed")
                self.assertEqual(self.writes(), [CHOWN_STATE])
                self.assertEqual(self.state(), after)

    def test_a_state_folder_that_is_a_link_is_refused_never_followed(self) -> None:
        elsewhere = self.root / "srv/zyxstate"
        elsewhere.parent.mkdir(parents=True)
        shutil.move(self.root / "var/lib/iem-dante", elsewhere)
        (self.root / "var/lib/iem-dante").symlink_to(elsewhere)
        r = self.provision()
        self.assertEqual(r.returncode, 1)
        self.assertIn("/var/lib/iem-dante is no folder", r.stderr)
        self.assertNotIn("state", self.steps(r))
        self.assertFalse([c for c in self.writes() if c[0] == "chown" or c[-1].startswith("/var/lib/iem")
                          and c[-1] != "/var/lib/iemmixer/mint"])

    def test_the_pepper_is_made_once_never_overwritten_and_its_bytes_go_nowhere(self) -> None:
        r = self.provisioned()
        data = bytes.fromhex((self.world / "creds-input.hex").read_text())
        self.assertEqual(len(data), 32)
        credential = self.root / "etc/credstore.encrypted/iemmixer-pepper"
        sealed = credential.read_bytes()
        self.assertEqual(sealed, b"FAKE-SEALED " + hashlib.sha256(data).hexdigest().encode() + b"\n")
        # Its bytes are in no file under the root, on no command line, in no
        # output (raw or hex).
        for path in self.root.rglob("*"):
            if path.is_file():
                self.assertNotIn(data, path.read_bytes(), path)
        calls = (self.world / "calls.log").read_bytes()
        self.assertNotIn(data.hex().encode(), calls)
        self.assertNotIn(json.dumps(data.decode("latin-1"))[1:-1].encode(), calls)
        self.assertNotIn(data.hex(), r.stdout + r.stderr)
        # Never made again, never overwritten: a second run, and a run on a
        # box whose credential came another way.
        self.clear_calls()
        self.provisioned()
        self.assertEqual(credential.read_bytes(), sealed)
        credential.write_bytes(b"another credential\n")
        self.provisioned()
        self.assertEqual(credential.read_bytes(), b"another credential\n")
        self.assertFalse([c for c in self.writes() if c[0] == "systemd-creds"])

    def test_an_empty_or_open_credential_is_refused_never_overwritten(self) -> None:
        credential = self.root / "etc/credstore.encrypted/iemmixer-pepper"
        write(credential, b"", 0o600)
        r = self.provision()
        self.assertEqual(r.returncode, 1)
        self.assertIn("never overwritten", r.stderr)
        self.assertEqual(credential.read_bytes(), b"")
        self.assertFalse([c for c in self.writes() if c[0] == "systemd-creds"])
        write(credential, b"sealed elsewhere\n", 0o644)
        r = self.provision()
        self.assertEqual(r.returncode, 1)
        self.assertIn("is 644 root, not 600 root", r.stderr)
        self.assertEqual(credential.read_bytes(), b"sealed elsewhere\n")

    def test_refusals_change_nothing(self) -> None:
        def no_dantesync() -> None:
            (self.root / "usr/local/bin/dantesync").unlink()

        def other_ntp() -> None:
            write(self.root / "etc/dantesync/config.json", json.dumps({"ntp_server": "other-master.test"}))

        def not_json() -> None:
            write(self.root / "etc/dantesync/config.json", "{not json")

        def not_root() -> None:
            db = self.db()
            db["uid"] = "1000"
            self.save_db(db)

        def cpus(n: int):
            def apply() -> None:
                db = self.db()
                db["nproc"] = n
                self.save_db(db)
            return apply

        cases = [
            (not_root, "not root"),
            (no_dantesync, "dantesync is not installed"),
            (other_ntp, "a follower of another NTP server"),
            (not_json, "cannot be read (JSONDecodeError)"),
            (lambda: self.set_unit("ptp4l.service", UnitFileState="enabled"), "ptp4l.service is enabled or running"),
            (lambda: self.set_unit("phc2sys.service", ActiveState="active"), "phc2sys.service is enabled or running"),
            (lambda: self.set_unit("iem-engine.service", ActiveState="active"), "iem-engine.service is active"),
            (lambda: self.set_unit("iem-dante.service", ActiveState="activating"), "iem-dante.service is activating"),
            (cpus(2), "the box has fewer than 4 CPUs"),
            (cpus(33), "the box has more than 32 CPUs"),
        ]
        saved = self.snapshot()
        for change, want in cases:
            with self.subTest(want=want):
                self.restore(saved)
                change()
                before, db = self.tree(), self.db()
                r = self.provision()
                self.assertEqual(r.returncode, 1, r.stdout)
                self.assertIn(want, r.stderr)
                self.assertIn("nothing changed", r.stderr)
                self.assertEqual(self.writes(), [])
                self.assertEqual(self.tree(), before)
                self.assertEqual(self.db(), db)
                self.assertEqual(self.steps(r), {})

    def test_a_bad_command_line_is_exit_2_and_changes_nothing(self) -> None:
        ntp, roster = str(self.stage / "ntp-server"), str(self.stage / "roster-ids")
        cases = [
            [],
            ["--admin", ADMIN, "--ntp-file", ntp],
            ["--admin", "root", "--ntp-file", ntp, "--roster-file", roster],
            ["--admin", "iemmixer", "--ntp-file", ntp, "--roster-file", roster],
            ["--admin", "Bad Name", "--ntp-file", ntp, "--roster-file", roster],
            ["--admin", ADMIN, "--ntp-file", str(self.tmp / "missing"), "--roster-file", roster],
            ["--admin", ADMIN, "--ntp-file", ntp, "--roster-file", roster, "--extra"],
        ]
        for argv in cases:
            with self.subTest(argv=argv):
                self.clear_calls()
                r = self.provision(*argv) if argv else subprocess.run(
                    ["bash", str(self.stage / "provision-root.sh")], env=self.env, capture_output=True, text=True)
                self.assertEqual(r.returncode, 2, r.stderr)
                self.assertEqual(self.calls(), [])
        write(self.stage / "roster-ids", "member1\nnot an id\n")
        r = self.provision()
        self.assertEqual(r.returncode, 2)
        self.assertIn("not a plain member id", r.stderr)

    def test_without_the_dante_interface_napi_is_enabled_and_applied_once_deploy_wrote_it(self) -> None:
        env = self.root / "etc/iemmixer/iem-dante.env"
        env.unlink()
        r = self.provisioned()
        self.assertEqual(self.steps(r)["napi"], "changed")
        self.assertIn("napi: no Dante interface yet", r.stdout)
        self.assertNotIn(["systemctl", "restart", "iem-napi.service"], self.writes())
        self.assertEqual(self.db()["units"]["iem-napi.service"]["UnitFileState"], "enabled")
        self.assertEqual((self.root / f"sys/class/net/{IFACE}/threaded").read_text().strip(), "0")
        # deploy.sh writes the interface; the next run applies and reads back.
        write(env, f"IEM_DANTE_IFACE={IFACE}\n")
        self.clear_calls()
        r = self.provisioned()
        self.assertEqual(self.steps(r)["napi"], "changed")
        self.assertIn(["systemctl", "restart", "iem-napi.service"], self.writes())
        self.assertEqual((self.root / f"sys/class/net/{IFACE}/threaded").read_text().strip(), "1")

    def test_a_priority_that_no_longer_holds_is_set_again_and_one_that_cannot_hold_fails(self) -> None:
        self.provisioned()
        write(self.root / "proc/irq" / IRQS[1] / "smp_affinity_list", "0-3\n")
        self.clear_calls()
        r = self.provisioned()
        self.assertEqual(self.steps(r)["napi"], "changed")
        self.assertIn(["systemctl", "restart", "iem-napi.service"], self.writes())
        self.assertEqual((self.root / "proc/irq" / IRQS[1] / "smp_affinity_list").read_text().strip(), "0")
        # A thread's priority that does not take: the run fails loudly.
        write(self.root / "proc" / "4102" / "stat", stat_line("4102", NAPI_THREADS["4102"], 0, 0))
        r = self.provision(FAKE_CHRT_IGNORED="1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("iem-napi", r.stderr)
        self.assertEqual(self.steps(r).get("napi"), None)

    def test_display_polling_is_turned_off_kept_off_and_a_box_without_it_has_nothing_to_turn_off(self) -> None:
        # Live 4 on the box (#72 phase D): with no screen attached, the i915
        # driver's output poll (every ~10 s, waking the GPU each time) held a
        # CPU for ~2 ms, and the engine's pump, woken onto that CPU, missed
        # its blocks: 66 re-bootstraps in 2 h, none in 30 minutes with the
        # poll off.
        self.provisioned()
        poll = self.root / POLL
        self.assertEqual(poll.read_text(encoding="utf-8"), "N\n")
        # Set back on (by hand, or a kernel that ignored the file at boot):
        # the next run sets it again and changes nothing else.
        write(poll, "Y\n")
        self.clear_calls()
        r = self.provisioned()
        self.assertEqual(self.steps(r)["display"], "changed")
        self.assertEqual(self.writes(), [["systemd-tmpfiles", "--create", DISPLAY_CONF]])
        self.assertEqual(poll.read_text(encoding="utf-8"), "N\n")
        # One that does not take fails the run loudly.
        write(poll, "Y\n")
        r = self.provision(FAKE_TMPFILES_IGNORED="1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("display output polling is still on", r.stderr)
        self.assertIsNone(self.steps(r).get("display"))
        # One systemd-tmpfiles refuses (it exits 73 when it cannot write a
        # value) fails the run the same way, with the step's own words.
        r = self.provision(FAKE_TMPFILES_RC="73")
        self.assertEqual(r.returncode, 1)
        self.assertIn(f"systemd-tmpfiles --create {DISPLAY_CONF} failed (exit 73)", r.stderr)
        self.assertIsNone(self.steps(r).get("display"))
        # A box without the parameter (no display driver): the file is
        # written, nothing to apply or read back.
        shutil.rmtree(self.root / "sys/module/drm_kms_helper")
        (self.root / DISPLAY_CONF.lstrip("/")).unlink()
        self.clear_calls()
        r = self.provisioned()
        self.assertEqual(self.steps(r)["display"], "changed")
        self.assertIn("display: no display output polling on this box", r.stdout)
        self.assertNotIn("systemd-tmpfiles", [c[0] for c in self.writes()])
        self.assertTrue((self.root / DISPLAY_CONF.lstrip("/")).read_text(encoding="utf-8").endswith(POLL_OFF))

    def test_swap_is_sized_to_the_memory_and_a_box_with_swap_keeps_its_own(self) -> None:
        for mem_kb, want in ((900_000, "2G"), (3_500_000, "4G"), (16_123_456, "8G")):
            with self.subTest(mem_kb=mem_kb):
                (self.root / "swapfile").unlink(missing_ok=True)
                write(self.root / "proc/swaps", SWAPS_HEADER)
                write(self.root / "proc/meminfo", f"MemTotal:       {mem_kb} kB\n")
                self.clear_calls()
                self.provisioned()
                self.assertIn(["fallocate", "-l", want, "--", "/swapfile"], self.writes())
        fstab = (self.root / "etc/fstab").read_text(encoding="utf-8")
        self.assertEqual(fstab.count("/swapfile none swap sw 0 0\n"), 1)
        # A box with its own active swap keeps it.
        (self.root / "swapfile").unlink()
        write(self.root / "proc/swaps", SWAPS_HEADER + "/swap.img\tfile\t\t4194300\t\t0\t\t-2\n")
        self.clear_calls()
        r = self.provisioned()
        self.assertEqual(self.steps(r)["swap"], "unchanged")
        self.assertFalse([c for c in self.writes() if c[0] in ("fallocate", "mkswap", "swapon")])

    def test_no_site_value_is_printed(self) -> None:
        r = self.provisioned()
        r2 = self.provision()
        for out in (r.stdout + r.stderr, r2.stdout + r2.stderr):
            for value in (NTP, ADMIN, IFACE):
                self.assertNotIn(value, out)


class RealtimeTests(World):
    """The realtime step (#72 phase F): measured on the box, the PREEMPT_RT
    kernel with CPUs 2-3 isolated removed every engine overrun; provision
    makes it the boot state a reboot keeps."""

    def grub(self) -> str:
        return (self.root / GRUB.lstrip("/")).read_text(encoding="utf-8")

    def workqueue(self) -> str:
        return (self.root / WORKQUEUE_CONF.lstrip("/")).read_text(encoding="utf-8")

    def test_the_first_run_makes_the_realtime_kernel_and_the_isolated_cores_the_boot_state(self) -> None:
        r = self.provisioned()
        self.assertEqual(self.steps(r)["realtime"], "changed")
        self.assertEqual(r.stdout.splitlines().count(AFTER_REBOOT), 1)
        writes = self.writes()
        at = writes.index([*TMP_INSTALL, GRUB])
        self.assertEqual(writes[at - 1:at + 4], [
            ["apt-get", "install", "-y", "--no-install-recommends", "linux-image-realtime"],
            [*TMP_INSTALL, GRUB],
            ["update-grub"],
            ["grub-set-default", RT_ENTRY],
            [*TMP_INSTALL, WORKQUEUE_CONF],
        ])
        self.assertEqual([c[0] for c in writes].count("update-grub"), 1)
        # grub's two keys set, every other line as it was; the file 0644.
        self.assertEqual(self.grub(), GRUB_AFTER)
        self.assertEqual((self.root / GRUB.lstrip("/")).stat().st_mode & 0o777, 0o644)
        self.assertEqual([line for line in self.workqueue().splitlines(keepends=True) if not line.startswith("#")],
                         [WORKQUEUE_LINE])
        self.assertEqual((self.root / WORKQUEUE_CONF.lstrip("/")).stat().st_mode & 0o777, 0o644)
        db = self.db()
        self.assertEqual(db["grubenv"], {"saved_entry": RT_ENTRY})
        # Installed, and held by the apt step of the same run.
        self.assertEqual(db["packages"]["linux-image-realtime"], "hold ok installed")
        self.assertTrue((self.root / "boot" / f"vmlinuz-{REALTIME}").is_file())
        # Nothing rebooted.
        self.assertFalse([c for c in writes if "reboot" in c or c[0] in ("reboot", "shutdown")])
        # A second run changes nothing; the box still runs the generic kernel.
        self.clear_calls()
        r = self.provisioned()
        self.assertEqual(self.steps(r)["realtime"], "unchanged")
        self.assertEqual(self.writes(), [])
        self.assertEqual(r.stdout.splitlines().count(AFTER_REBOOT), 1)
        # Once the box booted it, the line is gone.
        self.save_db({**self.db(), "uname": REALTIME})
        r = self.provisioned()
        self.assertEqual(self.steps(r)["realtime"], "unchanged")
        self.assertNotIn(AFTER_REBOOT, r.stdout)

    def test_the_isolated_cores_are_the_box_s_last_two(self) -> None:
        self.save_db({**self.db(), "nproc": 8})
        self.provisioned()
        self.assertIn('\nGRUB_CMDLINE_LINUX_DEFAULT="isolcpus=domain,managed_irq,6-7 nohz_full=6-7 rcu_nocbs=6-7 '
                      'irqaffinity=0-5"\n', self.grub())
        self.assertTrue(self.workqueue().endswith("\nw /sys/devices/virtual/workqueue/cpumask - - - - 3f\n"))
        # verify reads the same count and agrees.
        self.deploy_units()
        r = self.verify()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.verify_line(r)["realtime"], 1)

    def test_an_installed_package_is_kept_and_the_newest_realtime_kernel_is_saved(self) -> None:
        # 7.0.0-9 sorts after 7.0.0-38 as text, before it as a version.
        older = "7.0.0-9-realtime"
        db = self.db()
        for kernel in (older, REALTIME):
            db["packages"][f"linux-image-{kernel}"] = "install ok installed"
            write(self.root / "boot" / f"vmlinuz-{kernel}", "synthetic realtime kernel\n", 0o600)
        db["packages"]["linux-image-realtime"] = "install ok installed"
        db["grubenv"] = {"next_entry": "", "saved_entry": f"Advanced options for Ubuntu>Ubuntu, with Linux {older}"}
        self.save_db(db)
        r = self.provisioned()
        self.assertEqual(self.steps(r)["realtime"], "changed")
        self.assertFalse([c for c in self.writes() if c[:2] == ["apt-get", "install"]])
        self.assertEqual([c for c in self.writes() if c[0] == "grub-set-default"], [["grub-set-default", RT_ENTRY]])
        self.assertEqual(self.db()["grubenv"], {"next_entry": "", "saved_entry": RT_ENTRY})

    def test_a_saved_entry_changed_since_is_set_again_and_nothing_else_is_written(self) -> None:
        self.provisioned()
        db = self.db()
        db["grubenv"]["saved_entry"] = "0"
        self.save_db(db)
        self.clear_calls()
        r = self.provisioned()
        self.assertEqual(self.steps(r)["realtime"], "changed")
        self.assertEqual(self.writes(), [["grub-set-default", RT_ENTRY]])

    def test_grub_s_two_keys_are_set_once_and_every_other_line_is_kept(self) -> None:
        write(self.root / GRUB.lstrip("/"), "# a comment naming GRUB_DEFAULT=2\n"
              "GRUB_TIMEOUT=5\n"
              '  GRUB_CMDLINE_LINUX_DEFAULT="quiet"\n'
              'GRUB_CMDLINE_LINUX="console=ttyS0"\n'
              'GRUB_CMDLINE_LINUX_DEFAULT="$GRUB_CMDLINE_LINUX_DEFAULT splash"\n'
              "#GRUB_DEFAULT=saved\n", 0o644)
        self.provisioned()
        self.assertEqual(self.grub(), "# a comment naming GRUB_DEFAULT=2\n"
                         "GRUB_TIMEOUT=5\n"
                         f'GRUB_CMDLINE_LINUX_DEFAULT="{RT_CMDLINE}"\n'
                         'GRUB_CMDLINE_LINUX="console=ttyS0"\n'
                         "#GRUB_DEFAULT=saved\n"
                         "GRUB_DEFAULT=saved\n")
        self.deploy_units()
        self.assertEqual(self.verify_line(self.verify())["realtime"], 1)

    def test_a_failed_update_grub_or_a_saved_entry_that_does_not_take_fails_the_run(self) -> None:
        r = self.provision(FAKE_UPDATE_GRUB_RC="1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("update-grub failed (exit 1)", r.stderr)
        self.assertIsNone(self.steps(r).get("realtime"))
        r = self.provision(FAKE_GRUB_SET_IGNORED="1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("grub's saved entry is not the realtime kernel after grub-set-default", r.stderr)
        self.assertIsNone(self.steps(r).get("realtime"))


class SudoersTests(World):
    def sudoers_rules(self, path: Path) -> list[tuple[str, str, str]]:
        """(user, runas, command) of every rule, as sudo's own parser reads it."""
        cvtsudoers = shutil.which("cvtsudoers") or "/usr/bin/cvtsudoers"
        r = subprocess.run([cvtsudoers, "-f", "json", str(path)], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        rules = []
        for spec in json.loads(r.stdout)["User_Specs"]:
            users = [u["username"] for u in spec["User_List"]]
            for cmnd in spec["Cmnd_Specs"]:
                self.assertEqual(cmnd["Options"], [{"authenticate": False}])
                runas = [u["username"] for u in cmnd["runasusers"]]
                for c in cmnd["Commands"]:
                    rules.append((users[0], runas[0], c["command"]))
                self.assertEqual((len(users), len(runas)), (1, 1))
        return rules

    def real_visudo(self, path: Path) -> subprocess.CompletedProcess:
        visudo = next((p for p in ("/usr/sbin/visudo", "/usr/bin/visudo", "/sbin/visudo") if os.access(p, os.X_OK)),
                      None)
        self.assertIsNotNone(visudo, "this machine has no visudo; CI's runner has it")
        return subprocess.run([visudo, "-cf", str(path)], capture_output=True, text=True)

    def test_every_line_of_the_template_is_in_the_sudoers_file(self) -> None:
        self.provisioned()
        sudoers = self.root / "etc/sudoers.d/iemmixer-gen3"
        self.assertEqual(self.real_visudo(sudoers).returncode, 0)
        want = []
        for runas, command in template_lines():
            for text in sorted(expand(command)):
                program, _, args = text.partition(" ")
                if not program.startswith("/"):
                    program = ("/usr/sbin/" if program == "visudo" else "/usr/bin/") + program
                want.append((ADMIN, runas, f"{program} {args}"))
        rules = self.sudoers_rules(sudoers)
        self.assertEqual(sorted(rules), sorted(set(want)))
        self.assertEqual(len(rules), len(set(rules)), "a rule twice")
        # The PIN commands' '=' and ':' are escaped as sudoers needs them.
        self.assertIn(r"--property\=LoadCredentialEncrypted\=pepper\:/etc/credstore.encrypted/iemmixer-pepper",
                      sudoers.read_text(encoding="utf-8"))
        # Live 3's by-hand lines are in the template.
        lines = set(template_lines())
        self.assertIn(("root", "systemctl restart iem-engine"), lines)
        self.assertIn(("root", "systemctl reboot"), lines)

    def test_the_rule_escapes_what_sudoers_reads_as_syntax_and_a_bare_command_takes_no_arguments(self) -> None:
        write(self.stage / "sudo-lines.txt", "# a synthetic template\n\n"
              "root | systemctl show --property=Id,User a:b#c\n"
              "iemmixer | cat [x]*?!^\n"
              "root | /opt/zyx/bin/tool\n"
              "root | stat -c %a.%U.%s /x/{slot}\n")
        self.provisioned()
        sudoers = self.root / "etc/sudoers.d/iemmixer-gen3"
        self.assertEqual(self.real_visudo(sudoers).returncode, 0)
        self.assertEqual(self.sudoers_rules(sudoers), [
            (ADMIN, "root", "/usr/bin/systemctl show --property=Id,User a:b#c"),
            (ADMIN, "iemmixer", r"/usr/bin/cat \[x\]\*\?\!\^"),
            (ADMIN, "root", '/opt/zyx/bin/tool ""'),
            (ADMIN, "root", "/usr/bin/stat -c %a.%U.%s /x/a"),
            (ADMIN, "root", "/usr/bin/stat -c %a.%U.%s /x/b"),
        ])

    def test_a_template_the_rule_cannot_be_made_from_is_refused_with_nothing_changed(self) -> None:
        cases = [
            ("root | zyxtool --now\n", "names zyxtool, which is in none of sudo's secure_path folders"),
            ("root | cat {nothing}\n", "a placeholder that is none of"),
            ("admin | cat x\n", "runs a command as neither root nor iemmixer"),
            ("root | cat 'x y'\n", "a quote or a backslash"),
            ("root cat x\n", "not '<runas> | <command>'"),
        ]
        for text, want in cases:
            with self.subTest(want=want):
                write(self.stage / "sudo-lines.txt", text)
                before = self.tree()
                self.clear_calls()
                r = self.provision()
                self.assertEqual(r.returncode, 1, r.stdout)
                self.assertIn(want, r.stderr)
                self.assertIn("nothing changed", r.stderr)
                self.assertEqual(self.writes(), [])
                self.assertEqual(self.tree(), before)


class NapiTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.root = self.tmp / "root"
        self.world = self.tmp / "world"
        self.world.mkdir()
        (self.world / "db.json").write_text("{}", encoding="utf-8")
        bin_dir = self.tmp / "bin"
        bin_dir.mkdir()
        for name in ("chrt", "taskset"):
            p = bin_dir / name
            p.write_text(f"#!{sys.executable}\n{FAKE}", encoding="utf-8")
            p.chmod(0o755)
        kernel_trees(self.root, applied=False)
        self.env = {**os.environ, "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
                    "FAKE_WORLD": str(self.world), "FAKE_ROOT": str(self.root),
                    "IEM_NAPI_SYS": str(self.root / "sys"), "IEM_NAPI_PROC": str(self.root / "proc"),
                    "IEM_NAPI_IFACE_WAIT": "3", "IEM_NAPI_THREAD_WAIT": "3"}

    def napi(self, *args: str, **env: str) -> subprocess.CompletedProcess:
        return subprocess.run(["bash", str(NAPI), *(args or (IFACE,))], env={**self.env, **env},
                              stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=60)

    def calls(self) -> list[list[str]]:
        log = self.world / "calls.log"
        return [json.loads(line)["argv"] for line in log.read_text().splitlines()] if log.exists() else []

    def read(self, rel: str) -> str:
        return (self.root / rel).read_text(encoding="utf-8").strip()

    def test_it_sets_every_value_and_reads_it_back(self) -> None:
        r = self.napi()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("2 NAPI threads SCHED_FIFO 60 on core 0; 2 interrupts on core 0", r.stdout)
        self.assertEqual(self.read(f"sys/class/net/{IFACE}/threaded"), "1")
        for pid in NAPI_THREADS:
            fields = self.read(f"proc/{pid}/stat").rsplit(") ", 1)[1].split()
            self.assertEqual((fields[38], fields[37]), ("1", "60"), pid)
            self.assertIn("Cpus_allowed_list:\t0", self.read(f"proc/{pid}/status"))
        for irq in IRQS:
            self.assertEqual(self.read(f"proc/irq/{irq}/smp_affinity_list"), "0")
        expected = []
        for pid in sorted(NAPI_THREADS):
            expected += [["chrt", "-f", "-p", "60", pid], ["taskset", "-p", "-c", "0", pid]]
        self.assertEqual(self.calls(), expected)
        # The other interface (its name starts with the Dante one's) and
        # every other thread are untouched.
        self.assertEqual(self.read(f"sys/class/net/{OTHER}/threaded"), "0")
        for pid in OTHER_THREADS:
            self.assertIn("Cpus_allowed_list:\t0-3", self.read(f"proc/{pid}/status"))
        for irq in OTHER_IRQS:
            self.assertEqual(self.read(f"proc/irq/{irq}/smp_affinity_list"), "0-3")
        self.assertNotIn(IFACE, r.stdout + r.stderr)

    def test_check_only_reads_and_names_each_value_that_does_not_hold(self) -> None:
        r = self.napi("--check", IFACE)
        self.assertEqual(r.returncode, 1)
        self.assertIn("threaded NAPI is off", r.stderr)
        for pid in NAPI_THREADS:
            self.assertIn(f"NAPI thread {pid}: policy and priority 0 0, not 1 60", r.stderr)
            self.assertIn(f"NAPI thread {pid} runs on cores 0-3, not 0", r.stderr)
        for irq in IRQS:
            self.assertIn(f"interrupt {irq} runs on cores 0-3, not 0", r.stderr)
        self.assertEqual(self.calls(), [])
        self.assertEqual(self.read(f"sys/class/net/{IFACE}/threaded"), "0")
        kernel_trees(self.root, applied=True)
        r = self.napi("--check", IFACE)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stderr, "")
        self.assertEqual(self.calls(), [])

    def test_a_value_that_does_not_take_fails(self) -> None:
        r = self.napi(FAKE_CHRT_IGNORED="1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("policy and priority 0 0, not 1 60", r.stderr)
        self.assertIn("did not read back as set", r.stderr)

    def test_no_interface_no_thread_or_no_interrupt_fails_within_its_wait(self) -> None:
        r = self.napi("ethz7")
        self.assertEqual(r.returncode, 1)
        self.assertIn("the Dante interface did not appear within 3 tenths of a second", r.stderr)
        for pid in NAPI_THREADS:
            shutil.rmtree(self.root / "proc" / pid)
        r = self.napi()
        self.assertEqual(r.returncode, 1)
        self.assertIn("NAPI threads did not appear within 3 tenths of a second", r.stderr)
        kernel_trees(self.root, applied=False)
        write(self.root / "proc/interrupts", INTERRUPTS.replace(f"{IFACE}-TxRx", "other-TxRx"))
        r = self.napi()
        self.assertEqual(r.returncode, 1)
        self.assertIn("no interrupt of /proc/interrupts names the Dante interface", r.stderr)

    def test_a_bad_command_line_is_exit_2(self) -> None:
        for args in (["--check"], [IFACE, IFACE], ["bad name"], ["a" * 16]):
            with self.subTest(args=args):
                r = subprocess.run(["bash", str(NAPI), *args], env=self.env, capture_output=True, text=True)
                self.assertEqual(r.returncode, 2, r.stderr)
        r = self.napi(IEM_NAPI_THREAD_WAIT="soon")
        self.assertEqual(r.returncode, 2)
        self.assertEqual(self.calls(), [])

    def test_the_unit_runs_it_at_boot_before_the_clock_and_the_stack(self) -> None:
        unit = parse_unit(NAPI_UNIT)
        self.assertEqual(unit["Service"]["Type"], ["oneshot"])
        self.assertEqual(unit["Service"]["RemainAfterExit"], ["yes"])
        self.assertEqual(unit["Service"]["EnvironmentFile"], ["/etc/iemmixer/iem-dante.env"])
        self.assertEqual(unit["Service"]["ExecStart"], ["/usr/local/lib/iemmixer/napi-priority ${IEM_DANTE_IFACE}"])
        self.assertEqual(unit["Service"]["SendSIGKILL"], ["no"])
        self.assertEqual(unit["Unit"]["ConditionPathExists"], ["/etc/iemmixer/iem-dante.env"])
        self.assertEqual(unit["Unit"]["Before"][0].split(),
                         ["dantesync.service", "iem-engine.service", "iem-dante.service"])
        self.assertEqual(unit["Install"]["WantedBy"], ["multi-user.target"])
        # Ordering only: a failure here holds back neither the clock nor the stack.
        for key in ("Requires", "RequiredBy", "BindsTo", "PartOf", "Requisite"):
            self.assertNotIn(key, unit["Unit"])
            self.assertNotIn(key, unit["Install"])


class VerifyTests(World):
    def good_box(self) -> None:
        self.provisioned()
        self.deploy_units()
        self.clear_calls()

    def test_a_provisioned_and_deployed_box_passes(self) -> None:
        self.good_box()
        r = self.verify(FAKE_TPM2_RC="0")
        self.assertEqual(r.returncode, 0, r.stderr)
        line = self.verify_line(r)
        self.assertEqual(line, {"pass": 1, **{check: 1 for check in CHECKS}, "tpm2": 1, "realtime_running": 0})
        self.assertEqual(r.stderr, "")
        # Read only: no call it made changes anything.
        self.assertEqual(self.writes(), [])
        self.assertEqual(self.verify_line(self.verify())["tpm2"], 0)

    def test_each_check_fails_alone_naming_itself(self) -> None:
        self.good_box()
        saved = self.snapshot()

        def db_change(change):
            def apply() -> None:
                db = self.db()
                change(db)
                self.save_db(db)
            return apply

        def unit(name: str, **props: str):
            return db_change(lambda db: db["units"][name].update(props))

        def drop_in() -> None:
            write(self.root / "etc/systemd/system/iem-server.service.d/10-user.conf", f"[Service]\nUser={ADMIN}\n")
            self.run_fake("systemctl", "daemon-reload")

        def test_pepper() -> None:
            # The service user's own file: only no_test_pepper fails.
            write(self.root / "var/lib/iemmixer/site/secrets/pepper.test", "x\n")
            self.set_owner("var/lib/iemmixer/site/secrets/pepper.test", "iemmixer iemmixer")

        cases = {
            "user": (db_change(lambda db: db["group"].update(iemmixer="iemmixer:x:990:")),
                     "the admin is not in the group iemmixer"),
            "units": (drop_in, "iem-server.service does not run as iemmixer"),
            "boot": (unit("iem-engine.service", UnitFileState="enabled"), "iem-engine.service is enabled at boot"),
            "sudoers": (lambda: (self.root / "etc/sudoers.d/iemmixer-gen3").unlink(), "sudo -n visudo -c fails"),
            "pepper": (lambda: (self.root / "etc/credstore.encrypted/iemmixer-pepper").chmod(0o644),
                       "the pepper's credential is not 600 root"),
            "no_test_pepper": (test_pepper, "a plain test pepper is on the box"),
            "state": (lambda: self.set_owner("var/lib/iem-dante/state.toml", f"{ADMIN} {ADMIN}"),
                      "a file of the stack's state is not the service user's"),
            "journal": (db_change(lambda db: db["packages"].update(rsyslog="install ok installed")),
                        "rsyslog is installed"),
            "apt": (db_change(lambda db: db["packages"].update({"linux-image-generic": "install ok installed"})),
                    "an installed kernel package is not held"),
            "sleep": (unit("suspend.target", UnitFileState="static", LoadState="loaded"),
                      "suspend.target is not masked"),
            "swap": (lambda: write(self.root / "proc/swaps", SWAPS_HEADER), "no active swap"),
            "clock": (db_change(lambda db: db["units"].update({"chrony.service": {
                "LoadState": "loaded", "UnitFileState": "enabled", "ActiveState": "active"}})),
                "chrony.service is enabled or running beside dantesync"),
            "napi": (lambda: write(self.root / "proc/irq" / IRQS[0] / "smp_affinity_list", "0-3\n"),
                     "an interrupt of the Dante interface is not on core 0 alone"),
            "display": (lambda: write(self.root / POLL, "Y\n"), "display output polling is on"),
            "realtime": (lambda: write(self.root / GRUB.lstrip("/"),
                                       GRUB_AFTER.replace(RT_CMDLINE, RT_CMDLINE + " quiet"), 0o644),
                         "/etc/default/grub does not boot the saved entry with the engine's CPUs isolated"),
        }
        self.assertEqual(sorted(cases), sorted(CHECKS))
        for check, (change, reason) in cases.items():
            with self.subTest(check=check):
                self.restore(saved)
                change()
                r = self.verify()
                self.assertEqual(r.returncode, 1, r.stdout)
                line = self.verify_line(r)
                self.assertEqual({k: v for k, v in line.items() if v == 0 and k not in INFO},
                                 {"pass": 0, check: 0})
                self.assertEqual(r.stderr.splitlines(), [f"verify_box: {check}: {reason}"])

    def test_more_ways_each_check_fails(self) -> None:
        self.good_box()
        saved = self.snapshot()
        threads = self.root / "proc" / "4101"

        def unit(name: str, **props: str):
            return lambda: self.set_unit(name, **props)

        def db_update(key: str, changes: dict):
            def apply() -> None:
                db = self.db()
                for name, value in changes.items():
                    if value is None:
                        db[key].pop(name, None)
                    else:
                        db[key][name] = value
                self.save_db(db)
            return apply

        def cpus(n: int):
            return lambda: self.save_db({**self.db(), "nproc": n})

        grub, workqueue = self.root / GRUB.lstrip("/"), self.root / WORKQUEUE_CONF.lstrip("/")
        grub_reason = "/etc/default/grub does not boot the saved entry with the engine's CPUs isolated"
        saved_reason = "grub's saved entry is not the newest realtime kernel"
        workqueue_reason = f"no {WORKQUEUE_CONF} keeping kernel work off the isolated CPUs"
        cases = [
            ("realtime", db_update("packages", {"linux-image-realtime": None}), "linux-image-realtime is not installed"),
            ("apt", db_update("packages", {"linux-image-realtime": "install ok installed"}),
             "an installed kernel package is not held"),
            ("realtime", lambda: write(grub, GRUB_AFTER + "GRUB_DEFAULT=0\n", 0o644), grub_reason),
            ("realtime", lambda: write(grub, GRUB_AFTER.replace("GRUB_DEFAULT=saved\n", ""), 0o644), grub_reason),
            ("realtime", cpus(8), grub_reason),
            ("realtime", cpus(2), "the box has fewer than 4 or more than 32 CPUs"),
            ("realtime", lambda: (self.root / "boot" / f"vmlinuz-{REALTIME}").unlink(), "no realtime kernel in /boot"),
            ("realtime", db_update("grubenv", {"saved_entry": "0"}), saved_reason),
            ("realtime", db_update("grubenv", {"saved_entry": None}), saved_reason),
            # A newer realtime kernel than the saved one: the next provision run saves it.
            ("realtime", lambda: write(self.root / "boot/vmlinuz-7.0.0-40-realtime", "synthetic\n", 0o600),
             saved_reason),
            ("realtime", lambda: workqueue.unlink(), workqueue_reason),
            ("realtime", lambda: write(workqueue, WORKQUEUE_LINE.replace(" 3\n", " f\n")), workqueue_reason),
            ("units", lambda: (self.root / "etc/systemd/system/iem-alarm@.service").unlink(),
             "iem-alarm@.service is not installed"),
            ("units", unit("iem-dante.service", LoadState="not-found"), "iem-dante.service is not installed"),
            ("clock", unit("dantesync.service", ActiveState="failed"), "dantesync is not enabled and running"),
            ("clock", unit("ptp4l.service", ActiveState="active"), "ptp4l.service is enabled or running"),
            ("napi", unit("irqbalance.service", ActiveState="active"), "irqbalance runs"),
            ("napi", unit("iem-napi.service", UnitFileState="disabled"), "iem-napi is not enabled and active"),
            ("napi", lambda: write(threads / "stat", stat_line("4101", NAPI_THREADS["4101"], 1, 50)),
             "a NAPI thread is not SCHED_FIFO 60"),
            ("napi", lambda: write(threads / "status", "Cpus_allowed_list:\t0-1\n"),
             "a NAPI thread is not on core 0 alone"),
            ("napi", lambda: write(self.root / f"sys/class/net/{IFACE}/threaded", "0\n"),
             "threaded NAPI is off on the Dante interface"),
            ("napi", lambda: write(self.root / "etc/iemmixer/iem-dante.env", "IEM_DANTE_IFACE=\n"),
             "no Dante interface in /etc/iemmixer/iem-dante.env"),
            ("journal", lambda: write(self.root / "etc/systemd/journald.conf.d/50-iemmixer.conf",
                                      "[Journal]\nStorage=auto\n"), "journald is not persistent, 1G, 30day"),
            ("apt", unit("apt-daily.timer", UnitFileState="enabled"), "apt-daily.timer is not masked"),
            ("apt", lambda: write(self.root / "etc/apt/apt.conf.d/20auto-upgrades",
                                  'APT::Periodic::Unattended-Upgrade "1";\n'),
             "20auto-upgrades does not turn unattended upgrades off"),
            ("user", db_set_passwd(self, "iemmixer:x:991:990::/nonexistent:/bin/bash"), "the service user can log in"),
            ("pepper", lambda: (self.root / "etc/credstore.encrypted/iemmixer-pepper").unlink(),
             "the pepper's credential is not there (sudo -n stat)"),
            ("state", lambda: self.set_owner("var/lib/iemmixer/site/secrets/jwt_secret", "iemmixer root"),
             "a file of the stack's state is not the service user's"),
            ("state", lambda: write(self.root / "var/lib/iemmixer/engine/backup.json", "x\n"),
             "a file of the stack's state is not the service user's"),
            ("state", lambda: self.set_owner("var/lib/iemmixer", f"{ADMIN} {ADMIN}"),
             "a file of the stack's state is not the service user's"),
            ("state", lambda: shutil.rmtree(self.root / "var/lib/iem-dante"),
             "the stack's state folders cannot be searched (sudo -n find)"),
            ("display", lambda: (self.root / DISPLAY_CONF.lstrip("/")).unlink(),
             "no /etc/tmpfiles.d/iemmixer-display.conf turning display output polling off"),
            ("display", lambda: write(self.root / DISPLAY_CONF.lstrip("/"), POLL_OFF.replace(" N", " Y")),
             "no /etc/tmpfiles.d/iemmixer-display.conf turning display output polling off"),
        ]
        for check, change, reason in cases:
            with self.subTest(reason=reason):
                self.restore(saved)
                change()
                r = self.verify()
                self.assertEqual(r.returncode, 1, r.stdout)
                self.assertEqual(self.verify_line(r)[check], 0)
                self.assertIn(f"verify_box: {check}: {reason}", r.stderr)

    def test_display_fails_when_the_polling_cannot_be_read(self) -> None:
        # A box whose sudoers predates the display step's template line.
        self.good_box()
        r = self.verify(FAKE_SUDO_CAT_REFUSED="1")
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertEqual(self.verify_line(r)["display"], 0)
        self.assertIn("verify_box: display: the display output polling cannot be read (sudo -n cat)", r.stderr)

    def test_a_box_without_display_output_polling_passes_display_on_the_file_alone(self) -> None:
        self.good_box()
        shutil.rmtree(self.root / "sys/module/drm_kms_helper")
        r = self.verify()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.verify_line(r)["display"], 1)
        self.assertNotIn("sudo -n cat /" + POLL, [" ".join(c["argv"]) for c in self.calls()])

    def test_realtime_running_is_1_only_on_a_realtime_kernel_booted_with_the_isolated_cores(self) -> None:
        # Information beside the checks: provision sets the boot state, a
        # reboot runs it; verify passes either way.
        self.good_box()
        flag, cmdline = self.root / "sys/kernel/realtime", self.root / "proc/cmdline"
        booted = f"BOOT_IMAGE=/boot/vmlinuz-{REALTIME} root=UUID=0000 ro {RT_CMDLINE}\n"
        cases = [
            ("1\n", booted, 1),
            ("0\n", booted, 0),
            ("1\n", "BOOT_IMAGE=/boot/vmlinuz-7.0.0-1-generic root=UUID=0000 ro quiet splash\n", 0),
            ("1\n", booted.replace("nohz_full=2-3", "nohz_full=3"), 0),
            ("1\n", booted.replace("irqaffinity=0-1", "irqaffinity=0-11"), 0),
        ]
        for realtime, text, want in cases:
            with self.subTest(realtime=realtime, cmdline=text):
                write(flag, realtime)
                write(cmdline, text)
                r = self.verify()
                self.assertEqual(r.returncode, 0, r.stderr)
                line = self.verify_line(r)
                self.assertEqual((line["pass"], line["realtime"], line["realtime_running"]), (1, 1, want))
        flag.unlink()
        self.assertEqual(self.verify_line(self.verify())["realtime_running"], 0)

    def test_before_the_first_phase_d_deploy_units_fails(self) -> None:
        # Phase C's units without their drop-ins name no user: verify says so
        # until deploy.sh installs the service user's units.
        self.provisioned()
        r = self.verify()
        self.assertEqual(r.returncode, 1)
        line = self.verify_line(r)
        self.assertEqual({k for k, v in line.items() if v == 0 and k not in INFO}, {"pass", "units"})
        self.assertIn("verify_box: units: iem-engine.service does not run as iemmixer", r.stderr)

    def test_every_sudo_line_verify_sends_is_in_the_template(self) -> None:
        self.good_box()
        self.verify()
        sent = [" ".join(c["argv"]) for c in self.calls() if c["argv"][0] == "sudo"]
        self.assertEqual(len(sent), 5)
        self.assertIn(" ".join(["sudo", "-n", *FIND_STATE]), sent)
        self.assertIn("sudo -n cat /" + POLL, sent)
        allowed = {(runas, text) for runas, command in template_lines() for text in expand(command)}
        for runas, command, _listed in sudo_lines(sent):
            self.assertIn((runas, command), allowed, command)

    def test_the_line_names_no_value(self) -> None:
        self.good_box()
        write(self.root / "proc/swaps", SWAPS_HEADER)
        r = self.verify()
        for value in (ADMIN, IFACE, NTP):
            self.assertNotIn(value, r.stdout + r.stderr)
        line = self.verify_line(r)
        self.assertTrue(all(isinstance(v, int) for v in line.values()), line)
        self.assertEqual(list(line), ["pass", *CHECKS, *INFO])


def db_set_passwd(case: World, line: str):
    def apply() -> None:
        db = case.db()
        db["passwd"]["iemmixer"] = line
        case.save_db(db)
    return apply


# provision.sh's box: ssh and scp in a temporary folder (its home).
FAKE_SSH = r'''
import json, os, shutil, sys
box = os.environ["FAKE_BOX"]
args = sys.argv[1:]
data = sys.stdin.buffer.read() if not sys.stdin.isatty() else b""
with open(os.path.join(box, "calls.log"), "a", encoding="utf-8") as f:
    f.write(json.dumps({"argv": ["ssh"] + args, "stdin": data.decode("utf-8", "replace")}) + "\n")
while args and args[0] == "-o":
    args = args[2:]
cmd = " ".join(args[1:])
home = os.path.join(box, "home")
env = os.environ.get
if cmd == "id -un":
    print(env("FAKE_ADMIN", "boxadmin"))
elif cmd == "systemctl is-active iem-server iem-engine iem-dante":
    states = env("FAKE_STATES", "inactive inactive inactive").split()
    print("\n".join(states))
    sys.exit(0 if all(s == "active" for s in states) else 3)
elif cmd.startswith("mkdir -p "):
    os.makedirs(os.path.join(home, cmd.split()[-1]), exist_ok=True)
elif cmd.startswith("sudo -n bash .cache/iem-gen3-provision/provision-root.sh "):
    print("user changed")
    print("sudoers changed")
    sys.exit(int(env("FAKE_ROOT_RC", "0")))
elif cmd == "python3 -":
    print('{"verify":{"pass":1}}')
    sys.stderr.write(env("FAKE_VERIFY_ERR", ""))
    sys.exit(int(env("FAKE_VERIFY_RC", "0")))
else:
    sys.stderr.write("unexpected command: " + cmd + "\n")
    sys.exit(97)
'''

FAKE_SCP = r'''
import json, os, shutil, sys
box = os.environ["FAKE_BOX"]
args = sys.argv[1:]
with open(os.path.join(box, "calls.log"), "a", encoding="utf-8") as f:
    f.write(json.dumps({"argv": ["scp"] + args, "stdin": ""}) + "\n")
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
    shutil.copy(src, os.path.join(folder, os.path.basename(src)))
if os.environ.get("FAKE_FLAG_ON") == "scp":
    open(os.environ["IEMMIXER_EVENT_NOW"], "w").write("now\n")
'''

HOST = "tester@gen3.test"
BOX_ADMIN = "boxadmin"
STAGE = ".cache/iem-gen3-provision"


class ProvisionShTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.box = self.tmp / "box"
        (self.box / "home").mkdir(parents=True)
        bin_dir = self.tmp / "bin"
        bin_dir.mkdir()
        for name, body in (("ssh", FAKE_SSH), ("scp", FAKE_SCP)):
            p = bin_dir / name
            p.write_text(f"#!{sys.executable}\n{body}", encoding="utf-8")
            p.chmod(0o755)
        self.flag = self.tmp / "config" / "EVENT-NOW"
        self.flag.parent.mkdir()
        self.ops = self.tmp / "gen3-dantesync.json"
        self.ops.write_text(json.dumps({"ntp_server": NTP, "system": {"gm_allowlist": ["10.0.0.11"]}}),
                            encoding="utf-8")
        self.env = {**os.environ, "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}", "FAKE_BOX": str(self.box),
                    "IEMMIXER_EVENT_NOW": str(self.flag)}

    def provision(self, *argv: str, **env: str) -> subprocess.CompletedProcess:
        args = list(argv) or ["--host", HOST, "--ntp-from", str(self.ops)]
        return subprocess.run(["bash", str(PROVISION), *args], env={**self.env, **env}, stdin=subprocess.DEVNULL,
                              capture_output=True, text=True, timeout=60)

    def calls(self) -> list[dict]:
        log = self.box / "calls.log"
        return [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []

    def remote(self) -> list[str]:
        out = []
        for call in self.calls():
            args = call["argv"][1:]
            if call["argv"][0] != "ssh":
                out.append("scp")
                continue
            while args and args[0] == "-o":
                args = args[2:]
            self.assertEqual(args[0], HOST)
            out.append(" ".join(args[1:]))
        return out

    def test_it_copies_runs_the_root_script_then_verify_and_exits_with_verify(self) -> None:
        r = self.provision()
        self.assertEqual(r.returncode, 0, r.stderr)
        root_line = (f"sudo -n bash {STAGE}/provision-root.sh --admin {BOX_ADMIN} --ntp-file {STAGE}/ntp-server "
                     f"--roster-file {STAGE}/roster-ids")
        self.assertEqual(self.remote(), ["id -un", "systemctl is-active iem-server iem-engine iem-dante",
                                         f"mkdir -p {STAGE}", "scp", root_line, "python3 -"])
        self.assertTrue(all("BatchMode=yes" in c["argv"] for c in self.calls()))
        stage = self.box / "home" / STAGE
        for name in STAGE_FILES:
            self.assertEqual((stage / name).read_bytes(), (HERE / name).read_bytes(), name)
        self.assertEqual((stage / "ntp-server").read_text(), NTP + "\n")
        self.assertEqual((stage / "roster-ids").read_text().split(), roster_people())
        self.assertEqual(sorted(p.name for p in stage.iterdir()), sorted([*STAGE_FILES, "ntp-server", "roster-ids"]))
        # verify_box.py goes to the box's python3 on stdin, nothing left there.
        self.assertEqual(self.calls()[-1]["stdin"], VERIFY.read_text(encoding="utf-8"))
        self.assertIn("user changed", r.stdout)
        self.assertIn('{"verify":{"pass":1}}', r.stdout)
        # No site value printed.
        for value in (NTP, BOX_ADMIN, "10.0.0.11"):
            self.assertNotIn(value, r.stdout + r.stderr)
        # Its only sudo is the root script's, which runs on the admin's own
        # rule: it has no line in the template (it would be all of root).
        lines = sudo_lines(self.remote())
        self.assertEqual(lines, [("root", root_line[len("sudo -n "):], False)])
        self.assertNotIn("bash", {command.split()[0] for _runas, command in template_lines()})
        # verify's exit is its own.
        for rc in ("1", "3"):
            r = self.provision(FAKE_VERIFY_RC=rc, FAKE_VERIFY_ERR="verify_box: units: x\n")
            self.assertEqual(r.returncode, int(rc))
            self.assertIn("verify_box: units", r.stderr)

    def test_the_flag_refuses_before_any_call_and_right_before_the_root_script(self) -> None:
        self.flag.write_text("now\n")
        r = self.provision()
        self.assertEqual(r.returncode, 1)
        self.assertIn("an event is on", r.stderr)
        self.assertIn("nothing was sent", r.stderr)
        self.assertEqual(self.calls(), [])
        self.flag.unlink()
        r = self.provision(FAKE_FLAG_ON="scp")
        self.assertEqual(r.returncode, 1)
        self.assertIn("nothing changed on the box", r.stderr)
        self.assertEqual(self.remote()[-1], "scp")

    def test_a_running_stack_or_a_root_login_refuses_before_anything_is_sent(self) -> None:
        for states, want in (("active inactive inactive", "iem-server is 'active'"),
                             ("inactive inactive activating", "iem-dante is 'activating'"),
                             ("inactive deactivating inactive", "iem-engine is 'deactivating'")):
            with self.subTest(states=states):
                (self.box / "calls.log").unlink(missing_ok=True)
                r = self.provision(FAKE_STATES=states)
                self.assertEqual(r.returncode, 1)
                self.assertIn(want, r.stderr)
                self.assertIn("nothing was sent", r.stderr)
                self.assertEqual(self.remote(), ["id -un", "systemctl is-active iem-server iem-engine iem-dante"])
        (self.box / "calls.log").unlink()
        r = self.provision(FAKE_ADMIN="root")
        self.assertEqual(r.returncode, 1)
        self.assertIn("logs in as root", r.stderr)
        self.assertEqual(self.remote(), ["id -un"])

    def test_a_root_script_that_fails_runs_no_verify(self) -> None:
        r = self.provision(FAKE_ROOT_RC="1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("provision-root.sh failed on the box", r.stderr)
        self.assertNotIn("python3 -", self.remote())

    def test_a_bad_command_line_or_ops_file_is_exit_2_with_nothing_sent(self) -> None:
        bad_json = self.tmp / "bad.json"
        bad_json.write_text("{not json", encoding="utf-8")
        no_ntp = self.tmp / "no-ntp.json"
        no_ntp.write_text(json.dumps({"ntp_server": " "}), encoding="utf-8")
        cases = [
            [],
            ["--host", HOST],
            ["--ntp-from", str(self.ops)],
            ["--host", "-oProxyCommand=x", "--ntp-from", str(self.ops)],
            ["--host", HOST, "--ntp-from", str(self.tmp / "missing.json")],
            ["--host", HOST, "--ntp-from", str(bad_json)],
            ["--host", HOST, "--ntp-from", str(no_ntp)],
            ["--host", HOST, "--ntp-from", str(self.ops), "--now"],
        ]
        for argv in cases:
            with self.subTest(argv=argv):
                r = self.provision(*argv) if argv else subprocess.run(
                    ["bash", str(PROVISION)], env=self.env, capture_output=True, text=True)
                self.assertEqual(r.returncode, 2, r.stderr)
                self.assertEqual(self.calls(), [])
                self.assertNotIn(NTP, r.stderr)


if __name__ == "__main__":
    unittest.main()
