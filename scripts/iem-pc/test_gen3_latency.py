"""Tests for scripts/iem-pc/gen3_latency.py (#122): every engine input's
mic-to-ear path from a status, our subscriptions and the site's inputs and
in-ear outputs, against 1 ms at the device's rate (96 samples at 96 kHz).
Every value is synthetic (P6): invented device names, channel numbers within
the test site's."""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import gen3_latency as lat  # noqa: E402

STAGE = "zyxqw-stage"     # a stage rack: unicast, fpp 4, delay 16, feeds our RX 1 and 3
WIRELESS = "zyxqw-radio"  # a wireless receiver: its multicast flow, fpp 32, delay 48, feeds our RX 2
IEM = "zyxqw-iemrack"     # the in-ear receivers' device
FOH = "zyxqw-fohdesk"     # the engineer's headphones' device
D = 56
RATE = 96_000
# The test site's mixes: member1 TX 1/2, member2 TX 3/4, the engineer TX 21/22, the translator TX 23.
SITE = """
[engine]
channels = 64
engineer = "engineer"
inputs = [{ id = "mic1", rx = [1] }]
mixes = [
  { id = "member1", tx = [1, 2] },
  { id = "member2", tx = [3, 4] },
  { id = "engineer", tx = [21, 22] },
  { id = "translator", tx = [23] },
]
"""
IN_EAR = lat.in_ear_tx(SITE)


def status(rx=None, tx=None, d: int | None = D) -> dict:
    """A `ctl status` of the box's shape (dstack v0.6.137): D 56, the stage
    rack's flow at delay 16, the wireless device's at 48, the members' in-ear
    TX flow at 250 us and the engineer's TX flow at 1 ms."""
    doc = {
        "rx": rx if rx is not None else [
            {"flow": 1, "granted": 0, "fpp": 0, "channels": 0, "rx_channels": [], "delay": 0},   # an ended flow
            {"flow": 4, "device": STAGE, "granted": 1, "multicast": 0, "fpp": 4, "channels": 8,
             "rx_channels": [1, 3], "delay": 16},
            {"flow": 24, "device": WIRELESS, "granted": 0, "multicast": 1, "fpp": 32, "channels": 2,
             "rx_channels": [2], "delay": 48},
        ],
        "tx": tx if tx is not None else [
            {"flow": 1, "receiver": IEM, "live": 1, "fpp": 4, "channels": 4, "tx_channels": [1, 2, 3, None],
             "receiver_latency_us": 250},
            {"flow": 3, "live": 0, "fpp": 0, "channels": 0, "tx_channels": None, "receiver_latency_us": 0},
            {"flow": 5, "receiver": FOH, "live": 1, "fpp": 4, "channels": 8,
             "tx_channels": [21, 22, None, None, None, None, None, None], "receiver_latency_us": 1000},
        ],
    }
    if d is not None:
        doc["engine"] = {"media": 1, "offset": d, "playout": 24}
    return doc


def sub(device: str) -> dict:
    return {"tx_channel": "zyxqw 01", "tx_device": device, "status": 9}


SUBS = {"1": sub(STAGE), "2": sub(WIRELESS), "3": sub(STAGE)}
MICS = [("mic1", [1]), ("mic2", [2]), ("mic3", [3])]


def paths(doc: dict | None = None, subs: dict = SUBS, inputs=MICS, rate: int = RATE, exempt=None,
          in_ear=IN_EAR, budgets=None, optional=frozenset()) -> list[dict]:
    doc = status() if doc is None else doc
    return lat.paths(doc, subs, inputs, rate, lat.tx_flows(doc, in_ear), exempt, budgets, optional)


def verdict(rows: list[dict], doc: dict | None = None, rate: int = RATE, in_ear=IN_EAR) -> dict:
    return lat.verdict(rows, rate, lat.tx_flows(status() if doc is None else doc, in_ear))


def row(rows: list[dict], ident: str) -> dict:
    return next(r for r in rows if r["input"] == ident)


def tx_flow(tx_channels, us, channels: int = 4) -> dict:
    return {"flow": 1, "receiver": IEM, "channels": channels, "tx_channels": tx_channels, "receiver_latency_us": us}


