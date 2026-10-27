# Gen 3 phase E: the band test — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One command switches the band from iemmixer 2.x (the PC and its IEM card) to 3.x (the box) and one switches back, on the owner's signal, so the band plays through 3.x without being told; the owner judges by the band's reactions (owner, 2026-10-27, on #72).

**Architecture:** The design's name takeover (§5.5): the IEM card is renamed, the box takes the card's device name and TX channel names, and every device subscribed to the card by name follows without a change on it. The band's data, secrets and mix state move with the switch, the source stopped while they move; the public URL moves with a tunnel connector on the box; the box receives the card's inputs by mirroring the card's RX table onto our own device. A survey of everything subscribed to the card, and read-backs after every step, keep a half-done switch from going unnoticed.

**Tech Stack:** Rust (`iem-dante`: ARC, mDNS; `iem-server`; `iem-guard` where the PC's side needs it), Python (`scripts/iem-pc/iempc*.py`, `dante_card.py` with netaudio 0.4.2's library), bash (`scripts/gen3/provision*.sh`, `deploy.sh`), systemd (units, `systemd-creds`), cloudflared, PowerShell on the PC (DPAPI).

**Spec:** `docs/superpowers/specs/2026-10-24-gen3-linux-dante-design.md` (§0, G2, §5.4 phase E, §5.5 names, §5.6, §7 name takeover; updated 2026-10-27). Ticket #72: the decision of 2026-10-27, the survey of 2026-10-27, the latency question. Phase D: `docs/superpowers/plans/2026-10-26-gen3-phase-d-the-box.md`; `.claude/rules/gen3.md`, `guard.md`, `security-baseline.md`, `tunnel-watchdog.md`, `migration.md`, `dante-labels.md`.

## Choices made in this plan

