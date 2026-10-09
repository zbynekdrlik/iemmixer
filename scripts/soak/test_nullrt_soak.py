"""Tests for scripts/soak/nullrt_soak.py (S7 design note §8, #10): the pure
parsing, leg plan and verdict, then whole runs against stand-in binaries
(a fake engine on a Unix socket, a fake server, a fake soak client) with the
hour shrunk to a second. Every value is synthetic."""
from __future__ import annotations

import contextlib
import io
import json
import os
import re
import shutil
import signal
import stat
import struct
import sys
import tempfile
import textwrap
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import nullrt_soak as ns  # noqa: E402

ROOT = HERE.parent.parent
BUILD = "0123456789abcdef0123456789abcdef01234567"
H = ns.HOUR


def proc(rss=50_000, fds=20, threads=8, pid=100):
    return {"pid": pid, "alive": True, "rss_kb": rss, "fds": fds, "threads": threads}


def series(hours: float, every: float = 60, **kw) -> list[tuple[float, dict]]:
    """Samples of one steady process from 0 to `hours`, `kw` overriding by a function of t."""
    out, t = [], 0.0
    while t <= hours * H + 1e-9:
        out.append((t, proc(**{k: f(t) for k, f in kw.items()})))
        t += every
    return out


def summary(**kw) -> dict:
    return {"schema": 1, "complete": True, "seconds": 100.0, "frames": 5000, "expected_frames": 5000,
            "decode_errors": 0, "gaps": 0, "max_gap_ms": 21.0, "meter_frames": 1000, "reconnects": 0,
            "no_source": 0, "error": None, **kw}