class PathTests(unittest.TestCase):
    def test_the_box_today_the_stage_rack_at_the_budget_passes_the_wireless_path_fails(self) -> None:
        rows = paths()
        self.assertEqual(row(rows, "mic1"), {"input": "mic1", "rx": [1], "samples": 96, "ms": 1.0,
                                             "over_budget": False, "state": "ok"})
        self.assertEqual(row(rows, "mic2"), {"input": "mic2", "rx": [2], "samples": 128, "ms": 1.33,
                                             "over_budget": True, "state": "over"})
        self.assertEqual(verdict(rows)["pass"], 0)
        self.assertEqual(verdict(paths(inputs=[("mic1", [1]), ("mic3", [3])]))["pass"], 1)

    def test_under_at_and_above_the_budget(self) -> None:
        for delay, samples, state in ((8, 88, "ok"), (16, 96, "ok"), (17, 97, "over")):
            with self.subTest(delay=delay):
                doc = status(rx=[{"flow": 4, "device": STAGE, "channels": 8, "rx_channels": [1], "delay": delay}])
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
        # A channel a flow carries but without a subscription in our table is unsubscribed too (fail closed).
        rows = paths(subs={"1": sub(STAGE)}, inputs=[("mic3", [3])])
        self.assertEqual((rows[0]["state"], verdict(rows)["pass"]), ("unsubscribed", 0))
        # One channel of two without a subscription is unsubscribed too.
        rows = paths(subs={"1": sub(STAGE)}, inputs=[("keys", [1, 4])])
        self.assertEqual((rows[0]["state"], verdict(rows)["pass"]), ("unsubscribed", 0))
        # Nothing known at all is no pass, exempt or not.
        self.assertEqual(verdict(paths(subs={}, inputs=[("mic4", [4])], exempt={"mic4": "unused"}))["pass"], 0)

    def test_an_exempt_input_over_the_budget_passes_as_exempt(self) -> None:
        rows = paths(inputs=[("mic1", [1]), ("click", [2]), ("spare", [9])],
                     exempt={"click": "playback", "spare": "unused"})
        self.assertEqual(row(rows, "click"), {"input": "click", "rx": [2], "samples": None,
                                              "ms": None, "over_budget": False, "state": "exempt"})
        self.assertEqual(row(rows, "spare")["state"], "exempt")
        self.assertEqual(verdict(rows)["pass"], 1)
        # Without the exemption the same input fails.
        self.assertEqual(verdict(paths(inputs=[("mic1", [1]), ("click", [2])]))["pass"], 0)

    def test_a_subscribed_channel_no_flow_carries_is_unknown_never_a_pass(self) -> None:
        rows = paths(subs={"5": sub(STAGE)}, inputs=[("mic5", [5])])
        self.assertEqual((rows[0]["samples"], rows[0]["ms"], rows[0]["state"]), (None, None, "unknown"))
        self.assertEqual(verdict(rows)["pass"], 0)
        # A flow from the subscribed device that does not carry the channel counts nothing.
        doc = status(rx=[{"flow": 4, "device": STAGE, "channels": 8, "rx_channels": [1], "delay": 16}])
        self.assertEqual(paths(doc, {"5": sub(STAGE)}, [("mic5", [5])])[0]["state"], "unknown")
        # A flow without rx_channels (an older dstack) maps nothing.
        doc = status(rx=[{"flow": 4, "device": STAGE, "channels": 8, "delay": 16}])
        self.assertEqual(paths(doc, {"1": sub(STAGE)}, [("mic1", [1])])[0]["state"], "unknown")

    def test_an_optional_channel_no_flow_carries_is_off_and_never_fails(self) -> None:
        # A source a musician carries away (GEN3_OPTIONAL_RX), switched off:
        # the soak of 2026-11-19 failed on such an input as unknown.
        stage = [("mic1", [1]), ("mic3", [3]), ("keys", [5])]
        rows = paths(subs={**SUBS, "5": sub(STAGE)}, inputs=stage, optional=frozenset({5}))
        self.assertEqual((row(rows, "keys")["samples"], row(rows, "keys")["state"]), (None, "off"))
        self.assertEqual(verdict(rows)["pass"], 1)
        # Not optional: still unknown, still a fail.
        rows = paths(subs={**SUBS, "5": sub(STAGE)}, inputs=stage)
        self.assertEqual((row(rows, "keys")["state"], verdict(rows)["pass"]), ("unknown", 0))
        # Optional and on: guarded like any other input, over fails.
        doc = status(rx=[{"flow": 4, "device": STAGE, "channels": 8, "rx_channels": [5], "delay": 17}])
        rows = paths(doc, {"5": sub(STAGE)}, [("keys", [5])], optional=frozenset({5}))
        self.assertEqual((rows[0]["samples"], rows[0]["state"], verdict(rows, doc)["pass"]), (97, "over", 0))
        # An input on an optional channel that is off and a required one: the
        # required channel's path alone.
        rows = paths(subs={**SUBS, "5": sub(STAGE)}, inputs=[("mic1", [1, 5])], optional=frozenset({5}))
        self.assertEqual((rows[0]["samples"], rows[0]["state"]), (96, "ok"))
        # Receiving but no flow maps it (an unreadable mapping): unknown, a
        # fail, never off.
        doc = status()
        doc["channels"] = [{"rx": 5, "status": 0x01010009}]
        rows = paths(doc, {**SUBS, "5": sub(STAGE)}, stage, optional=frozenset({5}))
        self.assertEqual((row(rows, "keys")["state"], verdict(rows, doc)["pass"]), ("unknown", 0))
        doc["channels"] = [{"rx": 5, "status": 0x0001000A}]   # multicast receiving
        rows = paths(doc, {**SUBS, "5": sub(STAGE)}, stage, optional=frozenset({5}))
        self.assertEqual(row(rows, "keys")["state"], "unknown")
        doc["channels"] = [{"rx": 5, "status": 1}]   # Unresolved: off
        rows = paths(doc, {**SUBS, "5": sub(STAGE)}, stage, optional=frozenset({5}))
        self.assertEqual(row(rows, "keys")["state"], "off")
        # Off without a subscription is still unsubscribed (the table is ours).
        rows = paths(inputs=[("keys", [5])], optional=frozenset({5}))
        self.assertEqual(rows[0]["state"], "unsubscribed")
        # Only off inputs: nothing known, no pass.
        rows = paths(subs={"5": sub(STAGE)}, inputs=[("keys", [5])], optional=frozenset({5}))
        self.assertEqual(verdict(rows)["pass"], 0)

    def test_the_flow_is_matched_by_channel_never_by_the_device_s_name(self) -> None:
        # The subscription names another device than the flow's: the channel decides.
        rows = paths(subs={"1": sub("zyxqw-other")}, inputs=[("mic1", [1])])
        self.assertEqual((rows[0]["samples"], rows[0]["state"]), (96, "ok"))

    def test_without_the_engine_or_an_in_ear_flow_every_path_is_unknown(self) -> None:
        engineer_only = [tx_flow([21, 22, None, None], 1000)]
        for doc in (status(d=None), status(tx=engineer_only)):
            rows = paths(doc)
            self.assertEqual({r["state"] for r in rows}, {"unknown"})
            self.assertEqual(verdict(rows, doc)["pass"], 0)

    def test_an_input_fed_by_two_flows_takes_the_worst(self) -> None:
        rx = [{"flow": 4, "device": STAGE, "channels": 4, "rx_channels": [1, 3], "delay": 16},
              {"flow": 7, "device": WIRELESS, "channels": 4, "rx_channels": [1], "delay": 40},
              {"flow": 8, "device": "zyxqw-other", "channels": 4, "rx_channels": [9], "delay": 400}]
        rows = paths(status(rx=rx), {"1": sub(STAGE), "3": sub(STAGE)}, [("mic1", [1]), ("mic3", [3])])
        self.assertEqual((row(rows, "mic1")["samples"], row(rows, "mic3")["samples"]), (D + 40 + 24, D + 16 + 24))

    def test_an_input_on_two_channels_takes_its_worst_and_any_unknown(self) -> None:
        rows = paths(inputs=[("mic1", [1, 2])])
        self.assertEqual((rows[0]["samples"], rows[0]["state"]), (128, "over"))
        rows = paths(subs={**SUBS, "4": sub(STAGE)}, inputs=[("mic1", [1, 4])])
        self.assertEqual(rows[0]["state"], "unknown")


    def test_an_input_with_its_own_budget_passes_at_it_and_fails_above_it(self) -> None:
        # The owner's call for a device that cannot reach 1 ms: its path is
        # pinned at today's value, so any growth still fails.
        rows = paths(budgets={"mic2": 128})
        self.assertEqual((row(rows, "mic2")["state"], verdict(rows)["pass"]), ("ok", 1))
        rows = paths(budgets={"mic2": 127})
        self.assertEqual((row(rows, "mic2")["state"], verdict(rows)["pass"]), ("over", 0))
        # Another input keeps the 1 ms budget.
        doc = status(rx=[{"flow": 4, "device": STAGE, "channels": 8, "rx_channels": [1], "delay": 17}])
        rows = paths(doc, {"1": sub(STAGE)}, [("mic1", [1])], budgets={"mic2": 128})
        self.assertEqual((rows[0]["samples"], rows[0]["state"]), (97, "over"))

    def test_the_budget_list_parses_and_refuses(self) -> None:
        self.assertEqual(lat.parse_budgets(" mic2:128, hand1:128 "), {"mic2": 128, "hand1": 128})
        self.assertEqual(lat.parse_budgets(""), {})
        for text in ("mic2", "mic2:", ":128", "mic2:abc", "mic2:0", "mic2:-1", "mic2:1.5", "mic2:128,mic2:96"):
            with self.subTest(text=text):
                with self.assertRaises(ValueError):
                    lat.parse_budgets(text)


