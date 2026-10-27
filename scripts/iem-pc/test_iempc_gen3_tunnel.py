"""Tests for scripts/iem-pc/iempc_gen3_tunnel.py, the band's tunnel
connector between the IEM PC and the Gen 3 box (#72 phase E Task 6). No PC,
box or tunnel in CI: the PC's PowerShell session, the box's ssh and the
public URL are fakes sharing one state (which connector runs). Every value
is synthetic (P6: documentation IPs, invented names, an invented token)."""
from __future__ import annotations

import sys
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import iempc_gen3_tunnel as tunnel  # noqa: E402
from iempc_test_support import Base, ip  # noqa: E402
from test_iempc_gen3_data import template  # noqa: E402

CONNECTOR = "zyxqw" + "Q" * 75   # an invented connector value (P6), built at run time
PC, BOX = "9b08d0c", "d46a94e"


class TemplateTests(unittest.TestCase):
    def test_every_box_command_with_sudo_is_a_line_of_the_template(self) -> None:
        lines = template()
        for command in (tunnel.ENCRYPT, tunnel.CHMOD, tunnel.STAT, tunnel.BOX_START, tunnel.BOX_STOP):
            self.assertIn(("root", command.removeprefix("sudo -n ")), lines, command)
        for command in (tunnel.BOX_STATE, tunnel.BOX_VERSION):
            self.assertFalse(command.startswith("sudo"), command)


class PublicHashTests(unittest.TestCase):
    """The public URL's read (2026-10-27, live: the band's tunnel answered
    Python's default User-Agent with 403 and curl's with 200)."""

    def test_the_read_names_itself_and_reads_the_git_hash(self) -> None:
        import http.server
        import threading
        seen = []

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self) -> None:   # noqa: N802 (the stdlib's name)
                seen.append((self.path, self.headers.get("User-Agent", "")))
                ok = not self.headers.get("User-Agent", "").startswith("Python-urllib")
                body = b'{"git_hash": "zyxqw12"}' if ok else b"forbidden"
                self.send_response(200 if ok else 403)
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args) -> None:
                pass

        server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
        self.addCleanup(server.server_close)
        thread = threading.Thread(target=server.handle_request)
        thread.start()
        got = tunnel.public_hash(f"http://127.0.0.1:{server.server_port}/")
        thread.join(5)
        self.assertEqual(got, "zyxqw12")
        self.assertEqual(seen[0][0], "/api/version")
        self.assertTrue(seen[0][1].startswith("iempc"), seen[0][1])


