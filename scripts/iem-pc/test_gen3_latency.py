"""Tests for scripts/iem-pc/gen3_latency.py (#122): every mic input's
mic-to-ear path from a status, our subscriptions and the site's mic inputs,
against the budget of 96 samples. Every value is synthetic (P6): invented
device names, channel numbers within the test site's."""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import gen3_latency as lat  # noqa: E402

STAGE = "zyxqw-stage"     # a stage rack: unicast, fpp 4, delay 16
WIRELESS = "zyxqw-radio"  # a wireless receiver: its multicast flow, fpp 32, delay 48
D = 56


def status(rx=None, tx=None, d: int | None = D) -> dict:
    """A `ctl status` of the box's shape today: D 56, the stage rack's flow at
    delay 16, the wireless device's at 48, in-ear TX flows at 250 us and one
    TX flow to a receiver at 1 ms (not in-ear)."""
    doc = {
        "rx": rx if rx is not None else [
            {"flow": 1, "granted": 0, "fpp": 0, "channels": 0, "delay": 0},   # an ended flow: no device
            {"flow": 4, "device": STAGE, "granted": 1, "multicast": 0, "fpp": 4, "channels": 8, "delay": 16},
            {"flow": 24, "device": WIRELESS, "granted": 0, "multicast": 1, "fpp": 32, "channels": 2, "delay": 48},
        ],
        "tx": tx if tx is not None else [
            {"flow": 1, "live": 1, "fpp": 4, "channels": 4, "receiver_latency_us": 250},
            {"flow": 3, "live": 0, "fpp": 0, "channels": 0, "receiver_latency_us": 0},
            {"flow": 5, "live": 1, "fpp": 4, "channels": 8, "receiver_latency_us": 1000},
        ],
    }
    if d is not None:
        doc["engine"] = {"media": 1, "offset": d, "playout": 24}
    return doc


def sub(device: str, code: int = 9) -> dict:
    return {"tx_channel": "zyxqw 01", "tx_device": device, "status": code}


SUBS = {"1": sub(STAGE), "2": sub(WIRELESS, 10), "3": sub(STAGE)}
MICS = [("mic1", [1]), ("mic2", [2]), ("mic3", [3])]


def row(rows: list[dict], ident: str) -> dict:
    return next(r for r in rows if r["input"] == ident)


