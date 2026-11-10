"""Tests for scripts/iem-pc/gen3_receivers.py (#72): the line of `iem-dante-dev
receivers` read and its reads summed over a run. Every value is synthetic."""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gen3_receivers as gr  # noqa: E402

KEY = "0123456789abcdef"
OTHER = "fedcba9876543210"


def flow(key: str = KEY, slot: int = 3, latency_us: int = 250, beats: int = 5, peak_us: int | None = 120,
         late: int = 0, late_total: int = 7) -> dict:
    return {"key": key, "slot": slot, "latency_us": latency_us, "beats": beats, "peak_us": peak_us, "late": late,
            "late_total": late_total}


def line(*flows: dict, heard: int = 1) -> dict:
    return {"receivers": {"devices": 4, "unread": 0, "no_id": 0, "receivers": 1, "heard": heard,
                          "flows": list(flows), "red": False}}


class ParseTests(unittest.TestCase):
    def test_a_line_reads_into_its_flows(self) -> None:
        read = gr.parse(line(flow(), flow(OTHER, slot=1, peak_us=None), heard=2))
        self.assertEqual(read, gr.Read(2, (gr.Flow(KEY, 3, 250, 5, 120, 0, 7),
                                           gr.Flow(OTHER, 1, 250, 5, None, 0, 7))))

    def test_anything_else_is_no_read(self) -> None:
        bad = [None, [], {}, {"receivers": []}, {"health": {}},
               line(heard=-1), line(heard=True), {"receivers": {**line()["receivers"], "red": 0}},
               {"receivers": {**line()["receivers"], "flows": {}}},
               line(flow(key="0123")), line(flow(key="0123456789ABCDEF")), line(flow(key=5)), line("x"),
               line(flow(slot="3")), line(flow(late=-1)), line(flow(late_total=1.5)), line(flow(peak_us=-1)),
               line(flow(peak_us=False)), line(flow(beats=None))]
        for doc in bad:
            with self.subTest(doc=doc):
                self.assertIsNone(gr.parse(doc))

    def test_the_box_command_listens_five_seconds_on_our_address(self) -> None:
        self.assertEqual(gr.words("/bin/x", "192.0.2.20"),
                         ["/bin/x", "receivers", "--ip", "192.0.2.20", "--seconds", "5"])


class TallyTests(unittest.TestCase):
    def tally(self, *docs: dict | None) -> gr.Tally:
        t = gr.Tally()
        for d in docs:
            t.see(gr.parse(d) if d is not None else None)
        return t

    def test_steady_receivers_pass_with_their_margin(self) -> None:
        t = self.tally(line(flow(peak_us=120)), line(flow(peak_us=200, late_total=7)))
        self.assertEqual(t.numbers(), {"reads": 2, "failed": 0, "heard_max": 1, "flows_max": 1, "late": 0,
                                       "over": 0, "margin_us": 50})
        self.assertEqual(t.failures(), [])

    def test_a_receiver_s_late_count_is_summed_read_to_read_per_slot(self) -> None:
        t = self.tally(line(flow(late=1, late_total=7), flow(OTHER, slot=3, late_total=100)),
                       line(flow(late_total=9), flow(OTHER, slot=3, late_total=100)),
                       line(flow(late_total=2), flow(OTHER, slot=3, late_total=101)))
        # 1 in the first listen, then 7 -> 9, then a count that went back (2 from 0), the other receiver's 1.
        self.assertEqual(t.late, 1 + 2 + 2 + 1)
        self.assertEqual(t.failures(), ["receiver_late"])

    def test_a_peak_past_the_receiver_s_latency_is_red_one_at_the_latency_is_not(self) -> None:
        self.assertEqual(self.tally(line(flow(peak_us=250))).failures(), [])
        t = self.tally(line(flow(peak_us=251)), line(flow(peak_us=300)))
        self.assertEqual((t.over, t.margin_us, t.failures()), (2, -50, ["receiver_late"]))

    def test_a_flow_not_heard_is_not_judged(self) -> None:
        t = self.tally(line(flow(beats=0, late=4, late_total=50, peak_us=999), heard=0))
        self.assertEqual((t.late, t.over, t.flows_max, t.margin_us), (0, 0, 0, None))
        self.assertEqual(t.failures(), ["receiver_unread"])

    def test_no_read_that_heard_a_receiver_fails_unread(self) -> None:
        self.assertEqual(gr.Tally().failures(), ["receiver_unread"])
        t = self.tally(None, line(heard=0), None)
        self.assertEqual((t.reads, t.failed, t.heard_max, t.failures()), (1, 2, 0, ["receiver_unread"]))
        t = self.tally(None, line(flow(late=3)))
        self.assertEqual(t.failures(), ["receiver_late"])
        self.assertEqual(t.heard_max, 1)


if __name__ == "__main__":
    unittest.main()
