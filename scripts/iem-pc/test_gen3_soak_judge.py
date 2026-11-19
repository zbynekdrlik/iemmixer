"""Tests for scripts/iem-pc/gen3_soak_judge.py (#130): the soak gate's pure
judge, the recorded reads turned into the verdict's numbers. Every value is
synthetic (P6)."""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import gen3_soak_judge as judge  # noqa: E402


class JudgeTests(unittest.TestCase):
    def test_growth_counts_each_rise_and_a_count_that_went_back_from_zero(self) -> None:
        self.assertEqual(judge.growth([{"a": 1}, {"a": 3}, {"a": 2, "b": 1}, {"a": 2, "b": 1}]), (5, 1))
        self.assertEqual(judge.growth([{"a": 4}]), (0, 0))
        self.assertEqual(judge.growth([]), (0, 0))

    def test_a_restart_s_window_takes_its_reads_rises_and_the_rest_judge_as_ever(self) -> None:
        def s(blocks, ready=1, holdover=0, holdovers=0, re_anchors=0, epoch=0, media=1, missed=0, overruns=0):
            return {"ready": ready, "epoch": epoch, "re_anchors": re_anchors, "holdovers": holdovers,
                    "holdover": holdover, "rx_late": {}, "tx_holes": {}, "rx_holes": {}, "rx_off": 0, "lateness": 0,
                    "engine": {"media": media, "blocks": blocks, "late": 0, "missed": missed, "overruns": overruns,
                               "rebootstraps": missed // 3, "max_process_us": 9}}
        samples = [s(10), s(20, holdover=1, holdovers=1), s(30, ready=0, media=0, holdovers=1, re_anchors=1,
                                                            epoch=1, missed=3, overruns=1),
                   s(40, holdovers=1, re_anchors=1, epoch=1, missed=3, overruns=1),
                   s(40, holdover=1, holdovers=2, re_anchors=1, epoch=1, missed=3, overruns=1)]
        j = judge.judged(samples, [False, True, True, True, False])
        self.assertEqual(j["window"], {"holdovers": 1, "in_holdover": 1, "re_anchors": 2, "clock_not_ready": 1,
                                       "pump_free": 1, "pump_late": 0, "pump_missed": 3, "pump_rebootstraps": 1})
        # After the window the holdover and the frozen blocks count; the overrun inside it counts all the same.
        self.assertEqual((j["holdovers"], j["in_holdover"], j["re_anchors"], j["clock_not_ready"], j["pump_missed"],
                          j["pump_overruns"], j["pump_stalled"]), (1, 1, 0, 0, 0, 1, 1))
        # Without a window every read judges as before.
        plain = judge.judged(samples)
        self.assertEqual((plain["holdovers"], plain["re_anchors"], plain["pump_missed"], plain["pump_stalled"]),
                         (2, 2, 3, 2))
        self.assertEqual(plain["window"], dict.fromkeys(judge.WINDOW_KEYS, 0))

    def test_the_clock_recovers_at_the_first_clean_read_after_the_window_s_last_disturbed_one(self) -> None:
        def s(ready=1, holdover=0, media=1):
            return {"ready": ready, "holdover": holdover, "engine": {"media": media}}
        base = s()
        # Read times after the baseline; the restart at 100 s, the window 120 s.
        self.assertEqual(judge.recovered([base, s(), s(holdover=1), s(ready=0, media=0), s(), s()],
                                        [90.0, 101.0, 130.5, 160.0, 300.0], 100.0, 120.0), 60.0)
        self.assertEqual(judge.recovered([base, s(), s()], [101.0, 102.0], 100.0, 120.0), 1.0)
        self.assertIsNone(judge.recovered([base, s(holdover=1), s(ready=0)], [101.0, 210.0], 100.0, 120.0))
        # A disturbed read after the window does not move the recovery (it fails its own word).
        self.assertEqual(judge.recovered([base, s(holdover=1), s(), s(ready=0)], [110.0, 150.0, 250.0], 100.0, 120.0),
                         50.0)
        # Back only after the window: later than its bound (clock_not_recovered).
        self.assertEqual(judge.recovered([base, s(ready=0), s()], [210.0, 230.0], 100.0, 120.0), 130.0)
        self.assertIsNone(judge.recovered([base, {**s(), "engine": None}], [101.0], 100.0, 120.0))

    def test_a_hole_is_the_longest_wait_between_reads_and_the_run_s_edges(self) -> None:
        self.assertEqual(judge.hole([1.0, 2.0, 70.0], 0.5, 71.0), 68.0)
        self.assertEqual(judge.hole([], 3.0, 10.0), 7.0)


if __name__ == "__main__":
    unittest.main()
