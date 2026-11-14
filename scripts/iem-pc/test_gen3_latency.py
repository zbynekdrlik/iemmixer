"""Tests for scripts/iem-pc/gen3_latency.py (#122): every engine input's
mic-to-ear path from a status, our subscriptions and the site's inputs,
against 1 ms at the device's rate (96 samples at 96 kHz). Every value is
synthetic (P6): invented device names, channel numbers within the test
site's."""
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
RATE = 96_000


def status(rx=None, tx=None, d: int | None = D) -> dict:
    """A `ctl status` of the box's shape today: D 56, the stage rack's flow at
    delay 16, the wireless device's at 48, in-ear TX flows at 250 us and one
    TX flow to a receiver at 1 ms."""
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
NON_INEAR = frozenset({1000})


def paths(doc: dict | None = None, subs: dict = SUBS, inputs=MICS, rate: int = RATE, exempt=None) -> list[dict]:
    return lat.paths(status() if doc is None else doc, subs, inputs, rate, exempt)


def verdict(rows: list[dict], doc: dict | None = None, rate: int = RATE, non_inear=NON_INEAR) -> dict:
    return lat.verdict(rows, rate, lat.tx_flows(status() if doc is None else doc, non_inear))


def row(rows: list[dict], ident: str) -> dict:
    return next(r for r in rows if r["input"] == ident)


