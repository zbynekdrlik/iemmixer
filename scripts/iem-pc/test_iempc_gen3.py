"""Tests for scripts/iem-pc/iempc_gen3.py: `iempc gen3-loop` (#72 phase B,
plan Task 8 Steps 2 and 3) and `iempc gen3-mix` (#72 phase C, plan Task 8).
FakePc stands in for the PC (the guard's replies as an engine with the D5(b)
probe would give them: `Card`, `MixCard`), a stand-in `ssh` and `scp` first on
PATH for the Gen 3 box (its status reads from a scripted list, the source
settings, the event's stop, the engine's version, the tokens' mint, copy and
removal in a folder that plays the box user's home) and a stand-in `npx` for
the browser step (it records what it was given and opens or closes the box's
test input). Every value is synthetic (P6): the spare outputs are 94 and 95,
the test site's; the ids and channels are the test site's."""
from __future__ import annotations

import json
import os
import stat
import sys
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import iempc_gen3 as gen3  # noqa: E402
from iempc_test_support import OK, SHA, Base, ip  # noqa: E402
from test_iempc_live import RunningGuardCases  # noqa: E402

GEN3 = "tester@gen3.test"
SLOT, OTHER = 94, 95   # the test site's spare HIL outputs
JOB = 4242
BASE_TRIP = 129   # the card's own part of the round trip, samples
STATUS_CMD = f"{GEN3} /opt/iemmixer/bin/iem-dante-dev ctl status"

# The Gen 3 box: `ssh` answers `ctl status` from FAKE_GEN3/statuses.json (one
# entry per read, the last repeated; null: no socket), a source setting with
# FAKE_SOURCE_ANSWER (default ok) on stderr as the box's `1>&2` sends it, and
# the event's stop; every call is logged as one JSON line.
FAKE_SSH = r'''
import json, os, sys
box = os.environ["FAKE_GEN3"]
args = sys.argv[1:]
with open(os.path.join(box, "calls.log"), "a") as f:
    f.write(json.dumps(args) + "\n")
while args and args[0] == "-o":
    args = args[2:]
words = args[1:]
if words[:2] == ["/opt/iemmixer/bin/iem-dante-dev", "ctl"] and words[2:] == ["status"]:
    counter = os.path.join(box, "reads")
    n = int(open(counter).read()) if os.path.exists(counter) else 0
    open(counter, "w").write(str(n + 1))
    plan = json.load(open(os.path.join(box, "statuses.json")))
    doc = plan[min(n, len(plan) - 1)]
    if doc is None:
        sys.stderr.write("iem-dante-dev ctl: the control socket: Connection refused\n")
        sys.exit(1)
    print(json.dumps(doc))
elif words[:3] == ["/opt/iemmixer/bin/iem-dante-dev", "ctl", "source"] and words[-1] == "1>&2":
    answer = os.environ.get("FAKE_SOURCE_ANSWER", "ok")
    sys.stderr.write(answer + "\n")
    sys.exit(0 if answer == "ok" else 1)
elif words in (["sudo", "-n", "systemctl", "stop", "iem-dante"],
               ["sudo", "-n", "systemctl", "stop", "iem-server", "iem-engine", "iem-dante"]):
    sys.exit(0)
elif words == ["/opt/iemmixer/bin/iem-engine", "--version"]:
    print(os.environ["FAKE_ENGINE_VERSION"])
elif words[:2] == ["umask", "077"]:
    # The mint: each `--out` gets its token (FAKE_TOKENS), a new owner-only
    # file under the box's home, as `iem-soakclient token` writes it.
    home = os.path.join(box, "home")
    os.makedirs(os.path.join(home, ".cache", "iem-gen3-mix"), exist_ok=True)
    tokens = json.loads(os.environ["FAKE_TOKENS"])
    for i, word in enumerate(words):
        if word == "--out":
            name = os.path.basename(words[i + 1]).split(".")[0]
            if os.environ.get("FAKE_MINT_FAILS") == name:
                sys.stderr.write("secret-unreadable\n")
                sys.exit(1)
            fd = os.open(os.path.join(home, words[i + 1]), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            os.write(fd, (tokens[name] + "\n").encode())
            os.close(fd)
            print("token-written")
elif words[:2] == ["rm", "-f"]:
    for path in words[2:]:
        if os.path.exists(os.path.join(box, "home", path)):
            os.remove(os.path.join(box, "home", path))
else:
    sys.stderr.write("unexpected command: " + " ".join(words) + "\n")
    sys.exit(97)
'''

# The box's scp: copies `<host>:<path>` sources out of the box's home into the
# last argument (a folder); FAKE_SCP_FAILS makes it fail. Logged with ssh's calls.
FAKE_SCP = r'''
import json, os, shutil, sys
box = os.environ["FAKE_GEN3"]
args = sys.argv[1:]
with open(os.path.join(box, "calls.log"), "a") as f:
    f.write(json.dumps(["scp", *args]) + "\n")
if os.environ.get("FAKE_SCP_FAILS"):
    sys.stderr.write("scp: Connection closed\n")
    sys.exit(1)
rest = []
it = iter(args)
for a in it:
    if a == "-o":
        next(it)
    elif a != "-q":
        rest.append(a)
for src in rest[:-1]:
    shutil.copy(os.path.join(box, "home", src.split(":", 1)[1]), rest[-1])
'''

# The browser step: records its arguments, folder, LIVE_* values, the token
# file it was handed (its mode and contents) and any E2E PIN variable in
# FAKE_GEN3/npx.log; "opens" opens the box's test input (the file `open`),
# "closes" closes it. FAKE_SPEC_FAILS=<step> fails that step, FAKE_SPEC_FLAG=<step>
# writes the EVENT-NOW flag (FAKE_FLAG) during it.
FAKE_NPX = r'''
import json, os, stat, sys
box = os.environ["FAKE_GEN3"]
args = sys.argv[1:]
title = args[args.index("--grep") + 1]
step = "opens" if " opens " in title else "closes" if " closes " in title else "other"
path = os.environ.get("LIVE_TOKENS", "")
mode, folder_mode, tokens = None, None, None
if os.path.isfile(path):
    mode, tokens = stat.S_IMODE(os.stat(path).st_mode), json.load(open(path))
    folder_mode = stat.S_IMODE(os.stat(os.path.dirname(path)).st_mode)
entry = {"args": args, "cwd": os.getcwd(), "step": step, "mode": mode, "folder_mode": folder_mode, "tokens": tokens,
         "path": path,
         "live": {k: v for k, v in os.environ.items() if k.startswith("LIVE_") and k != "LIVE_TOKENS"},
         "pins": sorted(k for k in os.environ if k.startswith("E2E_") and k.endswith("_PIN"))}
with open(os.path.join(box, "npx.log"), "a") as f:
    f.write(json.dumps(entry) + "\n")
if os.environ.get("FAKE_SPEC_FLAG") == step:
    open(os.environ["FAKE_FLAG"], "w").write("now\n")
if os.environ.get("FAKE_SPEC_FAILS") == step:
    print("  1) [chromium] > the test failed: the fader did not rest at 0 dB")
    print("  1 failed")
    sys.exit(1)
if step == "opens":
    open(os.path.join(box, "open"), "w").write("open\n")
elif step == "closes" and os.path.exists(os.path.join(box, "open")):
    os.remove(os.path.join(box, "open"))
print("  1 passed")
'''


