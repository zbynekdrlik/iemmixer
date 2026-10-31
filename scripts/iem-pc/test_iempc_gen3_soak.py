"""Tests for scripts/iem-pc/iempc_gen3_soak.py, `iempc gen3-gate soak` (#72
phase H, plan docs/superpowers/plans/2026-10-28-gen3-phase-h-box-gates.md
Task 7): the soak on the Gen 3 box that serves the band, driven from the dev
box. No box exists in CI: test_iempc_gen3_gate's stand-in `ssh` plays the box
(a left HIL job, health, the builds, the service user's mint folder), and the
soak's own part of it, put in front, plays what the soak reads and changes:
the transient unit `iem-soak` (systemd-run, its state, stop and reset-failed;
the box's shell takes the quotes off, systemd expands the build from the
unit's EnvironmentFile), the client (it ends after FAKE_SOAK_READS status
reads and writes its summary into the mint folder), `iem-dante-dev ctl
status` with the engine's pump, the clock and the flows, `health || true`
and the stack's units in `systemctl show`. FakeGh takes the status post.
Every value is synthetic (P6)."""
from __future__ import annotations

import json
import os
import shlex
import sys
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import iempc_gen3 as g3  # noqa: E402
import iempc_gen3_soak as soak  # noqa: E402
import test_iempc_gen3_gate as gt  # noqa: E402  (the module: its test classes run there, not here)
from iempc_test_support import SHA, ip  # noqa: E402

sys.path.insert(1, str(HERE.parent / "gen3"))
from test_deploy import expand, sudo_lines, template_lines  # noqa: E402

MEMBER = "member9"   # a person of the test roster: the sudo rule's {member}
RUN = 1_900_000_321
MINT = "/var/lib/iemmixer/mint"
SUMMARY = f"{MINT}/soakclient.json"
ENV_FILE = "/run/iem-soak.env"
STAGED = ".cache/iem-gen3-gate/iem-soak.env"

# The box's commands the soak sends (after the host), as the stand-in logs them.
VERSION = "/opt/iemmixer/bin/iem-engine --version"
SERVER = "python3 -I -"
UNIT_SHOW = "systemctl show --property=LoadState,ActiveState,SubState,Result,ExecMainStatus iem-soak.service"
UNITS = ("systemctl show --property=Id,NRestarts,ActiveEnterTimestampMonotonic iem-engine.service "
         "iem-server.service")
HEALTH_ANY = "/opt/iemmixer/bin/iem-dante-dev health || true"
STAGE_ENV = f"mkdir -p .cache/iem-gen3-gate && cat > {STAGED}"
INSTALL_ENV = f"sudo -n install -D -m 0644 {STAGED} {ENV_FILE}"
RM_ENV = f"sudo -n rm -f {ENV_FILE}"
RM_SUMMARY = f"sudo -n -u iemmixer rm -f {SUMMARY}"
CAT_SUMMARY = f"sudo -n -u iemmixer cat {SUMMARY}"
STOP_UNIT = "sudo -n systemctl stop iem-soak"
RESET_UNIT = "sudo -n systemctl reset-failed iem-soak"
BEFORE = [gt.LEFT, gt.HEALTH, VERSION, SERVER, UNIT_SHOW, gt.CTL, UNITS]
TICK = [gt.CTL, HEALTH_ANY, UNITS, UNIT_SHOW]
AFTER = [CAT_SUMMARY, STOP_UNIT, RM_ENV, RM_SUMMARY]


def client(seconds: int, member: str = MEMBER) -> str:
    """The soak client's start for a run of `seconds`, as ssh hands it to the box's shell."""
    return ("sudo -n systemd-run --quiet --unit=iem-soak --remain-after-exit --uid=iemmixer --gid=iemmixer "
            f"--property=EnvironmentFile={ENV_FILE} --property=RuntimeMaxSec={seconds + 300} "
            "--property=TimeoutStopSec=10 --property=SendSIGKILL=no /opt/iemmixer/bin/iem-soakclient --direct "
            f"--member {member} --seconds {seconds} --out {SUMMARY} --expect-build '$IEM_SOAK_BUILD' "
            "--jwt-secret-file /var/lib/iemmixer/site/secrets/jwt_secret")


def start(seconds: int = 7200) -> list[str]:
    return [STAGE_ENV, INSTALL_ENV, RM_SUMMARY, client(seconds)]


CHANGES = (*gt.CHANGES, STAGE_ENV, INSTALL_ENV, RM_ENV, RM_SUMMARY, STOP_UNIT, RESET_UNIT,
           *(client(m * 60) for m in (10, 30, 60, 120)))

