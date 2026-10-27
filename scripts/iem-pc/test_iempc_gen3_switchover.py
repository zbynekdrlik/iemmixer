"""Tests for scripts/iem-pc/iempc_gen3_switchover.py, the switch between
iemmixer 2.x and 3.x (#72 phase E Task 7). The steps' own modules are tested
on their own; here they are stand-ins recording one shared order of events,
so the order, the undo and the side record are what is checked. Every value
is synthetic (P6: documentation IPs, invented names)."""
from __future__ import annotations

import sys
import tomllib
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import iempc_gen3_switchover as sw  # noqa: E402
from iempc_test_support import Base, ip  # noqa: E402
from test_iempc_gen3_data import template  # noqa: E402

CARD, SPARE, OURS = "Zyxqw-Iem-Card", "Zyxqw-Iem-Card-2x", "zyxqw-gen3"
DEVICE = (HERE.parent.parent / "config" / "test-dante-device.toml").read_text(encoding="utf-8")


class TemplateTests(unittest.TestCase):
    def test_every_sudo_command_is_a_line_of_the_template(self) -> None:
        lines = template()
        for command in (sw.INSTALL_CONFIG, sw.STOP, sw.START, sw.ENABLE, sw.DISABLE, sw.DISABLE_TUNNEL,
                        sw.RESET_FAILED):
            self.assertIn(("root", command.removeprefix("sudo -n ")), lines, command)
        self.assertIn(("iemmixer", sw.RESET_STATE.removeprefix("sudo -n -u iemmixer ")), lines)


