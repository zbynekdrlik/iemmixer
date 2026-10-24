# Gen 3: a Linux engine that is its own Dante device — design (epic #72)

Status: draft for the owner's reading, 2026-10-24. Decisions are the owner's from #72 (`DECIDED` comments, brainstorm of 2026-10-23 and 2026-10-24). Everything marked *(my call)* is the designer's and open to the owner's correction.

## 0. Zhrnutie pre vlastníka

- **Čo je 3.0:** iemmixer beží na Linuxe a sám je Dante zariadením. Žiadna Dante karta, žiadny Windows, žiadny REAPER, žiadny PipeWire.
- **V Dante Controlleri** je to bežné zariadenie: vstupy kapely a výstupy mixov sa smerujú v patchbaye ako dnes s kartou.
- **Pracujeme ako pri 2.0:** na našich skutočných linuxových boxoch v našej Dante sieti, s testami na živom hardvéri. Začíname na dev boxe, ktorý v Dante sieti už je.
- **Najprv musí fungovať celok** (zariadenie v Dante Controlleri, zvuk dnu a von, mixovanie, stránky členov). **Latencia sa doťahuje až na konci.** Teraz sa nič nekupuje.
- **Kapela nesmie nič spozorovať:** rovnaká latencia od mikrofónu po ucho ako dnes, žiadne praskanie. Nižšia latencia nie je cieľ.
- **Nasadenie ako pri 2.0:** nová krabička beží vedľa dnešného PC, potom prepnutie, dnešné PC ostáva istý čas ako záloha.

## 1. Intent and why

Today's engine runs on a Windows PC with a Dante PCIe card through ASIO. The card has no Linux driver, and the Windows appliance needs constant work to stay quiet (S1c). The owner wants the engine on a realtime Linux, speaking Dante itself, the way Waves SoundGrid servers do low-latency network audio on standard x86 hardware with a plain PREEMPT_RT kernel (recon on #72, 2026-10-24).

Success: the band notices nothing. The engineer and the members use the same pages, the same lineups and the same saved mixes; only how the engine runs and moves audio changes.

## 2. Goals, non-goals, anti-goals

**Goals**

- G1. The engine runs on Linux and is a Dante device in software: it appears in Dante Controller, receives the band's inputs and transmits the mixes, routed in the patchbay.
- G2. It can take over the IEM card's device name and TX channel names, so the IEM transmitters' subscriptions (stored by name) find it without re-patching.
- G3. Mixing, members' pages, lineups, saved mixes, talkback, listen and the tunnel work as in 2.x. Server, UI and the engine protocol stay; the audio backend changes.
- G4. The same mic-to-ear latency as today's card at B = 32, with no clicks. Reached in the last phase.
- G5. A dedicated headless box (N100 class) in the rack, built on camera-box's appliance machinery. Not bought before the function is proven.

**Non-goals**

- Lower latency than today.
- Dante redundancy (primary/secondary), AES67 mode, sample-rate changes from Dante Controller.
- A Dante Domain Manager setup or anything that changes other Dante devices.
- Certification by Audinate. The box is described as an unofficial device compatible with a subset of Dante.

**Anti-goals** (failure even if it technically works)

- PipeWire, JACK or any desktop audio server in the path.
- "PTP hacks": a second clock owner on the box, or steering the box's wall clock in a way that fights NTP.
- Containerised stand-in Dante devices as the proof of function (owner, 2026-10-24). Function is proven on the real Dante network.
- Touching another Dante device's subscriptions or settings (the standing Dante rule; only our own device is ours).

## 3. Constraints

- **Program rules hold:** P6 (no site data in this repo), Tier 0 (no local cargo compilation, CI builds), the RT contract I7 (no allocation, lock, log or syscall in the audio callback; `assert_no_alloc`, rtsan, bench), nothing force-killed (I8), the owner's event signals.
- **Licence:** the engine binary is already GPL through `iem-limiter-mga` (D1a). Dante protocol code may live in a GPL crate the engine links, and may take over or adapt parts of inferno (`GPL-3.0-or-later`) with attribution. Server, UI and guard stay separate permissive processes.
- **Clock owner:** one PTP owner per box. dantesync and our own Dante clock never run side by side on the same box.
- **Network:** a NIC from Intel (igb/igc family) is the target, as SoundGrid's servers show (Realtek-based servers dropped packets). Hardware timestamps are welcome, not required (see §5.2).