class Gen3TunnelTests(Base):
    def setUp(self) -> None:
        super().setUp()
        text = ip.env_path().read_text(encoding="utf-8")
        ip.env_path().write_text(text + "GEN3_HOST=admin@192.0.2.40\nGEN3_PUBLIC_URL=https://mixer.example.org/\n",
                                 encoding="utf-8")
        self.running = "pc"            # which connector runs
        self.pc_mode = "Auto"
        self.box_connects = True       # the box's connector reaches the edge
        self.pc_token = CONNECTOR
        self.mode = "644"
        self.sent: list[tuple[str, bytes]] = []
        self.scripts: list[str] = []
        self.order: list[str] = []        # the PC's scripts and the box's commands, as sent
        self.pc_reachable, self.box_build = True, BOX
        self.patch(ssh_ps=self.ssh_ps)
        for name, fake in (("box", self.box), ("public_hash", self.public), ("POLL_S", 0.0), ("PUBLIC_S", 0.0)):
            patcher = mock.patch.object(tunnel, name, fake)
            patcher.start()
            self.addCleanup(patcher.stop)

    def ssh_ps(self, env, script, timeout, event) -> str:
        self.scripts.append(script)
        self.order.append(script)
        if not self.pc_reachable:
            raise ip.StepError("ssh failed (exit 255): zyxqw no route")
        if script == tunnel.TOKEN_SCRIPT:
            return (self.pc_token or "no-token") + "\n"
        if script == tunnel.PC_STOP:
            self.running, self.pc_mode = ("box" if self.running == "box" else None), "Disabled"
            return "Stopped Disabled\n"
        if script == tunnel.PC_START:
            self.running, self.pc_mode = "pc", "Auto"
            return "Running Auto\n"
        if script == tunnel.PC_VERSION:
            return '{"git_hash": "%s"}\n' % PC
        self.fail(f"an unexpected PC script: {script}")

    def box(self, host: str, command: str, stdin: bytes, timeout: float) -> tuple[int, bytes, bytes]:
        self.sent.append((command, stdin))
        self.order.append(command)
        if command == tunnel.ENCRYPT:
            self.mode = "644"
            return 0, b"", b""
        if command == tunnel.CHMOD:
            self.mode = "600"
            return 0, b"", b""
        if command == tunnel.STAT:
            return 0, f"{self.mode}.root.420\n".encode(), b""
        if command == tunnel.BOX_START:
            if self.box_connects:
                self.running = "box"
            return 0, b"", b""
        if command == tunnel.BOX_STOP:
            if self.running == "box":
                self.running = None
            return 0, b"", b""
        if command == tunnel.BOX_STATE:
            return 0, (b"active\n" if self.running == "box" else b"inactive\n"), b""
        if command == tunnel.BOX_VERSION:
            return 0, ('{"git_hash": "%s"}' % self.box_build).encode(), b""
        self.fail(f"an unexpected box command: {command}")

    def public(self, url: str) -> str | None:
        self.assertEqual(url, "https://mixer.example.org/")
        return {"pc": PC, "box": BOX}.get(self.running)

    def commands(self) -> list[str]:
        return [c for c, _ in self.sent]

    def test_the_token_goes_only_into_the_boxs_credential(self) -> None:
        code, docs, err = self.run_main("gen3-tunnel", "token")
        self.assertEqual(code, 0, err)
        self.assertEqual(docs, [{"gen3_tunnel": {"token": "stored"}}])
        self.assertEqual(self.commands(), [tunnel.ENCRYPT, tunnel.CHMOD, tunnel.STAT])
        self.assertEqual(self.sent[0][1], CONNECTOR.encode())
        import base64
        for form in (CONNECTOR, base64.b64encode(CONNECTOR.encode()).decode()):
            self.assertNotIn(form, str(docs) + err + " ".join(self.commands()))

    def test_no_token_on_the_pc_changes_nothing_on_the_box(self) -> None:
        self.pc_token = ""
        code, docs, err = self.run_main("gen3-tunnel", "token")
        self.assertEqual((code, docs, self.sent), (1, [], []))
        self.assertIn("names no token", err)

    def test_to_box_stops_and_disables_the_pcs_connector_first_and_reads_the_public_url(self) -> None:
        self.mode = "600"
        code, docs, err = self.run_main("gen3-tunnel", "to-box")
        self.assertEqual(code, 0, err)
        self.assertEqual(docs, [{"gen3_tunnel": {"serves": "box", "build": BOX}}])
        self.assertEqual((self.running, self.pc_mode), ("box", "Disabled"))
        # The box's connector is up before the PC's goes: never no connector.
        self.assertLess(self.order.index(tunnel.BOX_START), self.order.index(tunnel.PC_STOP))

    def test_the_same_build_on_both_sides_is_refused(self) -> None:
        self.mode, self.box_build = "600", PC
        code, docs, err = self.run_main("gen3-tunnel", "to-box")
        self.assertEqual((code, docs), (1, []))
        self.assertIn("cannot tell them apart", err)
        self.assertNotIn(tunnel.BOX_START, self.order)
        self.assertNotIn(tunnel.PC_STOP, self.order)

    def test_a_box_that_does_not_take_the_url_puts_the_pcs_connector_back(self) -> None:
        self.mode, self.box_connects = "600", False
        code, docs, err = self.run_main("gen3-tunnel", "to-box")
        self.assertEqual((code, docs), (1, []))
        self.assertIn("does not name the box's build", err)
        self.assertIn("the PC's connector serves again", err)
        self.assertEqual((self.running, self.pc_mode), ("pc", "Auto"))

    def test_to_box_without_the_token_on_the_box_changes_nothing(self) -> None:
        self.mode = "missing"
        code, docs, err = self.run_main("gen3-tunnel", "to-box")
        self.assertEqual((code, docs), (1, []))
        self.assertIn("does not read back as 600 root", err)
        self.assertNotIn(tunnel.PC_STOP, self.scripts)

    def test_to_pc_stops_the_boxs_connector_first(self) -> None:
        self.running, self.pc_mode = "box", "Disabled"
        code, docs, err = self.run_main("gen3-tunnel", "to-pc")
        self.assertEqual(code, 0, err)
        self.assertEqual(docs, [{"gen3_tunnel": {"serves": "pc", "build": PC}}])
        self.assertLess(self.order.index(tunnel.PC_START), self.order.index(tunnel.BOX_STOP))
        self.assertEqual((self.running, self.pc_mode), ("pc", "Auto"))

    def test_a_pc_out_of_reach_on_the_way_back_leaves_the_boxs_connector_running(self) -> None:
        self.running, self.pc_mode, self.pc_reachable = "box", "Disabled", False
        code, docs, err = self.run_main("gen3-tunnel", "to-pc")
        self.assertEqual((code, docs), (1, []))
        self.assertEqual(self.running, "box")
        self.assertNotIn(tunnel.BOX_STOP, self.order)

    def test_during_an_event_only_the_way_back_runs(self) -> None:
        spec = ip.COMMANDS["gen3-tunnel"]
        self.assertEqual((spec.fn, spec.pc, spec.dev_time, spec.locked), (ip.cmd_gen3_tunnel, True, False, True))
        ip.EVENT_NOW.parent.mkdir(parents=True, exist_ok=True)
        ip.EVENT_NOW.write_text("ide event\n", encoding="utf-8")
        for verb in ("token", "to-box"):
            code, docs, err = self.run_main("gen3-tunnel", verb)
            self.assertEqual((code, docs), (1, []), verb)
            self.assertIn("only to-pc runs then", err)
        self.assertEqual(self.sent, [])
        self.running = "box"
        code, docs, err = self.run_main("gen3-tunnel", "to-pc")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.running, "pc")

    def test_missing_settings_are_refused(self) -> None:
        text = ip.env_path().read_text(encoding="utf-8")
        ip.env_path().write_text("".join(l + "\n" for l in text.splitlines() if not l.startswith("GEN3_PUBLIC_URL")),
                                 encoding="utf-8")
        code, docs, err = self.run_main("gen3-tunnel", "to-box")
        self.assertEqual((code, docs, self.sent), (1, [], []))
        self.assertIn("GEN3_PUBLIC_URL", err)


if __name__ == "__main__":
    unittest.main()