def device(rx_status: int = gen3.RECEIVING, ready: int = 1, live: int = 1, holes: int = 0, tx_holes: int = 0,
           late: int = 0, ring_late: int = 0, re_anchors: int = 0, epoch: int = 0, polls: int = 100) -> dict:
    """The Gen 3 device's status as `ctl status` prints it (ctl.rs)."""
    return {
        "clock": {"ready": ready, "epoch": epoch, "polls": polls, "re_anchors": re_anchors, "holdovers": 0,
                  "holdover": 0, "freq_ppb": 3},
        "requests": [{"port": 4455, "peer": 1, "op": 256, "n": 1}], "sent": [{"peer": 1, "op": 256, "n": 1}],
        "uncounted": 0,
        "tx": [{"flow": 1, "live": live, "fpp": 16, "channels": 1, "packets": 9000, "send_errors": 0,
                "keepalives": 12, "rebootstraps": 0, "holes": tx_holes}],
        "rx": [{"flow": 1, "granted": 1, "fpp": 16, "channels": 1, "latency": 96, "packets": 9000, "refused": 0,
                "stray": 0, "out_of_order": 0, "late": late, "unplaced": 0, "lateness": 20, "keepalives": 12,
                "keepalive_errors": 0, "recv_errors": 0}],
        "rx_poll_errors": 0,
        "channels": [{"rx": 1, "status": rx_status, "written": 144000, "holes": holes, "late": ring_late,
                      "duplicate": 0, "early": 0}],
        "sources": [], "checks": [],
    }


class Card:
    """The PC's side of the loop: the guard's replies as an engine with the
    D5(b) probe gives them. `activate` starts the engine again (one start
    more) with its HIL outputs. After a test signal the statuses read, one by
    one (PHASES): first a stale one (the engine's Status from before the
    signal: silent, the last burst's round trip), then the burst on both spare
    outputs, then on the other one alone, then silence. The round trip is
    `trip` of the echo offset the Gen 3 box was last set to; it shows from the
    first sounding status on. A signal sent while the last burst still sounds
    is counted in `overlaps`."""
    PHASES = ("stale", "both", "both", "other", "other")

    def __init__(self, gen3_log: Path) -> None:
        self.gen3_log = gen3_log
        self.spawns = 3
        self.activated = False
        self.restarts = True
        self.signals = 0
        self.phases: list[str] = []
        self.overlaps = 0
        self.last_trip = 0
        self.this_trip = 0
        self.slot_hears = True
        self.trip = lambda offset, burst: BASE_TRIP + offset

    def offset(self) -> int:
        sets = [a for a in (json.loads(line) for line in self.gen3_log.read_text(encoding="utf-8").splitlines())
                if "source" in a]
        return int(sets[-1][-2])

    def activate(self) -> tuple[int, str]:
        if self.restarts:
            self.spawns += 1
            self.activated = True
        return 0, json.dumps({"ok": True, "detail": f"activated {SHA}; the engine and the server run it"})

    def signal(self) -> tuple[int, str]:
        if any(p != "stale" for p in self.phases):
            self.overlaps += 1
        self.signals += 1
        self.last_trip, self.this_trip = self.this_trip, self.trip(self.offset(), self.signals)
        self.phases = list(self.PHASES)
        return 0, json.dumps({"ok": True, "detail": "test signal on mic1"})

    def status(self) -> tuple[int, str]:
        engine = {"build": SHA, "pid": 77, "parked": False, "faulted": False, "held": False, "spawns": self.spawns,
                  "hil": [], "loopback_samples": 0, "loopback_ms": 0.0}
        if self.activated:
            phase = self.phases.pop(0) if self.phases else "silent"
            slot = 0.03 if phase == "both" and self.slot_hears else 0.0
            other = 0.03 if phase in ("both", "other") else 0.0
            engine["hil"] = [{"tx": SLOT, "peak": slot}, {"tx": OTHER, "peak": other}]
            engine["loopback_samples"] = self.last_trip if phase == "stale" else self.this_trip
        return 0, json.dumps({"ok": True, "mode": "dev", "switching": None, "detail": f"mode dev; bundle {SHA}",
                              "alarms": [], "engine": engine})


class LoopBase(Base):
    ARGS = ("gen3-loop", "--offsets", "192,229", "--bursts", "5", "--slot", str(SLOT))
    SIGNAL = ("test-signal", "mic1", "-30", "2")

    def setUp(self) -> None:
        super().setUp()
        envfile = self.tmp / "iem-pc-gen3.env"
        envfile.write_text("PC_SSH=tester@pc.test\nPC_ROOT=X:\\root\nPC_ROOT_SCP=/X:/root\nPC_BIN=X:\\root\\bin\n"
                           f"GEN3_HOST={GEN3}\n", encoding="utf-8")
        self.patch(env_path=lambda: envfile, POLL_S=0.01)
        self.box = self.tmp / "gen3"
        self.box.mkdir()
        (self.box / "calls.log").write_text("", encoding="utf-8")
        d = self.tmp / "bin"
        d.mkdir(exist_ok=True)
        p = d / "ssh"
        p.write_text(f"#!{sys.executable}\n{FAKE_SSH}", encoding="utf-8")
        p.chmod(0o755)
        patcher = mock.patch.dict(os.environ, {"PATH": f"{d}{os.pathsep}{os.environ.get('PATH', '')}",
                                               "FAKE_GEN3": str(self.box)})
        patcher.start()
        self.addCleanup(patcher.stop)
        self.plan([device()])
        self.card = Card(self.box / "calls.log")
        self.pc.replies[("status",)] = self.card.status
        self.pc.replies[("activate", SHA)] = self.card.activate
        self.pc.replies[self.SIGNAL] = self.card.signal
        job = mock.patch.object(gen3, "job_run", return_value=JOB)
        job.start()
        self.addCleanup(job.stop)

    def plan(self, statuses: list) -> None:
        (self.box / "statuses.json").write_text(json.dumps(statuses), encoding="utf-8")

    def gen3_calls(self) -> list[str]:
        out = []
        for line in (self.box / "calls.log").read_text(encoding="utf-8").splitlines():
            args = json.loads(line)
            while args and args[0] == "-o":
                args = args[2:]
            out.append(" ".join(args))
        return out

    def sources(self) -> list[str]:
        return [c for c in self.gen3_calls() if " ctl source " in c]

    def iemmode_words(self) -> list[list[str]]:
        return [args for _, args, _ in self.pc.calls]

    def loop(self, *extra: str) -> tuple[int, list[dict], str]:
        return self.run_main(*self.ARGS, *extra)

    def refused(self, words: str, job_asked: bool = False) -> str:
        """Refused before anything changed: exit 1, no output, no source, no
        HIL job (asked for only when `job_asked`), never ended."""
        code, docs, err = self.loop()
        self.assertEqual((code, docs, self.sources()), (1, [], []), err)
        self.assertEqual(["job-begin", str(JOB)] in self.iemmode_words(), job_asked, err)
        self.assertNotIn(["job-end", str(JOB)], self.iemmode_words(), err)
        self.assertIn(words, err)
        self.assertNotIn("Traceback", err)
        return err


