# Gen 3 phase H: the live gates on the box — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the box that serves the band gets the three gates the PC has (HIL, the live E2E in a real browser, a soak), each with a public verdict on the build it runs, before 3.x is confirmed (phase G).

**Architecture:** the dev box drives the box over ssh (`iempc gen3-gate hil|live|soak`, beside `gen3-mix`): the box has no guard, the dev box's `EVENT-NOW` is the event gate, and "ide event" keeps 3.x running. The engine's existing HIL test mode (its `--test-signal` flag, the `HilTestSignal` command, every mix TX written as zero during a burst, the sine only on the spare HIL outputs) is reached through a new Linux supervisor client on the engine's control channel, with the guard's caps. A box "HIL job" is a runtime systemd drop-in adding the engine's test flags for the run. The verdicts reuse `live_verdict.py` and `soak_verdict.py`; the results go out as commit statuses `hil/gen3`, `live/gen3`, `soak/gen3`.

**Tech Stack:** Rust (`iem-engine` control protocol, a small supervisor CLI), Python (`scripts/iem-pc/iempc_gen3*.py`, the verdict scripts), Playwright (`e2e/tests/live/*.spec.ts` unchanged), systemd (runtime drop-ins), bash (`sudo-lines.txt`).

**Spec:** #72's finding and design brief of 2026-10-28 (the owner's question: did Gen 3 lower the gates?). The PC's gates: `.claude/rules/live.md`, `soak.md`, `guard.md`, the ops workflows `live.yml`, `hil.yml`, `soak.yml`. The box: `.claude/rules/gen3.md`, `gen3-new-box.md`.

## Choices made in this plan

1. **No GitHub runner on the box** *(my call)*: nothing would stop a runner on "ide event", and the dev box already drives the box over ssh with the event flag checked.
2. **The engine's own HIL mode, not the device's test sources** *(my call)*: `iem-dante-dev ctl source` only replaces a TX channel and is audible on an in-ear; the engine's HIL mode zeroes every mix TX during a burst and puts the sine only on the spare outputs, so no in-ear hears it.
3. **Dev time only, while 3.x serves.** During a burst every in-ear is silent for its 30 s; the flag is checked before every burst, a flag ends the burst (`StopTestSignal`) and the job. A live run lasts about 40 minutes.
4. **The PC's gates refuse while 3.x serves** (done first: `iempc.WHILE_2X_SERVES`), and the box gates refuse while 2.x serves.

## Global Constraints

- **P6:** no names, addresses, channel numbers or PINs in the repo; the ops env and ops files hold them.
- **Dante rule:** only the IEM card and our own device change; the box HIL's loop is a self-subscription of our own device.
- **I8:** nothing is force-killed; an engine restart goes through systemd, never more than two in ten minutes (`reset-failed` before a deliberate start).
- **Tier 0:** no local cargo compilation; CI builds and tests.
- **Owner's signals:** never ask whether an event runs; the flag is the gate.

## Review Focus

- A burst that reaches an in-ear: the HIL mode must be on (the engine started with `--test-signal`) before any `HilTestSignal`, and the mixes must read zero during it.
- A job that does not end: a dev box that dies mid-run leaves the drop-in in `/run` (gone at the next boot) and the test flag on; the next `gen3-gate` and `deploy.sh` must end a left job first.
- The restart budget: the job's begin, a panic test and the job's end are three starts in ten minutes.
- A site whose `hil_tx` is above the device's TX count: an engine start with `--test-signal` exits 3 and stays down; refused before any restart.

---

### Task 1: The PC's gates refuse while 3.x serves — done

- [x] `iempc.WHILE_2X_SERVES` (`dispatch-live`, `dispatch-hil`, `dispatch-soak`, `gen3-loop`, `gen3-mix`), red 86eab95, green 9220073.

### Task 2: The box's spare HIL outputs

