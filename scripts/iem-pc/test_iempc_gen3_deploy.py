"""Tests for scripts/iem-pc/iempc_gen3_deploy.py, `iempc gen3-deploy` (#72,
2026-10-28): the Gen 3 box deployed in place on either side of the switch.
deploy.sh is a stand-in program recording its command line, the environment
iempc gives it and the config file as it saw it (mode and text); the card's
helper (iempc_dante.ask_card) and the box's read (iempc_gen3_data.box) are
stand-ins too. deploy.sh's own behaviour, --serving included, is
scripts/gen3/test_deploy.py's. Every value is synthetic (P6: documentation
IPs, invented names)."""
from __future__ import annotations

import json
import os
import sys
import tomllib
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import iempc_gen3_deploy as dep  # noqa: E402
from iempc_test_support import SHA, Base, ip  # noqa: E402

# deploy.sh's closing line, as test_deploy.py pins it against the real script.
sys.path.insert(1, str(HERE.parent / "gen3"))
from test_deploy import GATES  # noqa: E402

CARD, SPARE = "Zyxqw-Iem-Card", "Zyxqw-Iem-Card-2x"
GEN3 = "admin@192.0.2.40"
CARD_IP = "192.0.2.10"
IFACE = "ethz9"
DEVICE = (HERE.parent.parent / "config" / "test-dante-device.toml").read_text(encoding="utf-8")
LABELS = {str(n): f"ZYXQW out {n:02d}" for n in range(1, 129)}
REAL_DEPLOY = dep.DEPLOY   # before any test puts its stand-in there

# deploy.sh: records what it got, then exits as control.json says (writing
# the flag first when asked, as "ide event" would meanwhile).
STAND_IN = r'''
import json, os, stat, sys
log, control = sys.argv[1], sys.argv[2]
args = sys.argv[3:]
c = json.load(open(control))
seen = {"args": args, "env": {k: os.environ.get(k) for k in ("IEMMIXER_EVENT_NOW", "IEMPC_STATE", "PC_ENV")}}
config = args[args.index("--config") + 1]
seen["config_mode"] = stat.S_IMODE(os.stat(config).st_mode)
seen["config_text"] = open(config, encoding="utf-8").read()
open(log, "a").write(json.dumps(seen) + "\n")
if c.get("flag"):
    open(os.environ["IEMMIXER_EVENT_NOW"], "w").write("now\n")
print("deploy.sh: slot-b: synthetic build; health healthy")
for line in c.get("say", []):
    print(line)
if c.get("exit"):
    sys.stderr.write("deploy.sh: zyxqw refused at the box\n")
sys.exit(c.get("exit", 0))
'''