class SwitchTests(Base):
    def setUp(self) -> None:
        super().setUp()
        config = self.tmp / "ops-device.toml"
        config.write_text(DEVICE, encoding="utf-8")
        python = self.tmp / "python"
        python.write_text("", encoding="utf-8")
        python.chmod(0o755)
        text = ip.env_path().read_text(encoding="utf-8")
        ip.env_path().write_text(text + "".join(f"{k}={v}\n" for k, v in {
            "GEN3_HOST": "admin@192.0.2.40", "GEN3_DANTE_IP": "192.0.2.41", "GEN3_DANTE_NAME": OURS,
            "GEN3_CARD_SPARE": SPARE, "GEN3_DEVICE_CONFIG": str(config), "GEN3_PUBLIC_URL": "https://mixer.example.org/",
            "DANTE_IEM_HOST": "192.0.2.10", "DANTE_IEM_NAME": CARD, "NETAUDIO_PYTHON": str(python)}.items()),
            encoding="utf-8")
        self.count = len(tomllib.loads(DEVICE)["tx"])
        self.card_name, self.serves_url, self.box_running = CARD, "pc", False
        self.events: list[str] = []
        self.installed: list[str] = []
        self.fail_at: str | None = None
        self.stops = True                 # STOP stops the stack
        self.renames_take = True
        self.after_manifest = {"engine/current.json": "a"}
        self.failed_units = False         # a unit left `failed` (exit 2/3, a crash loop) until reset-failed
        fakes = {
            (sw.iempc_dante, "ask_card"): self.ask_card, (sw.data, "box"): self.box, (sw, "copy"): self.copy,
            (sw, "answers_as"): self.answers_as, (sw.data, "send_band"): self.send_band,
            (sw.data, "pepper"): lambda *a: self.events.append("pepper"),
            (sw.tunnel, "credential_ok"): lambda *a, **k: None, (sw.tunnel, "to_box"): self.to_box,
            (sw.tunnel, "to_pc"): self.to_pc, (sw.switch, "mirror"): self.mirror, (sw, "POLL_S"): 0.0,
            (sw.data, "band_zip"): lambda *a: (b"", self.after_manifest),
            (sw, "HEALTH_S"): 0.0, (sw, "TX_S"): 0.0}
        for (module, name), fake in fakes.items():
            patcher = mock.patch.object(module, name, fake)
            patcher.start()
            self.addCleanup(patcher.stop)

    def step(self, name: str) -> None:
        self.events.append(name)
        if self.fail_at == name:
            raise ip.StepError(f"zyxqw {name} failed")

    def ask_card(self, ip_mod, card, verb, stdin="", extra=()):
        self.assertEqual(card.host, "192.0.2.10")
        if verb == "read":
            return {"name": self.card_name, "tx": {str(n): f"ZYXQW out {n:02d}" for n in range(1, 129)}}
        self.assertEqual((verb, card.name), ("rename", self.card_name))
        self.step(f"rename {extra[0]}")
        if not self.renames_take:
            return {"renamed": False, "name": self.card_name, "error": "the card does not hold the new name"}
        self.card_name = extra[0]
        return {"renamed": True, "name": extra[0], "error": None}

    def box(self, host, command, stdin, timeout):
        self.assertEqual(host, "admin@192.0.2.40")
        if command == sw.STATUS:
            return 0, b'{"tx": [{"flow": 1, "live": 1}]}', b""
        if command == sw.HEALTH:
            return (0 if self.box_running else 3), b"", b""
        if command == sw.STACK_STATE:
            if self.box_running:
                return 3, b"active\nactive\ninactive\n", b""
            return 3, (b"failed\nfailed\ninactive\n" if self.failed_units else b"inactive\n" * 3), b""
        self.step(command)
        if command == sw.START:
            self.box_running = True
        if command == sw.STOP and self.stops:
            self.box_running = False
        if command == sw.RESET_FAILED:
            self.failed_units = False
        return 0, b"", b""

    def copy(self, host, local, remote) -> int:
        self.installed.append(Path(local).read_text(encoding="utf-8"))
        return 0

    def answers_as(self, ip_mod, host, name) -> bool:
        if host == "192.0.2.10":
            return self.card_name == name
        return self.box_running and name == CARD

    def send_band(self, ip_mod, env, host, event) -> dict:
        self.step("band")
        return {"members": 8, "bytes": 1, "written": 8, "manifest": {"engine/current.json": "a"}}

    def mirror(self, ip_mod, card, ours, also=()):
        self.assertEqual(also, (CARD,))
        self.assertEqual((card.name, ours.name, ours.host), (SPARE, CARD, "192.0.2.41"))
        self.step("mirror")
        return 29, None

    def to_box(self, ip_mod, env, host, url):
        self.step("url to box")
        self.serves_url = "box"
        return {"serves": "box", "build": "d46a94e"}

    def to_pc(self, ip_mod, env, host, url):
        self.step("url to pc")
        self.serves_url = "pc"
        return {"serves": "pc", "build": "9b08d0c"}

    def side(self) -> int:
        return ip.read_json(ip.state_dir() / sw.SIDE, {}).get("side", 2)

    def test_to_3_runs_its_steps_in_order_and_records_the_side(self) -> None:
        code, docs, err = self.run_main("gen3-switch", "to-3")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.events, [sw.STOP, sw.RESET_FAILED, "band", "pepper", sw.INSTALL_CONFIG,
                                       sw.RESET_STATE, f"rename {SPARE}", sw.RESET_FAILED, sw.START, "mirror",
                                       "url to box", sw.ENABLE])
        self.assertEqual(docs, [{"gen3_switch": {"serves": "3.x", "steps": ["band", "config", "card", "start",
                                                                            "mirror", "url", "boot"],
                                                 "files": 8, "mirrored": 29, "changed_during_switch": 0}}])
        taken = tomllib.loads(self.installed[0])
        self.assertEqual((taken["name"], taken["tx"][0]), (CARD, "ZYXQW out 01"))
        self.assertEqual((self.side(), self.card_name, self.serves_url), (3, SPARE, "box"))
        # Already on 3.x: refused.
        code, docs, err = self.run_main("gen3-switch", "to-3")
        self.assertEqual((code, docs), (1, []))
        self.assertIn("3.x serves already", err)

    def test_a_failure_after_the_rename_undoes_every_step_newest_first(self) -> None:
        self.fail_at = "url to box"
        code, docs, err = self.run_main("gen3-switch", "to-3")
        self.assertEqual((code, docs), (1, []))
        self.assertIn("stopped after ['band', 'config', 'card', 'start', 'mirror']", err)
        self.assertIn("undone: 2.x serves again", err)
        after = self.events[self.events.index("url to box") + 1:]
        self.assertEqual(after, ["url to pc", sw.STOP, sw.RESET_FAILED, f"rename {CARD}", sw.INSTALL_CONFIG])
        self.assertEqual(tomllib.loads(self.installed[-1])["name"], tomllib.loads(DEVICE)["name"])
        self.assertEqual((self.side(), self.card_name, self.box_running), (2, CARD, False))

    def test_a_failed_start_is_undone_with_the_box_stopped_before_the_card_gets_its_name(self) -> None:
        self.fail_at = sw.START
        code, docs, err = self.run_main("gen3-switch", "to-3")
        self.assertEqual(code, 1)
        self.assertEqual(self.events[self.events.index(sw.START) + 1:],
                         [sw.STOP, sw.RESET_FAILED, f"rename {CARD}", sw.INSTALL_CONFIG])
        self.assertEqual((self.side(), self.card_name, self.box_running), (2, CARD, False))

    def test_a_box_that_does_not_stop_keeps_the_card_on_its_spare_name(self) -> None:
        # Never two devices under one name: the name goes back only once our stack reads stopped.
        self.fail_at, self.stops = "mirror", False
        code, docs, err = self.run_main("gen3-switch", "to-3")
        self.assertEqual(code, 1)
        self.assertIn("our device may still hold the card's name", err)
        self.assertIn("run `iempc gen3-switch to-2`", err)
        self.assertEqual((self.side(), self.card_name), (3, SPARE))
        self.assertNotIn(f"rename {CARD}", self.events)

    def test_a_rename_that_does_not_take_is_undone_by_reading_the_card(self) -> None:
        self.renames_take = False
        code, docs, err = self.run_main("gen3-switch", "to-3")
        self.assertEqual(code, 1)
        self.assertIn("did not take the name", err)
        self.assertNotIn(sw.START, self.events)
        self.assertEqual((self.side(), self.card_name), (2, CARD))

    def test_the_flag_during_the_switch_goes_back_to_2x(self) -> None:
        def mirror(ip_mod, card, ours, also=()):
            self.step("mirror")
            ip.EVENT_NOW.parent.mkdir(parents=True, exist_ok=True)
            ip.EVENT_NOW.write_text("ide event\n", encoding="utf-8")
            return 29, None
        with mock.patch.object(sw.switch, "mirror", mirror):
            code, docs, err = self.run_main("gen3-switch", "to-3")
        self.assertNotEqual(code, 0)   # a PC command whose flag came meanwhile goes on with the event path
        self.assertIn("the flag appeared", err)
        self.assertNotIn("url to box", self.events)
        self.assertEqual((self.side(), self.card_name, self.box_running), (2, CARD, False))

    def test_an_interrupt_is_undone_and_raised_again(self) -> None:
        def mirror(ip_mod, card, ours, also=()):
            raise KeyboardInterrupt
        with mock.patch.object(sw.switch, "mirror", mirror), self.assertRaises(KeyboardInterrupt):
            self.run_main("gen3-switch", "to-3")
        self.assertEqual((self.side(), self.card_name, self.box_running), (2, CARD, False))

    def test_units_left_failed_still_let_the_way_back_give_the_card_its_name(self) -> None:
        # The review of PR #83: an engine that exits 2 or 3, or a crash loop, leaves its unit `failed`
        # after the stop until reset-failed; the way back must not stall on it.
        self.assertEqual(self.run_main("gen3-switch", "to-3")[0], 0)
        self.failed_units = True
        code, docs, err = self.run_main("gen3-switch", "to-2")
        self.assertEqual(code, 0, err)
        self.assertEqual((self.side(), self.card_name), (2, CARD))

    def test_sigterm_during_the_switch_is_undone(self) -> None:
        import os
        import signal

        def mirror(ip_mod, card, ours, also=()):
            os.kill(os.getpid(), signal.SIGTERM)
            return 29, None
        before = signal.getsignal(signal.SIGTERM)
        with mock.patch.object(sw.switch, "mirror", mirror), self.assertRaises(SystemExit):
            self.run_main("gen3-switch", "to-3")
        self.assertEqual((self.side(), self.card_name, self.box_running), (2, CARD, False))
        self.assertEqual(signal.getsignal(signal.SIGTERM), before)   # the handler before it comes back

    def test_the_flag_after_the_urls_move_goes_back_to_2x(self) -> None:
        def to_box(ip_mod, env, host, url):
            self.step("url to box")
            self.serves_url = "box"
            ip.EVENT_NOW.parent.mkdir(parents=True, exist_ok=True)
            ip.EVENT_NOW.write_text("ide event\n", encoding="utf-8")
            return {"serves": "box", "build": "d46a94e"}
        with mock.patch.object(sw.tunnel, "to_box", to_box):
            code, docs, err = self.run_main("gen3-switch", "to-3")
        self.assertNotEqual(code, 0)
        self.assertNotIn(sw.ENABLE, self.events)
        self.assertEqual((self.side(), self.card_name, self.serves_url), (2, CARD, "pc"))

    def test_a_failed_read_after_a_complete_switch_still_reports_it(self) -> None:
        def broken(*a):
            raise ip.StepError("zyxqw the PC did not answer")
        with mock.patch.object(sw.data, "band_zip", broken):
            code, docs, err = self.run_main("gen3-switch", "to-3")
        self.assertEqual(code, 0, err)
        self.assertEqual((docs[0]["gen3_switch"]["serves"], docs[0]["gen3_switch"]["changed_during_switch"]),
                         ("3.x", None))
        self.assertEqual(self.side(), 3)

    def test_files_the_pc_changed_during_the_switch_are_counted(self) -> None:
        self.after_manifest = {"engine/current.json": "b", "server/lineups/x.json": "c"}
        code, docs, err = self.run_main("gen3-switch", "to-3")
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[0]["gen3_switch"]["changed_during_switch"], 2)

    def test_to_2_moves_the_url_first_then_the_box_and_the_card(self) -> None:
        self.assertEqual(self.run_main("gen3-switch", "to-3")[0], 0)
        self.events.clear()
        code, docs, err = self.run_main("gen3-switch", "to-2")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.events, ["url to pc", sw.DISABLE, sw.DISABLE_TUNNEL, sw.STOP, sw.RESET_FAILED,
                                       f"rename {CARD}", sw.INSTALL_CONFIG])
        self.assertEqual(tomllib.loads(self.installed[-1])["name"], tomllib.loads(DEVICE)["name"])
        self.assertEqual((self.side(), self.card_name, self.serves_url), (2, CARD, "pc"))
        # Run again (a way back that stopped part-way): the card keeps its name.
        self.events.clear()
        self.assertEqual(self.run_main("gen3-switch", "to-2")[0], 0)
        self.assertNotIn(f"rename {CARD}", self.events)

    def test_during_an_event_only_the_way_back_runs(self) -> None:
        spec = ip.COMMANDS["gen3-switch"]
        self.assertEqual((spec.pc, spec.dev_time, spec.locked), (True, False, True))
        ip.EVENT_NOW.parent.mkdir(parents=True, exist_ok=True)
        ip.EVENT_NOW.write_text("ide event\n", encoding="utf-8")
        code, docs, err = self.run_main("gen3-switch", "to-3")
        self.assertEqual((code, docs, self.events), (1, [], []))
        self.assertIn("only to-2 runs then", err)
        self.assertEqual(self.run_main("gen3-switch", "to-2")[0], 0)

    def test_a_dry_run_changes_nothing(self) -> None:
        code, docs, err = self.run_main("gen3-switch", "to-3", "--dry-run")
        self.assertEqual(code, 0, err)
        self.assertTrue(docs[0]["gen3_switch"]["dry_run"])
        self.assertEqual((self.events, self.installed, self.side()), ([], [], 2))

    def test_missing_or_bad_settings_are_refused(self) -> None:
        text = ip.env_path().read_text(encoding="utf-8")
        for bad in (text.replace(f"GEN3_CARD_SPARE={SPARE}", f"GEN3_CARD_SPARE={CARD.lower()}"),
                    text.replace(f"GEN3_CARD_SPARE={SPARE}", f"GEN3_CARD_SPARE={OURS}"),
                    text.replace("GEN3_HOST=admin@192.0.2.40", "GEN3_HOST=-oProxyCommand=x")):
            ip.env_path().write_text(bad, encoding="utf-8")
            code, docs, err = self.run_main("gen3-switch", "to-3")
            self.assertEqual((code, docs, self.events), (1, [], []))
            self.assertTrue("must differ" in err or "not an option" in err, err)


if __name__ == "__main__":
    unittest.main()