- [ ] **Step 1:** The box device config's TX list covers the site's `[guard] hil_tx` (the takeover gives each the card's label). A `gen3-gate` preflight refuses when any `hil_tx` is above the device's TX count. Tests in `test_iempc_gen3.py`. The preflight is done with Task 6 (`iempc_gen3_gate.counts_refusal`: also each spare output's D5(b) return within the RX count, and `iem-engine check-site` of the installed site; tests in `test_iempc_gen3_gate.py`); open: the ops device config's coverage, read on the box by the first run (Task 6 Step 2).
- [ ] **Step 2:** A read-only survey that no receiver but our own device subscribes to those TX channels (the rack, FOH), recorded on #72.

### Task 3: A supervisor client on Linux

- [x] **Step 1:** `iem-engine supervise test-signal <input> <dbfs> <seconds> [--listen]`, `supervise stop-test-signal`, `supervise status`: a client of the engine's control channel with the supervisor role (the guard's framing reused), the guard's caps (at most −20 dBFS, at most 60 s), refused when the engine runs without `--test-signal`. Tests against the control loop in-process (`crates/iem-engine/src/control/tests`). Done 5e36a39: `crates/iem-engine/src/supervise.rs` (`[--pipe <socket>]`, default the box unit's `/run/iemmixer/engine.sock`; the engine's own `iem_engine_proto` frames through `pipe::Framer`; the burst on every spare output of the engine's first `Status.hil`; 1 to 60 whole seconds; exit 0/1/2, 2 also off Unix), tests in `supervise/tests.rs`, `control/tests/supervise.rs`, `tests/pipes/binary.rs`; `.claude/rules/gen3.md`.
- [x] **Step 2:** The sudo lines to run it as the service user. Open: each line is an exact command (`sudo-lines.txt` has no placeholder for an input, a level or a length), so it needs the gate's fixed burst values, which Tasks 5 and 6 choose. Done with Task 6: `supervise status`, `supervise test-signal mic1 -30 10` (HIL v1's input, level and length) and `supervise stop-test-signal` as `iemmixer`; Task 5's live burst (30 s every 60 s) needs its own line.

### Task 4: The box HIL job

- [x] **Step 1:** `gen3-gate` begin: refuses with `EVENT-NOW`, while 2.x serves, or with a job left over; writes `/run/systemd/system/iem-engine.service.d/iem-hil.conf` (the test flags), `daemon-reload`, `reset-failed`, restart, waits healthy and the RX flows back. End: removes it, restarts, waits healthy. A left job is ended first by the next `gen3-gate` and by `deploy.sh`. Done: `iempc_gen3_gate.begin_job`/`end_job`/`end_left` (`--test-signal` only: the panic is not covered, below), the restart budget (`gen3-gate.json`), deploy.sh's `rm -f` of a left drop-in; `.claude/rules/gen3.md`, the box's HIL gate.
- [x] **Step 2:** Tests with the box's commands as stand-ins (as `test_iempc_gen3.py` does). Done: `scripts/iem-pc/test_iempc_gen3_gate.py`, `test_deploy.py`'s left-job tests.

### Task 5: `gen3-gate live`

- [ ] **Step 1:** Tokens minted on the box for the run's length (new sudo lines), the live specs with `LIVE_*` from the ops env against the band's public URL, a 30 s burst every 60 s through the supervisor client, the evidence (the client-error marker in the journal, the push count), `live_verdict.py report`, the status `live/gen3` on the box's build.
- [ ] **Step 2:** A run on the box in dev time, recorded on #72.

### Task 6: `gen3-gate hil`

- [x] **Step 1:** In a HIL job: versions and health, the D5(b) loop (a spare TX into our own spare RX by self-subscription, `Status.loopback_samples` and the spare outputs' peaks), the test signal at its level then silent, a panic (`InjectFault`, systemd starts it again), the alarm path's count; the status `hil/gen3`. Done: `iempc gen3-gate hil` (`iempc_gen3_gate.py`), every check but the panic, reported `panic: "not_covered"`: `iem-engine supervise` has no fault injection (a Rust change: an `inject-fault` command and its sudo line, then `--fault-injection` in the drop-in and a third start in the budget).
- [ ] **Step 2:** A run on the box, recorded on #72.

### Task 7: `gen3-gate soak`

- [ ] **Step 1:** `iem-soakclient` on the box (a transient unit as the service user, at most 2 h) beside polls of `health` and `ctl status`; the verdict `soak_verdict.py harness` plus Live 4's poll criteria (pump late / missed / re-bootstraps 0, no unit restart, no clock re-anchor); the status `soak/gen3`.
- [ ] **Step 2:** A run on the box, recorded on #72.

### Task 8: Wired in

- [ ] **Step 1:** `deploy.sh` names the three gates to run after a box deploy; `.claude/rules/gen3-new-box.md` section 7 uses them; phase G's confirmation needs the three statuses green on the box's build.