# The soak's part of the stand-in box, run before the HIL gate's handlers.
# soak.json: `unit` (iem-soak's state, null when not loaded), `client` (its
# arguments after the box's shell and systemd's expansion), `reads` (status
# reads while the client runs), `all` (every status read), `nr` and `enter`
# (the stack's units' NRestarts and ActiveEnterTimestampMonotonic). What a test
# makes go wrong (FAKE_GROW=<counter>, FAKE_CLOCK, FAKE_UNIT_RESTART,
# FAKE_UNIT_ENTER, FAKE_ENGINE_BACK, FAKE_SOAK_DEGRADED) shows from the
# client's second read on.
SOAK_PART = r'''
SOAK = os.path.join(box, "soak.json")
so = json.load(open(SOAK)) if os.path.exists(SOAK) else {
    "unit": None, "client": None, "reads": 0, "all": 0, "stops": 0, "runs": [],
    "nr": {"iem-engine.service": 0, "iem-server.service": 0},
    "enter": {"iem-engine.service": 1110000, "iem-server.service": 1120000}}
SOAK_SUMMARY = "/var/lib/iemmixer/mint/soakclient.json"
SOAK_ENV = "/run/iem-soak.env"
SOAK_STAGED = ".cache/iem-gen3-gate/iem-soak.env"
want = os.environ.get("FAKE_GROW", "")
clock_fault = os.environ.get("FAKE_CLOCK", "")
SAVES.append(lambda: (SOAK, so))

def soak_done(code=0):
    done(code)

def soak_fail(text, code=1):
    fail(text, code)

def late():
    return so["reads"] >= 2

def grown(key):
    return 1 if want == key and late() else 0

def client_ends():
    c = so["client"]
    if not os.environ.get("FAKE_NO_SUMMARY"):
        cut = os.environ.get("FAKE_SUMMARY") == "incomplete"
        seconds = 61.0 if cut else c["seconds"] + 0.5
        frames = int(50 * seconds)
        summary = {"schema": 1, "build": c["build"], "complete": not cut, "seconds": seconds, "frames": frames,
                   "expected_frames": frames, "decode_errors": 0, "gaps": int(os.environ.get("FAKE_GAPS", "0")),
                   "max_gap_ms": 21, "first_frame_ms": 40, "meter_frames": 10 * int(seconds), "reconnects": 0,
                   "no_source": 0, "error": "connection-lost" if cut else None}
        path = at(SOAK_SUMMARY)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        open(path, "w").write(json.dumps(summary) + "\n")
    code = int(os.environ.get("FAKE_CLIENT_EXIT", "0"))
    so["unit"] = ({"active": "failed", "sub": "failed", "result": "exit-code", "status": code} if code
                  else {"active": "active", "sub": "exited", "result": "success", "status": 0})

if line == "systemctl show --property=LoadState,ActiveState,SubState,Result,ExecMainStatus iem-soak.service":
    u = so["unit"]
    if u is None:
        print("Result=success\nExecMainStatus=0\nLoadState=not-found\nActiveState=inactive\nSubState=dead")
    else:
        print(f"Result={u['result']}\nExecMainStatus={u['status']}\nLoadState=loaded\nActiveState={u['active']}\n"
              f"SubState={u['sub']}")
    soak_done()
if line == ("systemctl show --property=Id,NRestarts,ActiveEnterTimestampMonotonic iem-engine.service "
            "iem-server.service"):
    blocks = []
    for u in ("iem-engine.service", "iem-server.service"):
        n = so["nr"][u] + (1 if os.environ.get("FAKE_UNIT_RESTART") == u and late() else 0)
        enter = so["enter"][u] + (5000 if os.environ.get("FAKE_UNIT_ENTER") == u and late() else 0)
        blocks.append(f"NRestarts={n}\nId={u}\nActiveEnterTimestampMonotonic={enter}")
    print("\n\n".join(blocks))
    soak_done()
if line == f"{DANTE} ctl status":
    so["all"] += 1
    u = so["unit"]
    if u is not None and u["sub"] == "running":
        so["reads"] += 1
        if so["reads"] == 1 and os.environ.get("FAKE_FLAG_ON") == "soak":
            flag()
        if so["reads"] >= int(os.environ.get("FAKE_SOAK_READS", "2")) and not os.environ.get("FAKE_NEVER_ENDS"):
            client_ends()
        if os.environ.get("FAKE_STATUS_FAILS"):
            soak_fail("ssh: connect to host port 22: Connection timed out", 255)
    elif os.environ.get("FAKE_BASELINE_FAILS"):
        soak_fail("ssh: connect to host port 22: Connection timed out", 255)
    blocks = 9_000_000 if os.environ.get("FAKE_STALL") else 9_000_000 + 3000 * so["all"]
    if os.environ.get("FAKE_ENGINE_BACK") and late():
        blocks = 3000
    clock = {"ready": 0 if clock_fault == "not_ready" and late() else 1,
             "epoch": 1 if clock_fault == "epoch" and late() else 0, "polls": 100 + so["all"],
             "re_anchors": 2 + grown("re_anchors"), "holdovers": 3 + grown("holdovers"),
             "holdover": 1 if clock_fault == "holdover" and late() else 0, "freq_ppb": -1200}
    engine = {"media": 1, "offset": 176, "playout": 36, "blocks": blocks, "late": 5 + grown("late"),
              "missed": 7 + grown("missed"), "overruns": 11 + grown("overruns"),
              "rebootstraps": 1 + grown("rebootstraps"), "max_process_us": 210}
    rx = [{"flow": f, "granted": 1, "fpp": 8, "channels": 4, "latency": 36, "packets": 1000 * so["all"],
           "refused": 0, "stray": 0, "out_of_order": 0,
           "late": (4 if f == 1 else 0) + (grown("rx_late") if f == 2 else 0), "unplaced": 0, "lateness": 14 + f, "lateness_min_ns": 375007, "keepalives": 9, "keepalive_errors": 0,
           "recv_errors": 0} for f in (1, 2)]
    tx = [{"flow": 1, "live": 1, "fpp": 8, "channels": 8, "packets": 2000 * so["all"], "send_errors": 0,
           "keepalives": 4, "rebootstraps": 0, "holes": 6 + grown("tx_holes")}]
    off = os.environ.get("FAKE_RX_OFF") == "1" and late()
    channels = [{"rx": n, "status": 1 if off and n == 104 else 0x01010009, "written": 1,
                 "holes": (2 if n == 101 else 0) + (grown("rx_holes") if n == 102 else 0), "late": 0,
                 "duplicate": 0, "early": 0} for n in (101, 102, 103, 104)]
    doc = {"clock": clock, "requests": [], "sent": [], "uncounted": 0, "tx": tx, "rx": rx, "rx_poll_errors": 0,
           "channels": channels, "sources": [], "checks": []}
    if not os.environ.get("FAKE_NO_ENGINE") and not (os.environ.get("FAKE_NO_ENGINE_AFTER") and late()):
        doc["engine"] = engine
    print(json.dumps(doc))
    soak_done()
if line == f"{DANTE} health || true":
    if os.environ.get("FAKE_SOAK_DEGRADED") and late():
        print(json.dumps({"health": {"state": "degraded", "reasons": ["holdover", "zyxqword"], "units": []}}))
    else:
        print(json.dumps({"health": {"state": "healthy", "reasons": [], "units": []}}))
    soak_done()
if line == f"mkdir -p .cache/iem-gen3-gate && cat > {SOAK_STAGED}":
    os.makedirs(os.path.join(home, ".cache/iem-gen3-gate"), exist_ok=True)
    open(os.path.join(home, SOAK_STAGED), "w").write(data)
    soak_done()
if line == f"sudo -n install -D -m 0644 {SOAK_STAGED} {SOAK_ENV}":
    os.makedirs(os.path.dirname(at(SOAK_ENV)), exist_ok=True)
    open(at(SOAK_ENV), "w").write(open(os.path.join(home, SOAK_STAGED)).read())
    soak_done()
if line == f"sudo -n rm -f {SOAK_ENV}":
    if os.path.exists(at(SOAK_ENV)):
        os.remove(at(SOAK_ENV))
    soak_done()
if words[:3] == ["sudo", "-n", "systemd-run"]:
    rest = words[2:]
    so["runs"].append(rest)
    if os.environ.get("FAKE_RUN_FAILS"):
        soak_fail("Failed to start transient service unit: Access denied")
    if so["unit"] is not None:
        soak_fail("Failed to start transient service unit: Unit iem-soak.service was already loaded or has a "
                  "fragment file.")
    env = {}
    if os.path.exists(at(SOAK_ENV)):
        for raw in open(at(SOAK_ENV)).read().splitlines():
            key, _, value = raw.partition("=")
            env[key] = value
    # The box's shell takes the quotes off; systemd expands $NAME from the unit's EnvironmentFile.
    args = [env.get(w[1:], "") if w.startswith("$") else w for w in (x.strip("'") for x in rest)]
    argv = args[args.index("/opt/iemmixer/bin/iem-soakclient") + 1:]
    so["client"] = {"args": argv, "build": argv[argv.index("--expect-build") + 1],
                    "seconds": int(argv[argv.index("--seconds") + 1])}
    so["unit"], so["reads"] = {"active": "active", "sub": "running", "result": "success", "status": 0}, 0
    soak_done()
if line == "sudo -n systemctl stop iem-soak":
    so["stops"] += 1
    if so["unit"] is None:
        soak_fail("Failed to stop iem-soak.service: Unit iem-soak.service not loaded.", 5)
    if so["unit"]["active"] != "failed" and not os.environ.get("FAKE_STOP_KEEPS"):
        so["unit"] = None
    soak_done()
if line == "sudo -n systemctl reset-failed iem-soak":
    if so["unit"] is None:
        soak_fail("Failed to reset failed state of unit iem-soak.service: Unit iem-soak.service not loaded.")
    if so["unit"]["active"] == "failed":
        so["unit"] = None
    soak_done()
'''
ANCHOR = 'if line == f"test -e {DROPIN} && echo left || echo none":'
assert gt.FAKE_SSH.count(ANCHOR) == 1, "the HIL gate's stand-in box changed: put the soak's part in front again"
SOAK_SSH = gt.FAKE_SSH.replace(ANCHOR, SOAK_PART + ANCHOR)

