"""Tests for scripts/iem-pc/iempc_gen3_live.py, `iempc gen3-gate live` (#72
phase H, plan docs/superpowers/plans/2026-10-28-gen3-phase-h-box-gates.md
Task 5): the live E2E on the Gen 3 box, driven from the dev box. No box, no
browser and no public URL exist in CI: test_iempc_gen3_gate's stand-in `ssh`
plays the box (the HIL job, the service user's mint folder, the burst, the
server's journal, the push count), a stand-in `npx` first on PATH plays
Playwright (it records what it was handed, logs the run's client-error
marker into the box's journal, waits for the bursts, writes the JSON report
of the live specs' titles), and `iempc_gen3_tunnel.public_hash` stands in
for the band's public URL. FakeGh takes the status post. Every value is
synthetic (P6)."""
from __future__ import annotations

import json
import os
import stat
import sys
import time
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import iempc_gen3 as g3  # noqa: E402
import iempc_gen3_gate as gate  # noqa: E402
import iempc_gen3_live as live  # noqa: E402
import iempc_gen3_tunnel as tunnel  # noqa: E402
import live_verdict  # noqa: E402
import test_iempc_gen3_gate as gt  # noqa: E402  (the module: its test classes run there, not here)
from iempc_test_support import SHA, ip  # noqa: E402

PUBLIC = "https://mixer.example.org"
MEMBER = "member2"   # a person of the test roster: the sudo rule's {member}
TALK = "zyxtalk"
TOKENS = {"engineer": "zyxeng.zyxpay.zyxsig", "member": "zyxmem.zyxpay.zyxsig"}
RUN = 1_900_000_123
TITLES = live_verdict.titles(gt.REPO / "e2e" / "tests" / "live")
PERIOD = 0.4   # BURST_PERIOD_S in the tests

# The box's commands the live gate adds to the HIL gate's.
VERSION = "/opt/iemmixer/bin/iem-engine --version"
MINT = "/var/lib/iemmixer/mint"
REMOVE = f"sudo -n -u iemmixer rm -f {MINT}/engineer.token {MINT}/member.token"
MINT_ENGINEER = ("sudo -n -u iemmixer /opt/iemmixer/bin/iem-soakclient token --jwt-secret-file "
                 "/var/lib/iemmixer/site/secrets/jwt_secret --sub engineer --engineer --seconds 3600 --out "
                 f"{MINT}/engineer.token")
MINT_MEMBER = ("sudo -n -u iemmixer /opt/iemmixer/bin/iem-soakclient token --jwt-secret-file "
               f"/var/lib/iemmixer/site/secrets/jwt_secret --sub {MEMBER} --seconds 3600 --out {MINT}/member.token")
COPY = [f"sudo -n -u iemmixer cat {MINT}/engineer.token", f"sudo -n -u iemmixer cat {MINT}/member.token"]
TOKEN_CALLS = [REMOVE, MINT_ENGINEER, MINT_MEMBER, *COPY, REMOVE]
BURST = f"{gt.SUPER} test-signal mic1 -20 30 --listen"
JOURNAL = "sudo -n journalctl -u iem-server.service -o cat -n 20000 --no-pager -q"
BEFORE = [*gt.PREFLIGHT, VERSION, gt.ALARM, *TOKEN_CALLS]
AFTER = [gt.STOP, JOURNAL, gt.ALARM, gt.NRESTARTS, *gt.END]
CHANGES = (*gt.CHANGES, BURST)