class LoopTests(LoopBase):
    def test_the_loop_passes_with_the_exact_shape(self) -> None:
        code, docs, err = self.loop()
        self.assertEqual(code, 0, err)
        self.assertEqual(docs, [{"gen3_loop": {
            "pass": 1, "offsets": [192, 229], "bursts": 5, "round_trips": [[321] * 5, [358] * 5], "not_heard": 0,
            "difference": 37, "expected": 37, "restarted": 0, "holes": 0, "late": 0, "ring_late": 0,
            "re_anchors": 0, "epoch": 0, "failed": [], "job": JOB, "job_end": 0}}])
        burst = [list(self.SIGNAL), *[["status"]] * 6]   # stale, sounding four times, silent
        self.assertEqual(self.iemmode_words(), [["status"], ["job-begin", str(JOB)], ["activate", SHA], ["status"],
                                                *burst * 10, ["job-end", str(JOB)]])
        modes = {tuple(args[:1]): event for _, args, event in self.pc.calls}
        self.assertEqual((modes[("job-begin",)], modes[("activate",)], modes[("test-signal",)], modes[("job-end",)],
                          modes[("status",)]), ("finish", "finish", "abandon", "finish", "abandon"))
        echo = f"{GEN3} /opt/iemmixer/bin/iem-dante-dev ctl source 1 echo 1"
        self.assertEqual(self.gen3_calls(), [STATUS_CMD, STATUS_CMD, f"{echo} 192 1>&2", f"{echo} 229 1>&2",
                                             STATUS_CMD])
        self.assertNotIn(str(SLOT), json.dumps(docs))   # the card's channel is never printed (P6)
        self.assertNotIn(GEN3, json.dumps(docs) + err)
        self.assertEqual((self.card.signals, self.card.overlaps), (10, 0))   # each burst ended before the next

    def test_the_offsets_may_go_down(self) -> None:
        code, docs, err = self.loop("--offsets", "229,192", "--bursts", "2")
        self.assertEqual(code, 0, err)
        doc = docs[-1]["gen3_loop"]
        self.assertEqual((doc["round_trips"], doc["difference"], doc["expected"]), ([[358] * 2, [321] * 2], -37, -37))

    def test_a_loop_of_another_shape_fails_and_still_ends_the_job(self) -> None:
        self.card.trip = lambda offset, burst: BASE_TRIP + offset + (1 if burst == 3 else 0)   # one burst a sample late
        code, docs, err = self.loop()
        self.assertEqual(code, 1, err)
        doc = docs[-1]["gen3_loop"]
        self.assertEqual((doc["pass"], doc["failed"], doc["difference"], doc["job_end"]), (0, ["unequal"], None, 0))
        self.assertEqual(doc["round_trips"][0], [321, 321, 322, 321, 321])
        self.assertEqual(self.iemmode_words()[-1], ["job-end", str(JOB)])

    def test_a_round_trip_that_does_not_move_by_the_offsets_difference_fails(self) -> None:
        self.card.trip = lambda offset, burst: BASE_TRIP   # a return looped elsewhere: our echo never comes first
        code, docs, err = self.loop()
        self.assertEqual(code, 1, err)
        doc = docs[-1]["gen3_loop"]
        self.assertEqual((doc["failed"], doc["difference"], doc["expected"]), (["difference"], 0, 37))

    def test_holes_late_packets_or_a_re_anchor_in_the_window_fail_it(self) -> None:
        for after, word in ((device(holes=5), "holes"), (device(tx_holes=32), "holes"), (device(late=1), "late"),
                            (device(ring_late=16), "ring_late"), (device(re_anchors=1, epoch=1), "re_anchor"),
                            (device(polls=7), "restarted"), (device(ready=0), "not_ready")):
            self.pc.calls.clear()
            (self.box / "reads").unlink(missing_ok=True)
            self.plan([device(), device(), after])
            self.card.signals = 0
            code, docs, err = self.loop("--bursts", "2")
            self.assertEqual(code, 1, (word, err))
            self.assertEqual(docs[-1]["gen3_loop"]["failed"], [word], word)

    def test_a_job_begin_refusal_ends_with_no_source_change(self) -> None:
        self.pc.replies[("job-begin", str(JOB))] = (1, json.dumps({"ok": False, "detail": "HIL job 77 has not ended"}))
        self.refused("iemmode job-begin refused (exit 1): HIL job 77 has not ended (nothing was changed: no HIL job, "
                     "no source)", job_asked=True)
        self.assertEqual(self.iemmode_words(), [["status"], ["job-begin", str(JOB)]])
        self.assertEqual(self.gen3_calls(), [STATUS_CMD])

    def test_a_refused_test_signal_still_ends_the_job(self) -> None:
        self.pc.replies[self.SIGNAL] = (1, json.dumps({"ok": False, "detail": "the engine refused the signal"}))
        code, docs, err = self.loop()
        self.assertEqual((code, docs), (1, []), err)
        self.assertIn("gen3-loop: iemmode test-signal refused (exit 1): the engine refused the signal", err)
        self.assertEqual(self.iemmode_words()[-1], ["job-end", str(JOB)])

    def test_a_refused_source_setting_is_named_and_ends_the_job(self) -> None:
        with mock.patch.dict(os.environ, {"FAKE_SOURCE_ANSWER": "error: no such RX channel"}):
            code, docs, err = self.loop()
        self.assertEqual((code, docs), (1, []), err)
        self.assertIn("ssh failed (exit 1): error: no such RX channel", err)
        self.assertNotIn(self.SIGNAL[0], [w[0] for w in self.iemmode_words()])
        self.assertEqual(self.iemmode_words()[-1], ["job-end", str(JOB)])

    def test_the_slot_must_be_one_of_the_engines_hil_outputs(self) -> None:
        code, docs, err = self.loop("--slot", "93")
        self.assertEqual((code, docs, self.sources()), (1, [], []), err)
        self.assertIn("the spare output --slot names is not among the restarted engine's HIL outputs", err)
        self.assertNotIn("93", err)
        self.assertEqual(self.iemmode_words()[-1], ["job-end", str(JOB)])

    def test_an_engine_that_is_not_started_again_is_a_bounded_failure(self) -> None:
        self.card.restarts = False
        with mock.patch.object(gen3, "ENGINE_WAIT_S", 0.2):
            code, docs, err = self.loop()
        self.assertEqual((code, docs, self.sources()), (1, [], []), err)
        self.assertIn("does not play within 0.2 s: the engine has not been started again", err)
        self.assertEqual(self.iemmode_words()[-1], ["job-end", str(JOB)])

    def test_a_burst_not_heard_on_the_slot_counts_as_not_heard(self) -> None:
        # The other spare output carries every burst, the slot none.
        self.card.slot_hears = False
        self.pc.replies[("test-signal", "mic1", "-30", "0.05")] = self.card.signal
        with mock.patch.object(gen3, "BURST_WAIT_S", 0.1):
            code, docs, err = self.loop("--bursts", "2", "--ttl", "0.05")
        self.assertEqual(code, 1, err)
        doc = docs[-1]["gen3_loop"]
        self.assertEqual((doc["not_heard"], doc["failed"][0], self.card.signals), (4, "not_heard", 4))

    def test_a_failed_job_end_fails_the_command_after_its_verdict(self) -> None:
        self.pc.replies[("job-end", str(JOB))] = (1, json.dumps({"ok": False, "detail": "HIL job 9 runs, not 4242"}))
        code, docs, err = self.loop("--bursts", "2")
        self.assertEqual(code, 1, err)
        self.assertEqual((docs[-1]["gen3_loop"]["pass"], docs[-1]["gen3_loop"]["job_end"]), (1, 1))
        self.assertIn(f"WARNING: iemmode job-end {JOB} answered exit 1", err)

    def test_the_flag_during_the_bursts_runs_the_event_path_and_leaves_the_job_to_the_switch(self) -> None:
        def signal_then_flag() -> tuple[int, str]:
            if self.card.signals == 2:
                self.flag()
            return self.card.signal()
        self.pc.replies[self.SIGNAL] = signal_then_flag
        code, docs, err = self.loop()
        self.assertEqual(code, ip.PREEMPTED, err)
        words = self.iemmode_words()
        self.assertNotIn(["job-end", str(JOB)], words)
        self.assertEqual(sum(1 for w in words if w[0] == "test-signal"), 3)
        self.assertIn(["event", "--signal"], words)
        self.assertEqual(docs[0], {"event": "ide event (flag file)", "action": "iempc event"})
        self.assertNotIn("gen3_loop", json.dumps(docs))

    def test_the_command_is_dev_time_locked_and_a_pc_command(self) -> None:
        spec = ip.COMMANDS["gen3-loop"]
        self.assertEqual((spec.pc, spec.dev_time, spec.locked), (True, True, True))
        self.flag()
        code, docs, err = self.loop()
        self.assertEqual((code, docs, self.pc.calls, self.gen3_calls()), (1, [], [], []))
        self.assertIn("'gen3-loop' runs only in dev time", err)


