"""Tests for scripts/iem-pc/iempc_gen3_roster.py (#145): a roster the box's
server refused, or a receiver's subscription to our device that no live TX
flow carries, fails loudly. The box (iempc_gen3_data.box, ssh) and netaudio
(an external program, NETAUDIO) are stand-ins; every value is synthetic (P6:
documentation IPs, invented names)."""
from __future__ import annotations

import json
import shlex
import sys
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import iempc_gen3_roster as ro  # noqa: E402
from iempc_test_support import Base, ip  # noqa: E402

sys.path.insert(1, str(HERE.parent / "gen3"))
from test_deploy import expand, sudo_lines, template_lines  # noqa: E402

GEN3 = "admin@192.0.2.40"
ROSTER = "[[people]]\nid = 'zyxqw'\n"
DEVICE = "zyxmixer"
TX = ["ZYXA L", "ZYXA R", "ZYXB L", "ZYXB R"]
STATE = f"name = {json.dumps(DEVICE)}\ntx = {json.dumps(TX)}\n"
RECV = "zyxstage"


def status(sha: str, ok: bool = True, problems: list[str] | None = None) -> bytes:
    return json.dumps({"sha256": sha, "ok": ok, "applied": ok, "problems": problems or []}).encode()


def ctl(*flows: tuple[str, int, list]) -> bytes:
    return (json.dumps({"tx": [{"receiver": r, "live": live, "tx_channels": ch} for r, live, ch in flows]})
            + "\n").encode()


def sub(rx_device: str, rx: str, tx: str, tx_device: str = DEVICE) -> dict:
    return {"rx_device": rx_device, "rx_channel": rx, "tx_channel": tx, "tx_device": tx_device}


class Clock:
    """A clock that moves only when slept."""
    def __init__(self) -> None:
        self.t = 0.0
        self.sleeps = 0

    def monotonic(self) -> float:
        return self.t

    def sleep(self, s: float) -> None:
        self.t += s
        self.sleeps += 1


class PureTests(unittest.TestCase):
    def test_the_roster_verdict(self) -> None:
        self.assertIsNone(ro.roster_verdict({"sha256": "other", "ok": True, "applied": True}, "want"))
        self.assertIsNone(ro.roster_verdict(None, "want"))
        self.assertEqual(ro.roster_verdict({"sha256": "want", "ok": True, "applied": True}, "want"), [])
        self.assertEqual(ro.roster_verdict({"sha256": "want", "ok": False, "applied": False,
                                            "problems": ["leader zyxqw: set member2 does not hear member1"]}, "want"),
                         ["leader zyxqw: set member2 does not hear member1"])
        # Refused with no problem named is still refused.
        self.assertEqual(len(ro.roster_verdict({"sha256": "want", "ok": True, "applied": False}, "want")), 1)

    def test_an_empty_slot_is_a_silent_subscription(self) -> None:
        # The 26.11 shape: the receiver subscribes TX 1 and 2, the flow carries [null, 2].
        subs = [(RECV, "Z 1", "ZYXA L"), (RECV, "Z 2", "ZYXA R")]
        flows = ro.carried(json.loads(ctl((RECV, 1, [None, 2]))))
        missing = ro.silent(subs, TX, flows)
        self.assertEqual(len(missing), 1)
        self.assertIn("'ZYXA L' (our TX 1)", missing[0])
        self.assertEqual(ro.silent(subs, TX, ro.carried(json.loads(ctl((RECV, 1, [1, 2]))))), [])

    def test_a_flow_that_is_not_live_carries_nothing(self) -> None:
        flows = ro.carried(json.loads(ctl((RECV, 0, [1, 2]))))
        self.assertEqual(flows, {})

    def test_a_tx_name_our_device_lacks_is_silent(self) -> None:
        missing = ro.silent([(RECV, "Z 1", "ZYXQ L")], TX, {RECV: {1}})
        self.assertIn("does not have", missing[0])

    def test_only_subscriptions_to_our_device_count(self) -> None:
        subs = ro.subscriptions_to([sub(RECV, "Z 1", "ZYXA L"), sub(RECV, "Z 9", "X", tx_device="zyxother")], DEVICE)
        self.assertEqual(subs, [(RECV, "Z 1", "ZYXA L")])

    def test_the_device_state(self) -> None:
        self.assertEqual(ro.our_device(STATE), (DEVICE, TX))
        with self.assertRaises(ValueError):
            ro.our_device("tx = []\n")


NETAUDIO_STAND_IN = r'''
import json, os, sys
replies = json.load(open(os.environ["FAKE_NETAUDIO"]))
count = os.environ["FAKE_NETAUDIO"] + ".n"
n = int(open(count).read()) if os.path.exists(count) else 0
open(count, "w").write(str(n + 1))
reply = replies[min(n, len(replies) - 1)]
sys.stdout.write(json.dumps(reply["out"]) if reply["out"] is not None else "")
sys.exit(reply.get("exit", 0))
'''