# Playwright: records its arguments, folder, LIVE_* values, the client-error
# marker's run id and attempt, the token file (its mode, its folder's mode,
# its contents) and any E2E PIN variable in FAKE_GEN3/npx.log; the
# client-error spec puts the run's marker into the box's journal (not with
# FAKE_NO_MARKER); FAKE_PUSH_LEFT leaves an engineer's push subscription
# behind; the specs run until FAKE_SPEC_BURSTS bursts reached the box (10 s at
# most); FAKE_SPEC_FLAG writes the EVENT-NOW flag then and runs on a little
# (the gate must leave it to end by itself); FAKE_SPEC_FAILS=<title> fails
# that test. Its JSON report goes to LIVE_RESULTS, `npx.done` marks its end.
FAKE_NPX = r'''
import json, os, stat, sys, time
box = os.environ["FAKE_GEN3"]
args = sys.argv[1:]
path = os.environ.get("LIVE_TOKENS", "")
mode = folder_mode = tokens = None
if os.path.isfile(path):
    mode, tokens = stat.S_IMODE(os.stat(path).st_mode), json.load(open(path))
    folder_mode = stat.S_IMODE(os.stat(os.path.dirname(path)).st_mode)
run = [os.environ.get("GITHUB_RUN_ID"), os.environ.get("GITHUB_RUN_ATTEMPT")]
entry = {"args": args, "cwd": os.getcwd(), "mode": mode, "folder_mode": folder_mode, "tokens": tokens, "path": path,
         "live": {k: v for k, v in os.environ.items() if k.startswith("LIVE_") and k != "LIVE_TOKENS"}, "run": run,
         "pins": sorted(k for k in os.environ if k.startswith("E2E_") and k.endswith("_PIN"))}
with open(os.path.join(box, "npx.log"), "a") as f:
    f.write(json.dumps(entry) + "\n")
print("Running the live specs using 1 worker", flush=True)
if not os.environ.get("FAKE_NO_MARKER"):
    with open(os.path.join(box, "journal.log"), "a") as f:
        f.write(f"INFO iem_server: client error panic=iemmixer-live-marker-{run[0]}-{run[1]}\n")
if os.environ.get("FAKE_PUSH_LEFT"):
    open(os.path.join(box, "push-left"), "w").write("1\n")
want = int(os.environ.get("FAKE_SPEC_BURSTS", "2"))
log = os.path.join(box, "bursts.log")
deadline = time.monotonic() + 10
while time.monotonic() < deadline:
    if os.path.exists(log) and len(open(log).read().splitlines()) >= want:
        break
    time.sleep(0.005)
if os.environ.get("FAKE_SPEC_FLAG"):
    os.makedirs(os.path.dirname(os.environ["FAKE_FLAG"]), exist_ok=True)
    open(os.environ["FAKE_FLAG"], "w").write("now\n")
    time.sleep(0.5)
failed = os.environ.get("FAKE_SPEC_FAILS", "")
titles = json.loads(os.environ["FAKE_TITLES"])
specs = [{"title": t, "tests": [{"status": "unexpected" if t == failed else "expected", "expectedStatus": "passed",
                                 "annotations": [{"type": "live_number", "description": "meter_fps=31"}]
                                 if t == titles[0] else []}]} for t in titles]
with open(os.environ["LIVE_RESULTS"], "w") as f:
    json.dump({"suites": [{"title": "live", "specs": specs, "suites": []}]}, f)
print("  1 failed" if failed else f"  {len(titles)} passed")
open(os.path.join(box, "npx.done"), "w").write("done\n")
sys.exit(1 if failed else 0)
'''


