"""Tests for scripts/pc-tuning/latency_report.py (pure functions over the
xperf texts, the spike's report and the PC samples)."""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import latency_report as lr  # noqa: E402

def usage_table(rows: dict[str, dict[int, int]], cpus: int = 16) -> str:
    """xperf -a dpcisr's whole-trace usage table (the layout of DPCISR_XPERF
    below) for a PC with `cpus` logical processors, then its blank line."""
    head = ", ".join(f"     CPU {c} Usage" for c in range(cpus)) + ","
    units = ", ".join("     usec      %" for _ in range(cpus)) + ", Module"
    body = [", ".join(f"{row.get(c, 0):>9}{0:>7.2f}" for c in range(cpus)) + f", {module}" for module, row in rows.items()]
    return "\n".join([head, units, *body]) + "\n\n"


DPCISR = """
--------------------------
DPC Info
--------------------------
""" + usage_table({"yaic.sys": {2: 45000}, "dxgkrnl.sys": {14: 3000}}) + """Total = 3000 for module yaic.sys
Elapsed Time, >        0 usecs AND <=        1 usecs,      0, or   0.00%
Elapsed Time, >        8 usecs AND <=       16 usecs,   2990, or  99.67%
Elapsed Time, >       64 usecs AND <=      128 usecs,     10, or   0.33%
Total = 20 for module dxgkrnl.sys
Elapsed Time, >      128 usecs AND <=      256 usecs,     15, or  75.00%
Elapsed Time, >      512 usecs AND <=     1024 usecs,      5, or  25.00%
--------------------------
Interrupt Info
--------------------------
""" + usage_table({"yaic.sys": {2: 9000}, "ndis.sys": {0: 40}}) + """Total = 3000 for module yaic.sys
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
        ran = {m: {c: us for c, us in row.items() if us} for m, row in d["usage"]["dpc"].items()}
        self.assertEqual(ran, {"yaic.sys": {"2": 45000}, "dxgkrnl.sys": {"14": 3000}})
        self.assertEqual(len(d["usage"]["dpc"]["yaic.sys"]), 16)   # one column per CPU

    def test_budget_names_modules_over_the_limits(self) -> None:
        d = lr.parse_dpcisr(DPCISR)
        findings = lr.budget_findings(d, watch_lps=[2, 14])
        self.assertIn("dpc dxgkrnl.sys: up to 1024 us on a watched CPU (budget 128)", findings)
        self.assertIn("dpc dxgkrnl.sys: up to 1024 us (a full period is 333)", findings)
        self.assertIn("isr ndis.sys: above 2048 us (a full period is 333)", findings)
        self.assertFalse([f for f in findings if "yaic.sys" in f])

    def test_empty_text(self) -> None:
        self.assertEqual(lr.parse_dpcisr(""), {"dpc": {}, "isr": {}, "usage": {"dpc": {}, "isr": {}}})


# xperf -a dpcisr's real layout, written by hand with synthetic modules and
# numbers (#32 B8). Per-CPU usage is a comma-separated TABLE, not a line per
# module: per section the whole-trace table (header `CPU n Usage`, one
# `usec %` column per CPU, the module last), the histograms, the 1-second
# interval table (a label column, spaces before the commas) and, at the end,
# the distribution table (bare `CPU n` columns, three values each). The PC
# writes CRLF.
DPCISR_XPERF = """
--------------------------
DPC Info

--------------------------
CPU Usage Summing By Module For the Whole Trace

CPU Usage from 0 us to 90000000 us:

     CPU 0 Usage,      CPU 1 Usage,      CPU 2 Usage,      CPU 3 Usage,
     usec      %,      usec      %,      usec      %,      usec      %, Module
      120   0.00,         0   0.00,     45000   0.05,         0   0.00, carddrv.sys
        0   0.00,         0   0.00,         0   0.00,      2500   0.00, gpudrv.sys
     1500   0.00,         0   0.00,       800   0.00,         0   0.00, nicdrv.sys
        1   0.00,         0   0.00,         0   0.00,         0   0.00, "Unknown"

