"""Tests for scripts/pc-tuning/latency_report.py (pure functions over the
xperf texts, the spike's report and the PC samples)."""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import latency_report as lr  # noqa: E402

DPCISR = """
--------------------------
DPC Info
--------------------------
Total = 3000 for module yaic.sys
Elapsed Time, >        0 usecs AND <=        1 usecs,      0, or   0.00%
Elapsed Time, >        8 usecs AND <=       16 usecs,   2990, or  99.67%
Elapsed Time, >       64 usecs AND <=      128 usecs,     10, or   0.33%
Total = 20 for module dxgkrnl.sys
Elapsed Time, >      128 usecs AND <=      256 usecs,     15, or  75.00%
Elapsed Time, >      512 usecs AND <=     1024 usecs,      5, or  25.00%
yaic.sys: 45000 usec (0.10% CPU 2 usage)
dxgkrnl.sys: 3000 usec (0.01% CPU 14 usage)
--------------------------
Interrupt Info
--------------------------
Total = 3000 for module yaic.sys
Elapsed Time, >        2 usecs AND <=        4 usecs,   3000, or 100.00%
Total = 2 for module ndis.sys
Elapsed Time, >     2048 usecs,      2, or 100.00%
"""


class DpcIsrTests(unittest.TestCase):
    def test_modules_maxima_and_counts_above_the_limits(self) -> None:
        d = lr.parse_dpcisr(DPCISR)
        self.assertEqual(d["dpc"]["yaic.sys"], {"count": 3000, "max_us": 128, "open": False, "over": {"64": 10, "128": 0, "256": 0, "512": 0}})
        self.assertEqual(d["dpc"]["dxgkrnl.sys"]["max_us"], 1024)
        self.assertEqual(d["dpc"]["dxgkrnl.sys"]["over"], {"64": 20, "128": 20, "256": 5, "512": 5})
        self.assertEqual(d["isr"]["ndis.sys"], {"count": 2, "max_us": 2048, "open": True, "over": {"64": 2, "128": 2, "256": 2, "512": 2}})
        self.assertEqual(d["usage"]["dpc"], {"yaic.sys": {"2": 45000}, "dxgkrnl.sys": {"14": 3000}})

    def test_budget_names_modules_over_the_limits(self) -> None:
        d = lr.parse_dpcisr(DPCISR)
        findings = lr.budget_findings(d, watch_lps=[2, 14])
        self.assertIn("dpc dxgkrnl.sys: up to 1024 us on a watched CPU (budget 128)", findings)
        self.assertIn("dpc dxgkrnl.sys: up to 1024 us (a full period is 333)", findings)
        self.assertIn("isr ndis.sys: above 2048 us (a full period is 333)", findings)
        self.assertFalse([f for f in findings if "yaic.sys" in f])

    def test_empty_text(self) -> None:
        self.assertEqual(lr.parse_dpcisr(""), {"dpc": {}, "isr": {}, "usage": {"dpc": {}, "isr": {}}})


def cpu(lp, t, ints, dpcs, dpc_t=0, int_t=0, idle=0, c1=0, c2=0, c3=0):
    return {"lp": lp, "t100ns": t, "interrupts": ints, "dpcs": dpcs, "dpc_time": dpc_t, "int_time": int_t,
            "idle_time": idle, "c1_time": c1, "c2_time": c2, "c3_time": c3}


class CpuRateTests(unittest.TestCase):
    def test_rates_between_samples(self) -> None:
        s = [{"cpus": [cpu(2, 0, 0, 0), cpu(3, 0, 5, 5)]},
             {"cpus": [cpu(2, 100_000_000, 30_000, 30_000, dpc_t=1_000_000, idle=90_000_000, c1=50_000_000), cpu(3, 100_000_000, 5, 5, idle=100_000_000)]},
             {"cpus": [cpu(2, 200_000_000, 90_000, 60_000, dpc_t=3_000_000, idle=180_000_000, c1=50_000_000)]}]
        r = lr.cpu_rates(s)
        self.assertEqual(r["2"]["int_s_mean"], 4500.0)
        self.assertEqual(r["2"]["int_s_max"], 6000.0)
        self.assertEqual(r["2"]["dpc_s_mean"], 3000.0)
        self.assertEqual(r["2"]["dpc_pct_max"], 2.0)
        self.assertEqual(r["2"]["busy_pct_mean"], 10.0)
        self.assertEqual(r["3"]["int_s_mean"], 0.0)
        self.assertEqual(r["3"]["busy_pct_mean"], 0.0)

    def test_fewer_than_two_samples_give_nothing(self) -> None:
        self.assertEqual(lr.cpu_rates([]), {})
        self.assertEqual(lr.cpu_rates([{"cpus": [cpu(0, 1, 1, 1)]}]), {})


REPORT = {"outcome": "done", "segments": [
    {"telemetry": {"callback_cpus": {"14": 900, "15": 100}, "callback_thread": 4242, "thread_switches": 0, "glitches_dropped": 1},
     "glitches": [{"kind": "late", "at_ns": 5, "value": 600000}, {"kind": "missed", "at_ns": 9, "value": 700000}], "glitches_unreported": 2},
    {"telemetry": {"callback_cpus": {"14": 50}, "callback_thread": 4243, "thread_switches": 1, "glitches_dropped": 0},
     "glitches": [{"kind": "missed", "at_ns": 11, "value": 800000}], "glitches_unreported": 0}]}