# The line of a passing run of 120 minutes (its two holes taken out: they are times).
PASS = {"pass": 1, "conclusion": "success", "minutes": 120, "client_exit": 0, "client_result": "success",
        "harness": {"problems": [], "seconds": 7200.5, "frames": 360_025, "expected_frames": 360_025, "gaps": 0,
                    "reconnects": 0, "decode_errors": 0, "meter_frames": 72_000},
        "reads": 3, "failed_reads": 0, "health_reads": 2, "not_healthy": 0, "health_reasons": [],
        "pump_late": 0, "pump_missed": 0, "pump_overruns": 0, "pump_rebootstraps": 0, "pump_stalled": 0,
        "max_process_us": 210, "clock_not_ready": 0, "holdovers": 0, "in_holdover": 0, "re_anchors": 0,
        "rx_late": 0, "tx_holes": 0, "rx_holes": 0, "rx_off": 0, "lateness_max": 16, "counts_back": 0, "unit_restarts": 0,
        "left_job": 0, "left_soak": 0, "failed": []}


class SoakBase(gt.GateBase):
    ARGS = ("gen3-gate", "soak")

    def setUp(self) -> None:
        super().setUp()
        text = ip.env_path().read_text(encoding="utf-8")
        ip.env_path().write_text(text + f"SOAK_MEMBER={MEMBER}\n", encoding="utf-8")
        (self.box / "root" / MINT.lstrip("/")).mkdir(parents=True)
        (self.tmp / "bin" / "ssh").write_text(f"#!{sys.executable}\n{SOAK_SSH}", encoding="utf-8")
        for target, name, value in ((soak, "STATUS_EVERY_S", 0.0), (soak, "HEALTH_EVERY_S", 0.0),
                                    (g3, "job_run", lambda: RUN)):
            p = mock.patch.object(target, name, value)
            p.start()
            self.addCleanup(p.stop)

    def soak_state(self) -> dict:
        path = self.box / "soak.json"
        return json.loads(path.read_text(encoding="utf-8")) if path.exists() else {}

    def set_soak(self, unit: dict) -> None:
        """A soak unit a dead run left (its client running, exited or failed)."""
        doc = {"unit": unit, "client": {"args": [], "build": SHA, "seconds": 600}, "reads": 0, "all": 0, "stops": 0,
               "runs": [], "nr": {"iem-engine.service": 0, "iem-server.service": 0},
               "enter": {"iem-engine.service": 1110000, "iem-server.service": 1120000}}
        (self.box / "soak.json").write_text(json.dumps(doc), encoding="utf-8")

    def again(self) -> None:
        super().again()
        (self.box / "soak.json").unlink(missing_ok=True)

    def line(self, docs: list[dict]) -> dict:
        doc = dict(docs[0]["gen3_gate"]["soak"])
        for key in ("poll_hole_s", "health_hole_s"):
            self.assertIsInstance(doc.pop(key), int, key)
        return doc

    def env_file_there(self) -> bool:
        return (self.box / "root" / ENV_FILE.lstrip("/")).exists()

    def summary_there(self) -> bool:
        return (self.box / "root" / SUMMARY.lstrip("/")).exists()

    def assert_private(self, text: str) -> None:
        super().assert_private(text)
        self.assertNotIn(MEMBER, text)

    def refused(self, words: str, **env: str) -> str:
        err = super().refused(words, **env)
        self.assertFalse(set(self.calls()) & set(CHANGES), self.calls())
        return err