class RefusalTests(LoopBase):
    def test_arguments_and_the_env_refuse_before_any_call(self) -> None:
        cases = [
            (("--offsets", "192,192"), "--offsets must be two different echo offsets"),
            (("--offsets", "0,229"), "--offsets must be"), (("--offsets", "192,65536"), "--offsets must be"),
            (("--offsets", "192"), "--offsets must be"), (("--offsets", "192,229,300"), "--offsets must be"),
            (("--offsets=-1,229",), "--offsets must be"), (("--offsets", "1\u0662,229"), "--offsets must be"),
            (("--bursts", "1"), "--bursts must be 2..20"), (("--bursts", "21"), "--bursts must be 2..20"),
            (("--slot", "0"), "--slot must be"), (("--slot", "65536"), "--slot must be"),
            (("--input", "mic 1"), "--input must be"), (("--input", "mic'1"), "--input must be"),
            (("--dbfs", "-19.5"), "--dbfs must be at most -20"), (("--dbfs=-inf",), "--dbfs must be"),
            (("--dbfs", "nan"), "--dbfs must be"), (("--ttl", "0"), "--ttl must be above 0"),
            (("--ttl", "61"), "--ttl must be"), (("--ttl", "nan"), "--ttl must be"),
        ]
        for extra, words in cases:
            self.pc.calls.clear()
            code, docs, err = self.loop(*extra)
            self.assertEqual((code, docs, self.pc.calls, self.gen3_calls()), (1, [], [], []), extra)
            self.assertIn(words, err, extra)
        for env, words in (("", "no GEN3_HOST in the private env"), ("GEN3_HOST=-oProxyCommand=x\n",
                                                                     "GEN3_HOST must be an ssh destination")):
            envfile = self.tmp / "other.env"
            envfile.write_text("PC_SSH=tester@pc.test\nPC_ROOT=X:\\root\nPC_ROOT_SCP=/X:/root\n" + env,
                               encoding="utf-8")
            with self.patched(env_path=lambda: envfile):
                code, docs, err = self.loop()
            self.assertEqual((code, docs, self.pc.calls, self.gen3_calls()), (1, [], [], []), env)
            self.assertIn(words, err)

    def test_an_open_s1a_window_refuses_it(self) -> None:
        self.open_window()
        self.refused("an S1a/S1c spike window is open")
        self.assertEqual((self.pc.calls, self.gen3_calls()), ([], []))

    def test_the_gen3_device_must_carry_the_loop(self) -> None:
        for status, words in ((device(rx_status=1), "RX 1 is not receiving"), (device(ready=0), "clock is not ready"),
                              (device(live=0), "no receiver takes its TX"), ({"clock": {}}, "status is not the "
                                                                                               "device's"),
                              (None, "the Gen 3 device did not answer")):
            (self.box / "reads").unlink(missing_ok=True)
            self.plan([status])
            err = self.refused(words)
            self.assertIn("(nothing was changed)", err)
            self.assertEqual(self.pc.calls, [], words)

    def test_the_pc_must_be_settled_in_dev_with_its_engine_playing(self) -> None:
        def reply(**kw) -> tuple[int, str]:
            doc = json.loads(self.card.status()[1])
            doc.update({k: v for k, v in kw.items() if k != "engine"})
            doc["engine"].update(kw.get("engine", {}))
            return 0, json.dumps(doc)
        for answer, words in (
                (reply(detail=f"mode dev; bundle {SHA}; HIL job 77"), "a HIL job runs (HIL job 77)"),
                (reply(mode="event", detail=f"mode event; bundle {SHA}"), "the guard is in mode event"),
                (reply(engine={"parked": True}), "the engine is parked"),
                (reply(engine={"held": None}), "the engine is held"),
                (reply(engine={"spawns": None}), "names no engine start count"),
                (reply(detail="mode dev; no bundle"), "no active bundle"),
                ((4, ""), "iemmode status failed (exit 4)")):
            self.pc.calls.clear()
            self.pc.replies[("status",)] = answer
            self.refused(words)
            self.assertEqual(self.iemmode_words(), [["status"]], words)

    def test_an_ops_hil_run_that_may_still_hold_the_pc_refuses_it(self) -> None:
        self.gh.workflow_runs["hil.yml"] = [{"databaseId": 31, "status": "in_progress",
                                             "createdAt": "2026-10-26T05:00:00Z"}]
        self.refused("a hil.yml run is in_progress")
        self.assertEqual(self.iemmode_words(), [["status"]])


class VerdictTests(unittest.TestCase):
    """`verdict`, pure: it passes only the exact shape."""
    CLEAN = {"ready": 1, "polls": 100, "re_anchors": 2, "epoch": 2, "holes": 7, "late": 3, "ring_late": 1}

    def judge(self, trips, heard=True, offsets=(192, 229), **after) -> dict:
        bursts = [[(rt, heard) for rt in at] for at in trips]
        return gen3.verdict(offsets, bursts, dict(self.CLEAN), {**self.CLEAN, "polls": 160, **after})

    def test_equal_round_trips_the_offsets_apart_pass(self) -> None:
        doc = self.judge([[321] * 5, [358] * 5])
        self.assertEqual((doc["pass"], doc["failed"], doc["difference"], doc["expected"]), (1, [], 37, 37))
        self.assertEqual((doc["holes"], doc["late"], doc["ring_late"], doc["re_anchors"], doc["epoch"]),
                         (0, 0, 0, 0, 0))

    def test_every_other_shape_fails_with_its_own_word(self) -> None:
        cases = [
            (dict(trips=[[321, 321, 320, 321, 321], [358] * 5]), ["unequal"]),
            (dict(trips=[[321] * 5, [359] * 5]), ["difference"]),
            (dict(trips=[[321] * 5, [321 - 37] * 5]), ["difference"]),   # the offsets' sign matters
            (dict(trips=[[0] * 5, [358] * 5]), ["no_measurement"]),
            (dict(trips=[[321, 0, 321, 321, 321], [358] * 5]), ["no_measurement", "unequal"]),
            (dict(trips=[[321] * 5, [358] * 5], heard=False), ["not_heard"]),
            (dict(trips=[[321] * 5, [358] * 5], holes=8), ["holes"]),
            (dict(trips=[[321] * 5, [358] * 5], late=4), ["late"]),
            (dict(trips=[[321] * 5, [358] * 5], ring_late=2), ["ring_late"]),
            (dict(trips=[[321] * 5, [358] * 5], re_anchors=3), ["re_anchor"]),
            (dict(trips=[[321] * 5, [358] * 5], epoch=3), ["re_anchor"]),
            (dict(trips=[[321] * 5, [358] * 5], ready=0), ["not_ready"]),
            (dict(trips=[[321] * 5, [358] * 5], polls=4, holes=0), ["restarted"]),
        ]
        for kw, failed in cases:
            doc = self.judge(**kw)
            self.assertEqual((doc["pass"], doc["failed"]), (0, failed), kw)
        restarted = self.judge([[321] * 5, [358] * 5], polls=4, holes=0)
        self.assertEqual((restarted["restarted"], restarted["holes"]), (1, None))

    def test_the_verdict_is_numbers_and_fixed_words_only(self) -> None:
        doc = self.judge([[321, 322], [358, 358]], heard=False, holes=9)
        for key, value in doc.items():
            if key == "failed":
                self.assertTrue(set(value) <= set(gen3.FAILURES), value)
            else:
                self.assertTrue(value is None or type(value) is int or (isinstance(value, list) and all(
                    type(v) is int or isinstance(v, list) for v in value)), (key, value))


