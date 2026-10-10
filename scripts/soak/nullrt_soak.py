#!/usr/bin/env python3
"""The 72 h NullRt soak on a dev box (S7 design note §8, #10; `.claude/rules/soak.md`).

It runs CI's NullRt engine, the server and the soak client (the `e2e` job's
artifact `nullrt-soak-linux-<sha>`) against the synthetic test site, samples
every 60 s for N hours (default 72) and judges long-run growth, not real time
(NullRt counts no missed periods):

- each process's RSS, open fds and threads (`/proc/<pid>/status`, `/proc/<pid>/fd`);
- the engine's `Status` counters (`late`, `missed`, `callbacks`, ...), read on
  an observe connection to its control pipe (read-only: an observer may send
  no change, the engine refuses one);
- the harness counters, the client's own summary file, rewritten every minute.

Green (design §8): no exit (the engine and the server run to the end, every
client leg ends 0); RSS growth after hour 1 at most 16 MB per process; fds and
threads flat (the last hour's lowest never above the highest of hour 1 to 2:
a leak raises the floor, a transient only a peak); every leg passes CI's
harness check (`soak_verdict.harness_problems`) with gaps 0; and the run
lasted its hours. Everything else is information.

The client runs at most `MAX_SECONDS` (36 000 s) per run and never opens a
socket twice, so the run is cut into equal legs of at most LEG_MAX_S (8 h),
one client process each, LEG_GAP_S apart (the server ends the last leg's
sessions, the member listen tap is one slot): 72 h is nine legs. A leg's
harness counters are its own; the seconds between two legs belong to no leg.

Provisioning is the e2e job's: config/test-site.toml copied into a new site
folder, an engineer and a member PIN drawn per run (never printed, never
written but as the server's argon2 hashes, set through `iem-server pin`).
The client signs its engineer token with the server's own JWT secret
(`--jwt-secret-file`, the PC soak's mode), so the run logs in nowhere.

Every stop is a graceful stop (program spec I8): the server gets SIGTERM, its
own stop signal; a client leg ends by itself (at its seconds, or about 12 s
after the server closed its sockets); the engine gets `Shutdown` on its
control pipe as a supervisor. Each wait is bounded, and a process that does
not end within it is left running, named on stderr and in the verdict (red),
never ended by force (nor through subprocess's run with a timeout, which ends
its child by force when the time is up: Popen and communicate instead). Children run in a session of their own, so a Ctrl-C at
the terminal reaches only this script, which then stops them gracefully.

Stdlib only, Linux only. Exit 0 green, 1 red, 2 a usage or setup error before
anything started."""
from __future__ import annotations

import argparse
import datetime as dt
import json
import math
import os
import re
import secrets
import shutil
import signal
import socket
import struct
import subprocess
import sys
import threading
import time
import tomllib
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "iem-pc"))
import soak_verdict  # noqa: E402  (the PC soak's harness check, reused per leg)

HOUR = 3600
LEG_MAX_S = 8 * HOUR       # the client's MAX_SECONDS is 36 000 s; legs stay below it
RSS_GROWTH_MAX_KB = 15625  # 16 MB (16 000 000 bytes) in /proc's kB (1024 bytes): 15 625 kB, toward red
MEMBER = "member9"         # as CI: a member no spec changes
MAX_FRAME = 1 << 20        # iem_engine_proto::MAX_FRAME
PROTO = 1                  # the engine protocol (iem_engine_proto::PROTO)
STATUS_KEYS = ("callbacks", "late", "missed", "overruns", "resets", "faulted", "parked", "process_max_us",
               "tap_overruns", "talkback_dropped", "cmd_backlog", "frames")
HARNESS_KEYS = ("complete", "seconds", "frames", "expected_frames", "decode_errors", "gaps", "max_gap_ms",
                "meter_frames", "reconnects", "no_source", "error")
BINARIES = ("iem-engine", "iem-server", "iem-soakclient")
READY_S = 30          # the engine's socket and the server's /api/version, each
SERVER_STOP_S = 30    # SIGTERM to the server's end
CLIENT_END_S = 30     # a client after its server closed: idle 10 s + close 2 s, with room
ENGINE_STOP_S = 30    # Shutdown to the engine's end
LEG_OVERRUN_S = 300   # a leg's client past its seconds: its build check, opens and closes, with room
LEG_GAP_S = 5.0       # between two legs: the server ends the last leg's sessions (the one member listen tap)
PIN_S = 60            # each `iem-server pin` call
EVERY_MAX_S = 600     # a sample at least every 10 min: hour 1 to 2 and the last hour hold several
TICK_S = 1.0          # the loop's look at the processes and the stop request
SHA = re.compile(r"[0-9a-f]{40}")