class SoakTests(SoakBase):
    def test_the_soak_passes_with_the_exact_calls_and_posts_its_status(self) -> None:
        code, docs, err = self.gate_run()
        self.assertEqual(code, 0, err)
        self.assertEqual(self.line(docs), PASS)
        self.assertEqual(self.calls(), [*BEFORE, *start(7200), *TICK, *TICK, *AFTER])
        # The client as the service user: the box's build expanded from the unit's file, the member, 120 minutes.
        self.assertEqual(self.soak_state()["client"]["args"],
                         ["--direct", "--member", MEMBER, "--seconds", "7200", "--out", SUMMARY, "--expect-build",
                          SHA, "--jwt-secret-file", "/var/lib/iemmixer/site/secrets/jwt_secret"])
        staged = (self.box / "home" / STAGED).read_text(encoding="utf-8")
        self.assertEqual(staged, f"IEM_SOAK_BUILD={SHA}\n")
        # The status on the box's build: success, a fixed description.
        self.assertEqual(self.posts(), [["api", "-X", "POST", f"repos/{ip.REPO}/statuses/{SHA}", "-f",
                                         "state=success", "-f", "context=soak/gen3", "-f",
                                         "description=the Gen 3 box's soak passed (iempc gen3-gate soak, 120 min)"]])
        # Nothing left on the box: the unit, its file, the summary; no HIL job, no engine start.
        self.assertEqual((self.soak_state()["unit"], self.env_file_there(), self.summary_there()), (None, False, False))
        self.assertEqual((self.state()["starts"], self.ledger()), (0, []))
        # The run's records in iempc's state: the summary and the reads, no member and no build.
        records = ip.STATE_DIR / soak.RECORDS / str(RUN)
        summary = json.loads((records / "soakclient.json").read_text(encoding="utf-8"))
        self.assertEqual((summary["complete"], summary["seconds"]), (True, 7200.5))
        polls = [json.loads(line) for line in (records / "polls.jsonl").read_text(encoding="utf-8").splitlines()]
        self.assertEqual([p["kind"] for p in polls], ["status", "status", "health", "status", "health"])
        self.assertNotIn(MEMBER, (records / "polls.jsonl").read_text(encoding="utf-8"))
        self.assert_private(err + json.dumps(docs))
        self.assertEqual(self.pc.calls, [])   # no PC call

    def test_each_length_has_its_own_fixed_start_and_other_lengths_are_refused_before_any_call(self) -> None:
        for minutes in (10, 30, 60, 120):
            with self.subTest(minutes=minutes):
                self.again()
                code, docs, err = self.gate_run_args("--minutes", str(minutes))
                self.assertEqual(code, 0, err)
                doc = self.line(docs)
                self.assertEqual((doc["minutes"], doc["harness"]["seconds"]), (minutes, minutes * 60 + 0.5))
                self.assertIn(client(minutes * 60), self.calls())
                self.assertIn(f"description=the Gen 3 box's soak passed (iempc gen3-gate soak, {minutes} min)",
                              self.posts()[0])
        for minutes in ("9", "11", "0", "45", "121", "-10"):
            with self.subTest(minutes=minutes):
                self.again()
                code, docs, err = self.gate_run_args("--minutes", minutes)
                self.assertEqual((code, docs, self.calls(), self.posts()), (1, [], [], []), err)
                self.assertIn("--minutes must be 10, 30, 60 or 120 (the sudo rule's fixed run lengths)", err)
        # The HIL and live gates take no length.
        for gate_name in ("hil", "live"):
            with self.subTest(gate=gate_name):
                self.again()
                code, docs, err = self.run_main("gen3-gate", gate_name, "--minutes", "10")
                self.assertEqual((code, docs, self.calls()), (1, [], []), err)
                self.assertIn("--minutes is the soak gate's only", err)

    def gate_run_args(self, *args: str, **env: str) -> tuple[int, list[dict], str]:
        with mock.patch.dict(os.environ, env):
            return self.run_main(*self.ARGS, *args)