class Gen3LoopGuardTests(RunningGuardCases, LoopBase):
    ARGV = LoopBase.ARGS
    HEAD = "no loop test"
    NOTHING = "(nothing was changed)"

    def passes(self) -> None:
        code, docs, err = self.run_main(*self.ARGV)
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[-1]["gen3_loop"]["pass"], 1)


# ==== gen3-mix ====

UI = "http://10.0.0.10:8080/"   # the synthetic LAN address (P6)
BUILD = "fedcba9876543210fedcba9876543210fedcba98"
TOKENS = {"engineer": "zyxhead.zyxengineer.zyxsig", "member": "zyxhead.zyxmember.zyxsig"}
D = 320   # the engine's offset: B 32 + P 192 + W 96
R, T = 102, 73   # mic2's RX and member2's first TX in the test site
VERSION_CMD = f"{GEN3} /opt/iemmixer/bin/iem-engine --version"
SIGNER_PATH = "/var/lib/iemmixer/site/secrets/jwt_secret"
BOX_TOKENS = [".cache/iem-gen3-mix/engineer.token", ".cache/iem-gen3-mix/member.token"]
MINT = (f"{GEN3} umask 077 && mkdir -p .cache/iem-gen3-mix && rm -f {' '.join(BOX_TOKENS)} && "
        f"/opt/iemmixer/bin/iem-soakclient token --jwt-secret-file {SIGNER_PATH} --sub engineer --engineer "
        f"--seconds 900 "
        f"--out {BOX_TOKENS[0]} && "
        f"/opt/iemmixer/bin/iem-soakclient token --jwt-secret-file {SIGNER_PATH} --sub member2 --seconds 900 "
        f"--out {BOX_TOKENS[1]}")
COPY = f"scp {GEN3}:{BOX_TOKENS[0]} {GEN3}:{BOX_TOKENS[1]} "
REMOVE = f"{GEN3} rm -f {' '.join(BOX_TOKENS)}"
SOURCE = f"{GEN3} /opt/iemmixer/bin/iem-dante-dev ctl source"


def mixdevice(rx: int = R, tx: int = T, media: int = 1, offset: int = D, blocks: int = 30000, rebootstraps: int = 0,
              sources: list | None = None, engine: bool = True, **kw) -> dict:
    """The engine-hosted device's status as `ctl status` prints it (ctl.rs):
    the loop's, RX `rx` subscribed, TX `tx` the engine's, and the pump's
    `engine` object."""
    doc = device(**kw)
    doc["channels"][0]["rx"] = rx
    doc["sources"] = [{"tx": tx, "source": "engine"}] if sources is None else sources
    if engine:
        doc["engine"] = {"media": media, "offset": offset, "playout": 192, "blocks": blocks, "late": 0, "missed": 0,
                         "overruns": 0, "rebootstraps": rebootstraps, "max_process_us": 41}
    return doc


def window(**after) -> list[dict]:
    """The three status reads of a run: the preflight, the window's start and its end."""
    return [mixdevice(), mixdevice(polls=110, blocks=40000), mixdevice(**{"polls": 160, "blocks": 90000, **after})]


class MixCard(Card):
    """The PC's side of the mix loop: Card's statuses, the round trip from
    what the box does now. TX T echoes RX R D samples later, or carries the
    engine's mix, which hears the test input only while the browser opened it
    (the stand-in npx's `open` file): `mix_trip(source, opened, burst)`."""

    def __init__(self, box: Path) -> None:
        super().__init__(box / "calls.log")
        self.box = box
        self.mix_trip = lambda source, opened, burst: BASE_TRIP + D if source == "echo" or opened else 0

    def source(self) -> str:
        sets = [a for a in (json.loads(line) for line in self.gen3_log.read_text(encoding="utf-8").splitlines())
                if "source" in a]
        return "echo" if sets and "echo" in sets[-1] else "engine"

    def signal(self) -> tuple[int, str]:
        if any(p != "stale" for p in self.phases):
            self.overlaps += 1
        self.signals += 1
        trip = self.mix_trip(self.source(), (self.box / "open").exists(), self.signals)
        self.last_trip, self.this_trip = self.this_trip, trip
        self.phases = list(self.PHASES)
        return 0, json.dumps({"ok": True, "detail": "test signal on mic1"})


class MixBase(LoopBase):
    ARGS = ("gen3-mix", "--slot", str(SLOT), "--member", "member2", "--box-input", "mic2", "--bursts", "2")

    def setUp(self) -> None:
        super().setUp()
        envfile = self.tmp / "iem-pc-gen3-mix.env"
        envfile.write_text("PC_SSH=tester@pc.test\nPC_ROOT=X:\\root\nPC_ROOT_SCP=/X:/root\nPC_BIN=X:\\root\\bin\n"
                           f"GEN3_HOST={GEN3}\nGEN3_UI_URL={UI}\n", encoding="utf-8")
        self.patch(env_path=lambda: envfile)
        d = self.tmp / "bin"
        for name, body in (("scp", FAKE_SCP), ("npx", FAKE_NPX)):
            p = d / name
            p.write_text(f"#!{sys.executable}\n{body}", encoding="utf-8")
            p.chmod(0o755)
        ip.EVENT_NOW.parent.mkdir(parents=True, exist_ok=True)
        patcher = mock.patch.dict(os.environ, {"FAKE_TOKENS": json.dumps(TOKENS), "FAKE_FLAG": str(ip.EVENT_NOW),
                                               "FAKE_ENGINE_VERSION": f"iem-engine 2.0.0-dev.45+{BUILD}"})
        patcher.start()
        self.addCleanup(patcher.stop)
        self.plan(window())
        self.card = MixCard(self.box)
        self.pc.replies[("status",)] = self.card.status
        self.pc.replies[("activate", SHA)] = self.card.activate
        self.pc.replies[self.SIGNAL] = self.card.signal

    def again(self) -> None:
        """A second run in the same test: the box's reads and every record afresh."""
        self.pc.calls.clear()
        for name in ("reads", "npx.log", "open"):
            (self.box / name).unlink(missing_ok=True)
        (self.box / "calls.log").write_text("", encoding="utf-8")

    def box_calls(self) -> list[str]:
        """The box's calls (ssh and scp), each without its options."""
        out = []
        for line in (self.box / "calls.log").read_text(encoding="utf-8").splitlines():
            args, words = json.loads(line), []
            it = iter(args)
            for a in it:
                if a == "-o":
                    next(it)
                elif a != "-q":
                    words.append(a)
            out.append(" ".join(words))
        return out

    def steps(self) -> list[dict]:
        log = self.box / "npx.log"
        return [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()] if log.exists() else []

    def signals(self) -> int:
        return sum(1 for w in self.iemmode_words() if w[0] == "test-signal")

    def assert_no_token_left(self, err: str, docs: list) -> None:
        """No token on either side, none printed, none on a command line."""
        left = sorted(p.name for p in (self.box / "home").rglob("*.token")) if (self.box / "home").exists() else []
        self.assertEqual(left, [])
        self.assertEqual(sorted(p.name for p in ip.state_dir().glob("gen3-mix-*")), [])
        calls = (self.box / "calls.log").read_text(encoding="utf-8")
        for token in TOKENS.values():
            self.assertNotIn(token, err + json.dumps(docs) + calls)
        for step in self.steps():
            self.assertFalse(Path(step["path"]).exists())