class LiveBase(gt.GateBase):
    ARGS = ("gen3-gate", "live")

    def setUp(self) -> None:
        super().setUp()
        text = ip.env_path().read_text(encoding="utf-8")
        ip.env_path().write_text(text + f"GEN3_PUBLIC_URL={PUBLIC}/\nLIVE_MEMBER={MEMBER}\nLIVE_TEST_INPUT=mic1\n"
                                 f"LIVE_TALKBACK_INPUT={TALK}\n", encoding="utf-8")
        (self.box / "root" / MINT.lstrip("/")).mkdir(parents=True)
        d = self.tmp / "npxbin"
        d.mkdir()
        p = d / "npx"
        p.write_text(f"#!{sys.executable}\n{FAKE_NPX}", encoding="utf-8")
        p.chmod(0o755)
        self.ssh_path = os.environ["PATH"]
        patcher = mock.patch.dict(os.environ, {"PATH": f"{d}{os.pathsep}{self.ssh_path}",
                                               "FAKE_TOKENS": json.dumps(TOKENS), "FAKE_TITLES": json.dumps(TITLES),
                                               "E2E_ENGINEER_PIN": "1234", "E2E_MEMBER_PIN": "5678"})
        patcher.start()
        self.addCleanup(patcher.stop)
        self.public = {"hash": SHA[:12], "asked": []}

        def public_hash(url: str) -> str | None:
            self.public["asked"].append(url)
            return self.public["hash"]
        for target, name, value in ((tunnel, "public_hash", public_hash), (g3, "job_run", lambda: RUN),
                                    (live, "BURST_PERIOD_S", PERIOD), (live, "LOOP_S", 0.02)):
            p = mock.patch.object(target, name, value)
            p.start()
            self.addCleanup(p.stop)

    def records(self) -> Path:
        return ip.STATE_DIR / "gen3-live" / str(RUN)

    def record(self, name: str):
        return json.loads((self.records() / name).read_text(encoding="utf-8"))

    def burst_lines(self) -> list[dict]:
        path = self.records() / "bursts.jsonl"
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()] if path.exists() else []

    def steps(self) -> list[dict]:
        log = self.box / "npx.log"
        return [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()] if log.exists() else []

    def sent_bursts(self) -> list[float]:
        log = self.box / "bursts.log"
        return [float(t) for t in log.read_text(encoding="utf-8").splitlines()] if log.exists() else []

    def doc(self, docs: list[dict]) -> dict:
        lines = [d["gen3_gate"]["live"] for d in docs if "gen3_gate" in d]
        self.assertEqual(len(lines), 1, docs)
        return lines[0]

    def wait_for_npx_end(self) -> None:
        """A stand-in npx the gate left running ends by itself (never force-ended)."""
        deadline = time.monotonic() + 15
        while not (self.box / "npx.done").exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertTrue((self.box / "npx.done").exists(), "the stand-in npx never ended")
        for proc in live.LEFT_RUNNING:
            proc.wait(timeout=15)

    def assert_no_token_left(self) -> None:
        """The tokens are gone from the box's mint folder and from this box's state."""
        self.assertEqual(list((self.box / "root" / MINT.lstrip("/")).iterdir()), [])
        self.assertEqual(sorted(p.name for p in ip.STATE_DIR.glob("gen3-live-*")), [])
        for step in self.steps():
            self.assertFalse(Path(step["path"]).exists())

    def assert_private(self, text: str) -> None:
        super().assert_private(text)
        for value in (PUBLIC, "mixer.example", MEMBER, TALK, *TOKENS.values()):
            self.assertNotIn(value, text)