class BoxBase(Base):
    def setUp(self) -> None:
        super().setUp()
        self.replies = self.tmp / "netaudio.json"
        script = self.tmp / "netaudio_stand_in.py"
        script.write_text(NETAUDIO_STAND_IN, encoding="utf-8")
        self.calls: list[tuple[str, bytes]] = []
        self.status_replies: list[bytes] = [status(ro.sha256(ROSTER))]
        self.ctl_replies: list[bytes] = [ctl((RECV, 1, [1, 2]))]
        self.netaudio([[sub(RECV, "Z 1", "ZYXA L"), sub(RECV, "Z 2", "ZYXA R")]])
        for module, name, fake in ((ro.data, "box", self.box), (ro, "NETAUDIO", (sys.executable, str(script)))):
            patcher = mock.patch.object(module, name, fake)
            patcher.start()
            self.addCleanup(patcher.stop)
        patcher = mock.patch.dict("os.environ", {"FAKE_NETAUDIO": str(self.replies)})
        patcher.start()
        self.addCleanup(patcher.stop)
        self.clock = Clock()

    def netaudio(self, outs: list, exits: list[int] | None = None) -> None:
        self.replies.write_text(json.dumps([{"out": o, "exit": (exits or [0] * len(outs))[i]}
                                            for i, o in enumerate(outs)]), encoding="utf-8")

    def box(self, host, command, data, timeout):
        self.calls.append((command, data))
        if command == ro.STATUS_READ:
            reply = self.status_replies[0] if len(self.status_replies) == 1 else self.status_replies.pop(0)
        elif command == ro.CTL_STATUS:
            reply = self.ctl_replies[0] if len(self.ctl_replies) == 1 else self.ctl_replies.pop(0)
        elif command == ro.STATE_READ:
            reply = STATE.encode()
        elif command in (ro.PUSH_NEW, ro.PUSH_MOVE):
            reply = b""
        else:
            return 1, b"", b"no such command\n"
        return 0, reply, b""

    def check(self):
        return ro.after_start(ip, GEN3, ROSTER, "zyxcmd", self.clock)


class AfterStartTests(BoxBase):
    def test_an_applied_roster_and_carried_flows_pass(self) -> None:
        self.assertEqual(self.check(), {"roster_applied": True, "subscriptions_carried": 2})

    def test_a_refused_roster_fails_naming_its_problems(self) -> None:
        self.status_replies = [status(ro.sha256(ROSTER), ok=False, problems=["set member2 does not hear member1"])]
        with self.assertRaises(ip.StepError) as e:
            self.check()
        self.assertIn("REFUSED", str(e.exception))
        self.assertIn("set member2 does not hear member1", str(e.exception))

    def test_the_status_of_another_roster_is_waited_for_then_fails(self) -> None:
        self.status_replies = [status("0" * 64)]
        with self.assertRaises(ip.StepError) as e:
            self.check()
        self.assertIn("did not read this roster", str(e.exception))
        self.assertGreaterEqual(self.clock.t, ro.ROSTER_S)

    def test_a_status_that_turns_to_this_roster_passes(self) -> None:
        self.status_replies = [status("0" * 64), b"not json", status(ro.sha256(ROSTER))]
        self.assertEqual(self.check()["roster_applied"], True)
        self.assertEqual([c for c, _ in self.calls].count(ro.STATUS_READ), 3)

    def test_an_empty_slot_that_stays_fails_naming_the_channel(self) -> None:
        self.ctl_replies = [ctl((RECV, 1, [None, 2]))]
        with self.assertRaises(ip.StepError) as e:
            self.check()
        self.assertIn("NO SOUND", str(e.exception))
        self.assertIn("'ZYXA L' (our TX 1)", str(e.exception))
        self.assertGreaterEqual(self.clock.t, ro.FLOWS_S)

    def test_a_slot_filled_while_flows_start_passes(self) -> None:
        self.ctl_replies = [ctl((RECV, 1, [None, 2])), ctl((RECV, 1, [None, 2])), ctl((RECV, 1, [1, 2]))]
        self.assertEqual(self.check()["subscriptions_carried"], 2)

    def test_a_receiver_netaudio_left_out_is_read_again(self) -> None:
        # netaudio exits 1 with a partial list when one device does not answer.
        self.netaudio([[sub("zyxelse", "Q", "ZYXB L")], [sub(RECV, "Z 1", "ZYXA L")]], exits=[1, 0])
        self.assertEqual(self.check()["subscriptions_carried"], 1)

    def test_a_receiver_never_read_fails_and_never_passes_as_no_subscription(self) -> None:
        self.netaudio([[sub("zyxelse", "Q", "ZYXB L")]], exits=[1])
        with self.assertRaises(ip.StepError) as e:
            self.check()
        self.assertIn("NOT checked", str(e.exception))
        self.assertIn(RECV, str(e.exception))

    def test_no_live_flow_by_the_bound_fails(self) -> None:
        self.ctl_replies = [ctl((RECV, 0, [1, 2]))]
        with self.assertRaises(ip.StepError) as e:
            self.check()
        self.assertIn("no live TX flow", str(e.exception))

    def test_flows_that_come_up_after_the_start_are_waited_for(self) -> None:
        # Right after an engine start no flow is live yet: never a pass on an empty set.
        self.ctl_replies = [ctl(), ctl(), ctl((RECV, 1, [1, 2]))]
        self.assertEqual(self.check()["subscriptions_carried"], 2)
        self.assertGreaterEqual(self.clock.t, ro.SETTLE_S)

    def test_a_receiver_whose_flow_comes_up_later_is_checked_too(self) -> None:
        other = "zyxother"
        self.netaudio([[sub(RECV, "Z 1", "ZYXA L"), sub(other, "Y 1", "ZYXB L")]])
        # Settled on RECV alone; the other receiver's flow comes up with its slot empty and stays so.
        self.ctl_replies = [ctl((RECV, 1, [1]))] * 8 + [ctl((RECV, 1, [1]), (other, 1, [None]))]
        with self.assertRaises(ip.StepError) as e:
            self.check()
        self.assertIn("'ZYXB L' (our TX 3)", str(e.exception))

    def test_a_subscribed_receiver_with_no_flow_at_all_fails(self) -> None:
        self.netaudio([[sub(RECV, "Z 1", "ZYXA L"), sub("zyxother", "Y 1", "ZYXB L")]])
        with self.assertRaises(ip.StepError) as e:
            self.check()
        self.assertIn("zyxother 'Y 1'", str(e.exception))

    def test_live_receivers_with_no_subscription_to_our_device_fail(self) -> None:
        self.netaudio([[sub(RECV, "Z 1", "ZYXA L", tx_device="zyxrenamed")]])
        with self.assertRaises(ip.StepError) as e:
            self.check()
        self.assertIn("NOT checked", str(e.exception))