class DeployBase(Base):
    def setUp(self) -> None:
        super().setUp()
        self.ops = self.tmp / "ops"
        self.ops.mkdir()
        (self.ops / "gen3-device.toml").write_text(DEVICE, encoding="utf-8")
        (self.ops / "site.toml").write_text("[engine]\n", encoding="utf-8")
        (self.ops / "roster.toml").write_text("[[people]]\nid = 'zyxqw'\n", encoding="utf-8")
        python = self.tmp / "python"
        python.write_text("", encoding="utf-8")
        python.chmod(0o755)
        self.keys = {"GEN3_HOST": GEN3, "GEN3_DEVICE_CONFIG": str(self.ops / "gen3-device.toml"),
                     "GEN3_SITE_FILE": str(self.ops / "site.toml"), "GEN3_ROSTER_FILE": str(self.ops / "roster.toml"),
                     "GEN3_CARD_SPARE": SPARE, "DANTE_IEM_HOST": CARD_IP, "DANTE_IEM_NAME": CARD,
                     "NETAUDIO_PYTHON": str(python)}
        self.base_env = ip.env_path().read_text(encoding="utf-8")
        self.write_env(self.keys)
        self.bundle = self.tmp / f"gen3-bundle-{SHA}"
        self.bundle.mkdir()
        ip.STATE_DIR.mkdir(parents=True)
        ip.EVENT_NOW.parent.mkdir(parents=True)
        self.log = self.tmp / "deploy.log"
        self.control = self.tmp / "control.json"
        self.control.write_text("{}", encoding="utf-8")
        script = self.tmp / "deploy_stand_in.py"
        script.write_text(STAND_IN, encoding="utf-8")
        self.card_calls: list[tuple[str, str, str]] = []
        self.card_doc = {"name": SPARE, "tx_count": 128, "tx": LABELS, "rx_count": 0, "rx": {}}
        self.box_calls: list[str] = []
        self.box_reply = (0, f"IEM_DANTE_IFACE={IFACE}\n".encode(), b"")
        for module, name, fake in ((dep, "DEPLOY", (sys.executable, str(script), str(self.log), str(self.control))),
                                   (dep.iempc_dante, "ask_card", self.ask_card), (dep.data, "box", self.box)):
            patcher = mock.patch.object(module, name, fake)
            patcher.start()
            self.addCleanup(patcher.stop)

    def write_env(self, keys: dict[str, str]) -> None:
        ip.env_path().write_text(self.base_env + "".join(f"{k}={v}\n" for k, v in keys.items()), encoding="utf-8")

    def ask_card(self, ip_mod, card, verb, stdin="", extra=()):
        self.card_calls.append((verb, card.host, card.name))
        if isinstance(self.card_doc, Exception):
            raise self.card_doc
        return self.card_doc

    def box(self, host, command, data, timeout):
        self.box_calls.append(f"{host} {command}")
        return self.box_reply

    def serves(self, side: int | None) -> None:
        record = ip.STATE_DIR / "gen3-side.json"
        if side is None:
            record.unlink(missing_ok=True)
        else:
            record.write_text(json.dumps({"side": side, "at": "2026-10-28T12:00:00+00:00"}), encoding="utf-8")

    def deploys(self) -> list[dict]:
        if not self.log.exists():
            return []
        return [json.loads(line) for line in self.log.read_text(encoding="utf-8").splitlines()]

    def leftovers(self) -> list[str]:
        return sorted(p.name for p in ip.STATE_DIR.glob("gen3-deploy-*"))

    def run_deploy(self, *extra: str) -> tuple[int, list[dict], str]:
        return self.run_main("gen3-deploy", "--bundle", str(self.bundle), "--sha", SHA, *extra)

    def options(self, config: str, serving: bool) -> list[str]:
        return ["--host", GEN3, "--bundle", str(self.bundle), "--sha", SHA, "--config", config, "--iface", IFACE,
                "--start", "engine", "--site-file", self.keys["GEN3_SITE_FILE"], "--roster-file",
                self.keys["GEN3_ROSTER_FILE"], *(["--serving"] if serving else [])]

    def assert_private(self, text: str) -> None:
        """Nothing of the private env, the card or the ops files in `text` (P6)."""
        for value in (GEN3, "192.0.2", CARD, SPARE, "ZYXQW", IFACE, str(self.ops), str(self.bundle), str(self.tmp)):
            self.assertNotIn(value, text)