class MixTests(MixBase):
    def test_the_mix_passes_with_the_exact_shape(self) -> None:
        code, docs, err = self.loop()
        self.assertEqual(code, 0, err)
        self.assertEqual(docs, [{"gen3_mix": {
            "pass": 1, "bursts": 2, "echo": [BASE_TRIP + D] * 2, "engine": [BASE_TRIP + D] * 2, "closed_heard": 0,
            "not_heard": 0, "difference": 0, "offset": D, "restarted": 0, "holes": 0, "late": 0, "ring_late": 0,
            "rebootstraps": 0, "re_anchors": 0, "failed": [], "job": JOB, "job_end": 0}}])
        burst = [list(self.SIGNAL), *[["status"]] * 6]   # stale, sounding four times, silent
        self.assertEqual(self.iemmode_words(), [["status"], ["job-begin", str(JOB)], ["activate", SHA], ["status"],
                                                *burst * 6, ["job-end", str(JOB)]])
        modes = {tuple(args[:1]): event for _, args, event in self.pc.calls}
        self.assertEqual((modes[("job-begin",)], modes[("activate",)], modes[("test-signal",)], modes[("job-end",)],
                          modes[("status",)]), ("finish", "finish", "abandon", "finish", "abandon"))
        calls = self.box_calls()
        self.assertEqual(calls[:3], [STATUS_CMD, VERSION_CMD, MINT])
        self.assertTrue(calls[3].startswith(COPY) and calls[3].endswith("/"), calls[3])
        self.assertEqual(Path(calls[3][len(COPY):]).parent, ip.state_dir())
        self.assertEqual(calls[4:], [REMOVE, STATUS_CMD, f"{SOURCE} {T} echo {R} {D} 1>&2", f"{SOURCE} {T} engine 1>&2",
                                     STATUS_CMD])
        steps = self.steps()
        self.assertEqual([s["step"] for s in steps], ["closes", "opens", "closes"])
        for s, title in zip(steps, (gen3.CLOSES, gen3.OPENS, gen3.CLOSES)):
            self.assertEqual(s["args"], ["playwright", "test", "--config", "playwright.gen3.config.ts", "--grep",
                                         f"{title}$"])
            self.assertEqual(s["cwd"], str(gen3.E2E))
            self.assertEqual(s["live"], {"LIVE_BASE_URL": UI, "LIVE_SHA": BUILD, "LIVE_MEMBER": "member2",
                                         "LIVE_TEST_INPUT": "mic2"})
            self.assertEqual((s["mode"], s["folder_mode"], s["tokens"], s["pins"]), (0o600, 0o700, TOKENS, []))
            self.assertEqual(Path(s["path"]).parent.parent, ip.state_dir())
        self.assert_no_token_left(err, docs)
        self.assertNotIn(str(SLOT), json.dumps(docs))   # the card's channel is never printed (P6)
        for value in (GEN3, UI):
            self.assertNotIn(value, json.dumps(docs) + err)
        self.assertEqual((self.card.signals, self.card.overlaps), (6, 0))   # each burst ended before the next

    def test_r_and_t_come_from_the_test_site_for_the_member_and_the_input(self) -> None:
        self.plan([mixdevice(rx=115, tx=75), mixdevice(rx=115, tx=75, polls=110, blocks=40000),
                   mixdevice(rx=115, tx=75, polls=160, blocks=90000)])
        code, docs, err = self.loop("--member", "member3", "--box-input", "keys")
        self.assertEqual((code, docs[-1]["gen3_mix"]["pass"]), (0, 1), err)
        sources = [c for c in self.box_calls() if c.startswith(SOURCE)]
        self.assertEqual(sources, [f"{SOURCE} 75 echo 115 {D} 1>&2", f"{SOURCE} 75 engine 1>&2"])
        self.assertIn("--sub member3 --seconds 900", self.box_calls()[2])
        self.assertEqual({(s["live"]["LIVE_MEMBER"], s["live"]["LIVE_TEST_INPUT"]) for s in self.steps()},
                         {("member3", "keys")})

    def test_an_engine_round_trip_one_sample_off_fails_the_difference(self) -> None:
        self.card.mix_trip = lambda source, opened, burst: (BASE_TRIP + D + (source == "engine")
                                                            if source == "echo" or opened else 0)
        code, docs, err = self.loop()
        self.assertEqual(code, 1, err)
        doc = docs[-1]["gen3_mix"]
        self.assertEqual((doc["pass"], doc["failed"], doc["difference"], doc["engine"], doc["job_end"]),
                         (0, ["difference"], 1, [BASE_TRIP + D + 1] * 2, 0))

    def test_a_closed_burst_heard_fails(self) -> None:
        self.card.mix_trip = lambda source, opened, burst: BASE_TRIP + D   # the browser's close never reached the mix
        code, docs, err = self.loop()
        self.assertEqual(code, 1, err)
        doc = docs[-1]["gen3_mix"]
        self.assertEqual((doc["failed"], doc["closed_heard"], doc["difference"]), (["closed_heard"], 2, 0))

    def test_a_pump_rebootstrap_holes_late_packets_or_a_re_anchor_in_the_window_fail_it(self) -> None:
        for after, word in ((dict(rebootstraps=1), "rebootstraps"), (dict(holes=5), "holes"),
                            (dict(tx_holes=64), "holes"), (dict(late=1), "late"), (dict(ring_late=16), "ring_late"),
                            (dict(re_anchors=1, epoch=1), "re_anchor"), (dict(epoch=1), "re_anchor"),
                            (dict(media=0), "not_media"), (dict(ready=0), "not_ready"),
                            (dict(blocks=10), "restarted")):
            self.again()
            self.plan(window(**after))
            code, docs, err = self.loop()
            self.assertEqual(code, 1, (word, err))
            self.assertEqual(docs[-1]["gen3_mix"]["failed"], [word], word)
            self.assert_no_token_left(err, docs)

    def test_a_browser_step_that_fails_ends_the_job_with_no_further_burst(self) -> None:
        for step, signals, steps in (("opens", 4, ["closes", "opens"]), ("closes", 2, ["closes"])):
            self.again()
            with mock.patch.dict(os.environ, {"FAKE_SPEC_FAILS": step}):
                code, docs, err = self.loop()
            self.assertEqual(code, 1, err)
            doc = docs[-1]["gen3_mix"]
            self.assertEqual((doc["pass"], doc["failed"][0], doc["job_end"]), (0, "browser", 0), step)
            self.assertEqual(doc["engine"], [])
            self.assertEqual((self.signals(), [s["step"] for s in self.steps()]), (signals, steps), step)
            self.assertEqual(self.iemmode_words()[-1], ["job-end", str(JOB)])
            title = gen3.OPENS if step == "opens" else gen3.CLOSES
            self.assertIn(f"WARNING: gen3-mix: the browser step '{title}' failed", err)
            self.assertIn("1 failed", err)   # Playwright's report, on stderr
            self.assert_no_token_left(err, docs)

    def test_a_refused_source_setting_is_named_and_ends_the_job(self) -> None:
        with mock.patch.dict(os.environ, {"FAKE_SOURCE_ANSWER": "error: no engine on that channel"}):
            code, docs, err = self.loop()
        self.assertEqual((code, docs), (1, []), err)
        self.assertIn("ssh failed (exit 1): error: no engine on that channel", err)
        self.assertEqual((self.signals(), self.steps()), (0, []))
        self.assertEqual(self.iemmode_words()[-1], ["job-end", str(JOB)])
        self.assert_no_token_left(err, docs)

    def test_tokens_that_were_not_minted_or_copied_refuse_with_nothing_changed(self) -> None:
        for env in ({"FAKE_MINT_FAILS": "member"}, {"FAKE_MINT_FAILS": "engineer"}, {"FAKE_SCP_FAILS": "1"}):
            self.again()
            with mock.patch.dict(os.environ, env):
                code, docs, err = self.loop()
            self.assertEqual((code, docs), (1, []), err)
            self.assertIn("no mix test: the browser's tokens were not minted", err)
            self.assertIn("(nothing was changed: no HIL job, no source)", err)
            self.assertEqual(self.iemmode_words(), [["status"]], env)
            self.assertEqual(self.box_calls()[-1], REMOVE, env)   # removed on the box whatever happened
            self.assert_no_token_left(err, docs)

    def test_a_job_begin_refusal_leaves_no_token_and_no_source(self) -> None:
        self.pc.replies[("job-begin", str(JOB))] = (1, json.dumps({"ok": False, "detail": "HIL job 77 has not ended"}))
        code, docs, err = self.loop()
        self.assertEqual((code, docs), (1, []), err)
        self.assertIn("no mix test: iemmode job-begin refused (exit 1): HIL job 77 has not ended", err)
        self.assertEqual([c for c in self.box_calls() if c.startswith(SOURCE)], [])
        self.assertEqual(self.steps(), [])
        self.assert_no_token_left(err, docs)

    def test_the_flag_during_a_browser_step_runs_the_event_path_and_leaves_the_job_to_the_switch(self) -> None:
        with mock.patch.dict(os.environ, {"FAKE_SPEC_FLAG": "opens"}):
            code, docs, err = self.loop()
        self.assertEqual(code, ip.PREEMPTED, err)
        words = self.iemmode_words()
        self.assertNotIn(["job-end", str(JOB)], words)
        self.assertIn(["event", "--signal"], words)
        self.assertEqual(self.signals(), 4)   # echo and closed bursts; none after the flag
        self.assertEqual(docs[0], {"event": "ide event (flag file)", "action": "iempc event"})
        self.assertNotIn("gen3_mix", json.dumps(docs))
        self.assert_no_token_left(err, docs)

    def test_no_pin_reaches_the_browser_step(self) -> None:
        with mock.patch.dict(os.environ, {"E2E_ENGINEER_PIN": "1111", "E2E_MEMBER_PIN": "2222"}):
            code, docs, err = self.loop()
        self.assertEqual(code, 0, err)
        self.assertEqual({tuple(s["pins"]) for s in self.steps()}, {()})

    def test_the_command_is_dev_time_locked_and_a_pc_command(self) -> None:
        spec = ip.COMMANDS["gen3-mix"]
        self.assertEqual((spec.pc, spec.dev_time, spec.locked), (True, True, True))
        self.flag()
        code, docs, err = self.loop()
        self.assertEqual((code, docs, self.pc.calls, self.box_calls()), (1, [], [], []))
        self.assertIn("'gen3-mix' runs only in dev time", err)


