"""Tests for scripts/gen3/live_guard.py (#72 phase B, plan Task 8 Steps 1
and 3): the decisions on synthetic status reads, and the guard itself
against a stand-in `ssh` first on PATH that plays the box (its status reads
from a scripted list, the stop) on a clock that moves only when the guard
sleeps. Every value is synthetic (P6)."""
from __future__ import annotations

import contextlib
import io
import json
import os
import random
import shutil
import signal
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import live_guard as lg  # noqa: E402

HOST = "tester@gen3.test"
RATE = 48000
ARC, FLOWS, INFO, CMC = 4440, 4455, 8700, 8800

# The box: `ssh` answers the guard's status reads from FAKE_BOX/statuses.json
# (one entry per read, the last one repeated; null is a failed read) and its
# stop, and logs every call as one JSON line.
FAKE_SSH = r'''
import json, os, sys, time
box = os.environ["FAKE_BOX"]
args = sys.argv[1:]
with open(os.path.join(box, "calls.log"), "a") as f:
    f.write(json.dumps(args) + "\n")
while args and args[0] == "-o":
    args = args[2:]
cmd = " ".join(args[1:])
if cmd == "/opt/iemmixer/bin/iem-dante-dev ctl status":
    counter = os.path.join(box, "reads")
    n = int(open(counter).read()) if os.path.exists(counter) else 0
    open(counter, "w").write(str(n + 1))
    plan = json.load(open(os.path.join(box, "statuses.json")))
    if str(n) == os.environ.get("FAKE_FLAG_AT"):
        open(os.environ["IEMMIXER_EVENT_NOW"], "w").write("now\n")
    if str(n) == os.environ.get("FAKE_SLOW_AT"):
        time.sleep(float(os.environ["FAKE_SLOW_S"]))
        open(os.path.join(box, "slow.done"), "w").write("ended\n")
    doc = plan[min(n, len(plan) - 1)]
    if doc is None:
        sys.stderr.write("iem-dante-dev ctl: the control socket: Connection refused\n")
        sys.exit(1)
    print(json.dumps(doc))
elif cmd == "sudo -n systemctl stop iem-server iem-engine iem-dante":
    code = int(os.environ.get("FAKE_STOP_EXIT", "0"))
    if code:
        sys.stderr.write("sudo: a password is required\n")
    sys.exit(code)
else:
    sys.stderr.write("unexpected command: " + cmd + "\n")
    sys.exit(97)
'''


def status(requests=(), sent=(), tx=(), rx=(), uncounted=0, polls=0) -> dict:
    """A status line as `iem-dante-dev ctl status` prints it (ctl.rs)."""
    return {
        "clock": {"ready": 1, "epoch": 0, "polls": polls, "re_anchors": 0, "holdovers": 0, "holdover": 0,
                  "freq_ppb": -12},
        "requests": [{"port": p, "peer": peer, "op": op, "n": n} for (p, peer, op), n in requests],
        "sent": [{"peer": peer, "op": op, "n": n} for (peer, op), n in sent],
        "uncounted": uncounted,
        "tx": [{"flow": f, "live": 1, "fpp": fpp, "channels": 2, "packets": pk, "send_errors": 0, "keepalives": 0,
                "rebootstraps": 0, "holes": 0} for f, fpp, pk in tx],
        "rx": [{"flow": f, "granted": 1, "fpp": fpp, "channels": 1, "latency": 96, "packets": pk, "refused": 0,
                "stray": 0, "out_of_order": 0, "late": 0, "unplaced": 0, "lateness": 3, "keepalives": 0,
                "keepalive_errors": 0, "recv_errors": 0} for f, fpp, pk in rx],
        "rx_poll_errors": 0, "channels": [], "sources": [], "checks": [],
    }