class DeployTests(DeployBase):
    def test_while_3x_serves_the_takeover_config_goes_with_serving(self) -> None:
        self.serves(3)
        code, docs, err = self.run_deploy()
        self.assertEqual(code, 0, err)
        # The card read under its spare name, the box's interface read, then one deploy.
        self.assertEqual(self.card_calls, [("read", CARD_IP, SPARE)])
        self.assertEqual(self.box_calls, [f"{GEN3} cat /etc/iemmixer/iem-dante.env"])
        [seen] = self.deploys()
        config = seen["args"][seen["args"].index("--config") + 1]
        self.assertEqual(seen["args"], self.options(config, serving=True))
        # The takeover config: a private file of iempc's state, 0600, removed after the deploy.
        self.assertEqual(Path(config).parent, ip.STATE_DIR)
        self.assertTrue(Path(config).name.startswith("gen3-deploy-"))
        self.assertEqual(seen["config_mode"], 0o600)
        self.assertFalse(Path(config).exists())
        self.assertEqual(self.leftovers(), [])
        taken, ops = tomllib.loads(seen["config_text"]), tomllib.loads(DEVICE)
        self.assertEqual(taken["name"], CARD)
        self.assertEqual(taken["state_file"], "/var/lib/iem-dante/takeover.toml")
        self.assertEqual(taken["tx"], [LABELS[str(n)] for n in range(1, len(ops["tx"]) + 1)])
        self.assertEqual({k: v for k, v in taken.items() if k not in ("name", "state_file", "tx")},
                         {k: v for k, v in ops.items() if k not in ("name", "state_file", "tx")})
        # deploy.sh reads iempc's flag, state folder (the side record) and private env.
        self.assertEqual(seen["env"], {"IEMMIXER_EVENT_NOW": str(ip.EVENT_NOW), "IEMPC_STATE": str(ip.STATE_DIR),
                                       "PC_ENV": str(ip.env_path())})
        self.assertEqual(docs, [{"gen3_deploy": {"serves": "3.x", "config": "takeover", "serving": True, "sha": SHA,
                                                 "deployed": True}}])
        self.assertIn("deploy.sh: slot-b: synthetic build; health healthy", err)
        self.assert_private(json.dumps(docs))

    def test_while_2x_serves_the_ops_config_goes_as_it_is_without_serving(self) -> None:
        for side in (None, 2):
            with self.subTest(side=side):
                self.serves(side)
                self.log.unlink(missing_ok=True)
                code, docs, err = self.run_deploy()
                self.assertEqual(code, 0, err)
                [seen] = self.deploys()
                self.assertEqual(seen["args"], self.options(self.keys["GEN3_DEVICE_CONFIG"], serving=False))
                self.assertEqual(seen["config_text"], DEVICE)
                self.assertEqual(docs, [{"gen3_deploy": {"serves": "2.x", "config": "usual", "serving": False,
                                                         "sha": SHA, "deployed": True}}])
        # No card read on this side; the card's keys are not needed then.
        self.assertEqual(self.card_calls, [])
        self.write_env({k: v for k, v in self.keys.items() if k in dep.BOTH})
        self.log.unlink()
        code, docs, err = self.run_deploy()
        self.assertEqual(code, 0, err)
        self.assertEqual(len(self.deploys()), 1)

    def test_the_dry_run_shows_the_plan_and_calls_nothing(self) -> None:
        for side, kind, serving in ((3, "takeover", True), (2, "usual", False)):
            with self.subTest(side=side):
                self.serves(side)
                code, docs, err = self.run_deploy("--dry-run")
                self.assertEqual(code, 0, err)
                plan = ["--host", "<GEN3_HOST>", "--bundle", "<--bundle>", "--sha", SHA, "--config", dep.KINDS[kind],
                        "--iface", "<the box's IEM_DANTE_IFACE (/etc/iemmixer/iem-dante.env)>", "--start", "engine",
                        "--site-file", "<GEN3_SITE_FILE>", "--roster-file", "<GEN3_ROSTER_FILE>",
                        *(["--serving"] if serving else [])]
                self.assertEqual(docs, [{"gen3_deploy": {"dry_run": True, "serves": f"{side}.x", "config": kind,
                                                         "deploy_sh": plan}}])
                self.assert_private(json.dumps(docs) + err)
        self.assertEqual((self.card_calls, self.box_calls, self.deploys(), self.leftovers()), ([], [], [], []))

    def test_deploy_sh_s_closing_line_names_the_box_s_three_gates_last(self) -> None:
        # deploy.sh's stdout reaches the operator on stderr once it succeeded, its closing line last: the box's
        # three gates on the build it now runs; stdout keeps the one JSON line.
        for side in (3, 2):
            with self.subTest(side=side):
                self.serves(side)
                self.control.write_text(json.dumps({"say": [GATES]}), encoding="utf-8")
                code, docs, err = self.run_deploy()
                self.assertEqual(code, 0, err)
                self.assertEqual(err.strip().splitlines()[-1], GATES)
                self.assertEqual(len(docs), 1)
                self.assertNotIn("gen3-gate", json.dumps(docs))
        # A deploy.sh that failed (its own refusals) names no gate: the error quotes its stderr only.
        self.control.write_text(json.dumps({"say": [GATES], "exit": 1}), encoding="utf-8")
        code, docs, err = self.run_deploy()
        self.assertEqual((code, docs), (1, []), err)
        self.assertNotIn("gen3-gate", err)

    def test_a_failed_deploy_fails_the_command_and_removes_the_private_config(self) -> None:
        self.serves(3)
        self.control.write_text(json.dumps({"exit": 1}), encoding="utf-8")
        code, docs, err = self.run_deploy()
        self.assertEqual((code, docs), (1, []), err)
        self.assertIn("failed (exit 1): deploy.sh: zyxqw refused at the box", err)
        [seen] = self.deploys()
        self.assertFalse(Path(seen["args"][seen["args"].index("--config") + 1]).exists())
        self.assertEqual(self.leftovers(), [])

    def test_a_flag_during_the_deploy_lets_it_end_then_runs_the_event_path(self) -> None:
        self.serves(3)
        self.control.write_text(json.dumps({"flag": True}), encoding="utf-8")
        code, docs, err = self.run_deploy()
        self.assertEqual(code, ip.PREEMPTED, err)
        self.assertEqual(docs[0], {"event": "ide event (flag file)", "action": "iempc event"})
        self.assertNotIn("gen3_deploy", json.dumps(docs))
        self.assertEqual(len(self.deploys()), 1)
        self.assertEqual(self.leftovers(), [])
        self.assertIn(["event", "--signal"], [args for _, args, _ in self.pc.calls])

    def test_a_card_that_cannot_give_the_takeover_refuses_before_the_deploy(self) -> None:
        self.serves(3)
        cases = (({"name": SPARE, "tx": {"1": "ZYXQW out 01"}}, "the card has no label for each of our"),
                 ({"name": SPARE, "tx": {"one": "x"}}, "dante_card.py read gave no TX labels"),
                 (ip.StepError("dante_card.py read failed (exit 1): the device names itself otherwise"),
                  "the device names itself otherwise"))
        for doc, words in cases:
            with self.subTest(words=words):
                self.card_doc = doc
                code, docs, err = self.run_deploy()
                self.assertEqual((code, docs), (1, []), err)
                self.assertIn(words, err)
                self.assertEqual((self.deploys(), self.leftovers()), ([], []))

    def test_a_box_without_its_interface_refuses_before_the_deploy(self) -> None:
        self.serves(3)
        cases = (((1, b"", b"cat: /etc/iemmixer/iem-dante.env: No such file or directory"),
                  "could not be read (ssh exit 1): a box no deploy has reached is deployed by hand first"),
                 ((0, b"IEM_DANTE_IFACE=eth 9\n", b""), "names no one Dante interface"),
                 ((0, b"IEM_DANTE_IFACE=a\nIEM_DANTE_IFACE=b\n", b""), "names no one Dante interface"))
        for reply, words in cases:
            with self.subTest(words=words):
                self.box_reply = reply
                code, docs, err = self.run_deploy()
                self.assertEqual((code, docs), (1, []), err)
                self.assertIn(words, err)
                self.assertEqual((self.card_calls, self.deploys(), self.leftovers()), ([], [], []))