def run_samples(hours: float, every: float = 600, legs: int = 1, engine=None, server=None) -> list[dict]:
    """A whole run's JSONL records: the engine and the server steady, one client per leg."""
    out, t, total = [], 0.0, hours * H
    while t <= total + 1e-9:
        leg = min(int(t // (total / legs)) + 1, legs)
        procs = {"engine": (engine or proc)(t) if engine else proc(pid=1),
                 "server": (server or proc)(t) if server else proc(pid=2),
                 "client": proc(pid=1000 + leg, rss=9000)}
        out.append({"t": t, "leg": leg, "procs": procs, "engine": {"late": 0, "callbacks": int(t * 3000)},
                    "harness": None})
        t += every
    return out


def legs_of(n: int, total_s: int, **kw) -> list[dict]:
    return [{"leg": i + 1, "seconds": s, "exit": 0, "summary": summary(**kw)}
            for i, s in enumerate(ns.plan_legs(total_s))]


STOPS = {"server": {"ended": True, "exit": 0}, "engine": {"ended": True, "exit": 0}}


class PureTests(unittest.TestCase):
    def test_proc_status_gives_rss_and_threads(self) -> None:
        text = "Name:\tiem-server\nState:\tS (sleeping)\nVmPeak:\t  999 kB\nVmRSS:\t   51234 kB\nThreads:\t17\n"
        self.assertEqual(ns.parse_proc_status(text), {"rss_kb": 51234, "threads": 17})
        self.assertEqual(ns.parse_proc_status("Name:\tz\nThreads:\t1\n"), {"rss_kb": None, "threads": 1})

    def test_frames_round_trip_and_keep_a_partial_one(self) -> None:
        a, b = {"type": "status", "late": 3}, {"type": "meters", "x": [1, 2]}
        data = ns.frame(a) + ns.frame(b)
        self.assertEqual(struct.unpack_from("<I", data)[0], len(json.dumps(a, separators=(",", ":"))))
        self.assertEqual(ns.split_frames(data), ([a, b], b""))
        msgs, rest = ns.split_frames(data[:-3])
        self.assertEqual((msgs, rest), ([a], ns.frame(b)[:-3]))
        self.assertEqual(ns.split_frames(rest + data[-3:]), ([b], b""))
        self.assertEqual(ns.split_frames(b"\x05\x00"), ([], b"\x05\x00"))

    def test_an_oversized_or_non_object_frame_is_refused(self) -> None:
        with self.assertRaisesRegex(ValueError, "exceeds"):
            ns.split_frames(struct.pack("<I", ns.MAX_FRAME + 1) + b"x")
        ns.split_frames(struct.pack("<I", ns.MAX_FRAME))   # the bound itself waits for its body
        body = b"[1,2]"
        with self.assertRaisesRegex(ValueError, "no JSON object"):
            ns.split_frames(struct.pack("<I", len(body)) + body)

    def test_status_and_harness_fields(self) -> None:
        msg = {"type": "status", "callbacks": 9, "late": 2, "missed": 0, "interval_hist": [[1, 2]], "faulted": False}
        f = ns.status_fields(msg)
        self.assertEqual(set(f), set(ns.STATUS_KEYS))
        self.assertEqual((f["callbacks"], f["late"], f["faulted"], f["resets"]), (9, 2, False, None))
        self.assertNotIn("interval_hist", f)
        self.assertEqual(ns.harness_fields(summary())["gaps"], 0)
        self.assertNotIn("schema", ns.harness_fields(summary()))
        self.assertIsNone(ns.harness_fields(None))
        self.assertIsNone(ns.harness_fields([1]))

    def test_legs_are_equal_and_within_the_clients_bound(self) -> None:
        self.assertEqual(ns.plan_legs(72 * 3600), [8 * 3600] * 9)
        self.assertEqual(ns.plan_legs(10 * 3600), [5 * 3600] * 2)
        self.assertEqual(ns.plan_legs(8 * 3600 + 1), [14401, 14400])
        self.assertEqual(ns.plan_legs(1), [1])
        self.assertEqual(ns.plan_legs(10, 4), [4, 3, 3])
        with self.assertRaises(ValueError):
            ns.plan_legs(0)
        # The client's own bound (iem-soakclient MAX_SECONDS), read from its source.
        lib = (ROOT / "crates/iem-soakclient/src/lib.rs").read_text(encoding="utf-8")
        max_seconds = int(re.search(r"pub const MAX_SECONDS: u64 = ([\d_]+);", lib).group(1).replace("_", ""))
        self.assertLessEqual(ns.LEG_MAX_S, max_seconds)
        for total in (3600, 72 * 3600, 100 * 3600 + 7):
            legs = ns.plan_legs(total)
            self.assertEqual(sum(legs), total)
            self.assertLessEqual(max(legs) - min(legs), 1)
            self.assertLessEqual(max(legs), ns.LEG_MAX_S)

    def test_the_growth_bound_is_16_mb_in_kb(self) -> None:
        self.assertEqual(ns.RSS_GROWTH_MAX_KB * 1024, 16_000_000)


class JudgeTests(unittest.TestCase):
    def test_a_steady_process_is_green_and_hour_one_does_not_count(self) -> None:
        pts = series(72, rss=lambda t: 40_000 + (60_000 if t >= 600 else 0))   # growth inside hour 1
        fails, nums = ns.judge_process("server#2", pts)
        self.assertEqual(fails, [])
        self.assertEqual((nums["judged"], nums["rss_growth_kb"], nums["lived_h"]), (True, 0, 72.0))

    def test_rss_growth_after_hour_one_is_bounded(self) -> None:
        at = ns.RSS_GROWTH_MAX_KB
        fails, nums = ns.judge_process("engine#1", series(5, rss=lambda t: 50_000 + (at if t > 4 * H else 0)))
        self.assertEqual((fails, nums["rss_growth_kb"]), ([], at))
        fails, _ = ns.judge_process("engine#1", series(5, rss=lambda t: 50_000 + (at + 1 if t > 4 * H else 0)))
        self.assertEqual(fails, [f"engine#1: RSS grew {at + 1} kB after hour 1 (at most {at} kB, 16 MB)"])
        # A peak that falls back still counts: the highest after hour 1.
        fails, _ = ns.judge_process("engine#1", series(5, rss=lambda t: 50_000 + (at + 1 if 3 * H < t < 3.5 * H
                                                                                   else 0)))
        self.assertEqual(len(fails), 1)
        # Measured from the first sample at or after hour 1, not from the start.
        fails, nums = ns.judge_process("x", series(5, rss=lambda t: 1000 if t < H else 90_000))
        self.assertEqual((fails, nums["rss_growth_kb"]), ([], 0))

    def test_fds_and_threads_must_stay_flat(self) -> None:
        for key in ("fds", "threads"):
            grows = {key: lambda t: 20 + (1 if t > 70 * H else 0)}
            fails, nums = ns.judge_process("server#2", series(72, **grows))
            self.assertEqual(fails, [f"server#2: {key} not flat: the last hour's highest 21, hour 1 to 2's 20"], key)
            self.assertEqual((nums[f"{key}_hour2_max"], nums[f"{key}_last_hour_max"]), (20, 21))
            # A rise that is gone before the last hour, or a fall, is flat.
            for f in (lambda t: 20 + (5 if 10 * H < t < 11 * H else 0), lambda t: 20 - (3 if t > 5 * H else 0)):
                self.assertEqual(ns.judge_process("server#2", series(72, **{key: f}))[0], [], key)
            # The baseline is hour 1 to 2's highest: a spike there allows the last hour that high.
            spike = {key: lambda t: 30 if 1.5 * H <= t < 1.5 * H + 60 else (30 if t > 71.5 * H else 20)}
            self.assertEqual(ns.judge_process("server#2", series(72, **spike))[0], [], key)

    def test_a_process_under_two_hours_is_not_judged(self) -> None:
        fails, nums = ns.judge_process("client#9", series(1.95, rss=lambda t: int(t * 1000)))
        self.assertEqual((fails, nums["judged"], nums["lived_h"]), ([], False, 1.95))
        self.assertTrue(ns.judge_process("client#9", series(2.0))[1]["judged"])
        # Times count from the process's own first sample.
        late = [(t + 50 * H, p) for t, p in series(1.5)]
        self.assertFalse(ns.judge_process("client#9", late)[1]["judged"])

    def test_samples_without_figures_are_skipped(self) -> None:
        pts = series(3) + [(3 * H + 60, {"pid": 1, "alive": True, "rss_kb": None, "fds": 3, "threads": 1})]
        self.assertEqual(ns.judge_process("e", pts)[0], [])
        self.assertEqual(ns.judge_process("e", [(0, {"pid": 1, "alive": True})]),
                         (["e: no readable sample"], {"judged": False}))


class VerdictTests(unittest.TestCase):
    TOTAL = 72 * 3600

    def green(self, **kw) -> dict:
        args = {"samples": run_samples(72, legs=9), "legs": legs_of(9, self.TOTAL), "stops": dict(STOPS),
                "total_s": self.TOTAL, "every_s": 600, "early": None}
        args.update(kw)
        return ns.verdict(**args)

    def test_a_whole_steady_run_is_green(self) -> None:
        v = self.green()
        self.assertEqual((v["conclusion"], v["summary"], v["failures"]), ("success", "green", []))
        self.assertEqual(v["numbers"]["sampled_h"], 72.0)
        self.assertEqual(v["numbers"]["legs"], 9)
        self.assertTrue(v["numbers"]["engine#1"]["judged"])
        self.assertTrue(v["numbers"]["client#1001"]["judged"])   # an 8 h leg is judged as any process
        self.assertEqual(v["numbers"]["engine_last_status"]["late"], 0)

    def test_red_names_the_first_failure(self) -> None:
        samples = run_samples(72, legs=9)
        samples[-1]["procs"]["engine"] = {"pid": 1, "alive": False, "exit": 70}
        legs = legs_of(9, self.TOTAL)
        legs[3]["summary"]["gaps"] = 2
        v = self.green(samples=samples, legs=legs, early="the engine exited (70)")
        self.assertEqual(v["conclusion"], "failure")
        self.assertEqual(v["failures"][:3], ["stopped early: the engine exited (70)",
                                             f"the engine exited (70) by {samples[-1]['t']:.0f} s",
                                             "leg 4: harness gaps 2, not 0"])
        self.assertEqual(v["summary"], "red: stopped early: the engine exited (70)")

    def test_each_rule_alone_is_red(self) -> None:
        cases = []
        s = run_samples(72, legs=9)
        s[5]["procs"]["server"] = {"pid": 2, "alive": False, "exit": 1}
        cases.append(({"samples": s}, "the server exited (1) by 3000 s"))
        cases.append(({"samples": run_samples(70, legs=9)}, "sampled 70.0 h of the 72 h"))
        cases.append(({"samples": []}, "sampled 0.0 h of the 72 h"))
        legs = legs_of(9, self.TOTAL)
        legs[0]["exit"] = 1
        cases.append(({"legs": legs}, "leg 1: the client ended 1, not 0"))
        legs = legs_of(9, self.TOTAL)
        legs[8]["summary"] = None
        cases.append(({"legs": legs}, "leg 9: the harness summary is unreadable"))
        legs = legs_of(9, self.TOTAL)
        legs[2]["summary"]["error"] = "connection-lost"
        cases.append(({"legs": legs}, "leg 3: the harness is not complete (error 'connection-lost')"))
        legs = legs_of(9, self.TOTAL)
        legs[2]["summary"]["complete"] = False
        cases.append(({"legs": legs}, "leg 3: the harness is not complete (error None)"))
        cases.append(({"legs": legs_of(9, self.TOTAL)[:8]}, "8 of the 9 legs ended"))
        cases.append(({"stops": {**STOPS, "engine": {"ended": False, "exit": None}}},
                      "the engine did not end within its graceful stop (left running)"))
        cases.append(({"stops": {**STOPS, "server": {"ended": True, "exit": 1}}},
                      "the server ended 1 at its graceful stop, not 0"))
        cases.append(({"stops": {**STOPS, "client": {"ended": False, "exit": None}}},
                      "the client did not end within its graceful stop (left running)"))
        grow = run_samples(72, legs=9, engine=lambda t: proc(pid=1, rss=50_000 + (20_000 if t > 60 * H else 0)))
        cases.append(({"samples": grow}, "engine#1: RSS grew 20000 kB after hour 1 (at most 15625 kB, 16 MB)"))
        cases.append(({"early": "signal SIGINT"}, "stopped early: signal SIGINT"))
        for kw, words in cases:
            v = self.green(**kw)
            self.assertEqual(v["conclusion"], "failure", words)
            self.assertIn(words, v["failures"], (words, v["failures"]))

    def test_a_client_ending_at_its_stop_is_no_failure_but_the_engine_must_end_0(self) -> None:
        v = self.green(stops={**STOPS, "client": {"ended": True, "exit": 1}})
        self.assertEqual(v["failures"], [])

    def test_an_engine_or_server_under_two_hours_is_red_a_short_client_leg_is_not(self) -> None:
        total = 3600
        v = ns.verdict(run_samples(1, every=60), legs_of(1, total), dict(STOPS), total, 60)
        self.assertIn("engine#1: lived 1.0 h, under the 2 h a judgement needs", v["failures"])
        self.assertIn("server#2: lived 1.0 h, under the 2 h a judgement needs", v["failures"])
        self.assertFalse(any(f.startswith("client") for f in v["failures"]))

    def test_the_last_sample_may_fall_one_interval_short(self) -> None:
        s = run_samples(72, legs=9)
        s[-1]["t"] = self.TOTAL - 600
        self.assertEqual(self.green(samples=s)["failures"], [])
        s[-1]["t"] = self.TOTAL - 601
        self.assertIn("sampled 71.83 h of the 72 h", self.green(samples=s)["failures"])


# ---- whole runs against stand-ins ----

FAKE_ENGINE = r'''#!/usr/bin/env python3
import json, os, socket, struct, sys, threading, time
args = sys.argv[1:]
assert args[0] == "run", args
pipe = args[args.index("--pipe") + 1]
log = open(os.environ["FAKE_LOG"], "a")
die_after = float(os.environ.get("FAKE_ENGINE_EXIT_AFTER", "0"))
done = threading.Event()
start = time.monotonic()
def frame(m):
    b = json.dumps(m).encode()
    return struct.pack("<I", len(b)) + b
def read(c):
    head = b""
    while len(head) < 4:
        x = c.recv(4 - len(head))
        if not x:
            return None
        head += x
    n = struct.unpack("<I", head)[0]
    body = b""
    while len(body) < n:
        body += c.recv(n - len(body))
    return json.loads(body)
def serve(c):
    try:
        hello = read(c)
        role = hello["role"]
        log.write(f"hello {role} {hello['proto']}\n"); log.flush()
        c.sendall(frame({"type": "hello", "proto": 1, "engine_build": "local", "role": role}))
        if role == "observe":
            n = 0
            while not done.is_set():
                n += 1
                c.sendall(frame({"type": "meters", "inputs": [0.0] * 30}))
                c.sendall(frame({"type": "status", "callbacks": n * 300, "late": 1, "missed": 0, "faulted": False,
                                 "parked": False, "interval_hist": [[333, n]]}))
                time.sleep(0.05)
        elif role == "supervisor":
            req = read(c)
            log.write(f"request {json.dumps(req)}\n"); log.flush()
            if req and req["cmd"] == {"op": "shutdown"}:
                done.set()   # first: the peer may close after the reply, as the real engine ends regardless
                c.sendall(frame({"type": "reply", "id": req["id"], "rev": 0}))
                c.sendall(frame({"type": "driver_released", "reason": "shutdown"}))
    except OSError:
        pass
srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
srv.bind(pipe)
srv.listen()
srv.settimeout(0.05)
while not done.is_set():
    if die_after and time.monotonic() - start > die_after:
        sys.exit(70)
    try:
        c, _ = srv.accept()
    except socket.timeout:
        continue
    threading.Thread(target=serve, args=(c,), daemon=True).start()
log.write("engine ended 0\n"); log.flush()
time.sleep(0.1)
'''

FAKE_SERVER = r'''#!/usr/bin/env python3
import http.server, json, os, signal, sys, threading
from pathlib import Path
log = open(os.environ["FAKE_LOG"], "a")
if sys.argv[1:2] == ["pin"]:
    pin = sys.stdin.readline().strip()
    log.write(f"pin {' '.join(sys.argv[2:])} {len(pin)} {pin.isdigit()} {pin in ' '.join(sys.argv)}\n")
    sys.exit(0)
config = Path(os.environ["IEMMIXER_CONFIG"])
(config.parent / "secrets").mkdir(exist_ok=True)
(config.parent / "secrets" / "jwt_secret").write_text("synthetic-secret\n")
log.write(f"server pipe {os.environ['IEMMIXER_ENGINE_PIPE']}\n"); log.flush()
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({"git_hash": os.environ["FAKE_HASH"]}).encode()
        self.send_response(200 if self.path == "/api/version" else 404)
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a):
        pass
httpd = http.server.ThreadingHTTPServer(("127.0.0.1", int(os.environ["PORT"])), H)
signal.signal(signal.SIGTERM, lambda *a: threading.Thread(target=httpd.shutdown).start())
httpd.serve_forever()
log.write("server ended 0\n"); log.flush()
'''

FAKE_CLIENT = r'''#!/usr/bin/env python3
import json, os, select, socket, sys, time, urllib.request
a = sys.argv[1:]
val = lambda f: a[a.index(f) + 1]
log = open(os.environ["FAKE_LOG"], "a")
if "IEM_SOAK_PIN" in os.environ:
    log.write("client saw IEM_SOAK_PIN\n"); sys.exit(2)
assert open(val("--jwt-secret-file")).read().strip()
seconds, out, base = float(val("--seconds")), val("--out"), val("--base")
log.write(f"client {seconds} {val('--member')} {val('--expect-build')} {'--direct' in a}\n"); log.flush()
def write(**kw):
    s = {"schema": 1, "complete": False, "seconds": 0, "frames": 0, "expected_frames": 0, "decode_errors": 0,
         "gaps": 0, "max_gap_ms": 0, "meter_frames": 0, "reconnects": 0, "no_source": 0, "error": None, **kw}
    open(out + ".tmp", "w").write(json.dumps(s)); os.replace(out + ".tmp", out)
assert json.loads(urllib.request.urlopen(base + "/api/version", timeout=5).read())["git_hash"]
# One connection held for the leg, as the real client's sockets: its close is the server's end.
held = socket.create_connection(("127.0.0.1", int(base.rsplit(":", 1)[1])), timeout=5)
end = time.monotonic() + seconds
while time.monotonic() < end:
    readable, _, _ = select.select([held], [], [], 0.1)
    if readable and held.recv(1) == b"":
        write(error="connection-lost"); sys.exit(1)
    write(seconds=seconds - (end - time.monotonic()), frames=1)
write(complete=True, seconds=seconds, frames=50, expected_frames=50)
'''


class RunTests(unittest.TestCase):
    """Whole runs, the hour shrunk to one second and a leg to three."""

    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="nsoak-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.bin = self.tmp / "bin"
        self.bin.mkdir()
        for name, text in (("iem-engine", FAKE_ENGINE), ("iem-server", FAKE_SERVER), ("iem-soakclient", FAKE_CLIENT)):
            p = self.bin / name
            p.write_text(textwrap.dedent(text), encoding="utf-8")
            p.chmod(p.stat().st_mode | stat.S_IXUSR)
        self.log = self.tmp / "fake.log"
        self.out = self.tmp / "run"
        env = {"FAKE_LOG": str(self.log), "FAKE_HASH": BUILD[:7], "IEM_SOAK_PIN": "1234"}
        patches = [mock.patch.dict(os.environ, env)]
        for name, value in (("HOUR", 1), ("LEG_MAX_S", 3), ("TICK_S", 0.05), ("READY_S", 10), ("SERVER_STOP_S", 10),
                            ("CLIENT_END_S", 10), ("ENGINE_STOP_S", 10), ("LEG_OVERRUN_S", 10)):
            patches.append(mock.patch.object(ns, name, value))
        for p in patches:
            p.start()
            self.addCleanup(p.stop)
        saved = {s: signal.getsignal(s) for s in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)}
        self.addCleanup(lambda: [signal.signal(s, h) for s, h in saved.items()])

    def run_soak(self, *extra: str, hours: str = "8") -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = ns.main(["--bin", str(self.bin), "--build", BUILD, "--out", str(self.out), "--hours", hours,
                            "--every", "0.25", "--repo", str(ROOT), *extra])
        return code, out.getvalue(), err.getvalue()

    def fake_log(self) -> list[str]:
        return self.log.read_text(encoding="utf-8").splitlines()

    def verdict(self) -> dict:
        return json.loads((self.out / "verdict.json").read_text(encoding="utf-8"))

    def samples(self) -> list[dict]:
        return [json.loads(line) for line in (self.out / "samples.jsonl").read_text(encoding="utf-8").splitlines()]

    def test_a_whole_run_is_green_and_every_process_stops_gracefully(self) -> None:
        code, out, err = self.run_soak()
        v = self.verdict()
        self.assertEqual(code, 0, (v["failures"], err))
        self.assertEqual(json.loads(out), {"conclusion": "success", "summary": "green"})
        log = self.fake_log()
        # Provisioning: the engineer and every member but the engineer, each PIN on stdin only, 4 digits.
        pins = [line for line in log if line.startswith("pin ")]
        self.assertEqual(pins[0], "pin set-engineer 4 True False")
        self.assertEqual(sorted(p.split()[2] for p in pins[1:]), sorted(f"member{n}" for n in range(1, 10)))
        self.assertTrue(all(p.endswith(" 4 True False") for p in pins))
        # Legs back to back, each with the client's flags; the PIN variable never reaches it.
        self.assertEqual([line for line in log if line.startswith("client")],
                         [f"client {s:.1f} member9 {BUILD} True" for s in (3, 3, 2)])
        self.assertNotIn("client saw IEM_SOAK_PIN", log)
        self.assertEqual([leg["exit"] for leg in v["legs"]], [0, 0, 0])
        self.assertEqual([leg["seconds"] for leg in v["legs"]], [3, 3, 2])
        # The engine was read as an observer, stopped by Shutdown as a supervisor; the server by SIGTERM.
        self.assertIn("hello observe 1", log)
        self.assertIn('request {"type": "request", "id": 1, "cmd": {"op": "shutdown"}}', log)
        self.assertEqual(log[-1], "engine ended 0")
        self.assertLess(log.index("server ended 0"), log.index("hello supervisor 1"))   # the server first
        self.assertEqual({k: (s["ended"], s["exit"]) for k, s in v["stops"].items()},
                         {"server": (True, 0), "engine": (True, 0)})
        # Samples: every process's figures, the engine's Status, the harness.
        samples = self.samples()
        self.assertGreater(len(samples), 20)
        mid = samples[len(samples) // 2]
        for name in ("engine", "server", "client"):
            p = mid["procs"][name]
            self.assertTrue(p["alive"], name)
            self.assertTrue(all(isinstance(p[k], int) and p[k] > 0 for k in ("rss_kb", "fds", "threads")), p)
        self.assertEqual(mid["engine"]["late"], 1)
        self.assertGreater(mid["engine"]["callbacks"], 0)
        self.assertEqual(mid["observer_reconnects"], 0)
        self.assertEqual(samples[-1]["procs"].get("client"), None)
        self.assertGreaterEqual(samples[-1]["t"], 8)
        # The run folder is the owner's only.
        self.assertEqual(stat.S_IMODE(self.out.stat().st_mode), 0o700)

    def test_an_engine_that_exits_stops_the_run_red_and_the_rest_stops_gracefully(self) -> None:
        with mock.patch.dict(os.environ, {"FAKE_ENGINE_EXIT_AFTER": "2"}):
            code, out, _ = self.run_soak()
        v = self.verdict()
        self.assertEqual(code, 1)
        self.assertEqual(v["failures"][0], "stopped early: the engine exited (70)")
        self.assertIn("red: stopped early: the engine exited (70)", out)
        self.assertEqual(v["stops"]["engine"]["exit"], 70)
        self.assertEqual((v["stops"]["server"]["ended"], v["stops"]["server"]["exit"]), (True, 0))
        # The running leg's client ended by itself once the server was gone.
        self.assertEqual((v["stops"]["client"]["ended"], v["stops"]["client"]["exit"]), (True, 1))
        self.assertEqual(v["legs"][-1]["harness"]["error"], "connection-lost")
        self.assertNotIn("request", " ".join(self.fake_log()))   # no Shutdown to an engine that had ended

    def test_a_stop_request_ends_the_run_gracefully_red(self) -> None:
        threading.Timer(1.5, signal.raise_signal, (signal.SIGINT,)).start()
        code, _, _ = self.run_soak()
        v = self.verdict()
        self.assertEqual(code, 1)
        self.assertEqual(v["failures"][0], "stopped early: signal SIGINT")
        log = self.fake_log()
        self.assertEqual(log[-1], "engine ended 0")
        self.assertLess(log.index("server ended 0"), log.index("hello supervisor 1"))
        self.assertEqual({k: s["ended"] for k, s in v["stops"].items()},
                         {"server": True, "client": True, "engine": True})

    def test_a_server_of_another_build_stops_before_any_leg(self) -> None:
        with mock.patch.dict(os.environ, {"FAKE_HASH": "fedcba9"}):
            code, _, err = self.run_soak()
        v = self.verdict()
        self.assertEqual(code, 1)
        self.assertIn("setup: the server names build 'fedcba9', not a prefix of --build", v["failures"][0])
        self.assertEqual(v["legs"], [])
        self.assertFalse(any(line.startswith("client") for line in self.fake_log()))
        self.assertEqual({k: s["exit"] for k, s in v["stops"].items()}, {"server": 0, "engine": 0})
        self.assertIn("graceful stop", err)

    def test_usage_errors_start_nothing(self) -> None:
        cases = [(["--build", "abc"], "--build must be the artifact's full commit SHA"),
                 (["--hours", "0"], "--hours must be at least 1 s"),
                 (["--every", "0"], "--every above 0"),
                 (["--repo", str(self.tmp)], "config/test-site.toml is missing")]
        for extra, words in cases:
            code, _, err = self.run_soak(*extra)
            self.assertEqual(code, 2, extra)
            self.assertIn(words, err, extra)
            self.assertFalse(self.out.exists(), extra)
        (self.bin / "iem-server").chmod(0o644)
        code, _, err = self.run_soak()
        self.assertEqual(code, 2)
        self.assertIn("not executable (an artifact download keeps no mode: chmod +x)", err)
        (self.bin / "iem-server").chmod(0o755)
        self.out.mkdir()
        code, _, err = self.run_soak()
        self.assertEqual((code, self.log.exists()), (2, False))
        self.assertIn("exists: each run gets a new folder", err)

    def test_no_force_end_verb_in_the_tool(self) -> None:
        """I8: the integrity scan refuses force-end verbs; the tool stops with
        SIGTERM to the server and Shutdown to the engine only."""
        sys.path.insert(0, str(ROOT / "scripts"))
        import check_integrity
        text = (HERE / "nullrt_soak.py").read_text(encoding="utf-8")
        self.assertIsNone(check_integrity.FORCE_KILL.search(text))
        self.assertIn("send_signal(signal.SIGTERM)", text)
        self.assertEqual(text.count("send_signal("), 1)


if __name__ == "__main__":
    unittest.main()
