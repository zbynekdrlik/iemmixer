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
        for command in (sw.INSTALL_CONFIG, sw.STOP, sw.START, sw.ENABLE, sw.DISABLE, sw.DISABLE_TUNNEL):
            self.assertIn(("root", command.removeprefix("sudo -n ")), lines, command)


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
        fakes = {
            (sw.iempc_dante, "ask_card"): self.ask_card, (sw.data, "box"): self.box, (sw, "copy"): self.copy,
            (sw, "answers_as"): self.answers_as, (sw.data, "send_band"): self.send_band,
            (sw.data, "pepper"): lambda *a: self.events.append("pepper"),
            (sw.tunnel, "credential_ok"): lambda *a, **k: None, (sw.tunnel, "to_box"): self.to_box,
            (sw.tunnel, "to_pc"): self.to_pc, (sw.switch, "mirror"): self.mirror, (sw, "POLL_S"): 0.0,
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
        self.card_name = extra[0]
        return {"renamed": True, "name": extra[0], "error": None}

    def box(self, host, command, stdin, timeout):
        self.assertEqual(host, "admin@192.0.2.40")
        if command == sw.STATUS:
            return 0, b'{"tx": [{"flow": 1, "live": 1}]}', b""
        if command == sw.HEALTH:
            return (0 if self.box_running else 3), b"", b""
        self.step(command)
        if command == sw.START:
            self.box_running = True
        if command == sw.STOP:
            self.box_running = False
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
        return {"members": 8, "bytes": 1, "written": 8}

    def mirror(self, ip_mod, card, ours):
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
        self.assertEqual(self.events, [sw.STOP, "band", "pepper", sw.INSTALL_CONFIG, f"rename {SPARE}", sw.START,
                                       "mirror", "url to box", sw.ENABLE])
        self.assertEqual(docs, [{"gen3_switch": {"serves": "3.x", "steps": ["band", "config", "card", "start",
                                                                            "mirror", "url", "boot"],
                                                 "members": 8, "mirrored": 29}}])
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
        self.assertIn("2.x serves again", err)
        after = self.events[self.events.index("url to box") + 1:]
        self.assertEqual(after, [sw.STOP, f"rename {CARD}", sw.INSTALL_CONFIG])
        self.assertEqual(tomllib.loads(self.installed[-1])["name"], tomllib.loads(DEVICE)["name"])
        self.assertEqual((self.side(), self.card_name, self.box_running), (2, CARD, False))

    def test_to_2_moves_the_url_first_then_the_box_and_the_card(self) -> None:
        self.assertEqual(self.run_main("gen3-switch", "to-3")[0], 0)
        self.events.clear()
        code, docs, err = self.run_main("gen3-switch", "to-2")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.events, ["url to pc", sw.DISABLE, sw.DISABLE_TUNNEL, sw.STOP, f"rename {CARD}",
                                       sw.INSTALL_CONFIG])
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
        ip.env_path().write_text(text.replace(f"GEN3_CARD_SPARE={SPARE}", f"GEN3_CARD_SPARE={CARD}"), encoding="utf-8")
        code, docs, err = self.run_main("gen3-switch", "to-3")
        self.assertEqual((code, docs, self.events), (1, [], []))
        self.assertIn("must differ", err)


if __name__ == "__main__":
    unittest.main()