class PathTests(unittest.TestCase):
    def test_the_box_today_the_stage_rack_at_the_budget_passes_the_wireless_path_fails(self) -> None:
        rows = paths()
        self.assertEqual(row(rows, "mic1"), {"input": "mic1", "rx": [1], "device": STAGE, "samples": 96, "ms": 1.0,
                                             "over_budget": False, "state": "ok"})
        self.assertEqual(row(rows, "mic2"), {"input": "mic2", "rx": [2], "device": WIRELESS, "samples": 128,
                                             "ms": 1.33, "over_budget": True, "state": "over"})
        self.assertEqual(verdict(rows)["pass"], 0)
        self.assertEqual(verdict(paths(inputs=[("mic1", [1]), ("mic3", [3])]))["pass"], 1)

    def test_under_at_and_above_the_budget(self) -> None:
        for delay, samples, state in ((8, 88, "ok"), (16, 96, "ok"), (17, 97, "over")):
            with self.subTest(delay=delay):
                doc = status(rx=[{"flow": 4, "device": STAGE, "channels": 8, "delay": delay}])
                rows = paths(doc, {"1": sub(STAGE)}, [("mic1", [1])])
                self.assertEqual((rows[0]["samples"], rows[0]["state"], rows[0]["over_budget"]),
                                 (samples, state, state == "over"))
                self.assertEqual(verdict(rows, doc)["pass"], int(state == "ok"))

    def test_an_input_not_named_mic_over_the_budget_fails(self) -> None:
        rows = paths(inputs=[("mic1", [1]), ("hand1", [2])])
        self.assertEqual((row(rows, "hand1")["samples"], row(rows, "hand1")["state"]), (128, "over"))
        self.assertEqual(verdict(rows)["pass"], 0)

    def test_an_unsubscribed_input_not_exempt_fails(self) -> None:
        rows = paths(subs={"1": sub(STAGE)}, inputs=[("mic1", [1]), ("eng_mic", [4])])
        self.assertEqual(row(rows, "eng_mic")["state"], "unsubscribed")
        self.assertEqual(verdict(rows)["pass"], 0)
        # One channel of two without a subscription is unsubscribed too.
        rows = paths(subs={"1": sub(STAGE)}, inputs=[("keys", [1, 4])])
        self.assertEqual((rows[0]["state"], verdict(rows)["pass"]), ("unsubscribed", 0))
        # Nothing known at all is no pass, exempt or not.
        self.assertEqual(verdict(paths(subs={}, inputs=[("mic4", [4])], exempt={"mic4": "unused"}))["pass"], 0)

    def test_an_exempt_input_over_the_budget_passes_as_exempt(self) -> None:
        rows = paths(inputs=[("mic1", [1]), ("click", [2]), ("spare", [9])],
                     exempt={"click": "playback", "spare": "unused"})
        self.assertEqual(row(rows, "click"), {"input": "click", "rx": [2], "device": None, "samples": None,
                                              "ms": None, "over_budget": False, "state": "exempt"})
        self.assertEqual(row(rows, "spare")["state"], "exempt")
        self.assertEqual(verdict(rows)["pass"], 1)
        # Without the exemption the same input fails.
        self.assertEqual(verdict(paths(inputs=[("mic1", [1]), ("click", [2])]))["pass"], 0)

    def test_an_input_whose_device_has_no_flow_is_unknown_never_a_pass(self) -> None:
        rows = paths(subs={"1": sub("zyxqw-gone")}, inputs=[("mic1", [1])])
        self.assertEqual((rows[0]["samples"], rows[0]["ms"], rows[0]["state"]), (None, None, "unknown"))
        self.assertEqual(verdict(rows)["pass"], 0)

    def test_an_unresolved_subscription_is_unknown(self) -> None:
        rows = paths(subs={"1": sub(STAGE, 1)}, inputs=[("mic1", [1])])
        self.assertEqual(rows[0]["state"], "unknown")
        self.assertEqual(verdict(rows)["pass"], 0)
        # The low 16 bits are the code: 0x01010009 is Receiving.
        rows = paths(subs={"1": sub(STAGE, 0x01010009)}, inputs=[("mic1", [1])])
        self.assertEqual(rows[0]["state"], "ok")

    def test_without_the_engine_or_an_in_ear_flow_every_path_is_unknown(self) -> None:
        for doc in (status(d=None), status(tx=[{"flow": 5, "channels": 8, "receiver_latency_us": 1000}])):
            rows = paths(doc)
            self.assertEqual({r["state"] for r in rows}, {"unknown"})
            self.assertEqual(verdict(rows, doc)["pass"], 0)

    def test_several_flows_from_one_device_take_the_worst_delay(self) -> None:
        rx = [{"flow": 4, "device": STAGE, "channels": 4, "delay": 16},
              {"flow": 7, "device": STAGE, "channels": 4, "delay": 40},
              {"flow": 8, "device": "zyxqw-other", "channels": 4, "delay": 400}]
        rows = paths(status(rx=rx), {"1": sub(STAGE)}, [("mic1", [1])])
        self.assertEqual(rows[0]["samples"], D + 40 + 24)

    def test_an_input_on_two_channels_takes_its_worst_and_any_unknown(self) -> None:
        rows = paths(inputs=[("mic1", [1, 2])])
        self.assertEqual((rows[0]["samples"], rows[0]["device"], rows[0]["state"]), (128, WIRELESS, "over"))
        rows = paths(subs={**SUBS, "4": sub("zyxqw-gone")}, inputs=[("mic1", [1, 4])])
        self.assertEqual(rows[0]["state"], "unknown")