class MixRefusalTests(MixBase):
    def refused_early(self, words: str, *extra: str) -> None:
        self.again()
        code, docs, err = self.loop(*extra)
        self.assertEqual((code, docs, self.pc.calls, self.box_calls()), (1, [], [], []), (extra, err))
        self.assertIn(words, err, extra)
        self.assertNotIn("Traceback", err)

    def test_arguments_and_ids_the_test_site_lacks_refuse_before_any_call(self) -> None:
        for extra, words in (
                (("--slot", "0"), "gen3-mix: --slot must be"),
                (("--member", "member 2"), "--member must be a person's id"),
                (("--box-input", "mic'2"), "--box-input must be an engine input id"),
                (("--bursts", "1"), "gen3-mix: --bursts must be 2..20"),
                (("--dbfs", "-19"), "gen3-mix: --dbfs must be"),
                (("--ttl", "0"), "gen3-mix: --ttl must be"), (("--input", "mic 1"), "gen3-mix: --input must be"),
                (("--member", "member99"), "gen3-mix: --member is no person of the test roster"),
                (("--member", "engineer"), "gen3-mix: --member is no person of the test roster"),
                (("--member", "member10"), "gen3-mix: --member has no mix with a TX channel"),
                (("--box-input", "nosuch"), "gen3-mix: --box-input is no input with an RX channel"),
                (("--box-input", "member2"), "gen3-mix: --box-input is no input with an RX channel")):
            self.refused_early(words, *extra)

    def test_the_env_refuses_before_any_call(self) -> None:
        for env, words in ((f"GEN3_UI_URL={UI}\n", "gen3-mix: no GEN3_HOST in the private env"),
                           (f"GEN3_HOST={GEN3}\n", "gen3-mix: no GEN3_UI_URL in the private env"),
                           (f"GEN3_HOST={GEN3}\nGEN3_UI_URL=ftp://10.0.0.10/\n",
                            "gen3-mix: GEN3_UI_URL must be an http:// or https:// URL")):
            envfile = self.tmp / "other.env"
            envfile.write_text("PC_SSH=tester@pc.test\nPC_ROOT=X:\\root\nPC_ROOT_SCP=/X:/root\n" + env,
                               encoding="utf-8")
            with self.patched(env_path=lambda: envfile):
                self.refused_early(words)

    def test_an_open_s1a_window_refuses_it(self) -> None:
        self.open_window()
        self.refused_early("an S1a/S1c spike window is open")

    def test_the_gen3_device_must_carry_the_mix_test(self) -> None:
        for status, words in ((mixdevice(rx_status=1), f"RX {R} is not receiving"),
                              (mixdevice(ready=0), "its media clock is not ready"),
                              (mixdevice(media=0), "the engine's pump is not on the media clock"),
                              (mixdevice(engine=False), "its status is not the engine-hosted device's"),
                              (mixdevice(offset=70000), "the engine's offset is no echo offset of 1..65535"),
                              (mixdevice(live=0), "no receiver takes its TX"),
                              (mixdevice(sources=[]), f"TX {T} is silent"),
                              (mixdevice(sources=[{"tx": 74, "source": "engine"}]), f"TX {T} is silent"),
                              (None, "the Gen 3 device did not answer")):
            self.again()
            self.plan([status])
            code, docs, err = self.loop()
            self.assertEqual((code, docs, self.pc.calls, self.box_calls()), (1, [], [], [STATUS_CMD]), words)
            self.assertIn("no mix test: ", err)
            self.assertIn(words, err)
            self.assertIn("(nothing was changed)", err)

    def test_the_pc_must_be_settled_in_dev_with_its_engine_playing(self) -> None:
        def reply(**kw) -> tuple[int, str]:
            doc = json.loads(self.card.status()[1])
            doc.update({k: v for k, v in kw.items() if k != "engine"})
            doc["engine"].update(kw.get("engine", {}))
            return 0, json.dumps(doc)
        for answer, words in ((reply(detail=f"mode dev; bundle {SHA}; HIL job 77"), "a HIL job runs (HIL job 77)"),
                              (reply(engine={"parked": True}), "the engine is parked"),
                              ((4, ""), "iemmode status failed (exit 4)")):
            self.again()
            self.pc.replies[("status",)] = answer
            code, docs, err = self.loop()
            self.assertEqual((code, docs, self.iemmode_words(), self.box_calls()), (1, [], [["status"]], [STATUS_CMD]))
            self.assertIn(f"no mix test: {words}", err)

    def test_an_ops_hil_run_that_may_still_hold_the_pc_refuses_it(self) -> None:
        self.gh.workflow_runs["hil.yml"] = [{"databaseId": 31, "status": "in_progress",
                                             "createdAt": "2026-10-26T05:00:00Z"}]
        code, docs, err = self.loop()
        self.assertEqual((code, docs, self.iemmode_words(), self.box_calls()), (1, [], [["status"]], [STATUS_CMD]))
        self.assertIn("no mix test: a hil.yml run is in_progress", err)

    def test_a_box_without_a_ci_build_refuses_before_any_token(self) -> None:
        for line in ("iem-engine 2.0.0-dev.45+local", "iem-engine 2.0.0-dev.45", ""):
            self.again()
            with mock.patch.dict(os.environ, {"FAKE_ENGINE_VERSION": line}):
                code, docs, err = self.loop()
            self.assertEqual((code, docs, self.box_calls()), (1, [], [STATUS_CMD, VERSION_CMD]), line)
            self.assertIn("no mix test: the box's iem-engine --version names no CI build", err)
            self.assertNotIn(["job-begin", str(JOB)], self.iemmode_words())