class PathTests(unittest.TestCase):
    def test_the_box_today_the_stage_rack_at_the_budget_passes_the_wireless_path_fails(self) -> None:
        rows = lat.paths(status(), SUBS, MICS)
        self.assertEqual(row(rows, "mic1"), {"input": "mic1", "rx": [1], "device": STAGE, "samples": 96, "ms": 1.0,
                                             "over_budget": False, "state": "ok"})
        self.assertEqual(row(rows, "mic2"), {"input": "mic2", "rx": [2], "device": WIRELESS, "samples": 128,
                                             "ms": 1.33, "over_budget": True, "state": "over"})
        self.assertEqual(lat.verdict(rows)["pass"], 0)

    def test_under_at_and_above_the_budget(self) -> None:
        for delay, samples, state in ((8, 88, "ok"), (16, 96, "ok"), (17, 97, "over")):
            with self.subTest(delay=delay):
                rx = [{"flow": 4, "device": STAGE, "channels": 8, "delay": delay}]
                rows = lat.paths(status(rx=rx), {"1": sub(STAGE)}, [("mic1", [1])])
                self.assertEqual((rows[0]["samples"], rows[0]["state"], rows[0]["over_budget"]),
                                 (samples, state, state == "over"))
                self.assertEqual(lat.verdict(rows)["pass"], int(state == "ok"))

    def test_the_budget_is_1_ms_at_96_khz(self) -> None:
        self.assertEqual((lat.BUDGET_SAMPLES, lat.RATE), (96, 96_000))

    def test_an_input_whose_device_has_no_flow_is_unknown_never_a_pass(self) -> None:
        rows = lat.paths(status(), {"1": sub("zyxqw-gone")}, [("mic1", [1])])
        self.assertEqual((rows[0]["samples"], rows[0]["ms"], rows[0]["state"]), (None, None, "unknown"))
        self.assertEqual(lat.verdict(rows)["pass"], 0)

    def test_an_unresolved_subscription_is_unknown(self) -> None:
        rows = lat.paths(status(), {"1": sub(STAGE, 1)}, [("mic1", [1])])
        self.assertEqual(rows[0]["state"], "unknown")
        self.assertEqual(lat.verdict(rows)["pass"], 0)
        # The low 16 bits are the code: 0x01010009 is Receiving.
        rows = lat.paths(status(), {"1": sub(STAGE, 0x01010009)}, [("mic1", [1])])
        self.assertEqual(rows[0]["state"], "ok")

    def test_without_the_engine_or_an_in_ear_flow_every_path_is_unknown(self) -> None:
        for doc in (status(d=None), status(tx=[{"flow": 5, "channels": 8, "receiver_latency_us": 1000}])):
            rows = lat.paths(doc, SUBS, MICS)
            self.assertEqual({r["state"] for r in rows}, {"unknown"})
            self.assertEqual(lat.verdict(rows)["pass"], 0)

    def test_an_input_with_no_subscription_has_no_path_and_fails_nothing(self) -> None:
        rows = lat.paths(status(), {"1": sub(STAGE)}, [("mic1", [1]), ("mic4", [4])])
        self.assertEqual(row(rows, "mic4")["state"], "unsubscribed")
        self.assertEqual(lat.verdict(rows)["pass"], 1)
        # Nothing known at all is no pass.
        self.assertEqual(lat.verdict(lat.paths(status(), {}, [("mic4", [4])]))["pass"], 0)

    def test_several_flows_from_one_device_take_the_worst_delay(self) -> None:
        rx = [{"flow": 4, "device": STAGE, "channels": 4, "delay": 16},
              {"flow": 7, "device": STAGE, "channels": 4, "delay": 40},
              {"flow": 8, "device": "zyxqw-other", "channels": 4, "delay": 400}]
        rows = lat.paths(status(rx=rx), {"1": sub(STAGE)}, [("mic1", [1])])
        self.assertEqual(rows[0]["samples"], D + 40 + 24)

    def test_the_tx_flow_to_a_receiver_at_1_ms_is_not_in_ear(self) -> None:
        self.assertEqual(lat.in_ear_samples(status()), 24)
        # The worst in-ear flow counts; a flow without channels does not.
        tx = [{"flow": 1, "channels": 4, "receiver_latency_us": 250},
              {"flow": 2, "channels": 4, "receiver_latency_us": 500},
              {"flow": 3, "channels": 0, "receiver_latency_us": 400},
              {"flow": 5, "channels": 8, "receiver_latency_us": 1000}]
        self.assertEqual(lat.in_ear_samples(status(tx=tx)), 48)
        self.assertIsNone(lat.in_ear_samples(status(tx=[{"flow": 5, "channels": 8, "receiver_latency_us": 501}])))

    def test_an_input_on_two_channels_takes_its_worst_and_any_unknown(self) -> None:
        rows = lat.paths(status(), SUBS, [("mic1", [1, 2])])
        self.assertEqual((rows[0]["samples"], rows[0]["device"], rows[0]["state"]), (128, WIRELESS, "over"))
        rows = lat.paths(status(), {**SUBS, "4": sub("zyxqw-gone")}, [("mic1", [1, 4])])
        self.assertEqual(rows[0]["state"], "unknown")


class SiteTests(unittest.TestCase):
    def test_the_mic_inputs_are_the_engine_inputs_whose_id_starts_with_mic(self) -> None:
        text = (HERE.parent.parent / "config" / "test-site.toml").read_text(encoding="utf-8")
        inputs = lat.mic_inputs(text)
        self.assertEqual(inputs, [(f"mic{n}", [n]) for n in range(1, 11)])

    def test_a_site_without_engine_inputs_or_with_a_bad_rx_is_refused(self) -> None:
        for text in ("", "[engine]\n", '[[engine.inputs]]\nid = "mic1"\nrx = "x"\n',
                     '[[engine.inputs]]\nid = "mic1"\nrx = [0]\n'):
            with self.subTest(text=text), self.assertRaises(ValueError):
                lat.mic_inputs(text)


class VerdictTests(unittest.TestCase):
    def test_the_printed_paths_carry_input_ids_and_numbers_only(self) -> None:
        doc = lat.verdict(lat.paths(status(), SUBS, MICS))
        self.assertEqual(doc["budget_samples"], 96)
        self.assertEqual(doc["paths"][1], {"input": "mic2", "samples": 128, "ms": 1.33, "over_budget": True,
                                           "state": "over"})
        text = repr(doc)
        for value in (STAGE, WIRELESS, "'rx'", "device"):
            self.assertNotIn(value, text)


if __name__ == "__main__":
    unittest.main()