class LiveTests(LiveBase):
    def test_the_live_gate_passes_with_the_exact_calls_and_posts_its_status(self) -> None:
        code, docs, err = self.gate_run()
        self.assertEqual(code, 0, err)
        self.assertEqual(docs, [{"gen3_gate": {"live": {
            "pass": 1, "conclusion": "success", "first_failure": None, "health": 1, "rx_back": 1, "rx_channels": 4,
            "hil_on": 1, "bursts": 2, "bursts_failed": 0, "browser_exit": 0, "client_log": "found",
            "push_before": 1, "push_after": 1, "unit_restarts": 0, "starts": 2, "left_job": 0, "job_end": 1,
            "hil_off": 1, "numbers": {"live_specs": len(TITLES), "bursts": 2, "push_before": 1, "push_after": 1,
                                      "meter_fps": 31},
            "failed": []}}}])
        # Before any change: the HIL gate's preflight (no ARC read), the box's build, the push count, the
        # tokens; then the job, two bursts while the browser ran, the stop, the evidence, the job's end.
        self.assertEqual(self.calls(), [*BEFORE, *gt.BEGIN, BURST, BURST, *AFTER])
        self.assertEqual(self.device.packets, [])
        self.assertEqual(self.public["asked"], [PUBLIC])
        self.assertEqual(self.posts(), [["workflow", "run", "gate-status.yml", "-R", ip.REPO, "-f", f"sha={SHA}", "-f",
                                         "state=success", "-f", "context=live/gen3", "-f",
                                         "description=the Gen 3 box's live E2E passed (iempc gen3-gate live)"]])
        # Playwright: the live config in e2e/, the run's values, no PIN, the owner-only token file.
        [step] = self.steps()
        self.assertEqual(step["args"], ["playwright", "test", "--config", "playwright.live.config.ts"])
        self.assertEqual(step["cwd"], str(g3.E2E))
        self.assertEqual(step["live"], {"LIVE_BASE_URL": PUBLIC, "LIVE_SHA": SHA, "LIVE_MEMBER": MEMBER,
                                        "LIVE_TEST_INPUT": "mic1", "LIVE_TALKBACK_INPUT": TALK,
                                        "LIVE_BURST_DBFS": "-20",
                                        "LIVE_RESULTS": str(self.records() / "results.json")})
        self.assertEqual(step["run"], [str(RUN), "1"])
        self.assertEqual((step["mode"], step["folder_mode"], step["tokens"], step["pins"]), (0o600, 0o700, TOKENS, []))
        self.assertEqual(Path(step["path"]).parent.parent, ip.state_dir())
        # The records live_verdict read, as the ops jobs write them.
        self.assertEqual(self.record("begin.json"), {"reason": "ready", "push_before": 1})
        self.assertEqual(self.record("pc.json"), {"reason": "browser-done"})
        self.assertEqual(self.burst_lines(), [{"t": int(gt.NOW), "exit": 0}] * 2)
        self.assertEqual(self.record("evidence.json"), {"job_end": "ok", "client_log": "found", "push_after": 1})
        self.assertIn("passed", (self.records() / "playwright.log").read_text(encoding="utf-8"))
        self.assertEqual(stat.S_IMODE(self.records().stat().st_mode), 0o700)
        # The job: the test signal only (no step of the live gate drives a fault: never --fault-injection),
        # the engine back without it, two starts in the budget.
        staged = (self.box / "home/.cache/iem-gen3-gate/iem-hil.conf").read_text(encoding="utf-8")
        self.assertEqual(staged.split("\n")[2:], ["[Service]", "ExecStart=", f"ExecStart={gt.UNIT_LINE} --test-signal",
                                                  ""])
        self.assertFalse(self.dropin_there())
        self.assertEqual((self.state()["hil"], self.state()["starts"], self.ledger()), (False, 2, [gt.NOW] * 2))
        self.assert_no_token_left()
        self.assert_private(err + json.dumps(docs))
        self.assertEqual(self.pc.calls, [])   # no PC call

    def test_a_burst_goes_out_every_period_while_the_browser_runs(self) -> None:
        with mock.patch.dict(os.environ, {"FAKE_SPEC_BURSTS": "3"}):
            code, docs, err = self.gate_run()
        self.assertEqual(code, 0, err)
        sent = self.sent_bursts()
        self.assertEqual(len(sent), 3)
        for a, b in zip(sent, sent[1:]):
            self.assertGreaterEqual(b - a, PERIOD - 0.01)
            self.assertLess(b - a, PERIOD * 2)
        self.assertEqual(self.doc(docs)["bursts"], 3)

    def test_failures_are_named_by_fixed_words_and_posted_as_a_failure(self) -> None:
        cases = (({"FAKE_SPEC_FAILS": TITLES[2]}, ["browser"], f"live test failed: {TITLES[2]}"),
                 ({"FAKE_NO_MARKER": "1"}, ["client_log"], "client log marker missing"),
                 ({"FAKE_JOURNAL_FAILS": "1"}, ["client_log"], "client log marker unreadable"),
                 ({"FAKE_PUSH_LEFT": "1"}, ["push"], "push subscriptions left behind (1 -> 2)"),
                 ({"FAKE_BURST_FAILS": "1"}, ["bursts"], "burst 1 exited 1"),
                 ({"FAKE_RM_FAILS": "1"}, ["job_end"], "job end failed"))
        for env, failed, first in cases:
            with self.subTest(env=env):
                self.again()
                for name in ("journal.log", "push-left", "bursts.log", "npx.log", "npx.done"):
                    (self.box / name).unlink(missing_ok=True)
                self.now += 1200   # the next run's budget
                code, docs, err = self.gate_run(**env)
                self.assertEqual(code, 1, err)
                doc = self.doc(docs)
                self.assertEqual((doc["pass"], doc["conclusion"], doc["failed"], doc["first_failure"]),
                                 (0, "failure", failed, first))
                self.assertEqual(self.posts()[0][7:], ["-f", "state=failure", "-f", "context=live/gen3", "-f",
                                                       "description=the Gen 3 box's live E2E failed: "
                                                       + ", ".join(failed)])
                self.assertTrue(set(doc["failed"]) <= set(live.LIVE_FAILURES))
                self.assert_no_token_left()
                self.assert_private(err + json.dumps(docs).replace(json.dumps(first), ""))

    def test_an_engine_not_up_in_the_job_runs_no_browser_and_is_red(self) -> None:
        with mock.patch.object(gate, "HEALTH_WAIT_S", 0.1):
            code, docs, err = self.gate_run(FAKE_UNHEALTHY_IN_JOB="1")
        self.assertEqual(code, 1, err)
        doc = self.doc(docs)
        self.assertEqual((doc["health"], doc["first_failure"], doc["failed"], doc["bursts"]),
                         (0, "begin engine-not-up", ["begin", "client_log"], 0))
        self.assertEqual((self.steps(), self.sent_bursts()), ([], []))
        self.assertEqual(self.record("begin.json"), {"reason": "engine-not-up", "push_before": 1})
        self.assertFalse((self.records() / "pc.json").exists())
        self.assertFalse(self.dropin_there())   # the job ended all the same
        self.assertEqual(self.posts()[0][8], "state=failure")
        self.assert_no_token_left()

    def test_an_engine_that_restarts_by_itself_in_the_job_fails_although_the_specs_pass(self) -> None:
        # During the run, after the begin's checks: a restart at the begin
        # leaves health degraded (restarted) and the begin is not ready.
        code, docs, err = self.gate_run(FAKE_CRASH_IN_RUN="1")
        self.assertEqual(code, 1, err)
        doc = self.doc(docs)
        self.assertEqual((doc["conclusion"], doc["pass"], doc["unit_restarts"], doc["failed"]),
                         ("success", 0, 1, ["restarted"]))
        self.assertEqual(self.posts()[0][7:], ["-f", "state=failure", "-f", "context=live/gen3", "-f",
                                               "description=the Gen 3 box's live E2E failed: restarted"])
        self.assertEqual(self.ledger(), [gt.NOW] * 3)

    def test_a_refused_status_post_fails_the_command_after_its_verdict(self) -> None:
        def gh(args, timeout=ip.GH_S):
            raise ip.StepError("gh api failed (exit 1): HTTP 403")
        self.patch(gh=gh)
        code, docs, err = self.gate_run()
        self.assertEqual((code, self.doc(docs)["pass"]), (1, 1))
        self.assertIn("WARNING: gen3-gate: the status live/gen3 was not posted", err)

    def test_a_left_job_is_ended_first_and_counts_in_the_budget(self) -> None:
        self.left_job()
        code, docs, err = self.gate_run()
        self.assertEqual(code, 0, err)
        self.assertEqual(self.doc(docs)["left_job"], 1)
        calls = self.calls()
        self.assertEqual(calls[len(gt.PREFLIGHT) + 1:len(gt.PREFLIGHT) + 10],
                         [gt.STATUS, gt.RM, gt.RELOAD, gt.RESET, gt.RESTART, gt.HEALTH, gt.CTL, gt.CTL, gt.STATUS])
        self.assertEqual(self.ledger(), [gt.NOW] * 3)