class CriteriaTests(SoakBase):
    def test_every_poll_criterion_fails_the_verdict_by_its_word_and_posts_a_failure(self) -> None:
        cases = (
            ({"FAKE_GROW": "late"}, "pump_late", "pump_late", 1),
            ({"FAKE_GROW": "missed"}, "pump_missed", "pump_missed", 1),
            ({"FAKE_GROW": "overruns"}, "pump_overruns", "pump_overruns", 1),
            ({"FAKE_GROW": "rebootstraps"}, "pump_rebootstraps", "pump_rebootstraps", 1),
            ({"FAKE_STALL": "1"}, "pump_stalled", "pump_stalled", 2),
            ({"FAKE_NO_ENGINE_AFTER": "1"}, "pump_stalled", "pump_stalled", 1),
            ({"FAKE_CLOCK": "not_ready"}, "clock_not_ready", "clock_not_ready", 1),
            ({"FAKE_CLOCK": "holdover"}, "holdover", "in_holdover", 1),
            ({"FAKE_GROW": "holdovers"}, "holdover", "holdovers", 1),
            ({"FAKE_GROW": "re_anchors"}, "re_anchor", "re_anchors", 1),
            ({"FAKE_CLOCK": "epoch"}, "re_anchor", "re_anchors", 1),
            ({"FAKE_GROW": "rx_late"}, "rx_late", "rx_late", 1),
            ({"FAKE_GROW": "tx_holes"}, "tx_holes", "tx_holes", 1),
            ({"FAKE_GROW": "rx_holes"}, "rx_holes", "rx_holes", 1),
            ({"FAKE_RX_OFF": "1"}, "rx_off", "rx_off", 1),
            ({"FAKE_UNIT_RESTART": "iem-engine.service"}, "restarted", "unit_restarts", 1),
            ({"FAKE_UNIT_RESTART": "iem-server.service"}, "restarted", "unit_restarts", 1),
            ({"FAKE_UNIT_ENTER": "iem-server.service"}, "restarted", "unit_restarts", 1),
            ({"FAKE_ENGINE_BACK": "1"}, "restarted", "counts_back", 1),
            ({"FAKE_SOAK_DEGRADED": "1"}, "health", "not_healthy", 1),
        )
        for env, word, key, value in cases:
            with self.subTest(env=env):
                self.again()
                code, docs, err = self.gate_run(**env)
                self.assertEqual(code, 1, err)
                doc = self.line(docs)
                self.assertEqual((doc["failed"], doc[key], doc["pass"], doc["conclusion"]), ([word], value, 0,
                                                                                             "failure"))
                self.assertEqual(self.posts()[0][4:], ["-f", "state=failure", "-f", "context=soak/gen3", "-f",
                                                       f"description=the Gen 3 box's soak failed: {word}"])
                self.assertTrue(set(doc["failed"]) <= set(soak.FAILURES))
                self.assertEqual(self.soak_state()["unit"], None)   # the unit ended all the same
                self.assert_private(err + json.dumps(docs))

    def test_a_degraded_health_names_only_known_reasons(self) -> None:
        code, docs, err = self.gate_run(FAKE_SOAK_DEGRADED="1")
        self.assertEqual(code, 1, err)
        self.assertEqual(self.line(docs)["health_reasons"], ["holdover"])

    def test_an_engine_restart_by_itself_counts_in_the_hil_gate_s_budget(self) -> None:
        code, docs, err = self.gate_run(FAKE_UNIT_RESTART="iem-engine.service")
        self.assertEqual(code, 1, err)
        self.assertEqual(self.ledger(), [gt.NOW])

    def test_the_harness_checks_judge_the_client_s_summary(self) -> None:
        cases = (
            ({"FAKE_SUMMARY": "incomplete", "FAKE_CLIENT_EXIT": "1"}, ["client", "harness"],
             ["harness incomplete (connection-lost)", "harness ran 61 s of 7200 s"], 1, "exit-code"),
            ({"FAKE_NO_SUMMARY": "1"}, ["harness"], ["no harness summary"], 0, "success"),
        )
        for env, failed, problems, exit_code, result in cases:
            with self.subTest(env=env):
                self.again()
                code, docs, err = self.gate_run(**env)
                self.assertEqual(code, 1, err)
                doc = self.line(docs)
                self.assertEqual((doc["failed"], doc["harness"]["problems"], doc["client_exit"],
                                  doc["client_result"]), (failed, problems, exit_code, result))
                self.assertEqual(self.soak_state()["unit"], None)
        # A failed unit is cleared with reset-failed, not stopped.
        self.again()
        code, docs, err = self.gate_run(FAKE_CLIENT_EXIT="1")
        self.assertEqual(self.calls()[-4:], [CAT_SUMMARY, RESET_UNIT, RM_ENV, RM_SUMMARY])

    def test_listen_gaps_are_counted_not_judged(self) -> None:
        # Owner on #72, 2026-10-29: the browser listen is low priority; its gaps never fail the gate.
        code, docs, err = self.gate_run(FAKE_GAPS="3")
        self.assertEqual(code, 0, err)
        doc = self.line(docs)
        self.assertEqual((doc["failed"], doc["harness"]["problems"], doc["harness"]["gaps"]), ([], [], 3))

    def test_reads_that_fail_or_leave_a_hole_fail_polls(self) -> None:
        code, docs, err = self.gate_run(FAKE_STATUS_FAILS="1")
        self.assertEqual(code, 1, err)
        doc = self.line(docs)
        self.assertEqual((doc["failed"], doc["reads"], doc["failed_reads"]), (["polls"], 1, 2))
        self.again()
        with mock.patch.object(soak, "POLL_HOLE_S", 0.0):
            code, docs, err = self.gate_run()
        self.assertEqual((code, self.line(docs)["failed"]), (1, ["polls"]), err)

    def test_a_client_that_never_ends_is_stopped_and_fails_not_finished(self) -> None:
        with mock.patch.object(soak, "wait_limit", lambda seconds: 0.3):
            code, docs, err = self.gate_run(FAKE_NEVER_ENDS="1")
        self.assertEqual(code, 1, err)
        doc = self.line(docs)
        self.assertEqual((doc["failed"], doc["client_exit"], doc["client_result"]),
                         (["not_finished", "harness"], None, None))
        calls = self.calls()
        self.assertEqual(calls[-4:], [STOP_UNIT, CAT_SUMMARY, RM_ENV, RM_SUMMARY])
        self.assertEqual(self.soak_state()["unit"], None)

    def test_a_refused_status_post_fails_the_command_after_its_verdict(self) -> None:
        def gh(args, timeout=ip.GH_S):
            raise ip.StepError("gh api failed (exit 1): HTTP 403")
        self.patch(gh=gh)
        code, docs, err = self.gate_run()
        self.assertEqual((code, docs[0]["gen3_gate"]["soak"]["pass"]), (1, 1))
        self.assertIn("WARNING: gen3-gate: the status soak/gen3 was not posted", err)