Total = 3261
Elapsed Time, >        4 usecs AND <=        8 usecs,    238, or   7.30%
Elapsed Time, >        8 usecs AND <=       16 usecs,   2990, or  91.69%
Elapsed Time, >       32 usecs AND <=       64 usecs,      1, or   0.03%
Elapsed Time, >       64 usecs AND <=      128 usecs,     25, or   0.77%
Elapsed Time, >      128 usecs AND <=      256 usecs,      7, or   0.21%
Total,                                                  3261

Total = 3000 for module carddrv.sys
Elapsed Time, >        8 usecs AND <=       16 usecs,   2990, or  99.67%
Elapsed Time, >       64 usecs AND <=      128 usecs,     10, or   0.33%
Total,                                                  3000

Total = 20 for module gpudrv.sys
Elapsed Time, >       64 usecs AND <=      128 usecs,     15, or  75.00%
Elapsed Time, >      128 usecs AND <=      256 usecs,      5, or  25.00%
Total,                                                    20

Total = 240 for module nicdrv.sys
Elapsed Time, >        4 usecs AND <=        8 usecs,    238, or  99.17%
Elapsed Time, >      128 usecs AND <=      256 usecs,      2, or   0.83%
Total,                                                   240

Total = 1 for module "Unknown"
Elapsed Time, >       32 usecs AND <=       64 usecs,      1, or 100.00%
Total,                                                     1

All Module = 3261,  Total = 3261,   EQUAL

--------------------------
Usage From 0 ms to 90000 ms, Summing In 1 second intervals. Intervals=90

                       ,      CPU 0 Usage ,      CPU 1 Usage ,      CPU 2 Usage ,      CPU 3 Usage
Start (ms) End (ms)    ,    (usec)      % ,    (usec)      % ,    (usec)      % ,    (usec)      %
         0-1000      :,        18   0.00,         0   0.00,       510   0.05,        28   0.00
      1000-2000      :,        17   0.00,         0   0.00,       498   0.05,        27   0.00

--------------------------
Interrupt Info

--------------------------
CPU Usage Summing By Module For the Whole Trace

CPU Usage from 0 us to 90000000 us:

     CPU 0 Usage,      CPU 1 Usage,      CPU 2 Usage,      CPU 3 Usage,
     usec      %,      usec      %,      usec      %,      usec      %, Module
        0   0.00,         0   0.00,      9000   0.01,         0   0.00, carddrv.sys
     4100   0.00,         0   0.00,         0   0.00,         0   0.00, nicdrv.sys

Total = 3002
Elapsed Time, >        2 usecs AND <=        4 usecs,   3000, or  99.93%
Elapsed Time, >     2048 usecs,      2, or   0.07%
Total,                                                  3002

Total = 3000 for module carddrv.sys
Elapsed Time, >        2 usecs AND <=        4 usecs,   3000, or 100.00%
Total,                                                  3000

Total = 2 for module nicdrv.sys
Elapsed Time, >     2048 usecs,      2, or 100.00%
Total,                                                     2

All Module = 3002,  Total = 3002,   EQUAL

--------------------------
Usage From 0 ms to 90000 ms, Summing In 1 second intervals. Intervals=90

                       ,      CPU 0 Usage ,      CPU 1 Usage ,      CPU 2 Usage ,      CPU 3 Usage
Start (ms) End (ms)    ,    (usec)      % ,    (usec)      % ,    (usec)      % ,    (usec)      %
         0-1000      :,        46   0.00,         0   0.00,       100   0.01,         0   0.00


Distribution of number of 2000 ms intervals w.r.t. DPC/ISR usage:

                ,                      CPU 0,                      CPU 1,                      CPU 2,                      CPU 3
 DPC/ISR Usage %,      DPC      ISR Combined,      DPC      ISR Combined,      DPC      ISR Combined,      DPC      ISR Combined