class FlagTests(LiveBase):
    def test_the_flag_stops_the_bursts_ends_the_job_reports_cancelled_and_runs_the_event_path(self) -> None:
        code, docs, err = self.gate_run(FAKE_SPEC_BURSTS="1", FAKE_SPEC_FLAG="1")
        self.wait_for_npx_end()
        self.assertEqual(code, ip.PREEMPTED, err)
        doc = self.doc(docs)
        self.assertEqual((doc["pass"], doc["conclusion"], doc["failed"]), (0, "cancelled", []))
        self.assertEqual(docs[1], {"event": "ide event (flag file)", "action": "iempc event"})
        # No burst after the flag; the burst stopped, the job ended without its waits; nothing more.
        self.assertEqual(len(self.sent_bursts()), 1)
        self.assertEqual(self.calls()[len(BEFORE) + len(gt.BEGIN):],
                         [BURST, gt.STOP, gt.RM, gt.RELOAD, gt.RESET, gt.RESTART])
        self.assertFalse(self.dropin_there())
        self.assertFalse(self.state()["hil"])
        self.assertEqual(self.posts(), [])   # never a status for a cancelled run
        self.assertEqual(self.record("pc.json"), {"reason": "left-dev"})
        self.assertIn(["event", "--signal"], [args for _, args, _ in self.pc.calls])
        self.assert_no_token_left()
        self.assert_private(err + json.dumps(docs))

    def test_a_flag_during_the_job_s_begin_reports_cancelled_and_starts_no_browser(self) -> None:
        original = gate.begin_job

        def then_flag(job, c):
            original(job, c)
            self.flag()
        with mock.patch.object(gate, "begin_job", then_flag):
            code, docs, err = self.gate_run()
        self.assertEqual(code, ip.PREEMPTED, err)
        self.assertEqual(self.doc(docs)["conclusion"], "cancelled")
        self.assertEqual((self.steps(), self.sent_bursts(), self.posts()), ([], [], []))
        self.assertFalse(self.dropin_there())
        self.assert_no_token_left()

    def test_a_failure_inside_the_job_ends_it_and_fails_the_command(self) -> None:
        def cut(job, c, run):
            raise ip.StepError("ssh failed (exit 255): Connection reset")
        with mock.patch.object(live, "evidence", cut):
            code, docs, err = self.gate_run()
        self.assertEqual((code, docs), (1, []), err)
        self.assertIn("ssh failed (exit 255): Connection reset", err)
        self.assertFalse(self.dropin_there())
        self.assertEqual((self.state()["hil"], self.state()["starts"]), (False, 2))
        self.assertEqual(self.posts(), [])
        self.assert_no_token_left()