class FlagTests(SoakBase):
    def test_the_flag_stops_the_client_reports_cancelled_and_runs_the_event_path(self) -> None:
        code, docs, err = self.gate_run(FAKE_FLAG_ON="soak")
        self.assertEqual(code, ip.PREEMPTED, err)
        doc = self.line(docs)
        self.assertEqual((doc["conclusion"], doc["pass"], doc["failed"], doc["harness"]),
                         ("cancelled", 0, [], {"problems": []}))
        self.assertEqual(docs[1], {"event": "ide event (flag file)", "action": "iempc event"})
        # No poll after the flag: the unit read and stopped (never force-ended), its file and summary gone.
        calls = self.calls()
        self.assertEqual(calls[calls.index(client(7200)) + 1:], [gt.CTL, UNIT_SHOW, STOP_UNIT, RM_ENV, RM_SUMMARY])
        self.assertEqual((self.soak_state()["unit"], self.env_file_there()), (None, False))
        self.assertEqual(self.posts(), [])
        self.assertIn(["event", "--signal"], [args for _, args, _ in self.pc.calls])
        self.assert_private(err + json.dumps(docs))

    def test_a_flag_before_the_start_changes_nothing_on_the_box(self) -> None:
        original = soak.baseline

        def then_flag(box, w):
            original(box, w)
            self.flag()
        with mock.patch.object(soak, "baseline", then_flag):
            code, docs, err = self.gate_run()
        self.assertEqual(code, ip.PREEMPTED, err)
        self.assertEqual(self.calls(), BEFORE)
        self.assertEqual(self.line(docs)["conclusion"], "cancelled")
        self.assertEqual(self.posts(), [])

    def test_a_start_that_fails_ends_the_run_and_fails_the_command(self) -> None:
        code, docs, err = self.gate_run(FAKE_RUN_FAILS="1")
        self.assertEqual((code, docs), (1, []), err)
        self.assertIn("Access denied", err)
        calls = self.calls()
        self.assertEqual(calls[calls.index(client(7200)) + 1:], [UNIT_SHOW, RM_ENV, RM_SUMMARY])
        self.assertEqual((self.env_file_there(), self.posts()), (False, []))