>=  0 AND <=   1,       45,       45,       45,       45,       45,       45,       44,       45,       44,       45,       45,       45
>   1 AND <=   5,        0,        0,        0,        0,        0,        0,        1,        0,        1,        0,        0,        0
---
Total:          ,       45,       45,       45,       45,       45,       45,       45,       45,       45,       45,       45,       45
"""


class XperfLayoutTests(unittest.TestCase):
    """parse_dpcisr on xperf's real per-CPU usage tables (#32 B8)."""

    def test_the_usage_table_maps_columns_to_cpus_and_rows_to_modules(self) -> None:
        d = lr.parse_dpcisr(DPCISR_XPERF)
        self.assertEqual(sorted(d["dpc"]), ['"Unknown"', "carddrv.sys", "gpudrv.sys", "nicdrv.sys"])
        self.assertEqual(d["usage"]["dpc"]["nicdrv.sys"], {"0": 1500, "1": 0, "2": 800, "3": 0})
        self.assertEqual(d["usage"]["dpc"]['"Unknown"'], {"0": 1, "1": 0, "2": 0, "3": 0})
        self.assertEqual(d["usage"]["isr"], {"carddrv.sys": {"0": 0, "1": 0, "2": 9000, "3": 0},
                                             "nicdrv.sys": {"0": 4100, "1": 0, "2": 0, "3": 0}})
        self.assertEqual(d["dpc"]["gpudrv.sys"], {"count": 20, "max_us": 256, "open": False, "over": {"64": 20, "128": 5, "256": 0, "512": 0}})
        self.assertEqual(d["isr"]["nicdrv.sys"], {"count": 2, "max_us": 2048, "open": True, "over": {"64": 2, "128": 2, "256": 2, "512": 2}})
        # The interval and distribution tables carry no module: nothing else is read as one.
        self.assertEqual(sorted(d["usage"]["dpc"]), sorted(d["dpc"]))
        self.assertEqual(lr.parse_dpcisr(DPCISR_XPERF.replace("\n", "\r\n")), d)

    def test_a_module_over_the_budget_on_a_watched_cpu_is_named(self) -> None:
        findings = lr.budget_findings(lr.parse_dpcisr(DPCISR_XPERF), watch_lps=[2])
        self.assertIn("dpc nicdrv.sys: up to 256 us on a watched CPU (budget 128)", findings)
        self.assertIn("isr nicdrv.sys: above 2048 us (a full period is 333)", findings)

    def test_a_module_over_the_budget_only_on_unwatched_cpus_is_no_watched_finding(self) -> None:
        # The watched-CPU condition's negative case (#32 B9): dropping it, or counting
        # a 0 us cell as "ran there", must fail a test.
        d = lr.parse_dpcisr(DPCISR_XPERF)
        self.assertFalse([f for f in lr.budget_findings(d, watch_lps=[2]) if f.startswith("dpc gpudrv.sys")])   # 256 us, only on CPU 3
        self.assertEqual(lr.budget_findings(d, watch_lps=[3]), ["dpc gpudrv.sys: up to 256 us on a watched CPU (budget 128)",
                                                                "isr nicdrv.sys: above 2048 us (a full period is 333)"])

    def test_modules_without_readable_per_cpu_usage_fail_loud(self) -> None:
        # Fail closed: a budget check that cannot see where a module ran must not
        # come back with no findings (a real 90 s trace did exactly that).
        start = DPCISR_XPERF.index("     CPU 0 Usage,")
        end = DPCISR_XPERF.index("\nTotal = 3261")
        with self.assertRaisesRegex(ValueError, "per-CPU usage"):
            lr.parse_dpcisr(DPCISR_XPERF[:start] + DPCISR_XPERF[end:])
        made_up = "DPC Info\nTotal = 3 for module x.sys\nElapsed Time, >  8 usecs AND <=  16 usecs,  3, or 100.00%\nx.sys: 45 usec (0.10% CPU 2 usage)\n"
        with self.assertRaisesRegex(ValueError, "per-CPU usage"):
            lr.parse_dpcisr(made_up)
        short = DPCISR_XPERF.replace('        1   0.00,         0   0.00,         0   0.00,         0   0.00, "Unknown"',
                                     '        1   0.00,         0   0.00,         0   0.00, "Unknown"')
        with self.assertRaisesRegex(ValueError, "columns"):
            lr.parse_dpcisr(short)


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