class RefusalTests(LiveBase):
    def refused_before_any_call(self, words: str, **env: str) -> str:
        code, docs, err = self.gate_run(**env)
        self.assertEqual((code, docs, self.calls(), self.steps()), (1, [], [], []), err)
        self.assertIn(words, err)
        self.assertNotIn("Traceback", err)
        self.assert_private(err)
        return err

    def test_the_env_s_values_are_refused_before_any_call(self) -> None:
        text = ip.env_path().read_text(encoding="utf-8")
        cases = ((f"GEN3_PUBLIC_URL={PUBLIC}/\n", "", "GEN3_PUBLIC_URL of the private env is not a bare https://"),
                 (f"GEN3_PUBLIC_URL={PUBLIC}/\n", "GEN3_PUBLIC_URL=http://mixer.example.org\n",
                  "GEN3_PUBLIC_URL of the private env is not a bare https://"),
                 (f"GEN3_PUBLIC_URL={PUBLIC}/\n", f"GEN3_PUBLIC_URL={PUBLIC}/zyxapp\n",
                  "GEN3_PUBLIC_URL of the private env is not a bare https://"),
                 (f"LIVE_MEMBER={MEMBER}\n", "", "LIVE_MEMBER of the private env is not an id"),
                 (f"LIVE_MEMBER={MEMBER}\n", "LIVE_MEMBER=zyx member\n", "LIVE_MEMBER of the private env is not an id"),
                 (f"LIVE_TALKBACK_INPUT={TALK}\n", "", "LIVE_TALKBACK_INPUT of the private env is not an id"),
                 ("LIVE_TEST_INPUT=mic1\n", "", "LIVE_TEST_INPUT of the private env is not an id"),
                 ("LIVE_TEST_INPUT=mic1\n", "LIVE_TEST_INPUT=zyxkeys\n", "LIVE_TEST_INPUT of the private env must be "
                  "mic1"),
                 (f"GEN3_HOST={gt.GEN3}\n", "", "no GEN3_HOST in the private env"))
        for drop, put, words in cases:
            with self.subTest(words=words, put=put):
                ip.env_path().write_text(text.replace(drop, put), encoding="utf-8")
                err = self.refused_before_any_call(words)
                self.assertNotIn("zyxkeys", err)
        ip.env_path().write_text(text, encoding="utf-8")
        with mock.patch.dict(os.environ, {"PATH": str(self.tmp / "bin")}):   # the stand-in ssh, no npx at all
            self.refused_before_any_call("npx is not on this box's PATH")

    def test_the_spec_is_the_gate_s_and_it_runs_only_while_3x_serves(self) -> None:
        (ip.STATE_DIR / "gen3-side.json").write_text(json.dumps({"side": 2}), encoding="utf-8")
        self.refused_before_any_call("2.x serves the band (gen3-side.json): 'gen3-gate' runs only while 3.x serves")

    def test_what_is_read_before_any_change_refuses_with_nothing_changed(self) -> None:
        cases = (({"FAKE_ENGINE_VERSION": "iem-engine 2.0.0-dev.50+local"}, None,
                  "no live gate: the box's iem-engine --version names no CI build"),
                 ({}, "fffffff", "no live gate: the band's public URL does not answer /api/version with the box's "
                                 "build"),
                 ({}, None, "no live gate: the band's public URL does not answer /api/version with the box's build"),
                 ({"FAKE_ALARMS_FAIL": "1"}, SHA[:12], "no live gate: the engineer's push subscriptions could not be "
                                                       "counted"),
                 ({"FAKE_UNHEALTHY": "1"}, SHA[:12], "no live gate: the box is not healthy"))
        for env, public, words in cases:
            with self.subTest(words=words, env=env):
                self.again()
                self.public["hash"] = public
                err = self.refused(words, **env)
                self.assertEqual(self.steps(), [])
                self.assertNotIn(BURST, self.calls())
                self.assertNotIn(MINT_ENGINEER, self.calls())

    def test_tokens_that_were_not_minted_or_copied_refuse_and_are_removed_on_both_boxes(self) -> None:
        for env, made in (({"FAKE_MINT_FAILS": "engineer"}, [MINT_ENGINEER]),
                          ({"FAKE_MINT_FAILS": "member"}, [MINT_ENGINEER, MINT_MEMBER])):
            with self.subTest(env=env):
                self.again()
                err = self.refused("no live gate: the browser's tokens were not minted", **env)
                self.assertIn("(nothing was changed: no HIL job)", err)
                self.assertEqual(self.calls()[len(gt.PREFLIGHT) + 2:], [REMOVE, *made, REMOVE])
                self.assertEqual(self.steps(), [])
                self.assert_no_token_left()

    def test_an_unreachable_box_is_named_by_fixed_words_never_by_its_address(self) -> None:
        # The review of PR #87: ssh's own failure (exit 255) names the host in its stderr: never quoted (P6),
        # at the box's build read and at the tokens' mint too (both ssh calls of iempc_gen3).
        for down, words in (("/opt/iemmixer/bin/iem-engine --version",
                             "no live gate: ssh to GEN3_HOST failed (exit 255) (nothing was changed)"),
                            (REMOVE, "no live gate: the browser's tokens were not minted (ssh to GEN3_HOST failed "
                                     "(exit 255))")):
            with self.subTest(down=down):
                self.again()
                err = self.refused(words, FAKE_SSH_DOWN=down)
                line = next(line for line in err.splitlines() if words in line)
                self.assertNotIn("gen3.test", line)
                self.assertNotIn("connect to host", line)
                self.assertEqual(self.steps(), [])

    def test_the_restart_budget_refuses_before_anything_changes(self) -> None:
        (ip.STATE_DIR / gate.LEDGER).write_text(json.dumps({"restarts": [gt.NOW - 100] * 2}), encoding="utf-8")
        self.refused("no live gate: the engine's restart budget: 2 engine starts in the last 600 s and 2 more would "
                     "pass the unit's 3: run it again in 500 s")