class TxTests(unittest.TestCase):
    def test_the_in_ear_latency_is_the_worst_flow_at_the_cut_or_below(self) -> None:
        self.assertEqual(lat.in_ear_samples(status(), RATE), 24)
        tx = [{"flow": 1, "channels": 4, "receiver_latency_us": 250},
              {"flow": 2, "channels": 4, "receiver_latency_us": 500},
              {"flow": 3, "channels": 0, "receiver_latency_us": 400},
              {"flow": 5, "channels": 8, "receiver_latency_us": 1000}]
        self.assertEqual(lat.in_ear_samples(status(tx=tx), RATE), 48)
        self.assertEqual(lat.in_ear_samples(status(tx=tx), 48_000), 24)
        self.assertIsNone(lat.in_ear_samples(status(tx=[{"flow": 5, "channels": 8, "receiver_latency_us": 501}]),
                                             RATE))

    def test_a_tx_flow_at_1_ms_not_listed_fails_listed_passes(self) -> None:
        rows = paths(inputs=[("mic1", [1])])
        self.assertEqual(lat.tx_flows(status()), [{"receiver_latency_us": 250, "state": "in_ear"},
                                                  {"receiver_latency_us": 1000, "state": "tx_unclassified"}])
        doc = verdict(rows, non_inear=frozenset())
        self.assertEqual((doc["pass"], doc["tx"][1]["state"]), (0, "tx_unclassified"))
        doc = verdict(rows, non_inear=frozenset({1000}))
        self.assertEqual((doc["pass"], doc["tx"][1]["state"]), (1, "non_inear"))
        # Another value listed classifies nothing.
        self.assertEqual(verdict(rows, non_inear=frozenset({999}))["pass"], 0)

    def test_the_env_lists_parse_and_refuse(self) -> None:
        self.assertEqual(lat.parse_non_inear(" 1000, 2000 "), frozenset({1000, 2000}))
        self.assertEqual(lat.parse_non_inear(""), frozenset())
        self.assertEqual(lat.parse_exempt("click:band pc playback, spare:no source"),
                         {"click": "band pc playback", "spare": "no source"})
        self.assertEqual(lat.parse_exempt(""), {})
        for text in ("1000us", "1.5", "-1"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                lat.parse_non_inear(text)
        for text in ("click", "click:", ":playback", "click:a,click:b"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                lat.parse_exempt(text)


class RateTests(unittest.TestCase):
    def test_the_budget_is_1_ms_at_the_device_s_rate(self) -> None:
        self.assertEqual(lat.sample_rate('name = "x"\nsample_rate = 48000\n'), 48_000)
        self.assertEqual(lat.budget_samples(48_000), 48)
        self.assertEqual(lat.budget_samples(96_000), 96)
        self.assertEqual(lat.budget_samples(44_100), 45)   # rounded up

    def test_at_48_khz_a_path_of_48_samples_passes_and_49_fails(self) -> None:
        for delay, state in ((8, "ok"), (9, "over")):
            with self.subTest(delay=delay):
                doc = status(rx=[{"flow": 4, "device": STAGE, "channels": 8, "delay": delay}], d=28)
                rows = paths(doc, {"1": sub(STAGE)}, [("mic1", [1])], rate=48_000)
                # 28 + delay + 250 us at 48 kHz (12).
                self.assertEqual((rows[0]["samples"], rows[0]["state"]), (28 + delay + 12, state))
                doc = verdict(rows, doc, rate=48_000)
                self.assertEqual((doc["budget_samples"], doc["rate"], doc["pass"]), (48, 48_000, int(state == "ok")))

    def test_a_device_config_without_a_rate_is_refused(self) -> None:
        for text in ('name = "x"\n', 'sample_rate = "96000"\n', "sample_rate = 0\n"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                lat.sample_rate(text)


class SiteTests(unittest.TestCase):
    def test_every_engine_input_of_the_site_is_read(self) -> None:
        text = (HERE.parent.parent / "config" / "test-site.toml").read_text(encoding="utf-8")
        inputs = lat.engine_inputs(text)
        self.assertEqual(inputs[:11], [(f"mic{n}", [n]) for n in range(1, 11)] + [("hand1", [11])])
        self.assertEqual(len(inputs), 24)
        self.assertEqual(inputs[-1], ("bgvs", [31, 32]))

    def test_a_site_without_engine_inputs_or_with_a_bad_entry_is_refused(self) -> None:
        for text in ("", "[engine]\n", '[[engine.inputs]]\nid = "mic1"\nrx = "x"\n',
                     '[[engine.inputs]]\nid = "keys"\nrx = [0]\n', '[[engine.inputs]]\nrx = [1]\n'):
            with self.subTest(text=text), self.assertRaises(ValueError):
                lat.engine_inputs(text)


class VerdictTests(unittest.TestCase):
    def test_the_printed_paths_carry_input_ids_and_numbers_only(self) -> None:
        doc = verdict(paths(exempt={"mic3": "zyxqwreason"}))
        self.assertEqual((doc["budget_samples"], doc["rate"]), (96, 96_000))
        self.assertEqual(doc["paths"][1], {"input": "mic2", "samples": 128, "ms": 1.33, "over_budget": True,
                                           "state": "over"})
        text = repr(doc)
        for value in (STAGE, WIRELESS, "'rx'", "device", "zyxqwreason"):
            self.assertNotIn(value, text)


if __name__ == "__main__":
    unittest.main()
