# Gen 3 phase B: audio through our Dante device — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Our device receives a real Dante device's channel, and a real Dante receiver hears our TX, sample-exact against the sender: a send-vs-return loop whose round trip moves by exactly the samples we tell it to.

**Architecture:** `iem-dante` gains a media clock (PTP time read from the box's dantesync, never a PTP owner of its own), flow control on UDP 4455 (server for our TX, client for our RX), the audio packet codec, a jitter ring per RX channel, TX sources (silence, counter, sine, echo) with checkers, a minimal subscription manager and the ARC and info answers that list flows and subscriptions. Two media threads (TX, RX) beside the existing tokio control side. A local control socket gives status and sets TX sources. No engine yet (phase C).

**Tech Stack:** Rust (workspace toolchain 1.98.1); tokio for the control side; std threads, `mio` (poll over the RX flow sockets) and `libc` (`clock_gettime`, `clock_nanosleep`, `sched_setscheduler`) for the media threads, both already in the lockfile; `mdns-sd` 0.21 for resolving `_netaudio-chan` records (and, for a device that publishes none, its `_netaudio-arc` and `_netaudio-dbc` records and an ARC read of its channels, Task 9b); dantesync (MIT, camera-box's PTPv1 follower) as the box's clock; Python for the deploy, guard and loop scripts.

**Spec:** `docs/superpowers/specs/2026-10-24-gen3-linux-dante-design.md` (§5.2 clock, §5.3 steps 3–6, §5.4 phase B, §5.5 dev-box clock, grandmaster, events, names, §5.6 testing). Ticket #72. Phase A: `docs/superpowers/plans/2026-10-25-gen3-phase-a-dante-device.md`, `.claude/rules/gen3.md`. Reference: teodly/inferno at `9767558` (`device_server/{flows_tx,flows_rx,flows_control_server,channels_subscriber,tx_multicasts,arc_server,info_mcast_server}.rs`, `protocol/{flows_control,proto_arc,mcast}.rs`, `media_clock.rs`, `ring_buffer.rs`); dantesync `src/time_server.rs`, `src/date_offset.rs` and `wire.rs`, `src/clock/linux.rs`, `install.sh`.

## Choices made in this plan

**The clock (design §5.2, §5.5).** dantesync does not keep `CLOCK_REALTIME` at PTP time. It holds `wall = PTP + D`, where PTP is the grandmaster's uptime and `D` is a fleet date offset that changes: by default one coordinated step a night of whole 200 ms, in its micro mode a step or slew every ~20 s. Reading `CLOCK_REALTIME` as PTP time would jump our timestamps by up to 19 200 samples at 96 kHz. So:

- PTP ns = `CLOCK_MONOTONIC` + k. `CLOCK_MONOTONIC` follows dantesync's frequency word (so it runs at the grandmaster's rate) and no step ever moves it (dantesync's own step law uses it as the reference for that reason).
- k = (`CLOCK_REALTIME` − D) − `CLOCK_MONOTONIC`, read once a second, with D taken from dantesync's own time server on the box (UDP 31900, the `DSYX` query: its wall and its PTP "now" give the D in effect; the extension names a scheduled step or slew). A D step moves the wall and D together, so k does not change.
- k is accepted only when dantesync says locked, no step is due within ±2 s and no slew runs. A different k must hold for 3 polls before it replaces the old one (a grandmaster change, a dantesync re-acquisition); each replacement is a counted, logged re-anchor, and the media threads re-align.
- Media time = PTP ns × rate / 10⁹ (integer, u128). Sample placement comes from the packets' own timestamps (§5.2); our clock decides only when we send and how late a packet arrived.
- dantesync measures no path delay, so our clock trails the grandmaster by the one-way delay (0.1–0.5 ms). That is the safe side for TX (inferno even stamps 0.5 ms in the past on purpose) and shows in the RX lateness we report.

**The loop test (phase B "done when").** The receiver for our TX is the IEM card's spare HIL return input (owner, #72, 2026-10-24). The options weighed:

| Option | Proves "a real receiver hears our TX, sample-exact" | Cost |
|---|---|---|
| A second instance of our device on the box | No: our code at both ends; also needs a second address in the band's subnet | an IP, a second service |
| Our RX subscribed to our own TX through the NIC | No, but a bit-exact self-check of our whole path on the real network | none: kept as Task 9 |
| The PC engine echoes the return input to a spare output, and we compare | Yes, bit for bit | new RT engine code and a PC bundle on the band's prod PC |
| **Our device echoes the card's spare HIL TX back to the card's spare HIL return; the PC's existing D5(b) latency probe measures the round trip** | **Yes: the round trip must be identical burst after burst and move by exactly ΔF when we change our echo offset by ΔF samples** | **one owner action, existing PC tooling (HIL job, `iemmode test-signal`, `Status.loopback_samples`)** |

Picked: the last row. Our RX 1 subscribes to the card's spare HIL TX X (ours to set). Our TX 1 echoes RX 1: the TX sample stamped T is the RX sample stamped T − F. The owner subscribes the card's spare HIL return X to our TX 1 in Dante Controller for the test and sets it back after. The engine's HIL sine on spare output X goes card → us → card, and its `LatencyProbe` reports the round trip in samples. Every Dante receiver plays a sample at its timestamp plus its latency, so the round trip is a constant plus F: identical over bursts, and exactly F₂ − F₁ apart between two offsets. What it does not check bit for bit is the content inside the card's return path (only at the onset sample); our own legs are checked bit for bit (Task 9's counter, Task 10's sine check, Task 3's echo tests).

**Other calls (mine):** RX sources in phase B are only the card's spare HIL TX and our own TX, never the stagebox (no band channel, no stagebox flow slot). Subscriptions are set through ARC 0x3010 on our own device (netaudio aimed at our device, or Dante Controller), persisted in the state file; no subscription list in the config. TX sources are set at runtime through the local control socket and never persisted: after a restart every TX channel is silent. From phase B the service is not enabled at boot, so a box reboot during an event never brings flows back unattended.

## Global Constraints

- **P6:** no real names, hosts, IPs, device or channel names or channel numbers in the repo. Tests use `test-device`, `peer-device`, `TX 1`…, `10.0.0.10`/`10.0.0.11`, MACs `02:00:00:00:00:0a`/`…:0b`. The card's spare HIL TX/return is "spare X" here; its real name, the fleet's NTP master and the grandmaster's address live in `iemmixer-ops` and the private env. A captured packet enters a test only with names, address and MAC replaced, scanned before the commit.
- **Dante rule:** our device only. Our RX subscriptions are ours (set on our device). The IEM card's spare HIL return is changed only by the owner in Dante Controller, only for the Task 11 test, set back right after. No band member's channel in any test. No other device's subscriptions or settings are changed. Reads of other devices (mDNS, ARC reads) are allowed.
- **Inferno parity:** every 4455 and new ARC answer is the one inferno gives, byte for byte (a refused request Dante Controller asks again at once: #72, 2026-10-26). Deviations only where inferno crashes or answers twice; each is listed in its task and leaves the requester the same outcome.
- **Our requests never loop:** at most one flow request per transmitter in flight and per 9 s, keepalives exactly 4 per second per flow; enforced in code, not by the guard.
- **Clock:** dantesync is the box's one PTP owner, as a follower. Our device never opens UDP 319/320 and never sends PTP; it can never be a grandmaster (design §5.5). Phase D replaces dantesync with our own follower.
- **Events:** "ide event" stops `iem-dante` (phase A's stop), which now ends every flow. dantesync keeps running (receive-only, like the fleet's other followers). Every script here refuses while `EVENT-NOW` exists; the PC side runs only in dev time (`iempc` dev-time commands).
- **Tier 0:** no local cargo compilation. Each task ends with a push to `dev` and a green CI run (test, clippy, coverage floor, `mutants-list`) before the next task builds on it (no stacked unrun code). Deploys use the `dante-bin` artifact of that push with its sha256.
- **Licence:** files adapted from inferno keep the GPL origin header as in phase A; the DSYX codec adapted from dantesync keeps its MIT copyright and permission notice in the header. `iem-dante` stays no dependency of `iem-engine` (`scripts/check_engine_deps.py`).
- **Logs** name counts, ports, opcodes, channel numbers, flow slots and peer ids (phase A's gap-guard ids), never a name, an address or a MAC. The control socket's status is numbers only.
- **Hot path** (packet build and parse, ring write and read, sources, checkers): preallocated, no allocation, no lock, no log. The media threads ask for `SCHED_FIFO` when `RLIMIT_RTPRIO` allows, else run normally and say so once.

## Review Focus

1. Our outgoing traffic is bounded whatever a peer does: a transmitter that refuses or never answers gets at most one request per 9 s; keepalives stay at 4 per second per flow; a stop on shutdown is one datagram per flow within 1 s.
2. 4455 parity: the handle, 0x0301, 0x0302, 0x0315, 0x0103 and "no reply" exactly where inferno gives them; inferno's panics (a missing string, channel ids past the datagram) and its double reply (0x0302 then a handle) become one answer or none, counted. A fuzz test feeds random datagrams.
3. Timestamps: a packet stamped T carries the source's samples T…T+fpp−1 and is sent no earlier than T + fpp duration; RX places by stamp across the 32-bit seconds wrap and a grandmaster uptime of years; the echo is exact (TX T ← RX T − F).
4. Clock: a `CLOCK_REALTIME` step of ±200 ms with the matching D moves nothing; a different k needs 3 polls; no packet leaves before the clock is ready; a re-anchor re-aligns TX and is counted.
5. ARC pages with 31-byte names everywhere and full subscriptions fit one datagram and list every channel and flow exactly once (16 RX channels with three names each no longer fit at phase A's page size: pages end early with `0x8112`, as Dante Controller steps by the count each page reports).
6. Stop: a SIGTERM with live flows ends within the unit's 10 s bound, transmitters get their stop, nothing restarts at boot.

---

### Task 1: The media clock from dantesync (`clock`)

**Files:**
- Modify: `Cargo.toml` (version, first commit), `crates/iem-dante/Cargo.toml` (`libc`), `crates/iem-dante/src/lib.rs`
- Create: `crates/iem-dante/src/clock.rs`, `crates/iem-dante/src/clock/dsyx.rs`, `crates/iem-dante/src/clock/tests.rs`, `crates/iem-dante/src/clock/dsyx/tests.rs`

**Interfaces:**
- Produces:
  - `dsyx::request(id: u32) -> [u8; 64]`, `dsyx::parse(reply: &[u8], id: u32) -> Result<Reading, DsyxError>`
  - `pub struct Reading { pub wall_ns: i64, pub locked: bool, pub settled: bool, pub mode: u8, pub freq_ppb: i32, pub date: Option<DateState> }`
  - `pub struct DateState { pub d_ns: i64, pub step_at_ptp_ns: Option<i64>, pub slewing: bool }` (D in effect = wall − the extension's PTP "now")
  - `pub fn sample(r: &Reading, wall_ns: i64, mono_ns: i64) -> Sample`, `pub enum Sample { Usable { k_ns: i64 }, NotLocked, NoDateState, StepNear, Slewing }`
  - `pub struct Tracker` with `feed(&mut self, s: Sample) -> Event`, `pub enum Event { Ready { k_ns: i64 }, Held, ReAnchored { delta_ns: i64 }, Holdover, NotReady }`; `TOLERANCE_NS = 20_000`, `CONFIRM = 3`, `STEP_GUARD_NS = 2_000_000_000`
  - `pub trait PtpClock: Send + Sync { fn now_ns(&self) -> Option<u64>; fn sleep_until(&self, ptp_ns: u64); fn epoch(&self) -> u32; }` (epoch counts re-anchors); `MonoClock` (Linux: `CLOCK_MONOTONIC` + an atomic k, `clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME)`); `ManualClock` for tests (time moves only in `sleep_until`)
  - `pub fn to_samples(ptp_ns: u64, rate: u32) -> u64`, `pub fn stamp(samples: u64, rate: u32) -> (u32, u32)`, `pub fn from_stamp(secs: u32, sub: u32, rate: u32, near: u64) -> u64` (the full sample index nearest `near`)
  - `pub async fn poll_dantesync(clock: Arc<MonoClock>, freq: Arc<AtomicI32>)`: 127.0.0.1:31900 once a second, a 200 ms reply timeout; logs ready, re-anchor (delta ns), holdover, never an address

- [ ] **Step 0:** First commit: bump `[workspace.package].version` above `main` (`2.0.0-dev.43` if main is still `.42`). Commit: `chore: bump version to 2.0.0-dev.43 (#72)`.
- [ ] **Step 1:** Tests:
  - `the_dsyx_reply_reads_as_dantesync_writes_it`: base and extension bytes laid out as dantesync's `build_response` and `encode_extension` write them (offsets from `time_server.rs`, values from its tests).
  - `a_64_byte_reply_has_no_date_state`, `a_wrong_magic_or_request_id_is_refused`, `random_replies_never_panic`.
  - `k_is_wall_minus_d_minus_mono`; `a_date_step_moves_wall_and_d_together_and_k_stays` (±200 ms).
  - `no_anchor_within_two_seconds_of_a_scheduled_step`, `none_while_a_slew_runs`, `not_locked_is_not_ready`.
  - `one_odd_poll_is_held_three_re_anchor` with the delta; `an_unlock_after_ready_is_holdover_and_keeps_k`.
  - `media_time_survives_years_of_uptime` (u128, no overflow), `stamps_split_and_join_across_the_seconds_wrap` at 96 kHz.
- [ ] **Step 2:** Implement. The poller is glue (excluded from mutation per function, with a reason); every decision is in `sample` and `Tracker`.
- [ ] **Step 3:** Push; CI green. Commit: `feat(dante): the media clock from dantesync's time server (#72)`.

### Task 2: dantesync on the Gen 3 box, and the baseline reads

**Files:**
- Create: `scripts/gen3/install-dantesync.sh` (`set -euo pipefail`), `scripts/gen3/test_install_dantesync.py`
- Modify: `crates/iem-dante/src/daemon.rs`, `src/bin/iem-dante-dev.rs` (`iem-dante-dev clock --seconds <n>`: one JSON line a poll, numbers only), `.github/workflows/ci.yml` only if the `integrity` job lists test files
- Private: `iemmixer-ops/site/gen3-dantesync.json` (the follower config), `GEN3_DANTESYNC_*` keys in the private env

- [ ] **Step 1:** Read, never change: the fleet's pinned dantesync version (camera-box's `DANTESYNC_VERSION_PIN`) and that release's `dantesync-linux-amd64` sha256.
- [ ] **Step 2:** The script, run from the dev box against `GEN3_HOST`:
  - refuses while `EVENT-NOW` exists, and when anything already holds UDP 319/320 or a PTP daemon (ptp4l, phc2sys, statime) is installed;
  - installs the pinned binary by sha256 (never "latest", never a source build), `/etc/dantesync/config.json` from the private file: a follower (`ntp_server` = the fleet's NTP master, `gm_allowlist` = the audio grandmaster, NTP server mode off), and the unit as dantesync's `install.sh` writes it; disables the other time services as `install.sh` does;
  - reads back `dantesync --version`, `systemctl is-active dantesync`, then (after a settle) a local `DSYX` reply: locked, the extension present, and the HTTP status (8898) `date_authority` follower with the allowed grandmaster.
- [ ] **Step 3:** Python tests with a stand-in ssh: the event flag, a busy 319, a wrong sha256, a missing extension each refuse with a reason and change nothing after the refusal.
- [ ] **Step 4:** Live: run it; then `iem-dante-dev clock --seconds 1800` on the box. Expected: ready within minutes of dantesync's lock, no re-anchor, k within 20 µs over 30 min.
- [ ] **Step 5:** Baseline reads (design §5.5), read only: the card's latency setting and its spare X's advertised `rate`, `enc`, `fpp`, `latency_ns` (mDNS TXT). Set our private config's `latency_ns` to 2 ms or the card's value if higher. Record the numbers (no names) on #72.
- [ ] **Step 6:** Commit: `feat(gen3): install dantesync as the box's PTP follower (#72)`.

### Task 3: Audio packets, the jitter ring, TX sources and checkers (`media`)

**Files:**
- Create: `crates/iem-dante/src/media.rs`, `media/packet.rs`, `media/ring.rs`, `media/source.rs`, `media/check.rs` and their `tests.rs`; `crates/iem-dante/tests/hot_path.rs`
- Modify: `crates/iem-dante/Cargo.toml` (dev-dependency `assert_no_alloc`, as `iem-engine`)

**Interfaces:**
- Produces:
  - `packet`: `MEDIA_HEADER = 9`; `write(out: &mut [u8], secs: u32, sub: u32, bps: usize, frames: usize, sample: impl FnMut(frame, slot) -> i32) -> usize` (byte 0 = 2 as inferno, seconds and sub-second samples big-endian, PCM 16/24/32-bit big-endian interleaved); `read_header(buf) -> Option<(u32, u32)>`; `read_frames(buf, slots, bps, f: impl FnMut(frame, slot, i32)) -> Result<usize, Ragged>`. Samples are left-justified `i32` (24-bit << 8), so phase C scales by 2⁻³¹.
  - `ring::Ring` (one per RX channel, 2¹⁶ slots, single writer and single reader, atomics): `write_at(pos: u64, samples: &[i32]) -> Write`, `read_at(pos: u64, out: &mut [i32]) -> Read { holes }`; counters written, holes, late (behind the reader), duplicate.
  - `source::Source { Silence, Counter, Sine { hz: f64, dbfs: f64 }, Echo { rx: usize, offset: u32 } }` with `fill(&mut self, stamp: u64, out: &mut [i32], rings: &[Ring]) -> Fill { holes }`. Counter: `(stamp mod 65536) << 8`, a self-describing ramp at about −42 dBFS; Sine capped at −20 dBFS (the HIL rule); Echo reads ring `rx` at `stamp − offset`.
  - `check::CounterCheck` (songplayer's loop vocabulary: `mismatch`, `dropout`, `slip(k)`, `no_return`; meters `checked`, `blind`) and `check::SineCheck` (known frequency; a residual jump is a `discontinuity`).

- [ ] **Step 1:** Tests:
  - packet: round trips at 16, 24, 32 bits; `24_bit_is_big_endian_and_sign_extended`; `the_header_is_byte_2_then_seconds_then_samples`; `a_ragged_or_short_packet_is_refused` and 10 000 seeded random buffers never panic.
  - ring: `samples_land_at_their_stamps`, `a_missing_packet_reads_as_a_hole`, `a_packet_behind_the_reader_is_late_not_written`, `a_duplicate_is_counted_and_harmless`, `a_wrapped_slot_never_reads_stale`.
  - source: `the_counter_names_its_own_stamp`, `the_sine_never_exceeds_minus_20_dbfs`, `echo_is_the_rx_sample_offset_earlier`, `echo_of_a_hole_is_silence_and_counted`.
  - check: a clean counter has no event; one dropped frame is one `dropout`; a shift by k is `slip(k)`; a changed value is `mismatch`; silence is `blind`; a clean sine has no discontinuity and one repeated sample is found.
  - `tests/hot_path.rs`: packet write/read, ring write/read, every source and both checkers under `assert_no_alloc`.
- [ ] **Step 2:** Implement. The ring may adapt inferno's `ring_buffer.rs` hole handling (origin header).
- [ ] **Step 3:** Push; CI green. Commit: `feat(dante): audio packets, the jitter ring, test sources and checkers (#72)`.

### Task 4: Flow control on UDP 4455 and our TX flows

**Files:**
- Create: `crates/iem-dante/src/flows.rs`, `flows/control.rs` (codec, adapted from inferno `protocol/flows_control.rs` and the parse in `flows_control_server.rs`), `flows/server.rs`, `media/tx.rs`, their `tests.rs`

**Interfaces:**
- Produces:
  - `control`: `PORT = 4455`, start code `0x1102`; ops `REQUEST 0x0100`, `STOP 0x0101`, `UPDATE 0x0102`; statuses `OK 1`, `FLOW_NOT_FOUND 0x0103`, `SAMPLE_RATE_MISMATCH 0x0301`, `BAD_REQUEST 0x0302`, `TOO_MANY_TX_FLOWS 0x0315`; `type Handle = [u8; 6]` (slot u32 + cookie u16); `struct FlowRequest { rx_host, rx_flow_name, sample_rate, bits, channels: Vec<Option<u16>>, fpp, rx: SocketAddrV4 }`; `build_request`, `parse_request`, `build_stop`/`parse_stop` (exactly 6 bytes), `build_update`/`parse_update`.
  - `server::handle(table: &mut TxTable, cfg: &DeviceConfig, req: &[u8]) -> FlowOutcome { Reply(Vec<u8>), Unanswered { op }, Misframed { op }, Ignored }` (pure); `server::serve(sock, shared)` counting refused and unanswered requests with phase A's `Gaps`.
  - `tx::TxTable` (32 slots, inferno's `MAX_FLOWS`): `add(req, now) -> Result<(slot, Handle), u16>` (an add for a destination already served sets its channels and returns its handle, as inferno), `stop(handle)`, `update(handle, channels, now)` (revives an expired flow, as inferno), `keepalive(slot, now)`, `expire(now)` (4 s without a datagram on the flow socket); `tx::Schedule { next: u64, fpp }` with `bootstrap(now)` (aligned to fpp), `due(now) -> Option<u64>`, a lag beyond the flow latency or a clock epoch change re-bootstraps, counted.
  - `tx::spawn(clock: Arc<dyn PtpClock>, shared: Arc<TxShared>) -> JoinHandle<()>`: sends the packet stamped T at T + fpp duration + `TX_MARGIN` (250 µs) from a socket connected to the receiver; reads keepalives non-blocking every 33 ms; nothing is sent before the clock is ready.
- Answers (inferno's, with the deviations named): a sample rate other than ours → 0x0301; a TX channel id past our channels, fpp 0 or above 256, or a packet that would not fit one datagram → 0x0302 once (inferno answers 0x0302 and then a handle for a bad channel, and loops on fpp 0); no free slot → 0x0315; stop or update of an unknown handle → 0x0103; a body of 16 bytes or less, a missing string or channel ids past the end, an unknown op1 or op2 ≠ 0 → no reply (inferno logs or panics), counted.

- [ ] **Step 1:** Tests:
  - `our_request_is_byte_for_byte_inferno_s` (strings offset `0x26 + 2n + 10`, `0x0802`, the 8-byte alignment, 12 zero bytes) and parses back equal.
  - One test per answer above, each checking the table did or did not change.
  - `the_33rd_flow_is_too_many`, `a_second_request_for_one_destination_keeps_its_handle`, `update_revives_an_expired_flow`.
  - Keepalive: a datagram at 3.9 s keeps the flow, none for 4 s expires it.
  - Schedule: the first stamp is a multiple of fpp; `nothing_is_sent_before_stamp_plus_fpp`; a lag beyond the latency re-bootstraps once; a new clock epoch re-bootstraps; packets per second = rate / fpp.
  - `random_datagrams_never_panic` (10 000 seeded, 0–600 bytes).
- [ ] **Step 2:** Implement. `serve` and the TX thread's loop are glue (per-function `exclude_re` with reasons); the struct-field deletion mutants are killed by tests.
- [ ] **Step 3:** Push; CI green. Commit: `feat(dante): flow control on UDP 4455 and the TX flows (#72)`.

### Task 5: Our RX: subscriptions, the flow-control client, receiving

**Files:**
- Create: `crates/iem-dante/src/subscribe.rs`, `subscribe/resolve.rs`, `flows/client.rs`, `media/rx.rs`, their `tests.rs`
- Modify: `crates/iem-dante/src/device.rs` (+ tests)

**Interfaces:**
- Produces:
  - `DeviceState.subscriptions: Vec<Option<Subscription>>` (`#[serde(default)]`: a phase A state file loads), `Subscription { tx_channel: String, tx_device: String }`; length the RX count or empty, anything else refused naming the file.
  - `SubStatus` with inferno's codes: `Unresolved 1`, `InProgress 8`, `TooManyTxFlows 0x14`, `TxFail 0x15`, `Receiving 0x0101_0009` (no multicast in phase B).
  - `resolve::Advertised { control: SocketAddrV4, tx_channel_id, nchan, bits, rate, dbcp1, fpp_min, fpp_max, latency_ns }` from a `_netaudio-chan` record; `trait Resolver` (mdns-sd on our interface; a fake in tests).
  - `subscribe::Manager` (pure core): `plan(&subs, &adverts, &flows) -> Vec<Action>` groups each transmitter's channels into flows of at most `min(nchan, 8)` (channels asking for the same TX channel share a slot); `tick(now) -> Vec<Action>`: a retry every 9 s for unresolved or failed channels, at most one request in flight per transmitter, a 3 s reply timeout; a flow without packets for 2 s (6 s after its start) is stopped and resolved again; a changed channel set stops that flow and requests a new one (no client-side `0x0102` in phase B; our server handles it). Fpp asked = the transmitter's `fpp_max` within one datagram (inferno); latency = max(its `latency_ns`, ours).
  - `client::request_flow(to, req) -> Result<Handle, ClientError>` (reply matched by op1 and sequence, as inferno), `client::stop_flow(to, handle)`.
  - `rx::spawn(clock, shared) -> JoinHandle<()>`: one thread, `mio::Poll` over up to 32 flow sockets (ephemeral ports on our address, 4 MB receive buffers); each datagram: stamp → sample index near now → the subscribed channels' rings; `13 37` to the audio's source every 250 ms per flow; per flow: packets, refused, out of order, late beyond the latency, the largest lateness since the last heartbeat (inferno's 0x8003).

- [ ] **Step 1:** Tests:
  - `a_phase_a_state_file_loads_without_subscriptions`; `a_subscription_survives_a_restart`; `a_wrong_length_list_is_refused`.
  - Manager: grouping and sharing; `a_refusing_transmitter_gets_one_request_per_9_s` over 10 simulated minutes (Review Focus 1); `a_silent_transmitter_never_has_two_requests_in_flight`; `a_flow_without_packets_is_stopped_and_resolved_again`; `removing_the_last_channel_stops_its_flow`; statuses move Unresolved → InProgress → Receiving.
  - RX: frames of 24-bit big-endian land at their stamps across the seconds wrap; garbage never panics; keepalives are 4 a second per flow.
- [ ] **Step 2:** Implement (the manager adapts inferno's `channels_subscriber.rs` resolve and scan rules; origin header).
- [ ] **Step 3:** Push; CI green. Commit: `feat(dante): RX subscriptions, the flow-control client and receiving (#72)`.

### Task 6: ARC and info for flows and subscriptions

**Files:**
- Modify: `crates/iem-dante/src/arc.rs`, `arc/tests.rs`, `info.rs`, `info/tests.rs`, `info/limit.rs`

**Changes (each as inferno answers it with flows and a subscriber):**

| Request | Phase B answer |
|---|---|
| 0x1000 | counts with inferno's 32 TX and 32 RX flows (the multicast flag stays clear) |
| 0x2200 | TX flows, 16 slots a page: socket descriptor `0x8002`, names descriptor, flow header (`0x11` unicast), channel ids, footer (inferno's `query_tx_flows`) |
| 0x3200 | RX flows: socket descriptor, descriptor 2 with the flow latency in ns, channel bitmasks (inferno's `query_rx_flows`) |
| 0x3000 | RX channels with the TX channel and device names and the status code |
| 0x3010 | subscribe or unsubscribe for real; the state is saved before the OK, a failed save refuses 0xffff and undoes; the channel reads Unresolved at once (Dante Controller asks right after) |
| 0x3014 | netaudio's unsubscribe, for real |

- A page that would pass one datagram ends early with `0x8112`; equal strings in a page are written once (Review Focus 5).
- Info, once the clock is ready (inferno adds them only with a clock): heartbeat records `0x8001` (dantesync's frequency adjustment in ppb), `0x8002` (peaks: inferno's `−40·log10`, RX rings and TX sources), `0x8003` (each RX flow's largest lateness in samples).
- The channel-change notification `07 2a 01 02` (inferno's bitmask) after a status change or an RX rename, inside the info bound as its own kind.
- Clock stats (`07 xx 00 21`) stay unanswered: inferno answers them only from its PTP daemon's master-clock file.

- [ ] **Step 1:** Tests: each new reply pinned to inferno's serializer layout; `every_opcode_inferno_answers_is_answered_as_inferno_answers_it` also with two TX and two RX flows; `pages_with_the_longest_names_fit_and_list_everything_once`; `a_subscribe_is_unresolved_at_once_and_saved_first`; `a_failed_save_refuses_and_undoes`; heartbeat bytes pinned with and without a clock; the notification is bounded.
- [ ] **Step 2:** Implement; ARC's `handle` stays pure (a subscription change is `ArcOutcome::Changed`; the daemon tells the manager, as it tells the announcer).
- [ ] **Step 3:** Push; CI green. Commit: `feat(dante): ARC flow lists, subscriptions and the heartbeat's clock records (#72)`.

### Task 7: The daemon: wiring, local control, stop, unit, deploy

**Files:**
- Create: `crates/iem-dante/src/ctl.rs` (+ tests), `crates/iem-dante/tests/loopback.rs`
- Modify: `crates/iem-dante/src/daemon.rs` (+ tests), `src/bin/iem-dante-dev.rs`, `scripts/gen3/iem-dante.service`, `scripts/gen3/deploy.sh`, `scripts/gen3/test_deploy.py`, `.cargo/mutants.toml`

**Interfaces:**
- Produces:
  - `serve` adds the clock poller, the 4455 server, the manager, the TX and RX threads and the control socket.
  - The control socket `/run/iem-dante/control.sock` (`RuntimeDirectory=iem-dante`, mode 0700, the service's user only), one line a request:
    - `status`: JSON, numbers only: clock (ready, epoch, re-anchors, holdovers); requests by port, peer id and opcode, cumulative; TX and RX flows by slot (packets, holes, late, largest lateness, keepalives, re-bootstraps); checker events and meters.
    - `source <tx> silence|counter|sine <hz> <dbfs>|echo <rx> <samples>`: runtime only, every TX channel silent at start.
  - `iem-dante-dev ctl <request>`, the client.
- Stop: SIGTERM or SIGINT stops each RX flow at its transmitter (`0x0101`, all within 1 s), ends the threads, sends the goodbyes and exits 0, inside the unit's 10 s.
- Unit: `LimitRTPRIO=80`, `RuntimeDirectory=iem-dante`. `deploy.sh` runs `systemctl disable` (in place of `enable`) and `restart`: a box reboot never starts the device. The sudoers rule swaps `enable` for `disable`.

- [ ] **Step 1:** Tests:
  - `tests/loopback.rs` (Linux, `ManualClock`, 127.0.0.1): our client requests a flow from our server, TX 1 sends the counter, RX 1 receives it. One simulated second has no mismatch, dropout or slip, then the stop releases the flow. Every wait is bounded.
  - `a_stop_with_a_silent_transmitter_ends_within_1_s`.
  - Control: each request parses; a bad one is an error line; the status is numbers only (no string field but fixed words).
  - `deploy.sh` tests: `disable` then `restart`; the refusals from phase A unchanged.
- [ ] **Step 2:** Implement; thread spawning and the socket accept loop are glue with reasons in `.cargo/mutants.toml`.
- [ ] **Step 3:** Push; CI green; `mutants-list` within the shard matrix (resize it before the PR if not). Open the dev→main PR (auto-merge when every gate is green). Commit: `feat(dante): the daemon's flows, control socket and stop (#72)`.

### Task 8: The live guard and the loop driver

**Files:**
- Create: `scripts/gen3/live_guard.py`, `scripts/gen3/test_live_guard.py`, `scripts/iem-pc/iempc_gen3.py`, `scripts/iem-pc/test_iempc_gen3.py`
- Modify: `scripts/iem-pc/iempc.py` (the `gen3-loop` command: dev time, locked)

- [ ] **Step 1:** `live_guard.py` (dev box): every second reads `iem-dante-dev ctl status` over ssh. It runs `sudo -n systemctl stop iem-dante` at once and exits 1 when:
  - any (port, peer, opcode) exceeds 20 requests a second for 3 s (phase A's loop was 2 250 a second; Dante Controller settles at one per 10 s);
  - our own flow requests exceed one per 5 s to a peer;
  - a TX or RX flow passes 1.25 × rate / fpp packets a second.
  It exits 0 when `EVENT-NOW` appears (the event's own stop runs). It prints a numbers-only line every 10 s and a summary.
- [ ] **Step 2:** `iempc gen3-loop --offsets F1,F2 --bursts N --slot X` (refused outside dev time, like every PC command):
  - `iemmode job-begin`;
  - for each offset: `ctl source 1 echo 1 F` on the Gen 3 box, then N times `iemmode test-signal` on spare X only, reading `loopback_samples` from `iemmode status` after each;
  - `iemmode job-end`.
  It prints a JSON verdict, numbers only: pass when the N values at each offset are equal and their difference is F2 − F1, and our status shows no hole, late packet or re-anchor in the window.
- [ ] **Step 3:** Python tests with stand-in ssh: a loop trips the stop within 3 s; ordinary traffic never trips it; the flag exits 0; the loop verdict passes only the exact shape; a job-begin refusal ends with no source change.
- [ ] **Step 4:** Commit: `feat(gen3): the live guard and the loop driver (#72)`.

### Task 9: Live 1 — Dante Controller with flows, and the self-loop

- [ ] **Step 1:** Deploy the CI artifact (Task 7's push) with `deploy.sh`; start `live_guard.py` for the whole session.
- [ ] **Step 2:** With Dante Controller open on its PC, capture the requests on the box for 2 minutes. Expected: the phase A table plus the flow lists, then about one request every 10 s, no gap but `0x2204`.
- [ ] **Step 3:** Self-loop: netaudio (aimed at our device as the receiver) subscribes RX 2 to our TX 2; `ctl source 2 counter`. Over 10 minutes the counter check shows 0 mismatch, dropout and slip, and holes, late packets and re-anchors are 0. TX packets a second = rate / fpp.
- [ ] **Step 4:** Restart the service: the subscription comes back from the state file and the counter check resumes clean. Then `sudo systemctl stop iem-dante` with the flow live: the stop is within bounds, and `status` before it showed the stop.
- [ ] **Step 5:** Unsubscribe RX 2; record the numbers on #72.

### Task 9b: resolve a transmitter's channels through its ARC

**Why (live, #72 2026-10-26):** our RX resolved `<channel>@<device>` only through `_netaudio-chan` records (as inferno). The band's two Yamaha Dante cards publish none: only `_netaudio-arc` (UDP 4440), `_netaudio-cmc` (8800) and `_netaudio-dbc` (4455). A subscription to the IEM card's channel stayed Unresolved and no flow request went out. In production 18 of the IEM inputs come from such a card, so this is a production requirement. Devices that publish channel records (a stage rack, a virtual soundcard) keep resolving exactly as before.

**Files:**
- Create: `crates/iem-dante/src/arc/client.rs` (+ `arc/client/tests.rs`, the fixture `arc/client/card-pages.txt`), `crates/iem-dante/src/subscribe/directory.rs` (+ `subscribe/directory/tests.rs`)
- Modify: `crates/iem-dante/src/arc.rs` (the module), `subscribe.rs` (the module), `subscribe/resolve.rs` (`Browse::browse(service)`, `Adverts::names_device`), `daemon.rs` (the resolver, the start), `daemon/tests.rs`, `subscribe/resolve/tests.rs`, `scripts/gen3/live_guard.py`, `scripts/gen3/test_live_guard.py`

**Interfaces:**
- Produces:
  - `arc::client`: `page_request(seq, op1, start)` (netaudio's read: start code 0x27ff, body `00 01 <start> 00 00`, the request the captures answered); `trait Exchange { ask(op1, start) -> Result<(status, body), ReadError> }`; `read_directory(&mut impl Exchange) -> Result<Directory, ReadError>`: 0x1000 (the TX count, body bytes 2–3), then the 0x2000 pages (each channel's id, the rate and bits of the descriptor it points at, its default name), then the 0x2010 pages (`[index, id, name]` of the channels with a name of their own: the card lists only those, from index 1; ours lists all, `[id, id, name]`; netaudio reads the id from the second field). A list's next page starts at start + count while the page says more (`0x8112`), or was full with fewer items than the TX count read; it ends at a page that says neither, at count 0, or after `MAX_PAGES` (32) pages. Read-only opcodes only. `Directory::find(name)` (case aside): a channel's own name first, then its default name. `Udp` (the exchange: one socket bound to our address and connected to the device's ARC, each request waits `REPLY_TIMEOUT` (1 s) for the reply of its sequence and opcode, never asks again, and is counted in the status's `sent` table by peer id and opcode).
  - `subscribe::directory`: `Book` (pure, times in ns): the devices' `_netaudio-arc` and `_netaudio-dbc` records (at most 256) and each asked device's reads (at most 64); `lookup(device, channel, now)` answers only from a directory read within `MAX_AGE` and otherwise registers a want; `next(now)` hands out one read at a time per device; `finished(job, result, now)`. `ArcDirectory`: the `Resolver` the manager calls (never blocks on the network: it reads the book and registers a want), one browse each of `_netaudio-arc` and `_netaudio-dbc` for the device's life and one reader thread (`ArcDirectory::start`, once, when a lookup first reached it). `Network { chan: Mdns, arc: ArcDirectory }`: `_netaudio-chan` first; a device any channel record names never reaches its ARC. The daemon's resolver becomes `(Own, Network)`.
  - What an ARC-resolved channel offers the manager (`Advertised`): the `_netaudio-dbc` record's address and port as flow control, the 0x2000 id, rate and bits, `nchan` 4, `fpp` 16,16, `dbcp1` 0x1102, `latency_ns` 0.

**Bounds (Review Focus 1 and the DC loop):**
- One reader thread for all devices: at most one ARC exchange in flight on the whole device. Each request waits at most 1 s; a refusal, no reply, a socket error or a page that does not read ends the read (no retry inside it). A read is at most 1 + 2 × 32 requests; the IEM card's is 6 (0x1000, four 0x2000 pages of 32, one 0x2010 page).
- Per device: the next read starts no earlier than `retry_after(failures)` after the last one ended (the manager's 9, 18, 36, 72 s); a failed read and a read asked because a name was not listed count a failure, a read of a missing or old directory that succeeds clears them. A device that never answers costs one request per read: at most 12 in 10 minutes.
- A directory answers for `MAX_AGE` (60 s) after its read, then the next lookup that needs it reads it again (a rename on the card shows within a minute; while channels are routed nothing is looked up, so nothing is read). A channel not listed asks for a read again within the same bound.
- A device's ARC is read only once its `_netaudio-arc` record has been known `SETTLE` (5 s) and only while no `_netaudio-chan` record names the device (both browses start together; a device that publishes channel records has them in long before), and only with its `_netaudio-dbc` record (no flow control, nothing to ask).
- Logs: one info line per directory read (peer id, channels, named, requests, ms); failures and lookups that found nothing are counted and logged once a minute at most (`ARC directory reads in a minute`, numbers only).

**fpp and latency (the flow request needs them, ARC names neither):** the card answers 0x1000/0x2000/0x2010 with no frames per packet and no latency (the captures; netaudio reads neither from them). fpp 16, fixed (`fpp_min = fpp_max`): the card's performance property 0x0210 reads 16, most likely its transmit frames per packet (evidence, not proof); inferno's receiver asks the transmitter's most; a 4-slot flow of 16 frames is 201 bytes at 24 bits, far inside one datagram. `nchan` 4: the card's own flows to a rack carry 4 slots; a transmitter that takes 8 takes 4. Latency: the transmitter's is not known, so `latency_ns` 0 and the manager plays out at ours (`max(its, ours)`; the card's configured latency 0.25 ms is below any we run).

- [ ] **Step 1:** Tests:
  - `arc/client`: the IEM card's replies (captured 2026-10-26, names replaced by synthetic ones of the same byte length, the 0x2010 channel ids by synthetic ones; every offset, count, flag and length kept) read as 128 channels with ids, rate 96 000, 24 bits and default names over four 0x2000 pages (three `0x8112`), and the 24 named ones from one 0x2010 page, in exactly 6 requests; our own server's pages (31-byte names, early `0x8112` ends) read back as its channels; the page rules (a full OK page asks the next, count 0 and `MAX_PAGES` end it, a malformed page or a refusal fails the read); `Udp` on loopback (another sequence skipped, the wait bounded, every request counted).
  - `subscribe/directory`: N lookups for one device → one read; a dead device over 10 simulated minutes of lookups every 250 ms → at most 12 reads of one request, never two within 9 s; a name not listed → reads bounded the same way; an old directory read again once; no read before `SETTLE`, without a `_netaudio-dbc` record, or for a device a channel record names; `(Own, Network)`: our records, then `_netaudio-chan`, then ARC; the manager asks the ARC-resolved transmitter for a 4-slot flow at fpp 16 on its `_netaudio-dbc` port; `ArcDirectory` on loopback against our own ARC server (the lookup answers once the read is done, requests counted).
  - The daemon starts each browse once (chan, arc, dbc).
  - `live_guard.py`: our ARC reads (`sent` op 0x1000, one per directory read) to one peer more than one within 5 s trip it, like our flow requests.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Push; CI green (test, clippy, coverage floor, `mutants-list`). Commit: `feat(dante): resolve a transmitter's channels through its ARC (#72)`.

### Task 10: Live 2 — receiving a real device's channel

- [ ] **Step 1:** After "event skončil": `iempc dev`; the PC in a HIL job.
- [ ] **Step 2:** netaudio (aimed at our device) subscribes RX 1 to the card's spare HIL TX X. The card publishes no channel record, so RX 1 resolves through its ARC (Task 9b). Expected: about 5 s after the device saw the card's records, one directory read (`sent`: 0x1000 once, 0x2000 four times, 0x2010 once to the card's peer id; the info line `a transmitter's channels read through its ARC` with channels 128, requests 6), then one flow request (op 256) for 4 slots at fpp 16 to the card's `_netaudio-dbc` port; the card answers it with a handle, RX 1 reads Receiving, and keepalives go out 4 a second. No further 0x1000 while RX 1 receives.
- [ ] **Step 3:** `iemmode test-signal` on spare X (sine, at most −20 dBFS), 5 bursts. Expected: `SineCheck` shows no discontinuity and holes are 0. Lateness lies between −1 ms and the flow latency (our clock agrees with the card's to within the path delay).
- [ ] **Step 4:** Keep a P6-scrubbed capture of our request and the card's reply as a test fixture (`flows/control` tests). Unsubscribe: our stop goes out, and the card's TX flow list (ARC 0x2200, read only) no longer has our flow.
- [ ] **Step 5:** Commit the fixture: `test(dante): pin a real transmitter's flow reply (#72)`; record on #72.

### Task 11: Live 3 — the loop through the IEM card (phase B done)

- [ ] **Step 1:** Read only, before: the card's spare return X subscription (to restore) and its RX latency.
- [ ] **Step 2:** RX 1 subscribed to the card's spare HIL TX X again (Task 10; resolved through the card's ARC, Task 9b), `ctl source 1 echo 1 F1`.
- [ ] **Step 3: The owner's one action**, asked once as a self-contained Slovak `❓` block: in Dante Controller, subscribe the IEM card's spare HIL return X to our test device's TX 1 for the test; after the test, set it back to what Step 1 read. Wait for the owner's go; nothing else in this task changes the card.
- [ ] **Step 4:** Expected after the subscription:
  - the card requests a flow from us (one 4455 request, a handle, keepalives 4 a second);
  - its RX X reads Receiving (ARC 0x3000 on the card, read only), and its 0x3200 names the flow latency.
  Keep the card's request, scrubbed, as a fixture for Task 4's parser.
- [ ] **Step 5:** `iempc gen3-loop --offsets 192,229 --bursts 5 --slot X`. Pass: five equal round trips at each offset, 37 samples apart. Our status shows no hole, late packet or re-anchor; the live guard did not trip.
- [ ] **Step 6:** The owner sets the return back; read it back equal to Step 1. Unsubscribe RX 1; `ctl source 1 silence`; stop the service.
- [ ] **Step 7:** Record on #72 (numbers only) and tick phase B. Update `.claude/rules/gen3.md` (clock, flows, subscriptions, control socket, guard, loop). Commit: `docs(rules): gen3 phase B (#72)`.

## Risks

- **The card does not take our flow** or Dante Controller flags our clock. First suspect: the unanswered clock stats (0x21). Fallback: answer them as inferno does, with the grandmaster's identity taken from dantesync once its PTPv1 header parse is fixed (camera-box #1471); until then dantesync's grandmaster id may read wrong.
- **dantesync in micro mode** slews D: k then drifts up to 100 µs/s and re-anchors. The fleet default (nightly step) leaves k alone; the loop test requires zero re-anchors in its window. Phase D's own follower removes the dependency.
- **Realtek NIC, SCHED_OTHER:** fine at relaxed latency; late packets show in the status and the guard. Phase F's tuning (Intel NIC, RT kernel) is not phase B's.