class SudoTests(LiveBase):
    def test_every_sudo_line_the_live_gate_sends_is_in_the_template(self) -> None:
        # A whole run, a run the flag ends while the browser runs, and one whose member token fails.
        sent: list[str] = []
        code, _, err = self.gate_run()
        self.assertEqual(code, 0, err)
        sent += self.calls()
        self.again()
        self.now += 1200
        for name in ("bursts.log", "npx.done"):
            (self.box / name).unlink(missing_ok=True)
        code, _, err = self.gate_run(FAKE_SPEC_BURSTS="1", FAKE_SPEC_FLAG="1")
        self.wait_for_npx_end()
        self.assertEqual(code, ip.PREEMPTED, err)
        sent += self.calls()
        ip.EVENT_NOW.unlink()
        self.again()
        self.now += 1200
        code, _, err = self.gate_run(FAKE_MINT_FAILS="member")
        self.assertEqual(code, 1, err)
        sent += self.calls()
        allowed = {(runas, text) for runas, command in gt.template_lines() for text in gt.expand(command)}
        lines = gt.sudo_lines(sent)
        for runas, command, listed in lines:
            self.assertIn((runas, command), allowed, command)
            self.assertFalse(listed, command)
        seen = {(runas, command) for runas, command, _listed in lines}
        for want in (JOURNAL, gt.INSTALL, gt.RM, gt.RELOAD, gt.RESET, gt.RESTART):
            self.assertIn(("root", want[len("sudo -n "):]), seen)
        for want in (REMOVE, MINT_ENGINEER, MINT_MEMBER, *COPY, BURST, gt.STOP, gt.ALARM):
            self.assertIn(("iemmixer", want[len("sudo -n -u iemmixer "):]), seen)