class SetupError(Exception):
    """A usage or setup problem found before or while starting (exit 2 when nothing ran yet)."""


# ---- pure: /proc, frames, legs, the verdict ----

def parse_proc_status(text: str) -> dict:
    """`VmRSS` (kB) and `Threads` from a `/proc/<pid>/status` text; a field
    that is missing (a zombie has no VmRSS) is None."""
    out: dict = {"rss_kb": None, "threads": None}
    for line in text.splitlines():
        key, _, value = line.partition(":")
        if key == "VmRSS":
            out["rss_kb"] = int(value.split()[0])
        elif key == "Threads":
            out["threads"] = int(value.strip())
    return out


def frame(msg: dict) -> bytes:
    """One engine control-pipe frame: a little-endian u32 length, then JSON."""
    body = json.dumps(msg, separators=(",", ":")).encode()
    return struct.pack("<I", len(body)) + body


def split_frames(buf: bytes) -> tuple[list[dict], bytes]:
    """The whole frames at the start of `buf` (each a JSON object) and the
    rest; a length over MAX_FRAME or a body that is no JSON object raises
    ValueError (the observer then reconnects)."""
    msgs, at = [], 0
    while len(buf) - at >= 4:
        (n,) = struct.unpack_from("<I", buf, at)
        if n > MAX_FRAME:
            raise ValueError(f"engine frame of {n} bytes exceeds {MAX_FRAME}")
        if len(buf) - at - 4 < n:
            break
        doc = json.loads(buf[at + 4:at + 4 + n])
        if not isinstance(doc, dict):
            raise ValueError("engine frame is no JSON object")
        msgs.append(doc)
        at += 4 + n
    return msgs, buf[at:]


def status_fields(msg: dict) -> dict:
    """The sampled fields of an engine `Status` message."""
    return {k: msg.get(k) for k in STATUS_KEYS}


def harness_fields(summary) -> dict | None:
    """The sampled fields of a client summary (None for no summary)."""
    if not isinstance(summary, dict):
        return None
    return {k: summary.get(k) for k in HARNESS_KEYS}


def plan_legs(total_s: int, leg_max_s: int | None = None) -> list[int]:
    """`total_s` cut into equal legs of at most `leg_max_s` (LEG_MAX_S by
    default; whole seconds, the first ones a second longer when it does not
    divide)."""
    if total_s < 1:
        raise ValueError("a soak lasts at least 1 s")
    n = math.ceil(total_s / (leg_max_s or LEG_MAX_S))
    base, extra = divmod(total_s, n)
    return [base + (1 if i < extra else 0) for i in range(n)]


def judge_process(name: str, points: list[tuple[float, dict]]) -> tuple[list[str], dict]:
    """RSS growth after hour 1 and flat fds and threads for one process, from
    its samples `(t, {rss_kb, fds, threads})` with `t` from its first sample.
    Returns (failures, numbers); a process that lived under 2 h is not judged
    (`judged: false`): the caller decides whether that is red."""
    pts = [(t, p) for t, p in points if all(isinstance(p.get(k), int) for k in ("rss_kb", "fds", "threads"))]
    if not pts:
        return [f"{name}: no readable sample"], {"judged": False}
    t0 = pts[0][0]
    pts = [(t - t0, p) for t, p in pts]
    end = pts[-1][0]
    nums: dict = {"lived_h": math.floor(end / HOUR * 100) / 100, "rss_max_kb": max(p["rss_kb"] for _, p in pts)}
    if end < 2 * HOUR:
        return [], {**nums, "judged": False}
    after = [p for t, p in pts if t >= HOUR]
    base = [p for t, p in pts if HOUR <= t < 2 * HOUR]
    last = [p for t, p in pts if t >= end - HOUR]
    if not base:   # a stall of an hour, or samples too far apart: nothing to judge against
        return [f"{name}: no readable sample in hour 1 to 2"], {**nums, "judged": False}
    growth = max(p["rss_kb"] for p in after) - after[0]["rss_kb"]
    nums.update(judged=True, rss_growth_kb=growth)
    fails = []
    if growth > RSS_GROWTH_MAX_KB:
        fails.append(f"{name}: RSS grew {growth} kB after hour 1 (at most {RSS_GROWTH_MAX_KB} kB, 16 MB)")
    for key in ("fds", "threads"):   # a leak raises the floor; a transient (a backup's thread) only a peak
        b, floor = max(p[key] for p in base), min(p[key] for p in last)
        nums[f"{key}_hour2_max"], nums[f"{key}_last_hour_min"] = b, floor
        nums[f"{key}_last_hour_max"] = max(p[key] for p in last)
        if floor > b:
            fails.append(f"{name}: {key} not flat: the last hour's lowest {floor}, above hour 1 to 2's highest {b}")
    return fails, nums


