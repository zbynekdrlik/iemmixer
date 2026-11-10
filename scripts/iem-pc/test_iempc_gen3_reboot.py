"""Tests for scripts/iem-pc/iempc_gen3_reboot.py, `iempc gen3-gate reboot` and
`reset` (#72). No box in CI: a stand-in Box plays the ssh calls, the restart
moves its boot id, and each test makes one thing come back wrong. Every value
is synthetic (P6)."""
from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import iempc_gen3_reboot as rb

sys.path.insert(1, str(HERE.parent / "gen3"))
from test_deploy import expand, template_lines

SHA = "a" * 40
LOCKED = "\x1b[32m[PHASE-LOCK] engaged: the frequency word now follows the PTP phase error (e=-9.5us, word +1ppm)\x1b[0m"


def state(rev: int, level: float = -16.4) -> str:
    return json.dumps({"payload": {"rev": rev, "state": {"mixes": {"engineer": {"out": {"volume_db": level}}}}}})


class Ip:
    class StepError(Exception):
        pass

    class Refused(Exception):
        pass


class FakeBox:
    """One boot's answers; `words` overrides one call's."""
    ip = Ip
    optional = frozenset()

    def __init__(self, **over) -> None:
        self.over = over

    def call(self, words, **_):
        key = " ".join(words)
        answers = {" ".join(rb.DANTESYNC_LOG): LOCKED, " ".join(rb.KERNEL_LOG): "EXT4-fs (sda2): recovery complete\n",
                   " ".join(rb.READ_STATE): state(10), " ".join(rb.SUMS): "sums",
                   "systemctl --failed --no-legend": "", "tc qdisc show": "qdisc tbf 1: root",
                   "systemctl is-active " + " ".join(rb.UNITS) + " || true": "active\n" * len(rb.UNITS),
                   " ".join(rb.gate.ENGINE_JOURNAL): json.dumps({"MESSAGE": "x state rev 10 loaded"}),
                   rb.g3.ENGINE_BIN + " --version": f"iem-engine 2.0.0+{SHA}\n",
                   "python3 -I -": "aaaaaaa\n"}
        answers.update(self.over.get("calls", {}))
        if key not in answers:
            raise AssertionError(key)
        return answers[key]

    def wait(self, test, _limit):
        return test()

    def healthy(self):
        return self.over.get("healthy", True)

    def device_status(self):
        """The device's status; `rx` sets the RX flows of every read, `reads`
        the flows read by read (the last one stays)."""
        reads = self.over.get("reads")
        rx = (reads.pop(0) if len(reads) > 1 else reads[0]) if reads else self.over.get("rx", FLOWS)
        return {"clock": {"ready": self.over.get("ready", 1)}, "channels": [{"rx": 1, "status": 0x0101_0009}],
                "rx": rx}


# The least lateness of a second of each RX flow before the restart: our now
# less the sender's stamp (synthetic numbers).
BASE_NS = 380_000
FLOWS = [{"flow": 1, "lateness_min_ns": 400_000}, {"flow": 2, "lateness_min_ns": BASE_NS}]


def behind(us: int) -> list[dict]:
    """The flows as a box clock `us` behind the band's reads them (a packet
    from our future is 0, as the judge clamps it)."""
    return [{**f, "lateness_min_ns": max(0, f["lateness_min_ns"] - us * 1000)} for f in FLOWS]


# A healthy box's RX flows (#72, 2.0.0-dev.68): two low-latency senders
# (4 frames a packet) arrive within their stamp's sample and read 0 steadily.
LIVE = [{"flow": 1, "lateness_min_ns": 153_388}, {"flow": 2, "lateness_min_ns": 119_238},
        {"flow": 3, "lateness_min_ns": 0}, {"flow": 4, "lateness_min_ns": 684_812},
        {"flow": 5, "lateness_min_ns": 360_379}, {"flow": 6, "lateness_min_ns": 0}]


def live_behind(us: int) -> list[dict]:
    return [{**f, "lateness_min_ns": max(0, f["lateness_min_ns"] - us * 1000)} for f in LIVE]


