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
    """The flows as a box clock `us` behind the band's reads them."""
    return [{**f, "lateness_min_ns": f["lateness_min_ns"] - us * 1000} for f in FLOWS]


def proved_seen(**over) -> rb.Seen:
    box, s = FakeBox(**over), rb.Seen(back_s=30.0)
    original = rb.soak.box_build
    rb.soak.box_build = lambda _box: over.get("sha", SHA)
    try:
        rb.prove(box, s, rb.time.monotonic(), "sums", SHA, over.get("base", BASE_NS))
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
                ("tx_late", {"rx": [{"flow": 1, "lateness_min_ns": 0}, *FLOWS]}),
                ("tx_late", {"rx": []}),
                ("tx_late", {"ready": 0}),
                ("tx_late", {"reads": [FLOWS, FLOWS, behind(200)]})):
            with self.subTest(word=word):
                self.assertIn(word, proved(**over))

    def test_tx_is_judged_from_ready_by_the_least_lateness_of_every_flow(self):
        # #72 6567516059: a clock behind the band's after ready is TX late at
        # every receiver. Within PHASE_US behind, or ahead, passes; a flow
        # not yet back only raises the least, so it never fails the boot.
        for rx in (behind(100), behind(-400), FLOWS[:1]):
            with self.subTest(rx=rx):
                self.assertNotIn("tx_late", proved(rx=rx))
        s = proved_seen(rx=behind(700))
        self.assertEqual((s.tx_late_us, s.tx_future), (-700.0, 0))
        self.assertEqual(rb.verdict("reboot", s, 9)["tx_late_us"], -700.0)

    def test_a_packet_from_our_future_fails_even_within_the_bound(self):
        # The judge clamps a packet stamped after our now to 0, so a small
        # baseline cannot show the whole shift: any 0 is a clock behind.
        s = proved_seen(base=100_000, rx=[{"flow": 1, "lateness_min_ns": 0}])
        self.assertEqual((s.tx_late_us, s.tx_future), (-100.0, 1))
        self.assertIn("tx_late", rb.failed(s, 9))

    def test_a_box_that_never_came_back_names_only_that(self):
        self.assertEqual(rb.failed(rb.Seen(), 1), ["not_back"])

    def test_a_level_that_did_not_come_back_fails_a_reset(self):
        s = rb.Seen(back_s=1.0, ready_s=1.0, healthy_s=1.0, rx_s=1.0, units_ok=True, shaped=True, phase_us=1.0,
                    fs_errors=0, rev=2, sums_same=True, build_same=True, level_ok=False, tx_late_us=0.0)
        self.assertEqual(rb.failed(s, 1), ["level"])


class PureTests(unittest.TestCase):
    def test_only_the_first_lock_s_error_counts_and_unlocks_after_it(self):
        log = "=== UNLOCKED before\n" + LOCKED + "\n=== UNLOCKED\n" + LOCKED.replace("-9.5", "-300") + "\n=== UNLOCKED\n"
        self.assertEqual(rb.first_lock(log), (-9.5, 2))

    def test_the_least_lateness_reads_only_a_ready_clock_s_flows(self):
        status = {"clock": {"ready": 1}, "rx": [{"flow": 1, "lateness_min_ns": 9}, {"flow": 2, "lateness_min_ns": 7},
                                                {"flow": 3}, {"flow": 4, "lateness_min_ns": -1},
                                                {"flow": 5, "lateness_min_ns": True}, "x"]}
        self.assertEqual(rb.least_lateness(status), 7)
        for bad in ({**status, "clock": {"ready": 0}}, {"clock": {"ready": 1}, "rx": []}, {"clock": {"ready": 1}},
                    {"clock": {"ready": 1}, "rx": "x"}, {}, None):
            with self.subTest(bad=bad):
                self.assertIsNone(rb.least_lateness(bad))

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
