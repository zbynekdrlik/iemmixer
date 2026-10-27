# Gen 3 phase F: latency — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 3.x's engine path no longer than 2.x's: D ≤ 129 samples (2.x measured 129, 3.x 224 at 1 ms latency, phase E Task 1), with no clicks over two hours.

**Architecture:** D = B + P + W (`crates/iem-dante/src/stream.rs`). B = 32 stays. P (the device's RX latency) shrinks to what the real sources' packets need. W (the work margin) shrinks to what one block's processing plus the TX send needs. Both are measured first, on the box's current kernel, then with a core/IRQ plan, then on the real-time kernel. Nothing is bought unless a measurement shows the hardware is the limit.

**Tech Stack:** Rust (`iem-dante`), bash (`scripts/gen3/provision.sh`, `verify_box.py`), the box's kernel command line and GRUB, Ubuntu's `linux-image-realtime` (in the 26.04 archive, 7.0.0-14 on 2026-10-27).

**Spec:** `docs/superpowers/specs/2026-10-24-gen3-linux-dante-design.md` §5.4 phase F. Ticket #72: phase E's numbers, Live 4's overrun finding (12 overruns in 2 h, longest block 530 µs of 333 µs), the low-priority display poll item. `.claude/rules/gen3.md`.

## Facts this plan starts from (read 2026-10-27)

- The box: Intel N100, 4 cores, no SMT; kernel 7.0.0-38-generic, `PREEMPT_DYNAMIC`, running `full (lazy)`; empty kernel command line.
- The Dante NIC is a Realtek r8169 (not Intel, as the design assumed), one queue, one MSI-X interrupt on CPU0; RX/TX interrupt coalescing already 0. The NIC's NAPI thread runs at FIFO 60 on core 0 (phase D).
- Timing now: B 32, P 96 (1 ms), W 96 (`WORK_MARGIN`, a constant): D 224. Live 4's judge: the two RX flows' least lateness per second p99 600 µs and 173 µs.
- Target: P + W ≤ 97 samples (about 1 ms together).

## Choices made in this plan

1. **Measure before changing** *(my call)*. Each step below is judged by the same numbers: block time (max and p99.99 of `ctl status`'s pump), overruns, late and missed blocks, each real source flow's lateness (the judge), TX holes on the card's HIL return, 30 minutes per step and two hours at the end.
2. **W becomes a setting** *(my call)*. It is a constant today; a sweep must not need a build per value. It moves to the device config next to `latency_ns`, default 96, refused below B.
3. **The real-time kernel stands beside the generic one** *(my call)*. GRUB boots `realtime` by default, and the generic kernel stays installed as the fallback entry. Both stay held, like today.
4. **The band test wins** *(owner, 2026-10-27)*. On "prepni na 3.x" a measurement stops and `gen3-switch to-3` runs with the timing the box has at that moment. A kernel change happens only with a reboot in dev time, and the box serves the switch only after `verify_box.py` passes on the new kernel.
5. **The display poll item rides along** *(low priority, owner 2026-10-27)*. Once the RT core is isolated, the poll is turned back on for one 30-minute run. If the RT core stays clear, provision's `display` step is dropped.

## Global Constraints

- **P6:** no addresses, device or channel names, or band data in the repo; real values only in `iemmixer-ops`.
- **Dante rule:** only the IEM card and our own device change. Our device's RX subscriptions to the real sources are made with `iempc gen3-mirror`, which is our own device's change.
- **Owner's event signals:** the only gate. While `EVENT-NOW` exists, nothing on the box changes unless 3.x serves the band.
- **I8:** nothing is force-killed. A reboot of the box needs `# airuleset:destructive-ok <reason>`.
- **Tier 0:** no local cargo compilation; each code task ends with a push and a green CI run.
- **Buy nothing** (owner) unless a measurement shows the NIC or the CPU is the limit.

## Review Focus

- A W below the real block time: every block is late. `Timing` must refuse W < B, and the 30-minute run must show 0 late.
- A P below a source's real lateness: that source's samples arrive after their playout point. The judge's per-flow numbers must show the margin.
- The RT kernel lacking a driver or a feature the stack uses (r8169, `systemd-creds`, the NAPI thread's priority): `verify_box.py` must pass on it before any measurement.
- A boot that hangs on the new kernel: the generic entry must stay in GRUB and be reachable.

---

### Task 1: W as a device setting

**Files:** `crates/iem-dante/src/stream.rs` (`WORK_MARGIN` becomes the default), the device config's parser (next to `latency_ns`), `crates/iem-dante/src/stream/tests.rs`, the config's doc in `.claude/rules/gen3.md`.

- [ ] **Step 1:** Failing tests: a config with `work_margin = 48` gives `Timing::offset` 32 + P + 48; a config without it gives W 96; `work_margin` below 32 is refused with an error naming the key.
- [ ] **Step 2:** Push; the `test` job fails on them. **Step 3:** Implement. **Step 4:** Push; pass.
- [ ] **Step 5:** Commit `feat(dante): the work margin is a device setting (#72)`.

### Task 2: Baseline on today's kernel

- [ ] **Step 1:** Deploy the build with Task 1 to the box (test name, not the takeover), `iempc gen3-mirror` so our device receives the real sources.
- [ ] **Step 2:** 30 minutes at P 1 ms, W 96: record block max and p99.99, overruns, late and missed, each source flow's lateness p50/p99/max, TX holes.
- [ ] **Step 3:** Sweep: P 0.5 ms and 0.25 ms, W 64 and 48, 10 minutes each, the same numbers. Record the smallest pair with 0 late and 0 holes on #72.

### Task 3: Core and IRQ plan

- [ ] **Step 1:** Provision step `rtcores`: the kernel command line gets `isolcpus=managed_irq,domain,3 nohz_full=3 rcu_nocbs=3 irqaffinity=0-2`; the unbound workqueues' cpumask leaves core 3; the engine's RT threads are pinned to core 3 and the NIC's interrupt and NAPI thread stay on core 0. `verify_box.py` checks each part.
- [ ] **Step 2:** Reboot (destructive-ok marker), verify, then Task 2's 30 minutes and sweep again. Record on #72.

### Task 4: The real-time kernel

- [ ] **Step 1:** Provision step `rtkernel`: install `linux-image-realtime` and hold it; GRUB default = the realtime entry, generic kept. `verify_box.py` checks `PREEMPT_RT` is running and the generic entry is still there.
- [ ] **Step 2:** Reboot, verify, then Task 2's 30 minutes and sweep. Record on #72.
- [ ] **Step 3:** The display poll on for 30 minutes at the chosen timing. If the RT core stays clear, drop provision's `display` step (the #72 low-priority item).

### Task 5: The new timing, proven

- [ ] **Step 1:** Choose P and W from the best step's numbers, plus a margin of one packet. Target D ≤ 129. If no step reaches it, record why on #72; the NIC is the first suspect, and buying a part is the owner's decision.
- [ ] **Step 2:** Put P and W in the ops device config (the takeover config inherits them).
- [ ] **Step 3:** Two hours, Live 4's way: `live_guard.py --seconds 7200`, health every 10 minutes, 0 late, 0 missed, 0 holes, no alarm.
- [ ] **Step 4:** Phase E Task 1's loop: 3.x's engine path in samples against 2.x's 129. Record on #72 and tick phase F.