def ready(rx: list[dict], clock: int = 1) -> dict:
    return {"clock": {"ready": clock}, "rx": rx}


def base_of(*reads: list[dict]):
    """The TX judge's baseline from reads of a ready clock's RX flows."""
    return rb.tx_baseline([ready(rx) for rx in reads])


def with_ns(rx: list[dict], **ns: int) -> list[dict]:
    """`rx` with flow N's lateness set by `fN=<ns>`."""
    return [{**f, "lateness_min_ns": ns.get(f"f{f['flow']}", f["lateness_min_ns"])} for f in rx]


def without(rx: list[dict], *flows: int) -> list[dict]:
    return [f for f in rx if f["flow"] not in flows]


def proved_seen(**over) -> rb.Seen:
    box, s = FakeBox(**over), rb.Seen(back_s=30.0)
    original = rb.soak.box_build
    rb.soak.box_build = lambda _box: over.get("sha", SHA)
    try:
        rb.prove(box, s, rb.time.monotonic(), "sums", SHA, over.get("base", base_of(FLOWS)))
    finally:
        rb.soak.box_build = original
    return s


def proved(**over) -> list[str]:
    return rb.failed(proved_seen(**over), over.get("rev_before", 9))


class ProveTests(unittest.TestCase):
    def test_a_clean_boot_passes(self):
        self.assertEqual(proved(), [])

    def test_each_thing_that_came_back_wrong_is_named(self):
        for word, over in (
                ("not_healthy", {"healthy": False}),
                ("units", {"calls": {"systemctl --failed --no-legend": "iem-napi.service failed\n"}}),
                ("units", {"calls": {"systemctl is-active " + " ".join(rb.UNITS) + " || true": "inactive\n"}}),
                ("no_shaping", {"calls": {"tc qdisc show": "qdisc fq_codel 0: root"}}),
                ("phase", {"calls": {" ".join(rb.DANTESYNC_LOG): LOCKED.replace("e=-9.5", "e=-721.7")}}),
                ("phase", {"calls": {" ".join(rb.DANTESYNC_LOG): "no lock\n"}}),
                ("unlocked", {"calls": {" ".join(rb.DANTESYNC_LOG): LOCKED + "\n[PTP] === UNLOCKED === Drift:+1us/s\n"}}),
                ("fs_errors", {"calls": {" ".join(rb.KERNEL_LOG): "EXT4-fs error (device sda2): bad block\n"}}),
                ("state", {"calls": {" ".join(rb.READ_STATE): "{"}}),
                ("state", {"rev_before": 11}),
                ("files", {"calls": {" ".join(rb.SUMS): "other"}}),
                ("build", {"sha": "b" * 40}),
                ("tx_late", {"rx": behind(700)}),
                ("tx_late", {"rx": FLOWS[:1]}),
                ("tx_late", {"rx": []}),
                ("tx_late", {"ready": 0}),
                ("tx_late", {"reads": [FLOWS, FLOWS, behind(200)]})):
            with self.subTest(word=word):
                self.assertIn(word, proved(**over))

    def test_tx_is_judged_from_ready_by_the_sorted_lateness_of_the_flows(self):
        # #72 6567516059: a clock behind the band's after ready is TX late at
        # every receiver. Within PHASE_US behind, or ahead, passes.
        for rx in (behind(100), behind(-400)):
            with self.subTest(rx=rx):
                self.assertNotIn("tx_late", proved(rx=rx))
        s = proved_seen(rx=behind(300))
        self.assertEqual((s.tx_late_us, s.tx_partial), (-300.0, 0))
        doc = rb.verdict("reboot", s, 9)
        self.assertEqual((doc["tx_late_us"], doc["tx_partial"], doc["tx_reads"]), (-300.0, 0, s.tx_reads))
        self.assertGreaterEqual(s.tx_reads, 3)   # the clock's wait, the RX wait and the watch

    def test_a_flow_not_yet_back_is_not_judged(self):
        # A read with fewer flows than the baseline is not judged; a watch
        # with no read judged fails (a flow that never came back).
        s = proved_seen(reads=[FLOWS[:1], behind(1000)[:1], FLOWS])
        self.assertEqual((s.tx_late_us, s.tx_partial), (0.0, 2))
        self.assertNotIn("tx_late", rb.failed(s, 9))
        s = proved_seen(rx=FLOWS[:1])
        self.assertEqual((s.tx_late_us, s.tx_reads), (None, 0))
        self.assertGreater(s.tx_partial, 0)
        self.assertIn("tx_late", rb.failed(s, 9))

    def test_a_low_flow_reading_0_after_the_restart_is_no_clock_shift(self):
        # Review of PR #106 (1): a flow above 0 before the restart and at 0
        # after it, with flow numbers changed, on a healthy box. Its drop is
        # its own lateness, under PHASE_US: no fail.
        base = base_of(LIVE)
        for rx, worst in ((with_ns(LIVE, f2=0), -119.2), (with_ns(LIVE, f1=0), -119.2),
                          ([{**f, "flow": 7 - f["flow"]} for f in with_ns(LIVE, f2=0)], -119.2)):
            with self.subTest(rx=rx):
                s = proved_seen(rx=rx, base=base)
                self.assertEqual(s.tx_late_us, worst)
                self.assertNotIn("tx_late", rb.failed(s, 9))

    def test_a_flow_at_0_vanishing_does_not_hide_a_clamped_flow(self):
        # Review of PR #106 (3): 200 us behind, two flows clamp to 0 while
        # the two flows at 0 before the restart are gone, or one is gone and
        # a new flow reads 0: the sorted values still show the shift.
        base = base_of(LIVE)
        gone = without(live_behind(200), 3, 6)
        self.assertIn("tx_late", proved(rx=gone, base=base))
        swapped = [*without(live_behind(200), 3), {"flow": 7, "lateness_min_ns": 0}]
        s = proved_seen(rx=swapped, base=base)
        self.assertEqual(s.tx_late_us, -200.0)
        self.assertIn("tx_late", rb.failed(s, 9))
        # A new flow above the others raises the sorted values: never a fail.
        self.assertNotIn("tx_late", proved(rx=[*LIVE, {"flow": 7, "lateness_min_ns": 900_000}], base=base))

    def test_flows_at_0_before_the_restart_are_normal(self):
        # #72, 2.0.0-dev.68: a low-latency sender reads 0 on a healthy box.
        # Its 0 carries no clock information: the box is judged by the rest.
        base = base_of(LIVE)
        self.assertIsNotNone(base)
        for rx in (LIVE, live_behind(100), live_behind(-400), [*LIVE, {"flow": 7, "lateness_min_ns": 0}]):
            with self.subTest(rx=rx):
                self.assertNotIn("tx_late", proved(rx=rx, base=base))
        s = proved_seen(rx=live_behind(100), base=base)
        self.assertEqual((s.tx_late_us, s.tx_partial), (-100.0, 0))

    def test_a_clock_behind_that_pushes_flows_to_0_fails(self):
        # 200 us behind: the flows at 153 and 119 us clamp to 0 and the least
        # of the rest (160 us) RISES over the baseline; the highest flow
        # still shows the whole shift.
        s = proved_seen(rx=live_behind(200), base=base_of(LIVE))
        self.assertEqual(s.tx_late_us, -200.0)
        self.assertIn("tx_late", rb.failed(s, 9))
        self.assertIn("tx_late", proved(rx=live_behind(700), base=base_of(LIVE)))

    def test_the_watch_reads_until_ready_plus_tx_watch_s(self):
        # A fake clock: each wait poll is 10 s. The clock goes ready at
        # 100 s, so the watch must read until 400 s; a clock behind the
        # band's from 380 s on is still inside it (it would not be inside
        # t0 + TX_WATCH_S).
        now = [0.0]

        class Clocked(FakeBox):
            def wait(self, test, limit):
                deadline = now[0] + limit
                while True:
                    if test():
                        return True
                    if now[0] > deadline:
                        return False
                    now[0] += 10.0

            def device_status(self):
                rx = behind(200) if 380.0 <= now[0] < 400.0 else FLOWS
                return {"clock": {"ready": int(now[0] >= 100.0)}, "channels": [{"rx": 1, "status": 0x0101_0009}],
                        "rx": rx}

        original = (rb.soak.box_build, rb.time.monotonic)
        rb.soak.box_build, rb.time.monotonic = (lambda _box: SHA), (lambda: now[0])
        try:
            s = rb.Seen(back_s=30.0)
            rb.prove(Clocked(), s, 0.0, "sums", SHA, base_of(FLOWS))
        finally:
            rb.soak.box_build, rb.time.monotonic = original
        self.assertEqual(s.ready_s, 100.0)
        self.assertGreaterEqual(now[0], 100.0 + rb.TX_WATCH_S)
        self.assertEqual(s.tx_late_us, -200.0)
        self.assertIn("tx_late", rb.failed(s, 9))

    def test_a_box_that_never_came_back_names_only_that(self):
        self.assertEqual(rb.failed(rb.Seen(), 1), ["not_back"])

    def test_a_level_that_did_not_come_back_fails_a_reset(self):
        s = rb.Seen(back_s=1.0, ready_s=1.0, healthy_s=1.0, rx_s=1.0, units_ok=True, shaped=True, phase_us=1.0,
                    fs_errors=0, rev=2, sums_same=True, build_same=True, level_ok=False, tx_late_us=0.0)
        self.assertEqual(rb.failed(s, 1), ["level"])


