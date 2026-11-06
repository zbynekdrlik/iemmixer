"""Tests for scripts/iem-pc/iempc_gen3_deploy.py, `iempc gen3-deploy` (#72, #86):
the Gen 3 box deployed in place with the ops device config as it is. deploy.sh
is a stand-in program recording its command line, the environment iempc gives
it and the config file as it saw it; the box's read (iempc_gen3_data.box) is a
stand-in too. deploy.sh's own behaviour is scripts/gen3/test_deploy.py's. Every
value is synthetic (P6: documentation IPs, invented names)."""
from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import iempc_gen3_deploy as dep  # noqa: E402
import iempc_gen3_gate as gate  # noqa: E402
from iempc_test_support import SHA, Base, ip  # noqa: E402

# deploy.sh's closing line, as test_deploy.py pins it against the real script.
sys.path.insert(1, str(HERE.parent / "gen3"))
from test_deploy import GATES  # noqa: E402

GEN3 = "admin@192.0.2.40"
IFACE = "ethz9"
DEVICE = (HERE.parent.parent / "config" / "test-dante-device.toml").read_text(encoding="utf-8")
REAL_DEPLOY = dep.DEPLOY   # before any test puts its stand-in there
NOW = 1_900_000_000.0

# deploy.sh: records what it got, then exits as control.json says (writing
# the flag first when asked, as "ide event" would meanwhile). It marks its
# engine start where IEMPC_START_MARK names, as deploy.sh does right before the
# start is sent: a deploy.sh that succeeded started the engine; a failed one
# only when control.json says `started`.
STAND_IN = r'''
import json, os, sys
log, control = sys.argv[1], sys.argv[2]
args = sys.argv[3:]
c = json.load(open(control))
seen = {"args": args, "env": {k: os.environ.get(k) for k in ("IEMMIXER_EVENT_NOW", "IEMPC_STATE", "PC_ENV")}}
config = args[args.index("--config") + 1]
seen["config_text"] = open(config, encoding="utf-8").read()
open(log, "a").write(json.dumps(seen) + "\n")
if c.get("flag"):
    open(os.environ["IEMMIXER_EVENT_NOW"], "w").write("now\n")
if os.environ.get("IEMPC_START_MARK") and c.get("started", not c.get("exit")):
    open(os.environ["IEMPC_START_MARK"], "a").write("start\n")
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
        self.keys = {"GEN3_HOST": GEN3, "GEN3_DEVICE_CONFIG": str(self.ops / "gen3-device.toml"),
                     "GEN3_SITE_FILE": str(self.ops / "site.toml"), "GEN3_ROSTER_FILE": str(self.ops / "roster.toml")}
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
        self.box_calls: list[str] = []
        self.box_reply = (0, f"IEM_DANTE_IFACE={IFACE}\n".encode(), b"")
        for module, name, fake in ((dep, "DEPLOY", (sys.executable, str(script), str(self.log), str(self.control))),
                                   (dep.data, "box", self.box)):
            patcher = mock.patch.object(module, name, fake)
            patcher.start()
            self.addCleanup(patcher.stop)

    def write_env(self, keys: dict[str, str]) -> None:
        ip.env_path().write_text(self.base_env + "".join(f"{k}={v}\n" for k, v in keys.items()), encoding="utf-8")

    def box(self, host, command, data, timeout):
        self.box_calls.append(f"{host} {command}")
        return self.box_reply

    def deploys(self) -> list[dict]:
        if not self.log.exists():
            return []
        return [json.loads(line) for line in self.log.read_text(encoding="utf-8").splitlines()]

    def leftovers(self) -> list[str]:
        return sorted(p.name for p in ip.STATE_DIR.glob("gen3-deploy-*"))

    def run_deploy(self, *extra: str) -> tuple[int, list[dict], str]:
        return self.run_main("gen3-deploy", "--bundle", str(self.bundle), "--sha", SHA, *extra)

    def options(self) -> list[str]:
        return ["--host", GEN3, "--bundle", str(self.bundle), "--sha", SHA, "--config",
                self.keys["GEN3_DEVICE_CONFIG"], "--iface", IFACE, "--start", "engine", "--site-file",
                self.keys["GEN3_SITE_FILE"], "--roster-file", self.keys["GEN3_ROSTER_FILE"]]

    def assert_private(self, text: str) -> None:
        """Nothing of the private env or the ops files in `text` (P6)."""
        for value in (GEN3, "192.0.2", IFACE, str(self.ops), str(self.bundle), str(self.tmp)):
            self.assertNotIn(value, text)


class DeployTests(DeployBase):
    def test_the_ops_config_goes_as_it_is(self) -> None:
        code, docs, err = self.run_deploy()
        self.assertEqual(code, 0, err)
        # The box's interface read, then one deploy, and no other device asked for anything.
        self.assertEqual(self.box_calls, [f"{GEN3} cat /etc/iemmixer/iem-dante.env"])
        [seen] = self.deploys()
        self.assertEqual(seen["args"], self.options())
        # The device's whole identity is that file, byte for byte.
        self.assertEqual(seen["config_text"], DEVICE)
        self.assertEqual(seen["env"], {"IEMMIXER_EVENT_NOW": str(ip.EVENT_NOW), "IEMPC_STATE": None,
                                       "PC_ENV": str(ip.env_path())})
        self.assertEqual(docs, [{"gen3_deploy": {"sha": SHA, "deployed": True}}])
        self.assertIn("deploy.sh: slot-b: synthetic build; health healthy", err)
        self.assertEqual(self.leftovers(), [])
        self.assert_private(json.dumps(docs))

    def test_no_side_record_changes_the_deploy(self) -> None:
        # The old record may still lie in the state folder: nothing reads it.
        for side in (None, 2, 3):
            with self.subTest(side=side):
                record = ip.STATE_DIR / "gen3-side.json"
                record.unlink(missing_ok=True)
                if side is not None:
                    record.write_text(json.dumps({"side": side}), encoding="utf-8")
                self.log.unlink(missing_ok=True)
                code, docs, err = self.run_deploy()
                self.assertEqual(code, 0, err)
                self.assertEqual([s["args"] for s in self.deploys()], [self.options()])

    def test_the_dry_run_shows_the_plan_and_calls_nothing(self) -> None:
        code, docs, err = self.run_deploy("--dry-run")
        self.assertEqual(code, 0, err)
        plan = ["--host", "<GEN3_HOST>", "--bundle", "<--bundle>", "--sha", SHA, "--config", "<GEN3_DEVICE_CONFIG>",
                "--iface", "<the box's IEM_DANTE_IFACE (/etc/iemmixer/iem-dante.env)>", "--start", "engine",
                "--site-file", "<GEN3_SITE_FILE>", "--roster-file", "<GEN3_ROSTER_FILE>"]
        self.assertEqual(docs, [{"gen3_deploy": {"dry_run": True, "deploy_sh": plan}}])
        self.assert_private(json.dumps(docs) + err)
        self.assertEqual((self.box_calls, self.deploys(), self.leftovers()), ([], [], []))

    def test_deploy_sh_s_closing_line_names_the_box_s_three_gates_last(self) -> None:
        # deploy.sh's stdout reaches the operator on stderr once it succeeded, its closing line last: the box's
        # three gates on the build it now runs; stdout keeps the one JSON line.
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

    def test_a_failed_deploy_fails_the_command(self) -> None:
        self.control.write_text(json.dumps({"exit": 1}), encoding="utf-8")
        code, docs, err = self.run_deploy()
        self.assertEqual((code, docs), (1, []), err)
        self.assertIn("failed (exit 1): deploy.sh: zyxqw refused at the box", err)
        self.assertEqual(len(self.deploys()), 1)
        self.assertEqual(self.leftovers(), [])

    def test_a_flag_during_the_deploy_lets_it_end_then_runs_the_event_path(self) -> None:
        self.control.write_text(json.dumps({"flag": True}), encoding="utf-8")
        code, docs, err = self.run_deploy()
        self.assertEqual(code, ip.PREEMPTED, err)
        self.assertEqual(docs[0], {"event": "ide event (flag file)", "action": "iempc event"})
        self.assertNotIn("gen3_deploy", json.dumps(docs))
        self.assertEqual(len(self.deploys()), 1)
        self.assertEqual(self.leftovers(), [])
        self.assertNotIn(["event", "--signal"], [args for _, args, _ in self.pc.calls])   # Gen 2 ended: the PC is left alone

    def test_a_box_without_its_interface_refuses_before_the_deploy(self) -> None:
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
                self.assertEqual((self.deploys(), self.leftovers()), ([], []))


class BudgetTests(DeployBase):
    """The review of PR #87: the engine start a deploy makes counts in the box
    gates' restart budget (gen3-gate.json), so `iempc gen3-gate hil` right
    after it is refused, naming the wait."""

    def ledger(self) -> list[float]:
        return ip.read_json(ip.state_dir() / gate.LEDGER, {}).get("restarts", [])

    def test_a_deploy_that_started_the_engine_counts_one_start(self) -> None:
        with mock.patch.object(gate, "wall", lambda: NOW):
            code, docs, err = self.run_deploy()
        self.assertEqual(code, 0, err)
        self.assertEqual(self.ledger(), [NOW])
        self.assertEqual(self.leftovers(), [])   # the mark is gone too
        # The gate then refuses its three starts until the deploy's start left the unit's window.
        self.assertIsNotNone(gate.budget_refusal(self.ledger(), NOW + 60, 600, 3, 3))

    def test_a_deploy_sh_that_failed_after_its_start_counts_it_one_that_refused_before_it_does_not(self) -> None:
        self.control.write_text(json.dumps({"exit": 1, "started": True}), encoding="utf-8")
        code, docs, err = self.run_deploy()
        self.assertEqual((code, len(self.ledger())), (1, 1), err)
        (ip.STATE_DIR / gate.LEDGER).unlink()
        self.control.write_text(json.dumps({"exit": 1}), encoding="utf-8")
        code, docs, err = self.run_deploy()
        self.assertEqual((code, self.ledger()), (1, []), err)
        self.assertEqual(self.leftovers(), [])

    def test_a_flag_during_the_deploy_still_counts_its_start(self) -> None:
        self.control.write_text(json.dumps({"flag": True}), encoding="utf-8")
        code, docs, err = self.run_deploy()
        self.assertEqual((code, len(self.ledger())), (ip.PREEMPTED, 1), err)

    def test_a_dry_run_or_a_refusal_counts_nothing(self) -> None:
        self.assertEqual(self.run_deploy("--dry-run")[0], 0)
        self.write_env({k: v for k, v in self.keys.items() if k != "GEN3_SITE_FILE"})
        self.assertEqual(self.run_deploy()[0], 1)
        self.assertEqual((self.ledger(), self.deploys()), ([], []))


class RefusalTests(DeployBase):
    def nothing_called(self) -> None:
        self.assertEqual((self.box_calls, self.deploys(), self.leftovers()), ([], [], []))

    def test_refused_while_the_flag_exists(self) -> None:
        self.flag()
        code, docs, err = self.run_deploy()
        self.assertEqual((code, docs), (1, []), err)
        self.assertIn("'gen3-deploy' runs only in dev time", err)
        self.nothing_called()

    def test_refused_without_a_bundle_or_a_sha(self) -> None:
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
        for drop in ("GEN3_HOST", "GEN3_SITE_FILE", "GEN3_ROSTER_FILE", "GEN3_DEVICE_CONFIG"):
            with self.subTest(drop=drop):
                self.write_env({k: v for k, v in self.keys.items() if k != drop})
                code, docs, err = self.run_deploy("--dry-run")
                self.assertEqual((code, docs), (1, []), err)
                self.assertIn(f"gen3-deploy: {drop} missing in the private env", err)
                self.assert_private(err)
        cases = ((dict(GEN3_SITE_FILE=str(self.tmp / "missing")), "GEN3_SITE_FILE is not a file"),
                 (dict(GEN3_ROSTER_FILE=str(self.ops)), "GEN3_ROSTER_FILE is not a file"),
                 (dict(GEN3_HOST="-oProxyCommand=x"), "GEN3_HOST must be an ssh destination"))
        for change, words in cases:
            with self.subTest(words=words):
                self.write_env({**self.keys, **change})
                code, docs, err = self.run_deploy()
                self.assertEqual((code, docs), (1, []), err)
                self.assertIn(words, err)
                self.assert_private(err)
        self.nothing_called()

    def test_the_command_is_dev_time_locked_a_box_command(self) -> None:
        spec = ip.COMMANDS["gen3-deploy"]
        self.assertEqual((spec.pc, spec.dev_time, spec.locked), (False, True, True))

    def test_deploy_is_this_checkouts_script(self) -> None:
        self.assertEqual(REAL_DEPLOY, ("bash", str(HERE.parent / "gen3" / "deploy.sh")))
        self.assertTrue(os.access(HERE.parent / "gen3" / "deploy.sh", os.R_OK))