class LeftTests(SoakBase):
    def test_a_left_hil_job_is_ended_first_then_the_soak_runs_on_the_engine_as_it_serves(self) -> None:
        self.left_job()
        code, docs, err = self.gate_run()
        self.assertEqual(code, 0, err)
        self.assertEqual(self.line(docs)["left_job"], 1)
        end = [gt.STATUS, gt.RM, gt.RELOAD, gt.RESET, gt.RESTART, gt.HEALTH, gt.CTL, gt.STATUS]
        self.assertEqual(self.calls()[:1 + len(gt.PREFLIGHT) + len(end) + len(BEFORE) - 1],
                         [gt.LEFT, *gt.PREFLIGHT, *end, *BEFORE[1:]])
        self.assertIn("a HIL job a dead run left is ended first", err)
        # The left job's end is the one engine start; the soak itself starts none.
        self.assertEqual((self.state()["starts"], self.ledger()), (1, [gt.NOW]))
        self.assertFalse(self.dropin_there())

    def test_a_soak_client_a_dead_run_left_is_ended_first(self) -> None:
        cases = (({"active": "active", "sub": "running", "result": "success", "status": 0}, STOP_UNIT),
                 ({"active": "active", "sub": "exited", "result": "success", "status": 0}, STOP_UNIT),
                 ({"active": "failed", "sub": "failed", "result": "exit-code", "status": 1}, RESET_UNIT))
        for unit, ended_by in cases:
            with self.subTest(unit=unit):
                self.again()
                self.set_soak(unit)
                code, docs, err = self.gate_run()
                self.assertEqual(code, 0, err)
                self.assertEqual(self.line(docs)["left_soak"], 1)
                calls = self.calls()
                at = calls.index(UNIT_SHOW)
                self.assertEqual(calls[at:at + 3], [UNIT_SHOW, ended_by, UNIT_SHOW])
                self.assertIn("a soak client a dead run left is ended first", err)

    def test_a_left_soak_unit_that_stays_refuses_the_run(self) -> None:
        self.set_soak({"active": "active", "sub": "running", "result": "success", "status": 0})
        code, docs, err = self.gate_run(FAKE_STOP_KEEPS="1")
        self.assertEqual((code, docs, self.posts()), (1, [], []), err)
        self.assertIn("no soak gate: the soak's unit a dead run left is still there", err)
        self.assertFalse(set(self.calls()) & {STAGE_ENV, INSTALL_ENV, client(7200)})


class RefusalTests(SoakBase):
    def test_every_refusal_before_any_call(self) -> None:
        text = ip.env_path().read_text(encoding="utf-8")
        for env_text, words in (
                (text.replace(f"GEN3_HOST={gt.GEN3}\n", ""), "no GEN3_HOST in the private env"),
                (text.replace(f"SOAK_MEMBER={MEMBER}\n", ""), "SOAK_MEMBER of the private env is not an id"),
                (text.replace(f"SOAK_MEMBER={MEMBER}\n", "SOAK_MEMBER=zyx member\n"),
                 "SOAK_MEMBER of the private env is not an id")):
            with self.subTest(words=words):
                self.again()
                ip.env_path().write_text(env_text, encoding="utf-8")
                code, docs, err = self.gate_run()
                self.assertEqual((code, docs, self.calls()), (1, [], []), err)
                self.assertIn(words, err)
                self.assertNotIn("zyx member", err)
        ip.env_path().write_text(text, encoding="utf-8")
        # While 2.x serves, and in an event: iempc's Spec, before any call.
        self.again()
        (ip.STATE_DIR / "gen3-side.json").write_text(json.dumps({"side": 2}), encoding="utf-8")
        code, docs, err = self.gate_run()
        self.assertEqual((code, docs, self.calls()), (1, [], []), err)
        self.assertIn("'gen3-gate' runs only while 3.x serves", err)

    def test_every_refusal_before_any_change(self) -> None:
        cases = (({"FAKE_UNHEALTHY": "1"}, "no soak gate: the box is not healthy"),
                 ({"FAKE_ENGINE_VERSION": "iem-engine 2.0.0-dev.50+local"},
                  "no soak gate: the box's iem-engine --version names no CI build"),
                 ({"FAKE_SERVER_HASH": "fffffff"},
                  "no soak gate: the box's server does not name the engine's build in /api/version"),
                 ({"FAKE_SERVER_HASH": ""},
                  "no soak gate: the box's server does not name the engine's build in /api/version"),
                 ({"FAKE_BASELINE_FAILS": "1"}, "no soak gate: the Gen 3 device's status could not be read"),
                 ({"FAKE_NO_ENGINE": "1"}, "no soak gate: the Gen 3 device's status shows no engine"))
        for env, words in cases:
            with self.subTest(words=words):
                self.again()
                err = self.refused(words, **env)
                self.assertNotIn(MEMBER, err)


class SshTests(SoakBase):
    def test_an_unreachable_box_at_the_build_read_is_named_by_fixed_words(self) -> None:
        # The review of PR #87: ssh's own failure (exit 255) names the host in its stderr: never quoted (P6).
        err = self.refused("no soak gate: ssh to GEN3_HOST failed (exit 255) (nothing was changed)",
                           FAKE_SSH_DOWN="/opt/iemmixer/bin/iem-engine --version")
        self.assertNotIn("gen3.test", err)
        self.assertNotIn("connect to host", err)


class SudoTests(SoakBase):
    def test_every_sudo_line_the_soak_sends_is_in_the_template(self) -> None:
        sent: list[str] = []

        def take() -> None:
            # The box's shell takes the quotes off before sudo reads the words.
            sent.extend(" ".join(shlex.split(c)) for c in self.calls())
            self.again()
        for minutes in ("10", "30", "60", "120"):
            code, _, err = self.run_main(*self.ARGS, "--minutes", minutes)
            self.assertEqual(code, 0, err)
            take()
        self.left_job()
        code, _, err = self.gate_run()
        self.assertEqual(code, 0, err)
        take()
        self.set_soak({"active": "failed", "sub": "failed", "result": "exit-code", "status": 1})
        code, _, err = self.gate_run(FAKE_CLIENT_EXIT="1")
        self.assertEqual(code, 1, err)
        take()
        code, _, err = self.gate_run(FAKE_FLAG_ON="soak")
        self.assertEqual(code, ip.PREEMPTED, err)
        take()
        allowed = {(runas, text) for runas, command in template_lines() for text in expand(command)}
        lines = sudo_lines(sent)
        for runas, command, listed in lines:
            self.assertIn((runas, command), allowed, command)
            self.assertFalse(listed, command)
        seen = {(runas, command) for runas, command, _listed in lines}
        for want in (INSTALL_ENV, RM_ENV, STOP_UNIT, RESET_UNIT,
                     *(" ".join(shlex.split(client(m * 60))) for m in (10, 30, 60, 120))):
            self.assertIn(("root", want[len("sudo -n "):]), seen)
        for want in (RM_SUMMARY, CAT_SUMMARY):
            self.assertIn(("iemmixer", want[len("sudo -n -u iemmixer "):]), seen)
        # The template's start lines name the build as systemd's variable, never a value.
        starts = [command for runas, command in template_lines() if "iem-soakclient --direct" in command]
        self.assertEqual(len(starts), 4)
        self.assertTrue(all("--expect-build $IEM_SOAK_BUILD " in c and "--member {member} " in c for c in starts))