1. **Name takeover, not re-patching** *(design G2/§5.5; the owner's question of 2026-10-27)*. The card is renamed by one ARC device-name write (0x1001) to it by IP, kept in the card across reboots; then the box takes the freed name. No receiver is touched: the stage rack (the members' path) and the FOH console's card follow by name. Back is the reverse. The spare name lives in the ops device config (site data).
2. **The band's data moves with the switch, both ways** *(my call)*. What moves: the server's band folder (lineups, the active lineup, the applied roster and the kept rosters, photos, push subscriptions, `pin_hashes.json`, `jwt_secret`, `vapid_private`, the HTTPS certificate) and the engine's `current.json` with its `baseline.json`. The source's server and engine are stopped while it moves, so no change is lost; every file is read back by checksum. **The pepper** moves once, PC to box: DPAPI unprotects it on the PC and its 32 bytes go through the ssh pipe straight into `systemd-creds encrypt` on the box, never written in clear, never printed; it replaces the box's own pepper (the box then verifies the band's PIN hashes and the members' sessions stay valid with the same `jwt_secret`). The way back copies the folder and the engine state, not the pepper (the same one on both).
3. **The public URL moves with a connector on the box** *(my call)*. The PC's tunnel is remotely managed (`tunnel-watchdog.md`); a second connector on the same tunnel would share its traffic, so exactly one runs: the switch stops the PC's cloudflared service, then starts the box's (token as a systemd credential from the secret provider, never in a file in the clear or on a command line). The box serves the origin the ingress names, with the band's certificate (HTTPS on 443, the server's TLS build).
4. **The box receives the card's inputs** *(design §5.5)*. Our device's RX n subscribes to what the card's RX n subscribes to, read from the card by IP (ARC 0x3010 on our own device only), with the box's site config equal to the PC's. Both receive in parallel during 3.x; the sources' flow capacity is checked before the band test.
5. **The owner's signals** *(my call, the `iemmode` analogue of design §5.5)*. "prepni na 3.x" runs the switch to 3.x, "prepni na 2.x" the switch back. "ide event" while 3.x serves the band never stops the box: the flag is written and the PC's event switch runs as today (its engine on the renamed card reaches nobody); while 2.x serves, "ide event" stops the box as today. While 3.x serves, the box's units are enabled at boot (it comes back serving after a power loss); while 2.x serves, nothing of the stack is enabled (phase D's rule). The active side is recorded on the box and on the dev box, and every tool reads it.
6. **The band test does not wait for phase F** *(the owner's decision of 2026-10-27 on #72: the band has played with much worse latencies; latency improves as an ongoing process)*. Task 1 measures mic-to-ear of both and records it before the test; the estimate is 2.x ~129 samples (1.3 ms) against 3.x 224 (2.3 ms) on the engine path.

## Global Constraints

- **P6:** the card's spare name, the TX channel names, the tunnel's identity and token, the certificate, addresses and the band's data never enter the repo; real values only in `iemmixer-ops` and the secret provider. Tests use `10.0.0.x` and invented words.
- **Dante rule** (CLAUDE.md: "Never change Dante subscriptions or any Dante device other than the IEM PC's own card"; the switch itself is the owner's decision of 2026-10-27 on #72): only the IEM card and our own device are changed (the card's name, our device's name, TX names and RX subscriptions); no other device's subscriptions or settings. The card's spare HIL return exception stays for HIL tests.
- **Owner's event signals:** the only gate; never ask whether an event runs. Every tool refuses while `EVENT-NOW` exists, except the switch back to 2.x on the owner's own "prepni na 2.x".
- **I8:** nothing is force-killed; services are stopped through systemd or the guard.
- **Tier 0:** no local cargo compilation; each task ends with a push and a green CI run.
- **Changes Thursday, fixes Sunday** (owner): a change that touches the PC lands before the Thursday event; before Sunday only fixes.
- **Secrets:** the pepper, `jwt_secret`, `vapid_private`, the certificate's key and the tunnel token are never printed, logged, put on a command line or written in clear outside their owner-only files; transfers are byte streams checked by length and checksum.
- **Logs** name counts, ports, opcodes, channel numbers and fixed words, never a name, an address or a MAC.

## Review Focus

1. **Never two devices under one name.** The box announces the card's name only after the card's rename read back; our device refuses (and alarms) a name another device announces; the way back renames the box first. Tests: Tasks 2, 3, 7.
2. **No change lost either way.** The source is stopped before its data moves, every file read back by checksum, and the switch refuses to start the target when any read-back differs. Tests: Task 5.
3. **The pepper never in clear.** Not on disk, not in a log, not on a command line, not in the transcript. Tests: Task 5 (the stand-ins record every argument and every file written).
4. **A half-done switch ends in a defined side.** Each step has its undo; a failed step undoes the steps before it and the switch reports which side serves the band, read back from the devices. Tests: Task 7.
5. **"ide event" while 3.x serves never cuts the band's audio.** Tests: Task 7.

---

### Task 1: Mic-to-ear of 2.x and 3.x

**Files:** `scripts/iem-pc/iempc_gen3.py` (or a sibling module) and its tests; #72 for the numbers.

- [x] **Step 1:** 2.x: in a dev entry, the PC engine's loopback probe (`crates/iem-engine/src/latency.rs`, `iemmode status` → `loopback_samples`) on a spare card output looped to a spare card input (the card's own subscription, set for the test, read back and removed). Record the round trip and the card's own RX latency setting (by Dante Controller's read or the card's ARC, whichever answers).
- [x] **Step 2:** 3.x: gen3-mix's numbers (449 with 225 of the PC and the card: engine path D = 224) and the box's RX latency.
- [x] **Step 3:** The comparison on #72: both engine paths from source packet to TX packet, in samples and ms; the difference (recorded; it does not gate the band test). **Measured 2026-10-27:** 2.x 129 samples (1.344 ms, 5 bursts equal), 3.x 224 (2.333 ms); 95 samples, ~1 ms more on 3.x (#72).

### Task 2: The card's rename (`dante_card.py`)

**Interfaces:** `rename_device(ip: str, new: str, expect_old: str) -> None` (ARC 0x1001 by IP; reads the name back; refuses when the card's name is not `expect_old`, when `new` is announced by another device, or when the read-back differs).

- [x] **Step 1: Tests:** the read-back; the refusals (another device, a silent card, a name Dante does not hold); `set_name` sent from `rename` alone (the source test).
- [x] **Step 2:** Implement (`dante_card.py rename <ip> <expected> <new>`; the check that no other device announces the new name is the caller's: the dev box's mDNS does not reach the card's subnet).
- [x] **Step 3:** Push; CI green (fb71c11). **Live, 2026-10-27:** the card took a spare name and its own back in ~0.6 s each, read back; the stage rack and the FOH card stayed with the card while no device held its name (45 s). **The full takeover, the same day:** with our device started under the card's name and TX labels, both receivers asked it for their flows within 8 s, and went back to the card within 5 s once it stopped and the card's name returned; nothing changed on either (#72, the correction of 2026-10-27; the re-patch question withdrawn).

### Task 3: Our device takes the card's name (a takeover config)

**As proven live on 2026-10-27:** no ARC 0x1001 support is needed. The switch writes a takeover device config: the ops device config with `name` = the card's name, `state_file` = a state file of its own (a saved name in the usual state file would win over the config), and the TX names = the card's TX labels read live by IP (dante_card.py `read`), so every channel a receiver subscribes to by name exists on our device; the way back installs the ops config again.

- [x] **Step 1: Tests:** `takeover_config(ops_text, card_labels, card_name) -> str` (pure): name, state file and TX names replaced, everything else byte for byte; the card's labels must cover our TX count and be unique, else refused; a name another device holds is checked by the switch's survey (Task 7).
- [x] **Step 2:** Implement; push; CI green.
- [ ] **Step 3 (open):** an mDNS conflict (another device announcing our name) is not detected by our device (mdns-sd renames silently); the switch's order (the card renamed and read back first) prevents it; detection in the device is a later improvement, recorded here.

### Task 4: The RX mirror

**Interfaces (as built):** `iempc gen3-mirror [--dry-run] [--clear]` (`iempc_gen3_switch.py`); `mirror_plan(card_doc, ours_doc, card_name, clear) -> list[dict]` (pure: our RX n ← the card's RX n; the card's self-subscriptions, its spare HIL returns, not mirrored; a subscribed card RX beyond our count refuses; `clear` unsubscribes ours); `gen3_arc.read_rx(ip)` (name, counts, every 0x3000 page: netaudio never reads the last 8 of our 136 RX) and `gen3_arc.subscription(...)` (0x3010, to our own device only), read back after.

- [x] **Step 1: Tests:** mapping, self-subscriptions, count refusals, refused changes, the flag, read-back (`test_iempc_gen3_switch.py`, `test_gen3_arc.py` with `FakeDante`).
- [x] **Step 2:** Implement; push; CI green (96fab9e, e2da641). **Live, 2026-10-27:** 29 of 29 sent and read back; after ~60 s our device received what the card receives (25 connected, the same 4 sources off; the FOH card's 18 channels in 5 flows, one flow about every 9 s), pump 0 late / 0 missed; `--clear` read back 0 and the six flows were stopped at once (journal). The switch runs the mirror about a minute before it moves the members' path.

### Task 5: The band's data and secrets, PC ↔ box

**Interfaces:** `iempc gen3-data to-box|to-pc [--pepper]`: refuses unless the source's server and engine are stopped (the PC's through the guard, the box's through systemd); copies the band folder and the engine state listed in choice 2; reads every file back by sha256; `--pepper` (to-box only, once) streams the PC's DPAPI-unprotected pepper into `systemd-creds encrypt` on the box. A deploy mode with the band's site and roster from `iemmixer-ops` instead of the test files (`deploy.sh --site band`).

- **Design as decided (#72, 2026-10-27, the Task 5 design comment):** one direction in the MVP (PC → box at the switch to 3.x; the way back returns to the PC's state at the switch); the PC's files read while its server runs; the pepper streamed from a DPAPI unprotect in the PC's ssh session (checked: it works there) into `systemd-creds encrypt` on the box; the box's receiver `scripts/gen3/band_receive.py` (done, tests `test_band_receive.py`: allowlisted members only, staged and swapped, old items kept); still to do: deploy installs it into `/opt/iemmixer/lib`, its `sudo -n -u iemmixer` line and the pepper's `systemd-creds` line in `sudo-lines.txt` (then provision again), the PC side (one PowerShell session: the files as a zip in memory, base64 on stdout, a manifest of sha256s), `iempc gen3-data to-box [--pepper] [--dry-run]`, and the box's band site (deploy with the ops site and roster).
- [x] **Step 1: Read first:** the guard's data step and how the PC's server is stopped and started (`crates/iem-guard`, `guard.md`); the server's folder layout (`lineup_store.rs`, `band_store.rs`, `secrets.rs`, `pin_store.rs`, `lib.rs`); the engine's persistence (`persist.rs`).
- [x] **Step 2: Tests** (stand-in ssh, PowerShell and systemd-creds): what is copied, in which order, each refusal (a running source, a missing file, a checksum mismatch, an existing different pepper without `--pepper`); the pepper never on a command line, in a log or in a file written in clear (the stand-ins record every argument and every write).
- [x] **Step 3:** Implement; push; CI green (1151114, d46a94e, 38d07a1). **Live 2026-10-27:** the band's site deployed, 106 files and the pepper moved, the box's engine on the PC's state revision, 8 members served, healthy (#72).

### Task 6: The box serves the band's URL

**As built (2026-10-27; the design comment on #72):** the tunnel's ingress sends the public host to `http://127.0.0.1:80` (read from the PC's connector's metrics `/config`). (a) The box's server listens on port 80 with `CAP_NET_BIND_SERVICE`, its one capability (0de49db; health reads port 80). (b) `scripts/gen3/install-cloudflared.sh` puts a pinned cloudflared `.deb` (the PC's version, 2026.2.0, by sha256) on the box, held, with `iem-tunnel.service` (DynamicUser, the token a credential, `--token-file %d/tunnel`, Type=notify), never enabled or started (51eb0a2, merged 1cce40d). (c) `iempc gen3-tunnel token|to-box|to-pc` (4f5defc): the token streamed into the box's credential; `to-box` stops and disables the PC's service, starts the box's unit and reads the public URL's `git_hash` as the box's, putting the PC's back on any failure; `to-pc` the reverse, allowed during an event.

- [x] **Step 1: Read first** (the ingress, the watchdog, the server's TLS path: no TLS on the box, plain HTTP on loopback as on the PC).
- [x] **Step 2: Tests** (`test_install_cloudflared.py`, `test_iempc_gen3_tunnel.py`, the unit and capability tests).
- [x] **Step 3:** Implement; push. **Live so far:** cloudflared installed and held on the box, the token stored (600 root), the box provisioned and verified. The move itself is part of Task 8.

### Task 7: The switch and the signals

**Interfaces:** `iempc gen3-switch to-3|to-2 [--dry-run]`. **to-3:** preflight (no flag; the survey: every device subscribed to the card is covered by the takeover table, else refuse naming the device's role; the box healthy on its test name; the active side 2) → data to-box → the box's stack on the band site → RX mirror → the card renamed (read back) → our device renamed and TX names applied (read back) → every surveyed subscriber resolved to the box (read by IP) → tunnel to-box (read back) → the box's units enabled at boot → active side 3. **to-2:** the reverse, the box renamed first. Each step's undo runs when a later step fails; the final line names the side that serves, read from the devices. `iempc event` reads the active side: 3 → the flag and the PC's switch, the box untouched; 2 → as today.

- [x] **Step 1: Tests** (stand-ins for every device call): the order both ways; every undo path; the dry run sends nothing; the event's two cases; refusals.
- [x] **Step 2:** Implement; the always-apply section of CLAUDE.md and the event memory gain the two signals; push; CI green; open the dev→main PR.

### Task 8: Live — the switch without the band

- [x] **Step 1:** Dev time (no flag). The survey again with the band's devices on.
- [x] **Step 2:** `gen3-switch to-3`: the stage rack's and the FOH card's subscriptions resolved to the box, a member's page on the public URL names the box's build with the member's existing session, a member's mix heard on the stage rack's path (the HIL return: phase D's loop), health healthy, the PC live.
- [x] **Step 3:** `gen3-switch to-2`: everything read back equal to before (the card's name and subscriptions, the subscribers, the URL, the band's data), the box stopped and not enabled.
- [x] **Step 4:** Record on #72 (numbers and states only). **Done 2026-10-27:** to-3 in 12 s (the receivers on our device, the public URL on the box, the FOH inputs complete ~50 s after the start), to-2 in 5 s, everything read back equal to before; two live defects fixed red then green (the public read's User-Agent, the card's own returns under its usual name).

### Task 9: The band test (on the owner's signal)

- [ ] **Step 1:** "prepni na 3.x" → `gen3-switch to-3`; the owner's verdict after the band played.
- [ ] **Step 2:** Reactions → "prepni na 2.x" → `gen3-switch to-2`. None → the owner's confirmation; phase G (3.x stays, 2.x a fallback for a while).
- [ ] **Step 3:** Record on #72; tick phase E.

## Risks

- **Dante Controller's 0x1001 reply layout** for our device is not yet captured: a rename from Dante Controller of our test device is the reference (Task 3 Step 1).
- **Receivers re-resolving after the takeover:** expected within seconds; Task 8 measures it on the stage rack.
- **The PC's server while 3.x serves:** its LAN page still answers on the PC; the tunnel is the band's URL. If members use the LAN address, the way back must not lose their changes: Task 5's refusal of a running source covers the copy, not a change made on the PC meanwhile (the switch to 3.x stops the PC's server through the guard until the way back).