class RunTests(unittest.TestCase):
    def refusal(self, rx: list[dict], clock: int = 1) -> str:
        """The refusal of `run` on a stand-in box with these RX flows, its
        boot id unreadable: past the TX judge's baseline it refuses there."""
        class Ctx:
            env = {"GEN3_HOST": "box"}
        box = FakeBox(rx=rx, ready=clock, calls={" ".join(rb.BOOT_ID): ""})
        saved = (rb.gate.Box, rb.soak.end_left_job, rb.soak.box_build)
        rb.gate.Box = lambda *_: box
        rb.soak.end_left_job, rb.soak.box_build = (lambda _box: False), (lambda _box: SHA)
        try:
            with self.assertRaises(Ip.Refused) as caught:
                rb.run(Ctx(), Ip, "reboot")
        finally:
            rb.gate.Box, rb.soak.end_left_job, rb.soak.box_build = saved
        return str(caught.exception)

    def test_a_healthy_box_with_flows_at_0_gets_its_baseline(self):
        # #72, 2.0.0-dev.68: refused here before any change. Review of PR
        # #106 (6): the baseline itself, every flow high to low.
        box = FakeBox(rx=LIVE)
        self.assertEqual(rb.sample_baseline(box, box.device_status()),
                         rb.Baseline((684_812, 360_379, 153_388, 119_238, 0, 0)))

    def test_the_baseline_is_sampled_over_several_reads(self):
        # Review of PR #106 (2): sender jitter lowers a flow in one read;
        # the baseline keeps each sorted value's least over the reads.
        box = FakeBox(reads=[with_ns(LIVE, f4=500_000), LIVE])
        self.assertEqual(rb.sample_baseline(box, ready(with_ns(LIVE, f2=90_000))),
                         rb.Baseline((500_000, 360_379, 153_388, 90_000, 0, 0)))
        self.assertGreaterEqual(rb.BASELINE_S, 30.0)

    def test_a_box_with_no_flow_above_phase_us_is_refused_before_any_change(self):
        for rx in ([{"flow": 1, "lateness_min_ns": 0}], [{"flow": 1, "lateness_min_ns": 150_000}]):
            with self.subTest(rx=rx):
                message = self.refusal(rx)
                self.assertIn("above 150 us", message)
                self.assertIn("nothing was changed", message)

    def test_a_clock_not_ready_is_refused_with_its_own_message(self):
        # Review of PR #106 (5).
        message = self.refusal(LIVE, clock=0)
        self.assertIn("clock is not ready", message)
        self.assertNotIn("above", message)
        self.assertIn("nothing was changed", message)