class PureTests(unittest.TestCase):
    def test_the_client_s_start_is_fixed_words_for_each_length(self) -> None:
        self.assertEqual(soak.client_words(10, "member2"), [
            "systemd-run", "--quiet", "--unit=iem-soak", "--remain-after-exit", "--uid=iemmixer", "--gid=iemmixer",
            "--property=EnvironmentFile=/run/iem-soak.env", "--property=RuntimeMaxSec=900",
            "--property=TimeoutStopSec=10", "--property=SendSIGKILL=no", "/opt/iemmixer/bin/iem-soakclient",
            "--direct", "--member", "member2", "--seconds", "600", "--out", SUMMARY, "--expect-build",
            "$IEM_SOAK_BUILD", "--jwt-secret-file", "/var/lib/iemmixer/site/secrets/jwt_secret"])
        self.assertEqual(soak.remote(["a", "$IEM_SOAK_BUILD", "b"]), ["a", "'$IEM_SOAK_BUILD'", "b"])

    def test_a_status_is_read_into_the_numbers_the_soak_judges(self) -> None:
        status = {"clock": {"ready": 1, "epoch": 0, "polls": 9, "re_anchors": 2, "holdovers": 3, "holdover": 0,
                            "freq_ppb": -5},
                  "rx": [{"flow": 1, "late": 4, "lateness": 15}, {"flow": 3, "late": 0, "lateness": 31}],
                  "tx": [{"flow": 2, "holes": 6}], "channels": [{"rx": 101, "holes": 2}, {"rx": 102, "holes": 0}],
                  "engine": {"media": 1, "blocks": 30, "late": 5, "missed": 7, "overruns": 11, "rebootstraps": 1,
                             "max_process_us": 210, "offset": 176}}
        self.assertEqual(soak.sample(status), {
            "ready": 1, "epoch": 0, "re_anchors": 2, "holdovers": 3, "holdover": 0,
            "engine": {"media": 1, "blocks": 30, "late": 5, "missed": 7, "overruns": 11, "rebootstraps": 1,
                       "max_process_us": 210},
            "rx_late": {"1": 4, "3": 0}, "tx_holes": {"2": 6}, "rx_holes": {"101": 2, "102": 0}, "rx_off": 0, "lateness": 31})
        self.assertIsNone(soak.sample({**status, "engine": None})["engine"])
        for bad in ({}, {**status, "clock": {"ready": 1}}, {**status, "rx": "x"},
                    {**status, "rx": [{"flow": 1, "late": -1, "lateness": 0}]}):
            self.assertIsNone(soak.sample(bad), bad)

    def test_growth_counts_each_rise_and_a_count_that_went_back_from_zero(self) -> None:
        self.assertEqual(soak.growth([{"a": 1}, {"a": 3}, {"a": 2, "b": 1}, {"a": 2, "b": 1}]), (5, 1))
        self.assertEqual(soak.growth([{"a": 4}]), (0, 0))
        self.assertEqual(soak.growth([]), (0, 0))

    def test_a_hole_is_the_longest_wait_between_reads_and_the_run_s_edges(self) -> None:
        self.assertEqual(soak.hole([1.0, 2.0, 70.0], 0.5, 71.0), 68.0)
        self.assertEqual(soak.hole([], 3.0, 10.0), 7.0)

    def test_the_units_are_read_from_systemctl_show(self) -> None:
        u = soak.unit_fields("Result=exit-code\nExecMainStatus=1\nLoadState=loaded\nActiveState=failed\n"
                             "SubState=failed\n")
        self.assertEqual((u.loaded, u.running, u.result, u.status), (True, False, "exit-code", 1))
        u = soak.unit_fields("LoadState=loaded\nActiveState=active\nSubState=running\nResult=success\n"
                             "ExecMainStatus=0\n")
        self.assertEqual((u.loaded, u.running), (True, True))
        u = soak.unit_fields("LoadState=loaded\nActiveState=active\nSubState=exited\nResult=success\n"
                             "ExecMainStatus=0\n")
        self.assertEqual((u.loaded, u.running, u.status), (True, False, 0))
        self.assertEqual(soak.unit_fields("LoadState=not-found\nActiveState=inactive\n").loaded, False)
        text = ("Id=iem-server.service\nNRestarts=2\nActiveEnterTimestampMonotonic=77\n\n"
                "ActiveEnterTimestampMonotonic=55\nNRestarts=0\nId=iem-engine.service\n")
        self.assertEqual(soak.stack_units(text), {"iem-engine.service": (0, "55"), "iem-server.service": (2, "77")})
        self.assertEqual(soak.stack_units("Id=iem-engine.service\nNRestarts=x\n"), {})


if __name__ == "__main__":
    unittest.main()