# CI's harness problem for the frames' share (soak_verdict): information only here.
FRAMES_SHARE = "frames "


def leg_failures(legs: list[dict]) -> list[str]:
    """Each leg: its client ended 0, then CI's harness check on its summary
    (`soak_verdict.harness_problems`: complete, its seconds less one, gaps 0,
    no reconnect, at least one frame, a meter frame), each problem named with
    its leg; the frames' share is information only (`frames_percent_min`)."""
    fails = []
    for leg in legs:
        n = leg["leg"]
        if leg.get("exit") != 0:
            fails.append(f"leg {n}: the client ended {leg.get('exit')!r}, not 0")
        fails += [f"leg {n}: {p}" for p in soak_verdict.harness_problems(leg.get("summary"), leg["seconds"] - 1, 0)
                  if not p.startswith(FRAMES_SHARE)]
    return fails


def frames_percent_min(legs: list[dict]) -> float | None:
    """The lowest leg's share of the expected listen frames, rounded down at
    0.01 %: information only (NullRt paces itself with the box's scheduler, so
    the share measures the test backend, not iemmixer; design §8 gates gaps)."""
    shares = []
    for leg in legs:
        s = leg.get("summary")
        if isinstance(s, dict) and type(s.get("frames")) is int and type(s.get("expected_frames")) is int \
                and s["expected_frames"] > 0:
            shares.append(s["frames"] * 10_000 // s["expected_frames"] / 100)
    return min(shares) if shares else None


def verdict(samples: list[dict], legs: list[dict], stops: dict, total_s: int, every_s: float,
            early: str | None = None) -> dict:
    """The run's verdict (pure). `samples`: the JSONL records; `legs`: per leg
    `{leg, seconds, exit, summary}`; `stops`: per process `{exit, ended}` of
    the graceful stop; `early`: why the run stopped before its hours, if it
    did. Every check runs; the first failure leads the summary."""
    fails: list[str] = []
    numbers: dict = {}
    if early:
        fails.append(f"stopped early: {early}")
    span = samples[-1]["t"] - samples[0]["t"] if samples else 0.0
    numbers["sampled_h"] = math.floor(span / HOUR * 100) / 100   # down: never reads as the hours
    if span < total_s - every_s:
        fails.append(f"sampled {numbers['sampled_h']} h of the {total_s / HOUR:g} h")
    for name in ("engine", "server"):   # no exit: both run at every sample
        gone = next((s for s in samples if ((s.get("procs") or {}).get(name) or {}).get("alive") is not True), None)
        if gone is not None:
            code = ((gone.get("procs") or {}).get(name) or {}).get("exit")
            fails.append(f"the {name} exited ({code!r}) by {gone['t']:.0f} s")
    fails += leg_failures(legs)
    numbers["frames_percent_min"] = frames_percent_min(legs)
    planned = len(plan_legs(total_s))
    if len(legs) < planned:
        fails.append(f"{len(legs)} of the {planned} legs ended")
    series: dict[str, list] = {}
    for s in samples:
        for name, p in (s.get("procs") or {}).items():
            if p and p.get("alive"):
                series.setdefault(f"{name}#{p.get('pid')}", []).append((s["t"], p))
    for key, points in sorted(series.items()):
        name = key.split("#", 1)[0]
        f, nums = judge_process(key, points)
        fails += f
        numbers[key] = nums
        if name in ("engine", "server") and not nums.get("judged"):
            fails.append(f"{key}: lived {nums.get('lived_h')} h, under the 2 h a judgement needs")
    for name, st in sorted(stops.items()):
        if not st.get("ended"):
            fails.append(f"the {name} did not end within its graceful stop (left running)")
        elif name != "client" and st.get("exit") != 0:
            fails.append(f"the {name} ended {st.get('exit')!r} at its graceful stop, not 0")
    last = next((e for e in samples[::-1] if e.get("engine")), {})
    numbers["engine_last_status"] = last.get("engine")
    numbers["legs"] = len(legs)
    green = not fails
    head = "green" if green else f"red: {fails[0]}"
    return {"conclusion": "success" if green else "failure", "summary": head, "failures": fails,
            "numbers": numbers}


# ---- the processes ----

def proc_sample(pid: int, popen: subprocess.Popen) -> dict:
    """One process's figures from /proc; a process that ended is `alive: false` with its exit code."""
    code = popen.poll()
    if code is not None:
        return {"pid": pid, "alive": False, "exit": code}
    try:
        st = parse_proc_status(Path(f"/proc/{pid}/status").read_text(encoding="utf-8"))
        fds = len(os.listdir(f"/proc/{pid}/fd"))
    except OSError as e:   # it ended between poll and read: the next sample sees its exit
        return {"pid": pid, "alive": popen.poll() is None, "exit": popen.poll(), "error": type(e).__name__}
    return {"pid": pid, "alive": True, **st, "fds": fds}


class Observer:
    """The engine's latest `Status`, read on an observe connection (a thread).
    A lost connection is opened again at the next sample (`reconnects`)."""

    def __init__(self, path: str) -> None:
        self.path = path
        self.lock = threading.Lock()
        self.status: dict | None = None
        self.at: float | None = None
        self.reconnects = -1
        self.error: str | None = None
        self.stop = threading.Event()
        self.thread: threading.Thread | None = None

    def ensure(self) -> None:
        if self.thread is None or not self.thread.is_alive():
            self.reconnects += 1
            self.thread = threading.Thread(target=self.run, name="engine-observer", daemon=True)
            self.thread.start()

    def run(self) -> None:
        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
                s.settimeout(5)
                s.connect(self.path)
                s.sendall(frame({"type": "hello", "proto": PROTO, "role": "observe", "client": "nullrt-soak"}))
                buf = b""
                while not self.stop.is_set():
                    try:
                        chunk = s.recv(65536)
                    except socket.timeout:
                        continue
                    if not chunk:
                        raise ConnectionError("the engine closed the observe connection")
                    msgs, buf = split_frames(buf + chunk)
                    for m in msgs:
                        if m.get("type") == "status":
                            with self.lock:
                                self.status, self.at = status_fields(m), time.monotonic()
        except (OSError, ValueError) as e:
            with self.lock:
                self.error = f"{type(e).__name__}: {e}"
            log(f"engine observer: {self.error}")

    def read(self) -> dict | None:
        with self.lock:
            if self.status is None:
                return None
            return {**self.status, "age_s": round(time.monotonic() - self.at, 1)}


def engine_shutdown(path: str) -> str | None:
    """The engine's graceful stop: `Shutdown` as a supervisor on its control
    pipe. Returns None once sent and answered without an error, else why not."""
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.settimeout(10)
            s.connect(path)
            s.sendall(frame({"type": "hello", "proto": PROTO, "role": "supervisor", "client": "nullrt-soak"}))
            s.sendall(frame({"type": "request", "id": 1, "cmd": {"op": "shutdown"}}))
            buf, deadline = b"", time.monotonic() + 10
            while time.monotonic() < deadline:
                chunk = s.recv(65536)
                if not chunk:
                    return None   # it closed: it is ending
                msgs, buf = split_frames(buf + chunk)
                for m in msgs:
                    if m.get("type") == "reply" and m.get("id") == 1:
                        return None if m.get("error") is None else f"the engine refused: {m['error']}"
                    if m.get("type") in ("driver_released", "driver_parked"):
                        return None
            return "no reply to Shutdown within 10 s"
    except (OSError, ValueError) as e:
        return f"{type(e).__name__}: {e}"


def wait_end(popen: subprocess.Popen, seconds: float) -> int | None:
    try:
        return popen.wait(timeout=seconds)
    except subprocess.TimeoutExpired:
        return None


def log(text: str) -> None:
    print(f"{dt.datetime.now(dt.timezone.utc).strftime('%H:%M:%S')} {text}", file=sys.stderr, flush=True)


# ---- the run ----

class Run:
    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.bin = Path(args.bin)
        self.out = Path(args.out)
        self.site = self.out / "site"
        self.config = self.site / "iemmixer.toml"
        self.sock = str(self.out / "engine.sock")
        self.port = 0
        self.total_s = 0
        self.legs_plan: list[int] = []
        self.next_leg_at = 0.0
        self.engine: subprocess.Popen | None = None
        self.server: subprocess.Popen | None = None
        self.client: subprocess.Popen | None = None
        self.client_leg: dict | None = None
        self.logs: list = []
        self.legs: list[dict] = []
        self.samples: list[dict] = []
        self.stop_asked: str | None = None
        self.observer: Observer | None = None
        self.t0 = 0.0

    # Setup -------------------------------------------------------------

    def check(self) -> None:
        a = self.args
        if not SHA.fullmatch(a.build):
            raise SetupError("--build must be the artifact's full commit SHA (40 lower-case hex)")
        if not (math.isfinite(a.hours) and a.hours * HOUR >= 1):
            raise SetupError("--hours must be finite and at least 1 s")
        if not 0 < a.every <= EVERY_MAX_S:
            raise SetupError(f"--every must be above 0 and at most {EVERY_MAX_S} s")
        self.total_s = round(a.hours * HOUR)
        self.legs_plan = plan_legs(self.total_s)
        for name in BINARIES:
            p = self.bin / name
            if not (p.is_file() and os.access(p, os.X_OK)):
                raise SetupError(f"{p} is missing or not executable (an artifact download keeps no mode: chmod +x)")
        if not (Path(a.repo) / "config" / "test-site.toml").is_file():
            raise SetupError(f"{a.repo}/config/test-site.toml is missing: --repo names the iemmixer checkout")
        if self.out.exists():
            raise SetupError(f"{self.out} exists: each run gets a new folder")
        if len(self.sock.encode()) > 100:
            raise SetupError(f"{self.sock} is too long for a Unix socket: choose a shorter --out")

    def env(self, **extra: str) -> dict:
        env = {k: v for k, v in os.environ.items() if k != "IEM_SOAK_PIN"}   # both credentials would be exit 2
        env.update(IEMMIXER_CONFIG=str(self.config), RUST_LOG="info", **extra)
        return env

    def spawn(self, name: str, argv: list[str], env: dict) -> subprocess.Popen:
        f = open(self.out / f"{name}.log", "ab")   # closed in finish()
        self.logs.append(f)
        log(f"start {name}")
        return subprocess.Popen(argv, env=env, stdin=subprocess.DEVNULL, stdout=f, stderr=subprocess.STDOUT,
                                start_new_session=True)

    def provision(self) -> None:
        """The e2e job's: the synthetic site, an engineer and a member PIN per run."""
        self.out.mkdir(parents=True, mode=0o700)
        self.site.mkdir(mode=0o700)
        shutil.copyfile(Path(self.args.repo) / "config" / "test-site.toml", self.config)
        eng, mem = (f"{n:04d}" for n in secrets.SystemRandom().sample(range(10000), 2))
        site = tomllib.loads(self.config.read_text(encoding="utf-8"))
        server = str(self.bin / "iem-server")
        steps = [(["pin", "set-engineer"], eng)] + [(["pin", "set-member", m["id"]], mem)
                                                    for m in site["members"] if m["id"] != "engineer"]
        for argv, pin in steps:   # the PIN on stdin only, never in argv
            child = subprocess.Popen([server, *argv], env=self.env(), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE, text=True, start_new_session=True)
            try:
                _, err = child.communicate(pin + "\n", timeout=PIN_S)
            except subprocess.TimeoutExpired:   # never ended by force: named, left to end by itself
                raise SetupError(f"iem-server {' '.join(argv)} did not end within {PIN_S} s "
                                 f"(pid {child.pid}, left running)") from None
            if child.returncode != 0:   # its output names no PIN: the PIN never left stdin
                raise SetupError(f"iem-server {' '.join(argv)} failed (exit {child.returncode}): {err[-400:]}")
        with socket.socket() as s:   # a free port on every address: the server binds 0.0.0.0
            s.bind(("0.0.0.0", 0))
            self.port = s.getsockname()[1]

    def start(self) -> None:
        state = self.out / "engine-state"
        self.engine = self.spawn("engine", [str(self.bin / "iem-engine"), "run", "--site", str(self.config),
                                            "--state-dir", str(state), "--pipe", self.sock, "--sine", "1000"],
                                 self.env())
        self.ready(lambda: Path(self.sock).is_socket(), self.engine, "the engine's control pipe")
        self.server = self.spawn("server", [str(self.bin / "iem-server")],
                                 self.env(IEMMIXER_ENGINE_PIPE=self.sock, PORT=str(self.port)))
        answers: list[dict] = []
        self.ready(lambda: answers.append(self.version()) or answers[-1] is not None, self.server,
                   "the server's /api/version")
        got = answers[-1].get("git_hash")
        if not (isinstance(got, str) and len(got) >= 7 and self.args.build.startswith(got)):
            raise SetupError(f"the server names build {got!r}, not a prefix of --build {self.args.build}")
        if not (self.site / "secrets" / "jwt_secret").is_file():
            raise SetupError("the server wrote no secrets/jwt_secret next to the site")
        self.observer = Observer(self.sock)

    def ready(self, ok, popen: subprocess.Popen, what: str) -> None:
        deadline = time.monotonic() + READY_S
        while time.monotonic() < deadline:
            if popen.poll() is not None:
                raise SetupError(f"{what}: the process ended ({popen.returncode}); see its log in {self.out}")
            if ok():
                return
            time.sleep(0.5)
        raise SetupError(f"{what} not ready within {READY_S} s; see the logs in {self.out}")

    def version(self) -> dict | None:
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{self.port}/api/version", timeout=5) as r:
                doc = json.loads(r.read())
        except (OSError, ValueError):
            return None
        return doc if isinstance(doc, dict) else None

    # Legs and samples --------------------------------------------------

    def start_leg(self) -> None:
        n = len(self.legs) + 1
        seconds = self.legs_plan[n - 1]
        summary = self.out / f"soakclient-leg{n}.json"
        argv = [str(self.bin / "iem-soakclient"), "--base", f"http://127.0.0.1:{self.port}", "--direct",
                "--member", MEMBER, "--expect-build", self.args.build, "--seconds", str(seconds),
                "--out", str(summary), "--jwt-secret-file", str(self.site / "secrets" / "jwt_secret")]
        self.client = self.spawn(f"client-leg{n}", argv, self.env())
        self.client_leg = {"leg": n, "seconds": seconds, "summary_path": str(summary), "pid": self.client.pid,
                           "started": time.monotonic()}

    def end_leg(self, code: int | None) -> None:
        leg = self.client_leg
        self.legs.append({"leg": leg["leg"], "seconds": leg["seconds"], "pid": leg["pid"], "exit": code,
                          "summary": read_summary(Path(leg["summary_path"]))})
        log(f"leg {leg['leg']} ended ({code})")
        self.client, self.client_leg = None, None

    def sample(self) -> dict:
        self.observer.ensure()
        procs = {"engine": proc_sample(self.engine.pid, self.engine), "server": proc_sample(self.server.pid,
                                                                                          self.server)}
        harness = None
        if self.client is not None:
            procs["client"] = proc_sample(self.client.pid, self.client)
            harness = harness_fields(read_summary(Path(self.client_leg["summary_path"])))
        rec = {"t": round(time.monotonic() - self.t0, 1), "at": dt.datetime.now(dt.timezone.utc).isoformat(
            timespec="seconds"), "leg": self.client_leg["leg"] if self.client_leg else None, "procs": procs,
            "engine": self.observer.read(), "observer_reconnects": self.observer.reconnects,
            "harness": harness}
        self.samples.append(rec)
        with open(self.out / "samples.jsonl", "a", encoding="utf-8") as f:
            f.write(json.dumps(rec) + "\n")
        return rec

    def loop(self) -> str | None:
        """Legs back to back, a sample every `every` s; returns why it stopped early, or None."""
        self.t0 = time.monotonic()
        self.start_leg()
        self.sample()
        k = 1
        while True:
            if self.stop_asked:
                return self.stop_asked
            for name, p in (("engine", self.engine), ("server", self.server)):
                if p.poll() is not None:
                    self.sample()
                    return f"the {name} exited ({p.returncode})"
            if self.client is not None:
                code = self.client.poll()
                late = time.monotonic() - self.client_leg["started"] > self.client_leg["seconds"] + LEG_OVERRUN_S
                if code is not None:
                    self.end_leg(code)
                    if code != 0:
                        self.sample()
                        return f"leg {self.legs[-1]['leg']}'s client ended {code}"
                    if len(self.legs) == len(self.legs_plan):
                        self.sample()
                        return None
                    self.next_leg_at = time.monotonic() + LEG_GAP_S
                elif late:
                    self.sample()
                    return f"leg {self.client_leg['leg']}'s client outlived its seconds by {LEG_OVERRUN_S} s"
            elif time.monotonic() >= self.next_leg_at:
                self.start_leg()
            if time.monotonic() - self.t0 >= k * self.args.every:
                self.sample()
                k = math.floor((time.monotonic() - self.t0) / self.args.every) + 1
            time.sleep(TICK_S)

    # The graceful stop ---------------------------------------------------

    def stop_all(self) -> dict:
        """Server (SIGTERM), then a client still running (it ends once its
        sockets close), then the engine (Shutdown); each wait bounded, never
        a forced end."""
        stops: dict = {}
        if self.server is not None:
            if self.server.poll() is None:
                log("graceful stop: the server (SIGTERM)")
                self.server.send_signal(signal.SIGTERM)
            code = wait_end(self.server, SERVER_STOP_S)
            stops["server"] = {"ended": code is not None, "exit": code, "pid": self.server.pid}
        if self.client is not None:
            code = wait_end(self.client, CLIENT_END_S)
            stops["client"] = {"ended": code is not None, "exit": code, "pid": self.client.pid}
            if code is not None:
                self.end_leg(code)
        if self.observer is not None:
            self.observer.stop.set()
        if self.engine is not None:
            why = None
            if self.engine.poll() is None:
                log("graceful stop: the engine (Shutdown)")
                why = engine_shutdown(self.sock)
            code = wait_end(self.engine, ENGINE_STOP_S)
            stops["engine"] = {"ended": code is not None, "exit": code, "pid": self.engine.pid, "request": why}
        for name, st in stops.items():
            if not st["ended"]:
                log(f"{name} (pid {st['pid']}) did not end within its graceful stop: left running, never forced")
        return stops

    def finish(self, early: str | None) -> dict:
        stops = self.stop_all()
        for f in self.logs:
            f.close()
        v = verdict(self.samples, self.legs, stops, self.total_s, self.args.every, early)
        v["build"] = self.args.build
        v["legs"] = [{k: leg[k] for k in ("leg", "seconds", "exit")} | {"harness": harness_fields(leg["summary"])}
                     for leg in self.legs]
        v["stops"] = stops
        (self.out / "verdict.json").write_text(json.dumps(v, indent=1) + "\n", encoding="utf-8")
        return v


def read_summary(path: Path):
    """A client summary (written whole by rename), or None when absent or unreadable."""
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None
    except (OSError, ValueError) as e:
        log(f"{path.name}: unreadable ({type(e).__name__})")
        return None


def parse_args(argv: list[str]) -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--bin", required=True, help="the folder holding the artifact's three binaries")
    p.add_argument("--build", required=True, help="the full commit SHA the artifact was built from")
    p.add_argument("--out", required=True, help="a new folder for the site, the logs, samples.jsonl and verdict.json")
    p.add_argument("--hours", type=float, default=72.0, help="the soak's length (default 72)")
    p.add_argument("--every", type=float, default=60.0, help="seconds between samples (default 60)")
    p.add_argument("--repo", default=str(Path(__file__).resolve().parents[2]), help="the iemmixer checkout")
    return p.parse_args(argv)


def main(argv: list[str]) -> int:
    run = Run(parse_args(argv))
    try:
        run.check()
    except SetupError as e:
        print(f"nullrt_soak: {e}", file=sys.stderr)
        return 2

    def asked(signum, _frame):
        run.stop_asked = f"signal {signal.Signals(signum).name}"

    for s in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(s, asked)
    early: str | None
    try:
        run.provision()
        run.start()
        log(f"soak: {len(run.legs_plan)} legs, {run.total_s} s, a sample every {run.args.every:g} s; out {run.out}")
        early = run.loop()
    except SetupError as e:
        early = f"setup: {e}"
        log(early)
    except BaseException:   # anything else: the processes still get their graceful stop, then it is raised
        run.finish("an error in nullrt_soak.py (its traceback follows)")
        raise
    v = run.finish(early)
    print(json.dumps({k: v[k] for k in ("conclusion", "summary")}))
    return 0 if v["conclusion"] == "success" else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