class PureTests(unittest.TestCase):
    def test_only_the_first_lock_s_error_counts_and_unlocks_after_it(self):
        log = "=== UNLOCKED before\n" + LOCKED + "\n=== UNLOCKED\n" + LOCKED.replace("-9.5", "-300") + "\n=== UNLOCKED\n"
        self.assertEqual(rb.first_lock(log), (-9.5, 2))

    def test_the_lateness_reads_only_a_ready_clock_s_flows(self):
        # Review of PR #106 (4): the device's judge is a u64 saturated at 0
        # (dstack media/rx.rs), so a negative is never sent; one counts as
        # the device's own clamp, 0, never dropped.
        status = ready([{"flow": 1, "lateness_min_ns": 9}, {"flow": 2, "lateness_min_ns": 7}, {"flow": 3},
                        {"flow": 4, "lateness_min_ns": -1}, {"flow": 5, "lateness_min_ns": True}, "x"])
        self.assertEqual(rb.lateness(status), [9, 7, 0])
        for bad in ({**status, "clock": {"ready": 0}}, ready([]), {"clock": {"ready": 1}}, ready("x"), {}, None):
            with self.subTest(bad=bad):
                self.assertIsNone(rb.lateness(bad))

    def test_the_baseline_needs_a_flow_above_phase_us_and_takes_flows_at_0(self):
        # #72, 2.0.0-dev.68: the gate refused the healthy box because two
        # low-latency flows read 0. With no flow above PHASE_US the judge
        # could not see a clock PHASE_US behind.
        self.assertEqual(base_of(LIVE), rb.Baseline((684_812, 360_379, 153_388, 119_238, 0, 0)))
        self.assertEqual(base_of([{"flow": 1, "lateness_min_ns": 0}, *FLOWS]), rb.Baseline((400_000, BASE_NS, 0)))
        for rx in ([], [{"flow": 1, "lateness_min_ns": 0}], [{"flow": 1}], [{"flow": 1, "lateness_min_ns": 150_000}]):
            with self.subTest(rx=rx):
                self.assertIsNone(base_of(rx))
        self.assertIsNone(rb.tx_baseline([]))
        self.assertIsNone(rb.tx_baseline([None, ready(FLOWS, clock=0)]))
        # The reads with the most flows count; one with fewer is a flow not back.
        self.assertEqual(base_of(FLOWS[:1], FLOWS), rb.Baseline((400_000, BASE_NS)))

    def test_a_journal_recovery_is_no_filesystem_error(self):
        self.assertEqual(rb.fs_errors("EXT4-fs (sda2): recovery complete\nEXT4-fs (sda2): orphan cleanup\n"), 0)
        self.assertEqual(rb.fs_errors("blk: I/O error, dev sda\n"), 1)

    def test_the_loaded_rev_reads_journald_s_byte_lists(self):
        text = "\x1b[32mstate rev 7 loaded\x1b[0m"
        lines = [json.dumps({"MESSAGE": "state rev 3 loaded"}), json.dumps({"MESSAGE": list(text.encode())}), "nope"]
        self.assertEqual(rb.loaded_rev("\n".join(lines)), 7)

    def test_a_state_file_reads_or_is_none(self):
        self.assertEqual(rb.state_of(state(4, -1.5)), (4, -1.5))
        for bad in ("", "{}", json.dumps({"payload": {"rev": "4"}})):
            self.assertIsNone(rb.state_of(bad))

    def test_a_ws_frame_is_masked_text(self):
        frame = rb.ws_frame("ab")
        self.assertEqual(frame[:2], bytes([0x81, 0x82]))
        self.assertEqual(bytes(b ^ frame[2 + i % 4] for i, b in enumerate(frame[6:])), b"ab")


class SudoTests(unittest.TestCase):
    def test_every_sudo_line_is_in_the_template(self):
        template = {(runas, text) for runas, command in template_lines() for text in expand(command)}
        for words in (rb.REBOOT, rb.SYSRQ_ON, rb.SYSRQ, rb.READ_STATE, rb.SUMS, rb.DANTESYNC_LOG, rb.KERNEL_LOG):
            runas = "root" if words[2] != "-u" else words[3]
            command = " ".join(words[2:] if words[2] != "-u" else words[4:])
            self.assertIn((runas, command), template)


if __name__ == "__main__":
    unittest.main()
