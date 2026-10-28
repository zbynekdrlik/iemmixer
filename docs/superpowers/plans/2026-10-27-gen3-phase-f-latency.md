# Gen 3 phase F: latency — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** mic-to-ear through 3.x no longer than through 2.x, measured end to end (the stage's packet to the moment the stage rack plays it), with no clicks over two hours.

**Architecture:** The whole chain is levers, not only D = B + P + W (`crates/iem-dante/src/stream.rs`): the stage's packets reach our RX (kernel receive path), wait for the playout point P, one block B, the work margin W, then our TX holds each packet until its stamp + fpp + `TX_MARGIN_NS` (250 µs), and the rack plays it at the latency our device advertises (today 1 ms; the IEM card's is 0.25 ms). Each lever is measured on the box with the stage on, one change at a time. The order follows #72's earlier work: the Waves SoundGrid recon of 2026-10-24 and the packet-priority direction of 2026-10-26 (memory `packet-priority-not-hardware`).

**Tech Stack:** Rust (`iem-dante`: stream, media/tx, media/rx, device config), the box's kernel and NIC settings (`scripts/gen3/provision*.sh`, `verify_box.py`), Ubuntu's `linux-image-realtime` (in the 26.04 archive), netaudio reads of the receivers' flows.

**Spec:** `docs/superpowers/specs/2026-10-24-gen3-linux-dante-design.md` §5.4 phase F. #72: the SoundGrid recon addendum (2026-10-24), the owner's decisions of 2026-10-24 (function first, latency last, nothing bought now) and 2026-10-26 (packet priority, not hardware), phase B's facts on the card (latency 0.25 ms, its TX fpp 16), Live 4's stall analysis (the display poll). `.claude/rules/gen3.md`.

## Facts this plan starts from (read 2026-10-27)

- **Our chain today:** RX playout P 96 (1 ms), B 32, W 96, so D 224; TX sends a packet at its stamp + fpp + 250 µs; our device advertises 1 ms; the card asks our TX for fpp 32.
- **The IEM card (2.x):** latency 0.25 ms, TX fpp 16 to the rack.
- **Phase E's comparison** (2.x 129 vs 3.x 224 samples) covered the engine path only, stamp to stamp. It left out what the rack adds: it plays our flows at our advertised latency, the card's at 0.25 ms. The real gap is likely larger than ~1 ms, so Task 1 measures it end to end first.
- **The box:** Intel N100, 4 cores; generic kernel 7.0.0-38, `PREEMPT_DYNAMIC` `full (lazy)`; `linux-image-realtime` 7.0.0-14 in the archive.
- **The Dante NIC:** Realtek r8169 (older SoundGrid servers used Realtek too), one queue, one interrupt on core 0. Coalescing 0, flow control off, threaded NAPI on at FIFO 60 (phase D), `napi_defer_hard_irqs` 1, `gro_flush_timeout` 20 µs, busy poll 0. EEE enabled (inactive now: it is off only while the switch does not use it).
- **Live 4:** 12 overruns in 2 h, longest block 530 µs of 333. The 2 ms stalls were the i915 display poll; it is off on the box (provision `display`), and its root cause is a low-priority item here.

## Choices made in this plan

1. **Measure end to end, with the stage on** *(owner, 2026-10-27: a run with the stage off measured other sources)*. The probe is the loop through the card's spare HIL returns: card → our RX → engine → our TX → the card. Its round trip changes by exactly what a lever changes, receiver latency included. Every run also reads the receivers' flow latency (netaudio `flow receiver-list`) and our RX flows' worst lateness, with all the stage sources sending.
2. **The biggest levers first, cheapest first:** what the rack adds (advertised latency, TX send time, fpp) is a setting or a small code change, and probably worth more than the kernel work. Then P, then the kernel receive path, then PREEMPT_RT with isolated cores, which lets W shrink.
3. **Every setting is in the device config** *(Task 0 done: `work_margin`)*. RX playout, advertised TX latency, TX fpp and TX send margin become settings too, so a sweep needs no build.
4. **Nothing is bought** *(owner, 2026-10-24)* unless a measurement shows the r8169 or the CPU is the limit. Then the owner decides (an Intel I226 NIC is the candidate).
5. **The band test wins** *(owner, 2026-10-27)*: on "prepni na 3.x" a measurement stops and `gen3-switch to-3` runs with the timing the box has at that moment.

## Global Constraints

- **P6:** no addresses, device or channel names, or band data in the repo; real values only in `iemmixer-ops`.
- **Dante rule:** only the IEM card and our own device change. The card's spare HIL returns may subscribe to our TX for a test only, removed after (#72 exception). Receivers' flows are only read.
- **Owner's event signals:** the only gate. While `EVENT-NOW` exists, nothing on the box changes unless 3.x serves the band.
- **I8:** nothing is force-killed. A reboot of the box needs `# airuleset:destructive-ok <reason>`.
- **Tier 0:** no local cargo compilation; each code task ends with a push and a green CI run.

## Review Focus