## 4. What exists to build on (recon on #72)

- **camera-box:** Ubuntu appliance installer, setup and verify gate; read-only root; deploy from CI artifacts with byte-verify; remote logs, dead-man timers; per-thread `SCHED_FIFO|RESET_ON_FORK`, `mlockall`; N150-class boxes. Not there: a PREEMPT_RT kernel, NIC tuning, latency measurements.
- **dantesync:** PTPv1 follower (MIT) with a pure phase-lock servo, frequency-step detector, spike filter. A header-parse bug is handed to camera-box for confirmation (camera-box #1471).
- **inferno:** a working map of the Dante protocol, interop-tested with Dante Controller: mDNS records, ARC control (UDP 4440), flow control (UDP 4455), the audio packet (9-byte header: PTP seconds and sample offset, then big-endian PCM), keepalives, heartbeat. Latency below 1 ms was never done there.
- **SoundGrid servers:** plain PREEMPT_RT, Intel NICs, interrupt moderation off, audio receive outside NAPI batching, non-audio traffic in a low-priority thread, preallocated buffers, a dedicated network with EEE off.
- **iemmixer 2.x:** the engine core and `Process`/`Block` interface in `iem-audio-io` are platform-neutral; `NullRt` and `Offline` already run on Linux; the ASIO backend is the only Windows-bound audio part. The guard, tray and PC tuning are Windows-specific.

## 5. Design

### 5.1 Shape

```
stagebox ──Dante──▶ [ box: dante crate ─RX ring─▶ engine core (Process) ─TX ring─▶ dante crate ] ──Dante──▶ IEM transmitters
                          ▲ PTP follower, media clock            ▲ control pipe (as today)
                          │                                       └── iem-server (UI, members, tunnel) unchanged
```

- A new backend in `iem-audio-io` (*(my call)* `dante` module, Linux-only) drives `Process` from the network instead of an ASIO callback. The engine core, DSP, state and protocol stay as they are.
- A new crate (*(my call)* `iem-dante`, GPL-3.0-or-later) holds the device: discovery, control, flows, the packet codec, the subscription manager and the clock. It knows nothing about mixing.
- The rest of the Windows-only parts (guard, tray, PC tuning) get Linux counterparts only where 3.0 needs them; systemd takes the supervisor's process role *(my call; the guard's lifecycle rules carry over in substance)*.

### 5.2 Clock and timing

- The box follows the Dante PTPv1 grandmaster with its own follower, reusing dantesync's servo ideas and pure modules. PTP time is kept as the box's own clock model, not by steering the wall clock *(my call)*.
- **Sample alignment comes from the packets, not from the box's clock.** Every Dante audio packet carries its media time (PTP seconds + sample offset), stamped by the sender's hardware-locked clock. The box places received samples by those timestamps and stamps its TX packets as RX media time plus a fixed processing offset. Its own clock only needs to be good enough to send each packet early enough for receivers' latency setting, to show as synced in Dante Controller, and to keep TX running with no RX flow. Software timestamps at dantesync's level (under 50 µs) are enough; a NIC PHC is a later refinement.

### 5.3 Dante device

Ordered by what makes the device usable:

1. mDNS advertisement and browsing (`_netaudio-arc`, `-cmc`, `-chan`), device and channel names.
2. ARC control: names, channel lists, renames, subscriptions, the reply Dante Controller needs to avoid a clock-domain warning.
3. Flow control: answering a receiver's flow request (we transmit), requesting flows from the stagebox (we receive), keepalives.
4. The audio codec and the RX/TX path with a jitter buffer.
5. Heartbeat and info multicast so Dante Controller shows the device healthy.
6. The subscription manager: name lookup, grouping into flows, retry after a device reboots, persisted state.

### 5.4 Phases (function first, latency last)

