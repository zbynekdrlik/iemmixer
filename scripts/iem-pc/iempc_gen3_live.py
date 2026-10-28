"""`iempc gen3-gate live` (#72 phase H, plan
docs/superpowers/plans/2026-10-28-gen3-phase-h-box-gates.md Task 5): the live
E2E on the Gen 3 box that serves the band. The PC's live run (the ops
live.yml, .claude/rules/live.md) driven from the dev box over ssh, its
verdict posted as the commit status `live/gen3` on the box's build. Its call
site is iempc_gen3_gate.run.

Dev time, locked, a box command, only while 3.x serves (iempc's Spec and
WHILE_3X_SERVES, as `gen3-gate hil`): the dev box's flag is the event gate.

**Before any call** (Refused): GEN3_HOST; GEN3_PUBLIC_URL a bare https://
origin once one trailing slash is taken off (the band's public URL: the
members' real path through the tunnel, the specs' LIVE_BASE_URL);
LIVE_MEMBER and LIVE_TALKBACK_INPUT ids; LIVE_TEST_INPUT exactly `mic1` (the
sudo rule's burst line names it; a refusal names the variable, never its
value); `npx` on PATH (Playwright in e2e/, full Chromium for the push spec).

**Before any change** (Refused, nothing changed): the HIL gate's preflight
without its ARC reads (iempc_gen3_gate.preflight: a left job, the unit's
drop-ins, its ExecStart and start limit, the device's counts against the
site's spare outputs, check-site, health, the RX channels Receiving); the
box's build (`iem-engine --version`, LIVE_SHA) with the public URL's
`/api/version` `git_hash` a prefix of it (7 hex digits or more); a left job
ended first; the restart budget for the job's begin and end; the engineer's
push subscriptions counted (`iem-server notify --count alarm`, push_before);
an engineer and a member token minted on the box for TOKEN_SECONDS
(iempc_gen3.mint_tokens: as the service user, each read back into a 0700
`gen3-live-*` folder of iempc's state, `tokens.json` 0600, JWT-shaped,
removed on the box at once; the folder is removed on every exit). A token is
never on a command line and never printed.

**The run** (live.yml's jobs in one process). pc-begin: the box's HIL job
(iempc_gen3_gate.begin_job: the engine restarted with --test-signal), ready
when healthy, the RX flows back and the spare outputs open, else
`engine-not-up` and no browser. browser: `npx playwright test --config
playwright.live.config.ts` in e2e/ (its output into the run's
playwright.log) with the LIVE_* values, LIVE_BURST_DBFS -20, LIVE_RESULTS
the run's results.json and the client-error marker's GITHUB_RUN_ID (the
run's number, iempc_gen3.job_run) and GITHUB_RUN_ATTEMPT 1, no PIN; it ends
by its own globalTimeout (40 min), and past BROWSER_LIMIT_S it is left to
end by itself (`not-finished`). pc: while it runs, a burst every
BURST_PERIOD_S (`supervise test-signal mic1 -20 30 --listen`, the flag
checked before each), each in bursts.jsonl with its exit code. pc-end: the
burst stopped, the marker in the server's journal (`journalctl -u
iem-server.service`'s last 20000 lines as root, as pc-end reads the PC
server log's last 20000; the marker names the run's number, so an earlier
run's never counts), the push count again, the engine's own restarts
(NRestarts: the budget counts them), then the job's end.

**The verdict**: live_verdict.report over the run's records (begin.json,
pc.json, bursts.jsonl, evidence.json and results.json in iempc's state,
gen3-live/<run>/, kept: no token) with this run's job results; then this
gate's own words, LIVE_FAILURES (every check that failed, not only the
first, and `restarted` for an engine that restarted by itself). One line
`{"gen3_gate": {"live": {...}}}`: numbers, fixed codes, the verdict's first
failure (its fixed codes and the public spec titles), never a token, URL,
host, member id or test error text. The status `live/gen3` on the box's
build (iempc_gen3_gate.post), success only when the verdict is green and no
word failed, its description the failed words. Exit 0 only when it passed
and the status was posted.

**A flag**: no burst after it; the burst stopped, the job ended without its
waits, the records written as a cut run (the cut job `cancelled`,
`left-dev`), so the verdict is `cancelled`, never red or green; the line,
never a status; then the event path (EventNow: iempc main). Playwright is
left to end by itself (I8: nothing is ever force-ended): outside a burst its
specs change nothing (`desk.ts`).

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path

import iempc_gen3 as g3
import iempc_gen3_gate as gate
import iempc_gen3_tunnel as tunnel
import live_verdict

NAME = "live"   # its refusals: "no live gate"
CONTEXT = "live/gen3"
ID = re.compile(r"[A-Za-z0-9_-]{1,64}")
ORIGIN = re.compile(r"https://[A-Za-z0-9.-]+")   # live.yml's check of LIVE_BASE_URL
# The burst: the PC's live run's (decision 4: 30 s every 60 s at the HIL
# ceiling, the listen probe on), fixed, as the sudo rule names it.
BURST_INPUT, BURST_DBFS, BURST_S = "mic1", -20, 30
BURST = (*gate.SUPERVISE, "test-signal", BURST_INPUT, str(BURST_DBFS), str(BURST_S), "--listen")
BURST_PERIOD_S = 60.0
LOOP_S = 1.0   # how often the loop looks at Playwright
BROWSER_LIMIT_S = 45 * 60.0   # live.yml's browser job; the config's globalTimeout is 40 min
TOKEN_SECONDS = 3600   # the job's begin (minutes) and the specs (40 min at most); the mint adds 10 min
JOURNAL = (*gate.SUDO, "journalctl", "-u", "iem-server.service", "-o", "cat", "-n", "20000", "--no-pager", "-q")
JOURNAL_S = 60
ATTEMPT = 1
SPEC_CONFIG = "playwright.live.config.ts"
SPECS = g3.E2E / "tests" / "live"
RECORDS = "gen3-live"
EXIT = re.compile(r"\(exit (-?[0-9]+)\)")   # iempc_core.guarded's words for a failed call
DESC_PASS = "the Gen 3 box's live E2E passed (iempc gen3-gate live)"
DESC_FAIL = "the Gen 3 box's live E2E failed: "
# The words a verdict names, in its order (`verdict`: the verdict was red and none of the others says why).
LIVE_FAILURES = ("begin", "bursts", "browser", "client_log", "push", "job_end", "restarted", "verdict")
# Playwright runs a flag or the bound left to end by themselves (never force-ended).
LEFT_RUNNING: list[subprocess.Popen] = []


@dataclass(frozen=True)
class Settings:
    host: str
    origin: str
    member: str
    talkback: str


@dataclass
class Run:
    """What the run did: the records' values and its jobs' results (live.yml's)."""
    number: int
    records: Path
    push_before: int
    begun: bool = False   # begin.json written: pc-begin done
    ready: bool = False
    pc: str = "skipped"
    browser: str = "skipped"
    browser_exit: int | None = None
    bursts: list[int] = field(default_factory=list)   # each burst's exit code
    client_log: str = "unreadable"
    push_after: int = -1
    end: str = "skipped"
    proc: subprocess.Popen | None = None


# ---- pure ----

def marker(number: int) -> str:
    """The client-error marker the live specs post (support/env.ts runMarker)."""
    return f"iemmixer-live-marker-{number}-{ATTEMPT}"


def exit_of(e: Exception) -> int:
    """A failed burst call's exit code (iempc_core.guarded's words), 1 when it names none."""
    m = EXIT.search(str(e))
    return int(m.group(1)) if m else 1


def failures(c: gate.Checks, run: Run) -> list[str]:
    """The checks that failed, as LIVE_FAILURES' words (pure)."""
    failed = []
    if not run.ready:
        failed.append("begin")
    if run.ready and (not run.bursts or any(code != 0 for code in run.bursts)):
        failed.append("bursts")
    if run.ready and run.browser != "success":
        failed.append("browser")
    if run.client_log != "found":
        failed.append("client_log")
    if run.push_after < 0 or run.push_after > run.push_before:
        failed.append("push")
    if run.end != "success":
        failed.append("job_end")
    if c.unit_restarts:
        failed.append("restarted")
    return failed


def line(v: dict, c: gate.Checks, run: Run, left: bool, failed: list[str]) -> dict:
    """The gate's line (pure): numbers, fixed codes, the verdict's first failure."""
    passed = v["conclusion"] == "success" and not failed
    return {
        "pass": int(passed), "conclusion": v["conclusion"], "first_failure": v["first_failure"],
        "health": int(c.health), "rx_back": int(c.rx_back), "rx_channels": c.rx_channels, "hil_on": int(c.hil_on),
        "bursts": len(run.bursts), "bursts_failed": sum(1 for code in run.bursts if code != 0),
        "browser_exit": run.browser_exit, "client_log": run.client_log,
        "push_before": run.push_before, "push_after": run.push_after,
        "unit_restarts": c.unit_restarts, "starts": c.starts, "left_job": int(left),
        "job_end": int(c.end_ok), "hil_off": int(c.hil_off), "numbers": v["numbers"], "failed": failed,
    }


# ---- before any change ----

def settings(ip, env: dict[str, str]) -> Settings:
    host = g3.gen3_host(ip, env, gate.WORDS)
    origin = env.get("GEN3_PUBLIC_URL", "").removesuffix("/")
    if not ORIGIN.fullmatch(origin):
        raise ip.Refused("gen3-gate: GEN3_PUBLIC_URL of the private env is not a bare https:// origin (the band's "
                         "public URL, the live specs' LIVE_BASE_URL)")
    for key in ("LIVE_MEMBER", "LIVE_TEST_INPUT", "LIVE_TALKBACK_INPUT"):
        if not ID.fullmatch(env.get(key, "")):
            raise ip.Refused(f"gen3-gate: {key} of the private env is not an id (1 to 64 of A-Z a-z 0-9 _ -)")
    if env["LIVE_TEST_INPUT"] != BURST_INPUT:
        raise ip.Refused(f"gen3-gate: LIVE_TEST_INPUT of the private env must be {BURST_INPUT}, the input the box's "
                         "burst drives (the sudo rule's exact line)")
    if shutil.which("npx") is None:
        raise ip.Refused("gen3-gate: npx is not on this box's PATH (the live specs run with Playwright in e2e/)")
    return Settings(host, origin, env["LIVE_MEMBER"], env["LIVE_TALKBACK_INPUT"])


def box_build(box: gate.Box) -> str:
    try:
        return g3.box_build(box.ip, box.ctx, box.host)
    except box.ip.StepError as e:
        raise box.ip.Refused(f"no live gate: {e} (nothing was changed)") from None


def refuse_other_public_build(ip, origin: str, sha: str) -> None:
    got = tunnel.public_hash(origin)
    if not (isinstance(got, str) and gate.HASH.fullmatch(got) and sha.startswith(got)):
        raise ip.Refused("no live gate: the band's public URL does not answer /api/version with the box's build (the "
                         "tunnel reaches another server, or none) (nothing was changed)")


def records_folder(ip, number: int) -> Path:
    """The run's records in iempc's state, owner-only; one of the same
    number (two runs in one second: only a test makes them) is replaced."""
    folder = ip.state_dir() / RECORDS / str(number)
    folder.parent.mkdir(mode=0o700, exist_ok=True)
    if folder.exists():
        shutil.rmtree(folder)
    folder.mkdir(mode=0o700)
    os.chmod(folder, 0o700)
    return folder


# ---- the records ----

def write(run: Run, name: str, doc: dict) -> None:
    (run.records / name).write_text(json.dumps(doc) + "\n", encoding="utf-8")


def add_burst(run: Run, code: int) -> None:
    run.bursts.append(code)
    with open(run.records / "bursts.jsonl", "a", encoding="utf-8") as f:
        f.write(json.dumps({"t": int(gate.wall()), "exit": code}) + "\n")


def verdict_of(run: Run, jobs: dict[str, str]) -> dict:
    """live_verdict's report over the run's records, as the ops report job reads them."""
    try:
        titles = live_verdict.titles(SPECS)
    except live_verdict.Bad:   # its check names the specs unreadable
        titles = None
    d = run.records
    return live_verdict.report(live_verdict.read_json(d / "results.json"), live_verdict.read_json(d / "begin.json"),
                               live_verdict.read_json(d / "pc.json"), live_verdict.read_json(d / "evidence.json"),
                               live_verdict.read_lines(d / "bursts.jsonl"), titles, jobs)


# ---- the browser and the bursts ----

def leave(run: Run) -> None:
    """Playwright still running is left to end by itself (its globalTimeout)."""
    if run.proc is not None and run.proc.poll() is None:
        LEFT_RUNNING.append(run.proc)
        gate.warn("the live specs still run: left to end by themselves (their globalTimeout; nothing is force-ended)")


def start_browser(run: Run, spec_env: dict[str, str]) -> subprocess.Popen | None:
    """`npx playwright test` of the live config in e2e/, without a PIN or
    another LIVE_* value, its output into the run's playwright.log; None (a
    warning) when it does not start."""
    env = {k: v for k, v in os.environ.items() if k not in g3.NO_PINS and not k.startswith("LIVE_")}
    env.update(spec_env)
    try:
        with open(run.records / "playwright.log", "w", encoding="utf-8") as log:
            return subprocess.Popen(["npx", "playwright", "test", "--config", SPEC_CONFIG], cwd=str(g3.E2E), env=env,
                                    stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
    except OSError as e:
        gate.warn(f"the live specs did not start ({type(e).__name__})")
        return None


def burst(job: gate.Job) -> int:
    """One burst, sent once the flag is checked; its exit code."""
    box = job.box
    box.ip.pause(box.ctx, 0)
    job.burst_sent = True
    try:
        box.call(list(BURST), change=True)
    except box.ip.StepError as e:
        gate.warn(f"a burst was not taken ({e})")
        return exit_of(e)
    return 0


def bursts_while(job: gate.Job, run: Run) -> str:
    """A burst every BURST_PERIOD_S while Playwright runs; pc.json's reason."""
    box, start, last = job.box, time.monotonic(), None
    while run.proc.poll() is None:
        now = time.monotonic()
        if now - start > BROWSER_LIMIT_S:
            return "not-finished"
        if last is None or now - last >= BURST_PERIOD_S:
            last = now
            add_burst(run, burst(job))
        box.ip.pause(box.ctx, LOOP_S)
    return "browser-done"


def pc(job: gate.Job, run: Run, spec_env: dict[str, str]) -> None:
    job.box.ip.pause(job.box.ctx, 0)   # a flag after the begin starts no browser
    run.pc, run.browser = "success", "failure"
    run.proc = start_browser(run, spec_env)
    if run.proc is None:
        write(run, "pc.json", {"reason": "browser-never-started"})
        return
    reason = bursts_while(job, run)
    write(run, "pc.json", {"reason": reason})
    if reason == "browser-done":
        run.browser_exit = run.proc.returncode
        run.browser = "success" if run.proc.returncode == 0 else "failure"
    else:
        leave(run)


# ---- the job ----

def begin(job: gate.Job, c: gate.Checks, run: Run) -> None:
    gate.begin_job(job, c)
    run.ready = c.health and c.rx_back and c.hil_on
    write(run, "begin.json", {"reason": "ready" if run.ready else "engine-not-up", "push_before": run.push_before})
    run.begun = True


def evidence(job: gate.Job, c: gate.Checks, run: Run) -> None:
    """pc-end's reads: the marker in the server's journal, the push count,
    the engine's own restarts (before the end's reset-failed clears them)."""
    box = job.box
    try:
        out = box.call(list(JOURNAL), timeout=JOURNAL_S)
        run.client_log = "found" if marker(run.number) in out else "missing"
    except box.ip.StepError as e:
        gate.warn(f"the server's journal could not be read ({e})")
    count = gate.alarm_count(box)
    run.push_after = count if count is not None else -1
    gate.read_restarts(box, c)


def finish(job: gate.Job, c: gate.Checks, run: Run) -> None:
    c.end_ok = gate.end_job(job, c, wait=True)
    ok = c.end_ok and c.hil_off
    run.end = "success" if ok else "failure"
    write(run, "evidence.json", {"job_end": "ok" if ok else "failed", "client_log": run.client_log,
                                 "push_after": run.push_after})


def live_job(job: gate.Job, c: gate.Checks, run: Run, spec_env: dict[str, str]) -> None:
    begin(job, c, run)
    if run.ready:
        pc(job, run, spec_env)
    gate.stop_burst(job, BURST_S)
    evidence(job, c, run)
    finish(job, c, run)


def cancelled(ip, c: gate.Checks, run: Run, left: bool) -> None:
    """A run the flag cut: its records as live.yml's cut jobs leave them,
    the verdict (cancelled), the line; never a status."""
    leave(run)
    if not run.begun:
        write(run, "begin.json", {"reason": "left-dev", "push_before": run.push_before})
    write(run, "pc.json", {"reason": "left-dev"})
    write(run, "evidence.json", {"job_end": "left-dev", "client_log": run.client_log, "push_after": run.push_after})
    v = verdict_of(run, {"begin": "success" if run.begun else "cancelled", "pc": "cancelled", "browser": "cancelled",
                         "end": "cancelled"})
    ip.emit({"gen3_gate": {"live": line(v, c, run, left, [])}})


# ---- the command ----

def run(ctx, ip) -> int:
    """`iempc gen3-gate live` (dev time, locked, while 3.x serves)."""
    s = settings(ip, ctx.env)
    box = gate.Box(ip, ctx, s.host, NAME)
    facts = gate.preflight(box, None)
    sha = box_build(box)
    refuse_other_public_build(ip, s.origin, sha)
    if facts.left:
        gate.end_left(box, facts)
    ended = "; the HIL job a dead run left was ended" if facts.left else ""
    gate.refuse_over_budget(ip, facts, 2, f"the job's begin and end: nothing was changed{ended}", NAME)
    push_before = gate.alarm_count(box)
    if push_before is None:
        raise ip.Refused(f"no live gate: the engineer's push subscriptions could not be counted (nothing was "
                         f"changed{ended})")
    job, c = gate.Job(box, facts), gate.Checks(rx_channels=len(facts.receiving))
    folder = Path(tempfile.mkdtemp(prefix="gen3-live-", dir=ip.state_dir()))
    try:
        try:
            tokens = g3.mint_tokens(ip, ctx, s.host, s.member, folder, TOKEN_SECONDS, "gen3-gate")
        except ip.StepError as e:
            raise ip.Refused(f"no live gate: the browser's tokens were not minted ({e}) (nothing was changed: no HIL "
                             f"job{ended})") from None
        number = g3.job_run()
        r = Run(number, records_folder(ip, number), push_before)
        spec_env = {"LIVE_BASE_URL": s.origin, "LIVE_SHA": sha, "LIVE_TOKENS": str(tokens), "LIVE_MEMBER": s.member,
                    "LIVE_TEST_INPUT": BURST_INPUT, "LIVE_TALKBACK_INPUT": s.talkback,
                    "LIVE_BURST_DBFS": str(BURST_DBFS), "LIVE_RESULTS": str(r.records / "results.json"),
                    "GITHUB_RUN_ID": str(number), "GITHUB_RUN_ATTEMPT": str(ATTEMPT)}
        try:
            live_job(job, c, r, spec_env)
        except ip.EventNow:
            gate.stop_burst(job, BURST_S)
            gate.end_job(job, None, wait=False)
            cancelled(ip, c, r, facts.left)
            raise
        except BaseException:
            leave(r)
            gate.stop_burst(job, BURST_S)
            gate.end_job(job, None, wait=True)
            raise
    finally:
        g3.drop_folder(folder)
    print(f"iempc: gen3-gate: the live run's records are {RECORDS}/{number} in iempc's state", file=sys.stderr,
          flush=True)
    v = verdict_of(r, {"begin": "success", "pc": r.pc, "browser": r.browser, "end": r.end})
    failed = failures(c, r)
    if v["conclusion"] != "success" and not failed:
        failed = ["verdict"]
    doc = line(v, c, r, facts.left, failed)
    ip.emit({"gen3_gate": {"live": doc}})
    passed = doc["pass"] == 1
    posted = gate.post(ip, sha, passed, DESC_PASS if passed else DESC_FAIL + ", ".join(failed), CONTEXT)
    return 0 if passed and posted else 1
