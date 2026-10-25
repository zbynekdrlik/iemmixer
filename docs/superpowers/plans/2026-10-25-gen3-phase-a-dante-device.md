# Gen 3 phase A: our Dante device in Dante Controller — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A daemon on the Gen 3 box announces our own Dante device. Dante Controller lists it under a test name with its RX and TX channels, and renames made in Dante Controller stick.

**Architecture:** A new GPL crate `iem-dante` holds the device's protocol pieces: identity and config, the request/response codec, the ARC server, CMC and the mDNS records. A small binary `iem-dante-dev` runs them on the box as a systemd service. No audio, no clock and no flows yet (phases B and C).

**Tech Stack:** Rust (workspace toolchain 1.98.1), tokio UDP sockets, `mdns-sd` 0.21 (MIT OR Apache-2.0) for the mDNS responder, serde/toml for config and state, systemd on Ubuntu 26.04.

**Spec:** `docs/superpowers/specs/2026-10-24-gen3-linux-dante-design.md` (§4 inferno, §5.1 shape, §5.3 steps 1–2, §5.5 events and names, §5.6 testing). Ticket #72.

## Global Constraints

- P6: no real names, hosts, IPs, Dante device or channel names in the repo. The box's device name and channel names live in the private ops repo, and the repo's tests use synthetic ones (`test-device`, `TX 1`…).
- Licence: `iem-dante` is `GPL-3.0-or-later` (design §3). It gets a `deny.toml` licence exception like `iem-limiter-mga`. A file adapted from inferno (`GPL-3.0-or-later OR AGPL-3.0-or-later`, teodly/inferno at `9767558`) keeps a header naming its origin.
- The engine's dependency closure does not change: `iem-dante` is no dependency of `iem-engine` in this phase, and `scripts/check_engine_deps.py` stays clean.
- The device is a follower and never sends PTP (design §5.5). Phase A sends no PTP and no audio at all.
- Dante rule: our device only. The tool never sends a command that changes another device. Reads of other devices (mDNS browsing, ARC reads) are allowed.
- "ide event" stops the device service. A failed stop warns but never delays the event (design §5.5).
- Device name ≤ 31 characters (Dante Controller ignores longer ones). Channel names follow the same ASCII rules inferno uses.
- Linux only: the crate builds on Linux, and the Windows CI job skips it with `cfg(target_os = "linux")` on the binary.

## Review Focus