- An advertised latency below our real send time: the rack plays silence or clicks. The rack's flow must show no late packets, and the loop must stay sample-exact.
- A P below a stage source's worst lateness: that source's samples arrive after their playout point (RX `late` > 0).
- A W below the real block time: blocks are late (`late` > 0, re-bootstraps).
- The RT kernel lacking something the stack uses (r8169, `systemd-creds`, the NAPI thread's priority): `verify_box.py` must pass on it before any measurement.
- A boot that hangs on the new kernel: the generic entry must stay in GRUB.

---

### Task 0: W as a device setting — done

- [x] `work_margin` in the device config, 32 to 4096 samples, default 96 (ce8495f, 598e9cf; CI green).

### Task 1: The real gap, end to end

- [x] **Step 1:** With the stage on, read every receiver flow from the card (the rack, FOH) and, in a dev-time takeover window or through the HIL loop, from our device: latency and fpp per flow. Record on #72. **Done 2026-10-27:** the IEM card and the stage rack play every flow at 0.25 ms; the card played ours at our advertised 1 ms (fpp 32), then at 0.25 ms (fpp 8) with `tx_latency_ns` (#72).
- [x] **Step 2:** The loop through the card for 2.x and 3.x (phase E Task 1's method), now with the receiver's latency inside the number. Record the real gap on #72. **Done 2026-10-28:** base 153 against 225 (the card plays our flow at 0.25 ms), 5 bursts equal (#72).

### Task 2: What the rack adds (settings and TX timing)

- [x] **Step 1:** Settings in the device config, defaults equal to today: `rx_playout_ns` (P, split from the advertised latency), `tx_fpp` (what our TX offers and grants), `tx_margin_ns` (today's 250 µs). Tests per setting: the parse, the bounds, and what the TX schedule and the flow grant use. **Done:** `tx_latency_ns`, `tx_margin_ns`, `tx_fpp_max` (91ee2ff); `latency_ns` stays the RX playout.
- [x] **Step 2:** TX sends a packet when its samples exist (the pump's block done), not at stamp + fpp + margin, if the measurement in Step 3 shows the hold costs latency at the receiver. Tests: the schedule's send time per fpp and margin. **Not needed (measured):** at 30 µs margin and fpp 8 the card's 0.25 ms holds with 0 TX restarts over 2 h on the realtime box.
- [x] **Step 3:** On the box: advertised latency 0.5 ms then 0.25 ms (the card's), TX fpp 16, margin swept down; the loop and the receivers' flows read each time; 0 late at the receiver, sample-exact loop. **Done:** 0.25 ms, fpp 8, margin 30 µs; send-time spread p99.9 50 µs (#72).

### Task 3: P from the stage's real lateness

- [x] **Step 1:** 30 minutes, stage on, every stage source flowing: each RX flow's worst lateness per second, the max over the run. **Done:** worst 14 (stage rack, fpp 4) and 26–27 (FOH card, fpp 16) on the realtime box.
- [x] **Step 2:** P = that max plus one packet; 10 minutes at it with 0 RX late. **Done:** P 36 (0.375 ms), 0 late over 2 h.

### Task 4: The kernel receive path (packet priority)

- [ ] **Step 1:** EEE off on the Dante NIC (`ethtool --set-eee … eee off`), in provision and verify.
- [x] **Step 2:** Busy polling on our RX sockets (`SO_BUSY_POLL`, `SO_PREFER_BUSY_POLL`, `napi_defer_hard_irqs` and `gro_flush_timeout` set for it) against today's threaded NAPI; measure the RX lateness and its spread (the judge) each way. **Measured, rejected for now:** on shared cores it trimmed the FOH flows a little and made 3 blocks late (#72).
- [x] **Step 3:** The NIC's interrupt and NAPI thread on a core of their own (with Task 5's isolation), above our audio threads and dantesync. **Done:** core 0 with the housekeeping, the engine's cores 2–3 isolated; `napi-priority` waits for the interrupt at boot (65a56d2).
- [x] **Step 4 (only if Steps 2–3 leave the receive path the limit):** early capture with AF_XDP for the audio flows (the Linux analogue of dantesync's pcap and of SoundGrid's RX in the interrupt). **Not needed:** the receive path is not the limit at P 0.375 ms.

### Task 5: PREEMPT_RT and isolated cores

- [x] **Step 1:** Provision step `rtcores`: `isolcpus=managed_irq,domain,3 nohz_full=3 rcu_nocbs=3 irqaffinity=0-2`, the unbound workqueues off core 3, the pump and TX/RX threads on core 3 (or 2–3), the NIC's interrupt per Task 4. `verify_box.py` checks each part. **Done as provision's `realtime` step** (0ed63eb) with `pump_cpu`/`io_cpu` in the device config (08915ca).
- [x] **Step 2:** Provision step `rtkernel`: `linux-image-realtime` installed and held, GRUB default to it, generic kept as fallback. Verify `PREEMPT_RT` runs. **Done in the same `realtime` step;** the box boots it by itself (verified after two reboots).
- [x] **Step 3:** After each reboot: verify, then 30 minutes; block time and overruns recorded on #72. **Done:** generic 90 overruns / 10 min, realtime 12, isolated 0 (#72).
- [x] **Step 4:** W swept down (64, 48, 40) at the chosen P: 0 late, 0 re-bootstraps per 10 minutes. **Done:** W 48, longest block 175 µs over 2 h.
- [ ] **Step 5:** The display poll on for 30 minutes with core 3 isolated. If the RT core stays clear, drop provision's `display` step (#72's low-priority item).

### Task 6: The new timing, proven

- [x] **Step 1:** The chosen settings into the ops device config (the takeover config inherits them). **Done:** iemmixer-ops PR #19.
- [x] **Step 2:** Two hours, Live 4's way, stage on: `live_guard.py --seconds 7200`, health every 10 minutes, 0 late, 0 missed, 0 holes, no alarm. **Done 2026-10-28:** 21.6 M blocks, 0/0/0/0, RX 0 late, TX 0 restarts, no holdover (#72).
- [x] **Step 3:** Task 1's end-to-end loop again: 3.x against 2.x. Record on #72 and tick phase F. If 3.x is still later, the remaining gap and its cause go on #72 (the NIC first suspect; buying a part is the owner's decision). **Done 2026-10-28:** 269 at F = D against 449 before; 3.x 140 against 2.x 153 on phase E's footing (#72).