class ReportTests(unittest.TestCase):
    def test_glitch_counts_include_the_uncounted(self) -> None:
        g = lr.glitch_counts(REPORT)
        self.assertEqual(g["by_kind"], {"late": 1, "missed": 2})
        self.assertEqual((g["unreported"], g["dropped"]), (2, 1))
        self.assertEqual(g["first"][0], {"kind": "late", "at_ns": 5, "value": 600000})

    def test_callback_view_merges_segments_and_polled_priorities(self) -> None:
        polls = [{"thread": {"base": 15, "current": 15}}, {"thread": None}, {"thread": {"base": 15, "current": 26}}]
        v = lr.callback_view(REPORT, polls)
        self.assertEqual(v["cpus"], {"14": 950, "15": 100})
        self.assertEqual(v["threads"], [4242, 4243])
        self.assertEqual(v["thread_switches"], 1)
        self.assertEqual(v["priority"], {"base_min": 15, "base_max": 15, "current_min": 15, "current_max": 26})

    def test_sentinel_changes_list_each_new_value_once(self) -> None:
        polls = [{"at": "t1", "plan": "a", "governor": "Stopped"}, {"at": "t2", "plan": "a", "governor": "Stopped"},
                 {"at": "t3", "plan": "b", "governor": "Running"}]
        self.assertEqual(lr.sentinel_changes(polls), [{"at": "t1", "plan": "a", "governor": "Stopped"}, {"at": "t3", "plan": "b", "governor": "Running"}])

    def test_hwlat_summary(self) -> None:
        r = {"outcome": "done", "hwlat": {"cpu": 14, "reads": 10, "over": 3, "gaps_us": {"p50": 12.0, "p99": 40.0, "p999": 40.0, "max": 55.5},
                                          "largest": [{"at_us": 1.0, "gap_us": 55.5}, {"at_us": 2.0, "gap_us": 40.0}]}}
        self.assertEqual(lr.hwlat_summary(r), {"cpu": 14, "outcome": "done", "reads": 10, "over": 3, "max_us": 55.5, "p999_us": 40.0, "largest_us": [55.5, 40.0]})


DUMPER = """BeginHeader
                    DPC,  TimeStamp,    CPU, ElapsedTime,  Routine
              Interrupt,  TimeStamp,    CPU, ElapsedTime,  Routine
                CSwitch,  TimeStamp, New Process Name ( PID),  New TID, NPri, CPU
EndHeader
                    DPC,       1000,      2,         12,  yaic.sys!0x10
                    DPC,       1400,     14,        300,  dxgkrnl.sys!0x20
              Interrupt,       1500,      2,          3,  yaic.sys!0x30
                CSwitch,       1600, asio_spike.exe (100),     4242,   26,  14
   UnknownEvent/Classic,       1650, iemmixer-glitch kind=missed at_qpc=1000 emit_qpc=1200 freq=10000000 value=700000
                    DPC,       5000,      2,         12,  yaic.sys!0x10
"""


class NearGlitchTests(unittest.TestCase):
    def test_header_driven_rows(self) -> None:
        fields, rows = lr.parse_dumper(DUMPER)
        self.assertEqual(fields["DPC"][3], "ElapsedTime")
        self.assertEqual(len(rows), 6)

    def test_activity_in_the_periods_before_each_glitch(self) -> None:
        near = lr.near_glitch(DUMPER, period_us=333)
        self.assertEqual(len(near), 1)
        g = near[0]
        # emit 1200, glitch 1000 ticks at 10 MHz: the glitch lies 20 us before the marker (1650 - 20 = 1630).
        self.assertEqual((g["kind"], g["at_us"], g["exact"]), ("missed", 1630.0, True))
        # The window is the 2 periods (666 us) before 1630: 964..1630.
        self.assertEqual([(e["event"], e["cpu"], e["us"]) for e in g["events"]],
                         [("DPC", "2", 12.0), ("DPC", "14", 300.0), ("Interrupt", "2", 3.0), ("CSwitch", "14", None)])
        self.assertEqual(g["events"][3]["what"], "asio_spike.exe (100)")

    def test_a_marker_without_its_text_widens_the_window(self) -> None:
        text = DUMPER.replace("iemmixer-glitch kind=missed at_qpc=1000 emit_qpc=1200 freq=10000000 value=700000", "3b6c1e0a-5d2f-4c8e-9a71-0e4f2d9b8c11")
        near = lr.near_glitch(text, period_us=333)
        self.assertEqual((near[0]["kind"], near[0]["exact"]), ("unknown", False))
        self.assertEqual(len(near[0]["events"]), 4)


class SummaryTests(unittest.TestCase):
    def test_summary_without_a_trace(self) -> None:
        s = lr.summarize("idle-32", {"stable": True}, REPORT, None, [], [], watch_lps=[2, 14])
        self.assertEqual((s["label"], s["dpcisr"], s["findings"]), ("idle-32", None, ["no trace"]))
        self.assertEqual(s["glitches"]["by_kind"]["missed"], 2)

    def test_summary_with_a_trace_lists_the_worst_modules_first(self) -> None:
        s = lr.summarize("load-32", {"stable": False}, REPORT, DPCISR, [], [{"provider": "WHEA-Logger", "id": 19, "count": 1}], watch_lps=[2, 14])
        self.assertEqual(s["dpcisr"]["dpc"][0]["module"], "dxgkrnl.sys")
        self.assertEqual(s["system_events"], [{"provider": "WHEA-Logger", "id": 19, "count": 1}])


if __name__ == "__main__":
    unittest.main()