1. A rename from Dante Controller to a name that is too long, empty or has illegal characters: the device answers an error status and keeps the old name, and its state file stays as it was.
2. A request with a truncated header, a length field that disagrees with the datagram, or an unknown opcode: answered with an error status or dropped, never a panic. A fuzz-style test feeds random datagrams.
3. Two Dante Controllers (or Controller plus a reader) asking at once with interleaved sequence numbers: each reply carries its own request's sequence number.
4. The box restarts: the device comes back with the renamed names, read from its state file, not the config defaults.
5. Paging: a channel list longer than one reply page (our 32 RX / 16 TX vs inferno's page size) returns every channel exactly once across pages.

---

### Task 1: The `iem-dante` crate, its config and its state

**Files:**
- Create: `crates/iem-dante/Cargo.toml`, `crates/iem-dante/src/lib.rs`, `crates/iem-dante/src/device.rs`, `crates/iem-dante/src/device/tests.rs`
- Modify: `Cargo.toml` (workspace members), `deny.toml` (licence exception), `scripts/check_version.py` if it lists crates, `.github/workflows/ci.yml` only if new crates need listing
- Create: `config/test-dante-device.toml` (synthetic)

**Interfaces:**
- Produces:
  - `pub struct DeviceConfig { pub name: String, pub manufacturer: String, pub model: String, pub rx: Vec<String>, pub tx: Vec<String>, pub sample_rate: u32, pub encoding: u8, pub latency_ns: u64, pub state_file: PathBuf }`
  - `DeviceConfig::from_toml(text: &str) -> Result<Self, ConfigError>`
  - `pub struct DeviceState { pub name: String, pub rx: Vec<String>, pub tx: Vec<String> }`
  - `DeviceState::load_or_default(cfg: &DeviceConfig) -> io::Result<Self>` and `DeviceState::save(&self, path: &Path) -> io::Result<()>` (synced write, then rename)
  - `pub fn valid_device_name(s: &str) -> bool` and `pub fn valid_channel_name(s: &str) -> bool`

- [ ] **Step 1:** Write the tests in `device/tests.rs`:
  - `a_config_reads_the_test_device`: `config/test-dante-device.toml` gives `name == "test-device"`, 32 RX, 16 TX, `sample_rate == 96000`, `encoding == 24`.
  - `names_follow_the_dante_rules`: a 31-character name is valid, 32 characters and the empty name are not, and a name with `@`, `=`, `.` or a space is refused (check inferno's `device_info.rs` and its channel-name rules for the exact set and mirror them).
  - `a_state_survives_a_restart`: save a state with a renamed TX 3, load it, get the renamed list.
  - `a_missing_state_is_the_config`: no file means the config's names.
  - `a_state_that_does_not_fit_the_config_is_refused`: the wrong channel count is an error naming the file, never silently truncated.
- [ ] **Step 2:** Run `cargo test -p iem-dante` and see it fail to compile or fail.
- [ ] **Step 3:** Implement `device.rs`, then add the crate to the workspace and `deny.toml`'s GPL exception (copy the `iem-limiter-mga` entry).
- [ ] **Step 4:** `cargo test -p iem-dante`, `cargo clippy -p iem-dante --all-targets -- -D warnings` and `python3 scripts/check_engine_deps.py` all pass.
- [ ] **Step 5:** Commit: `feat(dante): the iem-dante crate: device config and persisted names (#72)`.

### Task 2: The request/response codec

**Files:**
- Create: `crates/iem-dante/src/wire.rs`, `crates/iem-dante/src/wire/tests.rs`

**Interfaces:**
- Produces:
  - `pub struct Header { pub start: u16, pub len: u16, pub seq: u16, pub op1: u16, pub op2: u16 }`, 10 bytes big-endian
  - `Header::parse(buf: &[u8]) -> Result<(Header, &[u8]), WireError>`, which checks that `len` equals the datagram length
  - `pub fn reply(req: &Header, status: u16, body: &[u8]) -> Vec<u8>`, where `op2` carries the status: 1 = OK, `0x8112` = more pages
  - `pub const STATUS_OK: u16 = 0x0001; pub const STATUS_MORE: u16 = 0x8112;`

- [ ] **Step 1:** Tests:
  - `a_header_round_trips`.
  - `a_short_datagram_is_an_error`: fewer than 10 bytes.
  - `a_length_that_disagrees_is_an_error`.
  - `a_reply_keeps_the_request_sequence`.
  - `random_datagrams_never_panic`: 10 000 seeded random buffers of length 0 to 600 through `parse`.
- [ ] **Step 2:** Implement, reading the layout from inferno's `inferno_aoip/src/protocol/req_resp.rs` (adapted file, origin header).
- [ ] **Step 3:** The tests pass. Commit: `feat(dante): the request/response header (#72)`.

### Task 3: The ARC server

**Files:**
- Create: `crates/iem-dante/src/arc.rs`, `crates/iem-dante/src/arc/tests.rs`

**Interfaces:**
- Consumes: `Header`, `reply` (Task 2); `DeviceState`, `DeviceConfig`, the name checks (Task 1).
- Produces:
  - `pub fn handle(state: &mut DeviceState, cfg: &DeviceConfig, req: &[u8]) -> ArcOutcome`
  - `pub enum ArcOutcome { Reply(Vec<u8>), Changed(Vec<u8>) }`: `Changed` means the state changed, so the caller saves it and re-announces mDNS.
  - `pub async fn serve(sock: tokio::net::UdpSocket, shared: Arc<Mutex<Device>>)` on UDP 4440

**Opcodes in this phase** (read each body layout from inferno's `proto_arc.rs` and `device_server/arc_server.rs` at the pinned commit):

| Opcode | Handling |
|---|---|
| `0x1000` | counts (RX, TX, flows 0) |
| `0x1002`, `0x1003` | device names |
| `0x2000`, `0x2010` | TX channel list, paged |
| `0x2013` | TX rename |
| `0x3000` | RX channel list, paged; no subscriptions yet |
| `0x3001` | RX rename |
| `0x3300` | the fixed 8-byte reply |
| `0x1100`, `0x1102` | zeros |

Anything else answers an error status, and nothing changes.

- [ ] **Step 1:** Tests, each building the request bytes the way inferno's client side does and checking the decoded reply:
  - `counts_name_32_rx_and_16_tx`.
  - `the_tx_list_pages_and_names_every_channel_once`: Review Focus 5.
  - `a_tx_rename_changes_the_state_and_says_so`.
  - `an_invalid_rename_keeps_the_name`: Review Focus 1.
  - `an_unknown_opcode_changes_nothing`.
  - `two_interleaved_requests_get_their_own_sequence`: Review Focus 3.
  - `the_clock_reply_is_the_fixed_one`.
- [ ] **Step 2:** Implement `handle` (pure) and `serve` (a thin socket loop; logs each request's opcode at debug and each rename at info, ids and channel numbers only).
- [ ] **Step 3:** The tests pass and clippy is clean. Commit: `feat(dante): the ARC server: names, channels, renames (#72)`.

### Task 4: mDNS records and CMC

**Files:**
- Create: `crates/iem-dante/src/announce.rs`, `crates/iem-dante/src/announce/tests.rs`, `crates/iem-dante/src/cmc.rs`
- Modify: `crates/iem-dante/Cargo.toml` (`mdns-sd = "0.21"`)

**Interfaces:**
- Produces:
  - `pub fn records(state: &DeviceState, cfg: &DeviceConfig, host: &str, ip: Ipv4Addr) -> Vec<ServiceRecord>`: pure, the full record set
  - `pub struct ServiceRecord { pub service: &'static str, pub instance: String, pub port: u16, pub txt: Vec<(String, String)> }`
  - `pub fn announce(daemon: &mdns_sd::ServiceDaemon, recs: &[ServiceRecord]) -> Result<(), AnnounceError>`, which unregisters the old set first
  - `pub async fn serve_cmc(sock: UdpSocket, cfg: Arc<DeviceConfig>)` on UDP 8800: the `0x1001` advertisement reply

**Records:**

- `_netaudio-arc._udp` on 4440;
- `_netaudio-cmc._udp` on 8800;
- one `_netaudio-chan._udp` per TX channel, named `<channel>@<device>`.

The TXT keys and values come from inferno's `device_server/mdns_server.rs`: `id`, `rate`, `enc`, `latency_ns`, `fpp=max,min`, `nchan`, `dbcp1=0x1102`, and the arc/cmc keys there.

- [ ] **Step 1:** Tests on `records`:
  - `one_chan_record_per_tx_channel_named_channel_at_device`.
  - `a_rename_changes_the_chan_records`.
  - `the_txt_records_carry_the_rate_encoding_and_latency`: 96000, 24, `latency_ns` from the config.
- [ ] **Step 2:** Implement. `announce` is exercised in Task 6 on the box (multicast needs the real network); its pure part is `records`.
- [ ] **Step 3:** The tests pass. Commit: `feat(dante): mDNS records and the CMC reply (#72)`.

### Task 5: The `iem-dante-dev` daemon and its service

**Files:**
- Create:
  - `crates/iem-dante/src/bin/iem-dante-dev.rs`
  - `scripts/gen3/iem-dante.service`
  - `scripts/gen3/deploy.sh`, with `set -euo pipefail`: it refuses while `~/.config/iemmixer/EVENT-NOW` exists, copies a CI artifact by its sha256, installs the unit, restarts it and reads back `--version`
  - `scripts/gen3/test_deploy.py` if the script grows logic
- Modify: `.github/workflows/ci.yml` (build `iem-dante-dev` for x86_64 Linux in release mode and upload it as an artifact `iem-dante-dev-<sha>`), `scripts/iem-pc/iempc_core.py` / the `event` command (stop the 3.0 device)

**Interfaces:**
- Consumes: Tasks 1–4.
- Produces:
  - The binary: `iem-dante-dev --config <path>` binds ARC 4440 and CMC 8800, announces mDNS, and saves state on every change. It handles SIGTERM gracefully: it unregisters mDNS, then exits 0.
  - In the event path: one `ssh <gen3 box> sudo systemctl stop iem-dante` with a hard timeout of 5 s inside the event budget. Its result is logged and a failure is a warning (`GEN3_HOST` in the private env; absent means skipped).

- [ ] **Step 1:** A test in the iempc tests (`test_iempc_event.py` or the module the event command's tests live in):
  - `the_event_stops_the_gen3_device_and_never_waits_for_it`: a fake ssh that hangs is cut off at the timeout, the event still runs `iemmode event`, and a warning line is printed.
- [ ] **Step 2:** Implement the binary, the unit (`Restart=on-failure`, runs as the box's user, `AmbientCapabilities` none: every port is above 1024), `deploy.sh` and the event step.
- [ ] **Step 3:** The Python tests pass, and `cargo build -p iem-dante --release` works locally. Commit: `feat(dante): the iem-dante-dev daemon, its service and the event stop (#72)`.

### Task 6: Live check on the box, against Dante Controller

**Files:**
- Create: `docs/gen3/phase-a-check.md` (the steps and what was observed, no site values)
- Private: the device config (test name, channel names) in `iemmixer-ops/site/gen3-device.toml`

- [ ] **Step 1:** Set up the box once:
  - passwordless sudo for the deploy user, limited to `systemctl` on `iem-dante` and the unit install;
  - `python3-pip`, and `netaudio` in a venv as an independent reader.
- [ ] **Step 2:** Deploy the CI artifact with `scripts/gen3/deploy.sh`.
- [ ] **Step 3:** From the box run `netaudio device list`. Expected: our test device appears next to the real devices.
- [ ] **Step 4:** Check the channels: `netaudio channel list --name <test device>` shows 32 RX and 16 TX with the configured names.
- [ ] **Step 5:** Rename from a reader (netaudio's rename command, aimed only at our device), then restart the service and list again. Expected: the new name stays (Review Focus 4).
- [ ] **Step 6:** Dante Controller on the Dante Controller PC: take a screenshot of its device list through the interactive-session bridge (`windows-remote-gui`). Expected: the test device with its channels and no clock-domain error. Rename one TX channel there, then list again from the box.
- [ ] **Step 7:** Run "ide event" → `iempc event`, then check on the box that `systemctl is-active iem-dante` reads inactive. Afterwards `event skončil`.
- [ ] **Step 8:** Record the outcome on #72 (ids, counts and opcodes only) and tick the phase A checklist there.