| Phase | Done when | Where |
|---|---|---|
| A. Device visible | Our device appears in Dante Controller with the right name and channels; renames work. | dev box in the Dante network |
| B. Audio through | A real Dante device's channel is received, and our TX is heard by a real receiver, sample-exact against the sender (a send-vs-return check like songplayer's loop check). | dev box |
| C. Engine on Linux | iemmixer's engine runs on Linux with the Dante backend; the server, UI and a member's mix work end to end at a relaxed latency. | dev box |
| D. The box | A camera-box-style appliance image runs the 3.0 stack headless; deploy, logs and health checks work. | a Linux box we have *(which one: decided when the phase starts)* |
| E. Beside today's PC | The box runs beside today's PC on spare or test channels, never a band member's channel, with HIL and live tests as in 2.x. | Dante network |
| F. Latency | PREEMPT_RT, core and IRQ plan, NIC tuning (Intel, moderation off, audio queue), small packets, the B = 32 cycle; measured mic-to-ear equal to today. Hardware bought only if this phase needs it. | the box |
| G. Cutover | As in 2.0: the owner's signal, today's PC kept as fallback for a while. | — |

### 5.5 Clock on the dev box, events, names *(my calls, from the builder check)*

- **Dev box clock (phases A–C):** the dev box keeps its dantesync; our device reads time from the steered system clock there and never runs a second PTP owner. Our own PTP follower is built and proven on the 3.0 box (phase D), where dantesync is not installed. Sample alignment comes from packet timestamps anyway (§5.2).
- **Never a grandmaster:** our device never sends PTP Announce or Sync in any phase. It is a follower only, so it can never win the BMC on the band's network.
- **Owner's event signals:** "ide event" freezes all 3.0 work, as it freezes PC work today, and takes our experimental device off the network (its service stopped) until "event skončil". After the cutover the box follows the same lifecycle rules as today's PC.
- **Names:** until the cutover the device runs under its own test name, and the IEM card keeps its name. At the cutover the card goes offline first, then the box takes the card's device name and TX channel names. Going back is the reverse, through one tool command driven by the owner's signal (the `iemmode event` analogue). The box persists its own RX subscriptions.
- **Baseline:** before phase F, today's settings are read (read-only) from the IEM card and the receivers, and today's mic-to-ear latency is measured with the existing HIL loop. That number is phase F's target. The receivers' latency settings bound our TX timing in every phase, so they are read before phase B.
- **Protocol capture:** no switch mirror port. Reference traffic comes from what our boxes legitimately see: multicast from any box on the subnet, ARC on the Dante Controller PC, and the flows our own device takes part in. inferno fills the rest.

### 5.6 Testing

- Unit and protocol tests run in CI on Linux (packet codec, mDNS records, ARC messages, servo, the RT contract as today).
- Function is proven on the real Dante network: the dev box against real Dante devices and Dante Controller, the same way 2.x proved itself with HIL and live tests on the IEM PC.
- Every live test uses spare or test channels. Our device's own RX subscriptions are ours to set (receiving from the stagebox takes one of its flow slots). The receiver for our TX in phases B and E is the IEM card's spare HIL return inputs (owner, #72, 2026-10-24). They subscribe to our device's test TX only for a test, never on a band member's channel, and the subscription is removed afterwards. No other device's subscriptions are changed.

## 6. Left to the builder

- Module and crate names, the internal ring and thread layout, the jitter-buffer size during phases A–E.
- Which parts of inferno are adapted and which are rewritten, as long as each adapted file keeps its attribution and lives in the GPL crate.
- The order of work inside each phase, and how the phase-B sample check is built.

## 7. Risks

- **Protocol gaps:** fields inferno names `unknown*`; Dante Controller's strictness. Mitigation: capture the real IEM card's traffic as reference (read-only), compare field by field.
- **Name takeover:** the old card and the box must never be online under the same name at once.
- **Subnets:** the stagebox and the IEM card sit on a different routed subnet than the dev box. Phase B may need a box with a leg in that subnet; resolved when phase B starts, without changing other devices.
- **Latency:** unproven at B = 32 on a small box until phase F. Accepted by the owner: function first, better hardware if needed.