class RefusalTests(DeployBase):
    def nothing_called(self) -> None:
        self.assertEqual((self.card_calls, self.box_calls, self.deploys(), self.leftovers()), ([], [], [], []))

    def test_refused_while_the_flag_exists_on_either_side(self) -> None:
        self.flag()
        for side in (3, 2):
            with self.subTest(side=side):
                self.serves(side)
                code, docs, err = self.run_deploy()
                self.assertEqual((code, docs), (1, []), err)
                self.assertIn("'gen3-deploy' runs only in dev time", err)
        self.nothing_called()

    def test_refused_without_a_bundle_or_a_sha(self) -> None:
        self.serves(3)
        for argv in (["gen3-deploy", "--sha", SHA], ["gen3-deploy", "--bundle", str(self.bundle)]):
            with self.subTest(argv=argv):
                with self.assertRaises(SystemExit) as raised:
                    self.run_main(*argv)
                self.assertEqual(raised.exception.code, 2)
        for argv, words in ((["--bundle", str(self.bundle), "--sha", SHA.upper()], "not a full commit SHA"),
                            (["--bundle", str(self.bundle), "--sha", SHA[:-1]], "not a full commit SHA"),
                            (["--bundle", str(self.tmp / "missing"), "--sha", SHA], "--bundle is not a folder"),
                            (["--bundle", "", "--sha", SHA], "--bundle is not a folder")):
            with self.subTest(words=words, argv=argv):
                code, docs, err = self.run_main("gen3-deploy", *argv)
                self.assertEqual((code, docs), (1, []), err)
                self.assertIn(words, err)
        self.nothing_called()

    def test_missing_keys_refuse_naming_them_never_their_values(self) -> None:
        self.serves(3)
        for drop in ("GEN3_HOST", "GEN3_SITE_FILE", "GEN3_ROSTER_FILE", "GEN3_DEVICE_CONFIG", "GEN3_CARD_SPARE",
                     "DANTE_IEM_NAME"):
            with self.subTest(drop=drop):
                self.write_env({k: v for k, v in self.keys.items() if k != drop})
                code, docs, err = self.run_deploy("--dry-run")
                self.assertEqual((code, docs), (1, []), err)
                self.assertIn(f"gen3-deploy: {drop} missing in the private env", err)
                self.assert_private(err)
        cases = ((dict(GEN3_SITE_FILE=str(self.tmp / "missing")), "GEN3_SITE_FILE is not a file"),
                 (dict(GEN3_ROSTER_FILE=str(self.ops)), "GEN3_ROSTER_FILE is not a file"),
                 (dict(GEN3_HOST="-oProxyCommand=x"), "GEN3_HOST must be an ssh destination"),
                 (dict(DANTE_IEM_HOST="224.0.0.1"), "DANTE_IEM_HOST is not a unicast IPv4 address"),
                 (dict(GEN3_CARD_SPARE="Zyxqw Spare"), "GEN3_CARD_SPARE is not a Dante device name"),
                 (dict(GEN3_CARD_SPARE=CARD.lower()), "GEN3_CARD_SPARE and DANTE_IEM_NAME must differ"))
        for change, words in cases:
            with self.subTest(words=words):
                self.write_env({**self.keys, **change})
                code, docs, err = self.run_deploy()
                self.assertEqual((code, docs), (1, []), err)
                self.assertIn(words, err)
                self.assert_private(err)
        self.nothing_called()

    def test_the_command_is_dev_time_locked_a_box_command_on_either_side(self) -> None:
        spec = ip.COMMANDS["gen3-deploy"]
        self.assertEqual((spec.pc, spec.dev_time, spec.locked), (False, True, True))
        self.assertNotIn("gen3-deploy", ip.WHILE_2X_SERVES)
        self.assertNotIn("gen3-deploy", ip.WHILE_3X_SERVES)

    def test_deploy_is_this_checkouts_script(self) -> None:
        self.assertEqual(REAL_DEPLOY, ("bash", str(HERE.parent / "gen3" / "deploy.sh")))
        self.assertTrue(os.access(HERE.parent / "gen3" / "deploy.sh", os.R_OK))