class SiteChannelTests(unittest.TestCase):
    """`site_channels` on this checkout's test site and roster (pure)."""

    def setUp(self) -> None:
        import tomllib
        self.site = tomllib.loads(gen3.SITE.read_text(encoding="utf-8"))
        self.roster = tomllib.loads(gen3.ROSTER.read_text(encoding="utf-8"))

    def test_r_is_the_inputs_first_rx_and_t_the_members_first_tx(self) -> None:
        for member, box_input, want in (("member2", "mic2", (102, 73)), ("member3", "keys", (115, 75)),
                                        ("member1", "hand1", (111, 71)), ("member9", "bgvs", (131, 87))):
            self.assertEqual(gen3.site_channels(self.site, self.roster, member, box_input), want)

    def test_ids_the_site_or_roster_lacks_are_named(self) -> None:
        for member, box_input, words in (("member99", "mic2", "no person of the test roster"),
                                         ("engineer", "mic2", "no person of the test roster"),
                                         ("translator", "mic2", "no person of the test roster"),
                                         ("member11", "mic2", "has no mix with a TX channel"),
                                         ("member2", "member2", "no input with an RX channel")):
            with self.assertRaises(ValueError) as e:
                gen3.site_channels(self.site, self.roster, member, box_input)
            self.assertIn(words, str(e.exception))
        with self.assertRaises(ValueError):
            gen3.site_channels({}, self.roster, "member2", "mic2")
        broken = {"engine": {"mixes": [{"id": "member2", "tx": []}], "inputs": [{"id": "mic2", "rx": [True]}]}}
        with self.assertRaises(ValueError):
            gen3.site_channels(broken, self.roster, "member2", "mic2")


class MixVerdictTests(unittest.TestCase):
    """`mix_verdict`, pure: it passes only the exact shape."""
    CLEAN = {"ready": 1, "media": 1, "polls": 100, "re_anchors": 2, "epoch": 2, "holes": 7, "late": 3, "ring_late": 1,
             "blocks": 30000, "rebootstraps": 1}
    TRIP = 449

    def judge(self, echo=None, engine=None, closed=None, heard=True, browser=True, **after) -> dict:
        echo = [self.TRIP] * 5 if echo is None else echo
        engine = [self.TRIP] * 5 if engine is None else engine
        closed = [0] * 5 if closed is None else closed
        run = gen3.MixRun([(t, heard) for t in echo], [(t, True) for t in closed], [(t, True) for t in engine],
                          browser)
        return gen3.mix_verdict(5, run, dict(self.CLEAN), {**self.CLEAN, "polls": 160, "blocks": 90000, **after}, D)

    def test_equal_round_trips_through_the_echo_and_the_engine_pass(self) -> None:
        doc = self.judge()
        self.assertEqual((doc["pass"], doc["failed"], doc["difference"], doc["offset"], doc["closed_heard"]),
                         (1, [], 0, D, 0))
        self.assertEqual([doc[k] for k in ("holes", "late", "ring_late", "rebootstraps", "re_anchors")], [0] * 5)

    def test_every_other_shape_fails_with_its_own_word(self) -> None:
        cases = [
            (dict(engine=[self.TRIP + 1] * 5), ["difference"]),   # one sample off
            (dict(engine=[self.TRIP - 1] * 5), ["difference"]),
            (dict(engine=[self.TRIP, self.TRIP, self.TRIP + 1, self.TRIP, self.TRIP]), ["unequal"]),
            (dict(echo=[self.TRIP, self.TRIP - 1, self.TRIP, self.TRIP, self.TRIP]), ["unequal"]),
            (dict(engine=[0] * 5), ["no_measurement"]),
            (dict(echo=[0] * 5), ["no_measurement"]),
            (dict(closed=[0, 0, self.TRIP, 0, 0]), ["closed_heard"]),
            (dict(heard=False), ["not_heard"]),
            (dict(engine=[], closed=[], browser=False), ["browser"]),
            (dict(rebootstraps=2), ["rebootstraps"]),
            (dict(holes=8), ["holes"]),
            (dict(late=4), ["late"]),
            (dict(ring_late=2), ["ring_late"]),
            (dict(re_anchors=3), ["re_anchor"]),
            (dict(epoch=3), ["re_anchor"]),
            (dict(ready=0), ["not_ready"]),
            (dict(media=0), ["not_media"]),
            (dict(blocks=10, holes=0), ["restarted"]),
            (dict(polls=4), ["restarted"]),
        ]
        for kw, failed in cases:
            doc = self.judge(**kw)
            self.assertEqual((doc["pass"], doc["failed"]), (0, failed), kw)
        restarted = self.judge(blocks=10)
        self.assertEqual((restarted["restarted"], restarted["holes"], restarted["rebootstraps"]), (1, None, None))
        cut = self.judge(engine=[], closed=[], browser=False)
        self.assertEqual((cut["difference"], cut["engine"], cut["echo"]), (None, [], [self.TRIP] * 5))

    def test_the_verdict_is_numbers_and_fixed_words_only(self) -> None:
        doc = self.judge(engine=[self.TRIP, 450, 0, 1, 2], closed=[1, 0, 0, 0, 0], heard=False, holes=9)
        self.assertEqual(set(doc), {"pass", "bursts", "echo", "engine", "closed_heard", "not_heard", "difference",
                                    "offset", "restarted", "holes", "late", "ring_late", "rebootstraps", "re_anchors",
                                    "failed"})
        for key, value in doc.items():
            if key == "failed":
                self.assertTrue(value and set(value) <= set(gen3.MIX_FAILURES), value)
                self.assertEqual(value, sorted(value, key=gen3.MIX_FAILURES.index))
            else:
                self.assertTrue(value is None or type(value) is int or (isinstance(value, list) and all(
                    type(v) is int for v in value)), (key, value))


class Gen3MixGuardTests(RunningGuardCases, MixBase):
    ARGV = MixBase.ARGS
    HEAD = "no mix test"
    NOTHING = "(nothing was changed)"

    def passes(self) -> None:
        code, docs, err = self.run_main(*self.ARGV)
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[-1]["gen3_mix"]["pass"], 1)


if __name__ == "__main__":
    unittest.main()