class TxTests(unittest.TestCase):
    def test_the_in_ear_latency_is_the_worst_flow_carrying_an_in_ear_output(self) -> None:
        self.assertEqual(lat.in_ear_samples(lat.tx_flows(status(), IN_EAR), RATE), 24)
        tx = [tx_flow([1, 2, None, None], 250), tx_flow([3, 4], 500), tx_flow([5], 400, channels=0),
              tx_flow([21, 22], 1000)]
        self.assertEqual(lat.in_ear_samples(lat.tx_flows(status(tx=tx), IN_EAR), RATE), 48)
        self.assertEqual(lat.in_ear_samples(lat.tx_flows(status(tx=tx), IN_EAR), 48_000), 24)

    def test_the_engineer_s_output_at_1000_us_does_not_count_when_non_in_ear(self) -> None:
        self.assertEqual(lat.tx_flows(status(), IN_EAR), [{"receiver_latency_us": 250, "state": "in_ear"},
                                                          {"receiver_latency_us": 1000, "state": "non_inear"}])
        rows = paths(inputs=[("mic1", [1])])
        self.assertEqual((rows[0]["samples"], verdict(rows)["pass"]), (96, 1))
        # The same flow carrying one member's channel too counts: 1 ms against the budget.
        tx = [tx_flow([1, 2, None, None], 250), tx_flow([21, 22, 3, None], 1000)]
        doc = status(tx=tx)
        rows = paths(doc, inputs=[("mic1", [1])])
        self.assertEqual((rows[0]["samples"], rows[0]["state"], verdict(rows, doc)["pass"]),
                         (D + 16 + 96, "over", 0))
        # Were the engineer's output an in-ear one, its 1 ms would count as well.
        rows = paths(inputs=[("mic1", [1])], in_ear=IN_EAR | {21})
        self.assertEqual(rows[0]["samples"], D + 16 + 96)

    def test_an_in_ear_tx_channel_on_a_flow_with_receiver_latency_0_fails(self) -> None:
        for us in (0, None):
            with self.subTest(us=us):
                tx = [tx_flow([1, 2, None, None], 250), tx_flow([3, 4], us)]
                doc = status(tx=tx)
                self.assertEqual(lat.tx_flows(doc, IN_EAR), [{"receiver_latency_us": 250, "state": "in_ear"},
                                                             {"receiver_latency_us": None, "state": "tx_unread"}])
                rows = paths(doc, inputs=[("mic1", [1])])
                self.assertEqual((rows[0]["state"], verdict(rows, doc)["pass"]), ("ok", 0))
        # Only the engineer's channels at 0 (a multicast flow nobody reads): no in-ear, no failure.
        tx = [tx_flow([1, 2, None, None], 250), tx_flow([21, 22], 0)]
        doc = status(tx=tx)
        self.assertEqual(lat.tx_flows(doc, IN_EAR)[1], {"receiver_latency_us": None, "state": "non_inear"})
        self.assertEqual(verdict(paths(doc, inputs=[("mic1", [1])]), doc)["pass"], 1)
        # A slot with no flow (channels 0) is never read.
        tx = [tx_flow([1, 2, None, None], 250), tx_flow([3, 4], 0, channels=0)]
        self.assertEqual(len(lat.tx_flows(status(tx=tx), IN_EAR)), 1)

    def test_a_tx_flow_whose_channels_cannot_be_read_fails(self) -> None:
        for carried in (None, "1,2", [0, 1], [1.0]):
            with self.subTest(carried=carried):
                doc = status(tx=[tx_flow([1, 2, None, None], 250), tx_flow(carried, 250)])
                self.assertEqual(lat.tx_flows(doc, IN_EAR)[1]["state"], "tx_unmapped")
                self.assertEqual(verdict(paths(doc, inputs=[("mic1", [1])]), doc)["pass"], 0)

    def test_the_exemption_list_parses_and_refuses(self) -> None:
        self.assertEqual(lat.parse_exempt("click:band pc playback, spare:no source"),
                         {"click": "band pc playback", "spare": "no source"})
        self.assertEqual(lat.parse_exempt(""), {})
        for text in ("click", "click:", ":playback", "click:a,click:b"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                lat.parse_exempt(text)


class RateTests(unittest.TestCase):
    def test_the_budget_is_1_ms_at_the_device_s_rate(self) -> None:
        self.assertEqual(lat.sample_rate('name = "x"\nsample_rate = 48000\n'), 48_000)
        self.assertEqual(lat.budget_samples(48_000), 48)
        self.assertEqual(lat.budget_samples(96_000), 96)
        self.assertEqual(lat.budget_samples(44_100), 44)   # rounded down: never above 1 ms

    def test_at_48_khz_a_path_of_48_samples_passes_and_49_fails(self) -> None:
        for delay, state in ((8, "ok"), (9, "over")):
            with self.subTest(delay=delay):
                doc = status(rx=[{"flow": 4, "device": STAGE, "channels": 8, "rx_channels": [1], "delay": delay}],
                             d=28)
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
    TEST_SITE = (HERE.parent.parent / "config" / "test-site.toml").read_text(encoding="utf-8")

    def test_every_engine_input_of_the_site_is_read(self) -> None:
        inputs = lat.engine_inputs(self.TEST_SITE)
        self.assertEqual(inputs[:11], [(f"mic{n}", [n]) for n in range(1, 11)] + [("hand1", [11])])
        self.assertEqual(len(inputs), 24)
        self.assertEqual(inputs[-1], ("bgvs", [31, 32]))

    def test_the_in_ear_outputs_are_every_mix_s_tx_but_the_engineer_s(self) -> None:
        # The test site: member1-9 on TX 1-18, the engineer on 21/22, the translator on 23.
        self.assertEqual(lat.in_ear_tx(self.TEST_SITE), frozenset(range(1, 19)) | {23})
        self.assertEqual(IN_EAR, frozenset({1, 2, 3, 4, 23}))

    def test_a_site_without_engine_inputs_or_with_a_bad_entry_is_refused(self) -> None:
        for text in ("", "[engine]\n", '[[engine.inputs]]\nid = "mic1"\nrx = "x"\n',
                     '[[engine.inputs]]\nid = "keys"\nrx = [0]\n', '[[engine.inputs]]\nrx = [1]\n'):
            with self.subTest(text=text), self.assertRaises(ValueError):
                lat.engine_inputs(text)

    def test_a_site_whose_in_ear_outputs_cannot_be_told_is_refused(self) -> None:
        mixes = 'mixes = [{ id = "member1", tx = [1, 2] }, { id = "engineer", tx = [21, 22] }]\n'
        self.assertEqual(lat.in_ear_tx('[engine]\nengineer = "engineer"\n' + mixes), frozenset({1, 2}))
        for text in ("", "[engine]\n" + mixes,                      # no engineer named
                     '[engine]\nengineer = "eng"\n' + mixes,         # the engineer's mix is not there
                     '[engine]\nengineer = "engineer"\nmixes = []\n',
                     '[engine]\nengineer = "engineer"\nmixes = [{ id = "engineer", tx = [] }]\n',
                     '[engine]\nengineer = "engineer"\nmixes = [{ tx = [1] }]\n'):
            with self.subTest(text=text), self.assertRaises(ValueError):
                lat.in_ear_tx(text)


class VerdictTests(unittest.TestCase):
    def test_the_printed_paths_carry_input_ids_and_numbers_only(self) -> None:
        doc = verdict(paths(exempt={"mic3": "zyxqwreason"}))
        self.assertEqual((doc["budget_samples"], doc["rate"]), (96, 96_000))
        self.assertEqual(doc["paths"][1], {"input": "mic2", "samples": 128, "ms": 1.33, "over_budget": True,
                                           "state": "over"})
        text = repr(doc)
        for value in (STAGE, WIRELESS, IEM, FOH, "'rx'", "device", "receiver'", "zyxqwreason"):
            self.assertNotIn(value, text)


if __name__ == "__main__":
    unittest.main()