class RosterCommandTests(BoxBase):
    def setUp(self) -> None:
        super().setUp()
        self.roster = self.tmp / "roster.toml"
        self.roster.write_text(ROSTER, encoding="utf-8")
        ip.env_path().write_text(ip.env_path().read_text(encoding="utf-8")
                                 + f"GEN3_HOST={GEN3}\nGEN3_ROSTER_FILE={self.roster}\n", encoding="utf-8")
        ip.STATE_DIR.mkdir(parents=True, exist_ok=True)
        ip.EVENT_NOW.parent.mkdir(parents=True, exist_ok=True)

    def test_the_roster_goes_to_the_box_then_both_reads(self) -> None:
        with mock.patch.object(ro, "time", self.clock):
            code, docs, err = self.run_main("gen3-roster")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.calls[0], (ro.PUSH_NEW, ROSTER.encode()))
        self.assertEqual(self.calls[1], (ro.PUSH_MOVE, b""))
        self.assertEqual(docs, [{"gen3_roster": {"sent": True, "roster_applied": True, "subscriptions_carried": 2}}])
        self.assertNotIn(GEN3, json.dumps(docs))

    def test_it_runs_in_an_event_too(self) -> None:
        ip.EVENT_NOW.write_text("now\n", encoding="utf-8")
        with mock.patch.object(ro, "time", self.clock):
            code, _, err = self.run_main("gen3-roster")
        self.assertEqual(code, 0, err)

    def test_a_refused_roster_fails_the_command(self) -> None:
        self.status_replies = [status(ro.sha256(ROSTER), ok=False, problems=["zyxproblem"])]
        with mock.patch.object(ro, "time", self.clock):
            code, docs, err = self.run_main("gen3-roster")
        self.assertNotEqual(code, 0)
        self.assertIn("zyxproblem", err)
        self.assertEqual(docs, [])

    def test_a_file_that_is_not_toml_sends_nothing(self) -> None:
        self.roster.write_text("[[people\n", encoding="utf-8")
        code, _, err = self.run_main("gen3-roster")
        self.assertNotEqual(code, 0)
        self.assertIn("nothing was sent", err)
        self.assertEqual(self.calls, [])

    def test_every_sudo_line_it_sends_is_in_the_template(self) -> None:
        with mock.patch.object(ro, "time", self.clock):
            code, _, err = self.run_main("gen3-roster")
        self.assertEqual(code, 0, err)
        allowed = {(runas, text) for runas, command in template_lines() for text in expand(command)}
        sent = [" ".join(shlex.split(c)) for c, _ in self.calls]
        lines = sudo_lines(sent)
        self.assertEqual(len(lines), 3)
        for runas, command, listed in lines:
            self.assertIn((runas, command), allowed, command)
            self.assertFalse(listed, command)


if __name__ == "__main__":
    unittest.main()