def ordinary(t: float) -> dict:
    """The device's counts `t` seconds into a session of ordinary traffic:
    Dante Controller's opening (each list asked three times in the first
    second) and then one ask per 10 s, the info and CMC requests, the card's
    one flow request, our flow requests to a transmitter that refuses (one
    per 9 s, the manager's bound) and our ARC reads of its channels (one per
    9 s, the directory's bound, each a 0x1000, four 0x2000 pages and one
    0x2010 page), a TX flow at fpp 16 and an RX flow at fpp 32, each a few
    packets off the exact rate either way."""
    s = int(t)
    arc = {(ARC, 1, op): (3 if t >= 1 else 0) + s // 10 for op in (0x1000, 0x1002, 0x2000, 0x2200, 0x2204, 0x3000)}
    jitter = 20 if s % 2 else -20
    return status(
        requests=[*arc.items(), ((INFO, 1, 0x61), 1 + s // 10), ((CMC, 2, 0x1001), s // 5),
                  ((FLOWS, 2, 0x0100), 1 if t >= 5 else 0)],
        sent=[((3, 0x0100), 1 + s // 9), ((3, 0x0101), 0), ((3, 0x1000), 1 + s // 9),
              ((3, 0x2000), 4 * (1 + s // 9)), ((3, 0x2010), 1 + s // 9)],
        tx=[(1, 16, max(0, round(t * RATE / 16) + jitter))],
        rx=[(1, 32, max(0, round(t * RATE / 32) - jitter))],
        polls=s)


def loop(t: float, per_second: int = 2250, key=(ARC, 1, 0x2204)) -> dict:
    """Ordinary traffic plus one (port, peer, opcode) asked `per_second`."""
    doc = ordinary(t)
    doc["requests"].append({"port": key[0], "peer": key[1], "op": key[2], "n": int(per_second * t)})
    return doc


def feed(watch: lg.Watch, doc: dict, t0: float, t1: float | None = None) -> lg.Trip | None:
    return watch.feed(lg.sample(t0, t0 if t1 is None else t1, doc))


def first_trip(make, seconds: int, rate: int = RATE) -> tuple[float | None, lg.Trip | None, lg.Watch]:
    """Feeds one read a second (at 0, 1, 2 …): the time of the first trip."""
    watch = lg.Watch(rate)
    for t in range(seconds + 1):
        trip = feed(watch, make(t), float(t))
        if trip is not None:
            return float(t), trip, watch
    return None, None, watch


class WatchTests(unittest.TestCase):
    """The decisions, pure."""

    def test_a_loop_of_2250_requests_a_second_trips_at_3_s(self) -> None:
        t, trip, _ = first_trip(loop, 60)
        self.assertEqual(t, 3.0)
        self.assertEqual(trip.kind, "requests")
        self.assertEqual(trip.numbers, {"port": ARC, "peer": 1, "op": 0x2204, "rate": 2250.0, "seconds": 3.0})

    def test_twenty_a_second_is_the_bound_and_twenty_one_for_3_s_trips(self) -> None:
        self.assertEqual(first_trip(lambda t: loop(t, 20), 600)[:2], (None, None))
        t, trip, _ = first_trip(lambda t: loop(t, 21, (INFO, 4, 0x61)), 60)
        self.assertEqual((t, trip.numbers["port"], trip.numbers["peer"], trip.numbers["op"]), (3.0, INFO, 4, 0x61))

    def test_a_burst_shorter_than_3_s_never_trips(self) -> None:
        def burst(t: float) -> dict:
            doc = ordinary(t)
            n = 0 if t < 10 else (500 if t < 11 else (1000 if t < 12 else 1001))   # 2 s of 500 a second
            doc["requests"].append({"port": ARC, "peer": 5, "op": 0x3010, "n": n})
            return doc
        t, trip, watch = first_trip(burst, 120)
        self.assertEqual((t, trip), (None, None))
        self.assertEqual(watch.peaks.request_rate, 500.0)

    def test_the_sum_past_the_tables_is_a_key_of_its_own(self) -> None:
        def flood(t: float) -> dict:
            doc = ordinary(t)
            doc["uncounted"] = int(300 * t)
            return doc
        t, trip, _ = first_trip(flood, 30)
        self.assertEqual((t, trip.kind, trip.numbers), (3.0, "requests", {"uncounted": 1, "rate": 300.0,
                                                                           "seconds": 3.0}))

    def test_ordinary_traffic_never_trips_over_ten_minutes_of_uneven_reads(self) -> None:
        rng = random.Random(72)
        watch = lg.Watch(RATE)
        t = 0.0
        for _ in range(600):
            length = rng.uniform(0.02, 0.6)   # one ssh round trip; the status was taken somewhere inside it
            taken = t + rng.uniform(0, length)
            self.assertIsNone(feed(watch, ordinary(taken), t, t + length), t)
            t += max(length, rng.uniform(0.8, 1.6))
        self.assertEqual(watch.restarts, 0)
        self.assertLessEqual(watch.peaks.flow_requests, 1)
        self.assertLessEqual(watch.peaks.arc_reads, 1)

    def test_a_steady_19_a_second_never_reads_above_20_whatever_the_reads_take(self) -> None:
        # The rate divides by the widest gap two statuses can have, so a read
        # that took long never makes a rate read high.
        rng = random.Random(19)
        watch = lg.Watch(RATE)
        t = 0.0
        for _ in range(300):
            length = rng.uniform(0.0, 0.9)
            taken = t + rng.choice((0.0, length))   # the status at the read's very start or end
            self.assertIsNone(feed(watch, loop(taken, 19), t, t + length), t)
            t += 1.0 + rng.uniform(0, 0.3)
        # Whole counts: one request more than 19 a second may fall into a gap of a second.
        self.assertLessEqual(watch.peaks.request_rate, lg.REQUEST_RATE)

    def test_two_of_our_flow_requests_to_a_peer_within_5_s_trip(self) -> None:
        def twice(t: float) -> dict:
            doc = ordinary(t)
            doc["sent"].append({"peer": 6, "op": 0x0100, "n": (1 if t >= 2 else 0) + (1 if t >= 6 else 0)})
            return doc
        t, trip, _ = first_trip(twice, 60)
        self.assertEqual((t, trip.kind), (6.0, "flow_requests"))
        self.assertEqual(trip.numbers, {"peer": 6, "requests": 2, "within_s": 5.0})

    def test_two_of_our_arc_reads_of_a_peer_within_5_s_trip(self) -> None:
        def twice(t: float) -> dict:
            doc = ordinary(t)
            doc["sent"].append({"peer": 7, "op": 0x1000, "n": (1 if t >= 3 else 0) + (1 if t >= 7 else 0)})
            return doc
        t, trip, _ = first_trip(twice, 60)
        self.assertEqual((t, trip.kind), (7.0, "arc_reads"))
        self.assertEqual(trip.numbers, {"peer": 7, "reads": 2, "within_s": 5.0})

    def test_arc_reads_six_seconds_apart_or_their_pages_never_trip(self) -> None:
        def apart(t: float) -> dict:
            doc = ordinary(t)
            doc["sent"].append({"peer": 7, "op": 0x1000, "n": (1 if t >= 3 else 0) + (1 if t >= 9 else 0)})
            doc["sent"].append({"peer": 7, "op": 0x2000, "n": 32 * int(t)})   # pages are not reads
            return doc
        self.assertEqual(first_trip(apart, 60)[:2], (None, None))

    def test_flow_requests_six_seconds_apart_or_stops_never_trip(self) -> None:
        def apart(t: float) -> dict:
            doc = ordinary(t)
            doc["sent"].append({"peer": 6, "op": 0x0100, "n": (1 if t >= 2 else 0) + (1 if t >= 8 else 0)})
            doc["sent"].append({"peer": 6, "op": 0x0101, "n": int(t)})   # stops are not requests
            return doc
        self.assertEqual(first_trip(apart, 60)[:2], (None, None))

    def test_a_tx_or_rx_flow_above_1_25_rate_over_fpp_trips(self) -> None:
        for direction in ("tx", "rx"):
            def fast(t: float, d=direction) -> dict:
                doc = ordinary(t)
                doc[d].append({**doc[d][0], "flow": 7, "fpp": 32, "packets": int(1.26 * RATE / 32 * t)})
                return doc
            t, trip, _ = first_trip(fast, 30)
            self.assertEqual((t, trip.kind), (1.0, "packets"), direction)
            self.assertEqual(trip.numbers, {"direction": direction, "flow": 7, "pps": 1890.0, "limit": 1875.0})

            def brisk(t: float, d=direction) -> dict:
                doc = ordinary(t)
                doc[d].append({**doc[d][0], "flow": 7, "fpp": 32, "packets": int(1.24 * RATE / 32 * t)})
                return doc
            self.assertEqual(first_trip(brisk, 30)[:2], (None, None), direction)

    def test_a_slot_whose_flow_changed_gets_the_lenient_limit(self) -> None:
        # Slot 1 goes from fpp 32 to fpp 16 between two reads: its packets of
        # that second are judged at fpp 16's rate, never at 32's.
        def changed(t: float) -> dict:
            doc = ordinary(t)
            doc["rx"][0]["fpp"] = 32 if t < 5 else 16
            doc["rx"][0]["packets"] = int(min(t, 5) * RATE / 32 + max(0.0, t - 5) * RATE / 16)
            return doc
        self.assertEqual(first_trip(changed, 30)[:2], (None, None))

    def test_counts_that_go_back_start_the_comparison_again(self) -> None:
        # The service restarted at 10 s: its counts start from 0, and a loop
        # after it is still found.
        def restarted(t: float) -> dict:
            return ordinary(t) if t < 10 else loop(t - 10)
        t, trip, watch = first_trip(restarted, 60)
        self.assertEqual((t, trip.kind, watch.restarts), (13.0, "requests", 1))

    def test_a_status_of_another_shape_is_no_sample(self) -> None:
        for broken in ({"requests": "none"}, {"uncounted": -1}, {"clock": None}, {"tx": [{"flow": 1}]},
                       {"sent": [{"peer": "1", "op": 256, "n": 1}]}):
            with self.assertRaises(ValueError, msg=broken):
                lg.sample(0.0, 0.1, {**status(), **broken})

    def test_a_read_at_the_same_moment_is_waited_past(self) -> None:
        watch = lg.Watch(RATE)
        self.assertIsNone(feed(watch, loop(0), 5.0))
        self.assertIsNone(feed(watch, loop(100), 5.0))   # no time between them: no rate yet
        self.assertEqual(watch.last.t, 5.0)


class GuardTests(unittest.TestCase):
    """live_guard.main against the stand-in ssh."""

    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.box = self.tmp / "box"
        self.box.mkdir()
        bin_dir = self.tmp / "bin"
        bin_dir.mkdir()
        p = bin_dir / "ssh"
        p.write_text(f"#!{sys.executable}\n{FAKE_SSH}", encoding="utf-8")
        p.chmod(0o755)
        self.flag = self.tmp / "config" / "EVENT-NOW"
        self.flag.parent.mkdir()
        self.env_file = self.tmp / "config" / "iem-pc.env"
        self.env_file.write_text(f"# iempc's private env\nPC_SSH=tester@pc.test\nGEN3_HOST=\"{HOST}\"\n",
                                 encoding="utf-8")
        patcher = mock.patch.dict(os.environ, {"PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
                                               "FAKE_BOX": str(self.box), "IEMMIXER_EVENT_NOW": str(self.flag),
                                               "PC_ENV": str(self.env_file)})
        patcher.start()
        self.addCleanup(patcher.stop)
        self.now = 1000.0

    def plan(self, statuses: list) -> None:
        (self.box / "statuses.json").write_text(json.dumps(statuses), encoding="utf-8")

    def clock(self):
        test = self

        class Clock:
            def now(self) -> float:
                return test.now

            def sleep(self, seconds: float) -> None:
                test.now += seconds
        return Clock()

    def guard(self, *argv: str, clock=None) -> tuple[int, list[dict], str]:
        """One run of the guard, bounded: 30 simulated seconds unless the
        test says otherwise, so a guard that misses its end fails, never hangs."""
        if "--seconds" not in argv:
            argv = (*argv, "--seconds", "30")
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = lg.main(["--rate", str(RATE), *argv], clock=clock or self.clock())
        return code, [json.loads(line) for line in out.getvalue().splitlines() if line.strip()], err.getvalue()

    def calls(self) -> list[list[str]]:
        log = self.box / "calls.log"
        if not log.exists():
            return []
        return [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]

    def commands(self) -> list[str]:
        """Each call's host and command, its ssh options taken off."""
        out = []
        for args in self.calls():
            while args and args[0] == "-o":
                args = args[2:]
            out.append(" ".join(args))
        return out

    STATUS = f"{HOST} /opt/iemmixer/bin/iem-dante-dev ctl status"
    STOP = f"{HOST} sudo -n systemctl stop iem-server iem-engine iem-dante"

    def test_a_loop_trips_the_stop_within_3_s(self) -> None:
        self.plan([loop(t) for t in range(30)])
        code, docs, err = self.guard()
        self.assertEqual(code, 1, err)
        self.assertEqual(self.commands(), [self.STATUS] * 4 + [self.STOP])
        end = docs[-1]
        self.assertEqual((end["end"], end["t_s"], end["stop_exit"]), ("requests", 3.0, 0))
        self.assertEqual(end["trip"], {"port": ARC, "peer": 1, "op": 0x2204, "rate": 2250.0, "seconds": 3.0})
        self.assertIn("TRIPPED: requests", err)
        self.assertIn("the Gen 3 stack stopped", err)
        # Every call goes out batch-mode with bounded connects, never asking for a password.
        for args in self.calls():
            self.assertEqual(args[:9], ["-o", "BatchMode=yes", "-o", "ConnectTimeout=5", "-o",
                                        "ServerAliveInterval=1", "-o", "ServerAliveCountMax=5", HOST])

    def test_ordinary_traffic_never_trips_it(self) -> None:
        self.plan([ordinary(t) for t in range(25)])
        code, docs, err = self.guard("--seconds", "25")
        self.assertEqual(code, 0, err)
        self.assertNotIn(self.STOP, self.commands())
        self.assertEqual(len(self.commands()), 25)
        self.assertEqual([d["t_s"] for d in docs], [10.0, 20.0, 25.0])
        line, end = docs[0], docs[-1]
        self.assertEqual((line["reads"], line["failed"], line["tx_flows"], line["rx_flows"]), (11, 0, 1, 1))
        self.assertEqual((end["end"], end["reads"], end["flow_requests_5s_max"]), ("seconds", 25, 1))
        self.assertEqual(end["arc_reads_5s_max"], 1)
        self.assertLess(end["packet_ratio_max"], 1.25)
        self.assertLessEqual(end["request_rate_max"], 3.0)
        # Numbers only: keys, numbers and the end's fixed word.
        for doc in docs:
            for key, value in doc.items():
                self.assertTrue(type(value) in (int, float) or (key == "end" and value in ("seconds",)), (key, value))

    def test_the_engine_s_pump_counts_are_reported_and_never_trip(self) -> None:
        def with_engine(t: int) -> dict:
            doc = ordinary(t)
            doc["engine"] = {"media": 1, "offset": 320, "playout": 192, "blocks": 3000 * t,
                             "late": 1000 * t, "missed": 500 * t, "overruns": 7 * t,
                             "rebootstraps": 40 * t, "max_process_us": 150}
            return doc
        self.plan([with_engine(t) for t in range(15)])
        code, docs, err = self.guard("--seconds", "12")
        self.assertEqual(code, 0, err)
        self.assertNotIn(self.STOP, self.commands())
        line = docs[0]
        self.assertEqual((line["pump_late"], line["pump_missed"], line["pump_rebootstraps"]),
                         (10000, 5000, 400))

    def test_without_an_engine_the_line_has_no_pump_counts(self) -> None:
        self.plan([ordinary(t) for t in range(15)])
        code, docs, err = self.guard("--seconds", "12")
        self.assertEqual(code, 0, err)
        self.assertFalse(any(k.startswith("pump_") for d in docs for k in d), docs)

    def test_the_flag_exits_0_and_stops_nothing(self) -> None:
        self.plan([ordinary(t) for t in range(30)])
        with mock.patch.dict(os.environ, {"FAKE_FLAG_AT": "5"}):
            code, docs, err = self.guard()
        self.assertEqual(code, 0, err)
        self.assertEqual(self.commands(), [self.STATUS] * 6)
        self.assertEqual((docs[-1]["end"], docs[-1]["t_s"]), ("flag", 6.0))

    def test_flow_requests_and_packets_trip_the_stop_too(self) -> None:
        def twice(t: int) -> dict:
            doc = ordinary(t)
            doc["sent"].append({"peer": 6, "op": 0x0100, "n": (1 if t >= 2 else 0) + (1 if t >= 4 else 0)})
            return doc
        self.plan([twice(t) for t in range(30)])
        code, docs, _ = self.guard()
        self.assertEqual((code, docs[-1]["end"], docs[-1]["trip"]["peer"]), (1, "flow_requests", 6))
        self.assertEqual(self.commands()[-1], self.STOP)
        (self.box / "reads").unlink()
        (self.box / "calls.log").unlink()

        def doubled(t: int) -> dict:
            doc = ordinary(t)
            doc["tx"][0]["packets"] = 2 * round(t * RATE / 16)   # every packet sent twice
            return doc
        self.plan([doubled(t) for t in range(30)])
        code, docs, _ = self.guard()
        self.assertEqual((code, docs[-1]["end"], docs[-1]["t_s"]), (1, "packets", 1.0))
        self.assertEqual(docs[-1]["trip"], {"direction": "tx", "flow": 1, "pps": 6000.0, "limit": 3750.0})
        self.assertEqual(self.commands(), [self.STATUS] * 2 + [self.STOP])

    def test_a_failed_stop_still_exits_1_and_names_the_way_by_hand(self) -> None:
        self.plan([loop(t) for t in range(30)])
        with mock.patch.dict(os.environ, {"FAKE_STOP_EXIT": "1"}):
            code, docs, err = self.guard()
        self.assertEqual((code, docs[-1]["stop_exit"]), (1, 1))
        self.assertIn("WARNING: the stop failed (exit 1): sudo: a password is required; stop it by hand: ssh "
                      "<GEN3_HOST> sudo -n systemctl stop iem-server iem-engine iem-dante", err)
        self.assertNotIn(HOST, err)

    def test_failed_reads_are_counted_and_compared_across(self) -> None:
        # Reads 3 to 6 fail (the socket is gone a moment): the guard reads on,
        # says so once, and compares the next good read with the last good one.
        plan = [loop(t, 15) for t in range(30)]
        for t in range(3, 7):
            plan[t] = None
        self.plan(plan)
        code, docs, err = self.guard("--seconds", "12")
        self.assertEqual(code, 0, err)
        self.assertEqual((docs[0]["reads"], docs[0]["failed"]), (7, 4))
        self.assertEqual(err.count("a status read failed (exit 1)"), 1)
        self.assertEqual(err.count("status reads answer again"), 1)
        self.assertEqual(docs[-1]["request_rate_max"], 15.0)

    def test_a_read_that_outlives_its_bound_is_left_running_and_none_starts_beside_it(self) -> None:
        self.plan([ordinary(t) for t in range(30)])
        with mock.patch.dict(os.environ, {"FAKE_SLOW_AT": "1", "FAKE_SLOW_S": "1.5"}), \
                mock.patch.object(lg, "READ_S", 0.2):
            code, docs, err = self.guard("--seconds", "6")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.commands(), [self.STATUS] * 2)   # only the slow read after the first
        self.assertEqual((docs[-1]["reads"], docs[-1]["failed"]), (1, 5))
        self.assertIn("a status read still runs after 0.2 s (left to end by itself)", err)
        deadline = time.monotonic() + 15
        while not (self.box / "slow.done").exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertTrue((self.box / "slow.done").exists(), "the slow read never ended by itself")

    def test_a_signal_ends_it_with_its_summary(self) -> None:
        self.plan([ordinary(t) for t in range(30)])
        test = self

        class Clock:
            def now(self) -> float:
                return test.now

            def sleep(self, seconds: float) -> None:
                test.now += seconds
                if test.now >= 1004:
                    signal.raise_signal(signal.SIGTERM)
        before = signal.getsignal(signal.SIGTERM)
        code, docs, err = self.guard(clock=Clock())
        self.assertEqual((code, docs[-1]["end"], docs[-1]["reads"]), (0, "signal", 4), err)
        self.assertIs(signal.getsignal(signal.SIGTERM), before)

    def test_refusals_before_any_call(self) -> None:
        self.plan([ordinary(0)])
        cases = [
            ("no env file", lambda: self.env_file.unlink(), "missing (iempc's private env"),
            ("no GEN3_HOST", lambda: self.env_file.write_text("PC_SSH=a\n", encoding="utf-8"), "no GEN3_HOST"),
            ("an option", lambda: self.env_file.write_text("GEN3_HOST=-oProxyCommand=x\n", encoding="utf-8"),
             "must be an ssh destination"),
            ("not KEY=VALUE", lambda: self.env_file.write_text("GEN3_HOST\n", encoding="utf-8"), "not KEY=VALUE"),
            ("the flag", lambda: self.flag.write_text("now\n", encoding="utf-8"), "an event is on"),
        ]
        for what, change, words in cases:
            saved = self.env_file.read_text(encoding="utf-8")
            change()
            code, docs, err = self.guard()
            self.assertEqual((code, docs, self.calls()), (2, [], []), what)
            self.assertIn(words, err, what)
            self.env_file.write_text(saved, encoding="utf-8")
            self.flag.unlink(missing_ok=True)
        for argv, words in ((["--rate", "47999"], "--rate must be one of"), (["--seconds", "0"], "above 0")):
            out, err = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                code = lg.main(["--rate", str(RATE), *argv], clock=self.clock())
            self.assertEqual((code, out.getvalue(), self.calls()), (2, "", []), argv)
            self.assertIn(words, err.getvalue(), argv)


if __name__ == "__main__":
    unittest.main()