class PureTests(unittest.TestCase):
    def test_the_failure_words_follow_the_records(self) -> None:
        c = gate.Checks(health=True, rx_back=True, hil_on=True, end_ok=True, hil_off=True)
        good = live.Run(number=RUN, records=Path("."), push_before=1, ready=True, browser="success", bursts=[0, 0],
                        client_log="found", push_after=1, end="success")
        self.assertEqual(live.failures(c, good), [])
        bad = live.Run(number=RUN, records=Path("."), push_before=1, ready=True, browser="failure", bursts=[0, 2],
                       client_log="missing", push_after=-1, end="failure")
        c.unit_restarts = 1
        self.assertEqual(live.failures(c, bad), ["bursts", "browser", "client_log", "push", "job_end", "restarted"])
        self.assertEqual(live.failures(c, live.Run(number=RUN, records=Path("."), push_before=1)),
                         ["begin", "client_log", "push", "job_end", "restarted"])

    def test_a_burst_s_exit_code_is_read_from_the_failed_call(self) -> None:
        self.assertEqual(live.exit_of(ip.StepError("ssh failed (exit 2): iem-engine supervise: usage")), 2)
        self.assertEqual(live.exit_of(ip.StepError("ssh failed (exit 255): Connection reset")), 255)
        self.assertEqual(live.exit_of(ip.StillRunning("ssh still running after 30 s")), 1)

    def test_the_marker_names_the_run_and_its_attempt(self) -> None:
        self.assertEqual(live.marker(RUN), f"iemmixer-live-marker-{RUN}-1")


if __name__ == "__main__":
    unittest.main()
