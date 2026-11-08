---
paths:
  - "scripts/gen3/provision.sh"
  - "scripts/gen3/provision-root.sh"
  - "scripts/gen3/verify_box.py"
  - "scripts/gen3/install-dantesync.sh"
  - "scripts/gen3/install-cloudflared.sh"
  - "scripts/gen3/deploy.sh"
  - "scripts/gen3/napi-priority"
---

# A Gen 3 box from zero (#72; the cookbook)

**One command:** `iempc gen3-new-box --host <admin@fresh> --bundle <gen3-bundle-<sha>> --sha <sha> [--iface <name>] [--from-backup <stamp>]` runs sections 2, 3, 5 and 6 in order; a rerun resumes. It needs sections 0 and 1 done by hand (the admin with a passwordless sudo rule of its own, which provision needs too) and `GEN3_DANTESYNC_CONFIG`, `GEN3_DANTESYNC_VERSION`, `GEN3_DANTESYNC_SHA256`, `GEN3_CLOUDFLARED_VERSION`, `GEN3_CLOUDFLARED_SHA256` in iempc's private env, beside the deploy's keys. `pump_cpu` and `io_cpu` of the device config must fit the new box first (section 4). It refuses GEN3_HOST. The result is a spare: its units stay disabled, and its device runs under the ops config's name plus `-spare` with a state file of its own (the serving name beside the serving box would be a second device on the band's network). A band data set comes from the running box, or from `--from-backup` when that box no longer answers.

**Off-box copy:** `iempc gen3-backup` (the stack keeps running) writes `~/.config/iemmixer/box-backup/<stamp>/` (0700; band.zip, manifest.json, pepper, 0600). It holds the band's secrets and stays there: never in this repo, never in the ops repo. The box needs a deploy that carries `band_send.py`, and a provision for the rule of `systemd-creds decrypt`.

**Drift:** `iempc gen3-drift` compares the box with this checkout: the units, the helpers, the sudo template provision left in its stage, the device config, the build (a commit after it that changes no crate is no drift), `verify_box.py`, the units that start at boot, the pinned dantesync and cloudflared. Exit 1 names each difference.

**What it does not carry:** the Dante state file (`state.toml`, our RX subscriptions) stays behind on purpose: to keep them, copy it from the old box with the stack stopped, before the first start under the band's name.

**A spare takes the band over:** stop the old box if it still runs, set `GEN3_HOST` to the spare, run `iempc gen3-deploy` (it installs the ops device config and enables the units), put the tunnel's token into the box's credential by hand (the value only on stdin of `sudo -n systemd-creds encrypt --name=tunnel --with-key=auto - /etc/credstore.encrypted/iemmixer-tunnel`, then `chmod 0600`), `systemctl enable --now iem-tunnel`, then prove it (section 7).

Everything below was worked out on the first box (Intel N100, 4 cores, Realtek r8169, Ubuntu 26.04) in phases D–F; the details and numbers live in `.claude/rules/gen3.md` and on #72. Real names, addresses and the ops files' contents never enter this repo (P6): they are in `iemmixer-ops` and iempc's private env. Every step is dev time only (never while `EVENT-NOW` exists).

## 0. Hardware

- x86-64 with **at least 4 and at most 32 CPUs** (`nproc --all`): provisioning isolates the last two for the engine and refuses anything else.
- A wired NIC on the band's Dante network, its link up at 1 Gbit/s. The r8169 holds phase F's timing on the realtime kernel; an Intel i210/i225/i226 is the upgrade path only if a measurement shows the NIC is the limit (owner: buy nothing unless a measurement says so).
- No screen needed. Display output polling stays off (provision's `display` step): with it on, the i915 poll held the NIC's core for ~2 ms and 245–637 RX packets a flow came late in 30 minutes (2026-10-28).

## 1. The operating system

- Ubuntu 26.04 server on the internal disk (the first box: a clean install from a USB stick; the stock HWE kernel is fine, provisioning adds the realtime one).
- The admin user with the dev box's SSH key, `sshd` enabled at boot, DHCP or a fixed address on the Dante subnet.
- Host name: the site's role-venue pattern (the owner rejected `devN`).
- Put the host into iempc's private env (`GEN3_HOST`, `GEN3_DANTE_IP`, `GEN3_DANTE_NAME`, ...) as the first box has it.

## 2. Time: dantesync

`scripts/gen3/install-dantesync.sh --host <user@host> --version <vX.Y.Z> --sha256 <asset sha256> --config <ops follower config.json>`: the owner of the box's system clock, a follower only, `system.gm_allowlist` the audio network's subnet. Only the engine's in-process follower listens to PTP beside it (`CAP_NET_BIND_SERVICE` in `iem-engine.service`, dstack v0.6.23); nothing else may own time or PTP (provision refuses another PTP daemon and disables NTP).

## 3. Provisioning, then one reboot

`scripts/gen3/provision.sh --host <user@host> --ntp-from <ops>/site/gen3-dantesync.json --band-roster <ops>/site/roster.toml` (stack stopped; `--band-roster` puts the band's ids into the sudo rule's `{member}` beside the test roster's: the live gate mints a band member's token). It does, idempotently, with a read-back and `verify_box.py` at the end:

- service user `iemmixer`, folders, state ownership, the PIN pepper credential (`systemd-creds`, never in clear);
- journald on disk; apt: kernel meta packages held, unattended upgrades masked; sleep masked; swap;
- dantesync as the only clock;
- **`realtime`:** `linux-image-realtime` (Ubuntu's PREEMPT_RT, in the normal archive) as grub's saved default, the generic kernel kept as a menu entry; the kernel command line isolates the last two CPUs (`isolcpus=domain,managed_irq`, `nohz_full`, `rcu_nocbs`, `irqaffinity` on the rest); unbound kernel work off them (tmpfiles); `update-grub` again whenever `grub.cfg` lacks the line or the entry. Check `/etc/default/grub.d/*.cfg` sets neither key (it would win);
- **`napi`:** the Dante NIC's packet priority at every boot (`iem-napi.service`): threaded NAPI at SCHED_FIFO 60 on core 0, its interrupt on core 0 (it waits up to 20 s for the interface to be opened), **EEE off**;
- **`napi` also shapes the Dante interface** (`iem-napi.service`): TSO/GSO off, root `prio` with `tbf` 100 Mbit on the non-audio bands, PTP (UDP 319/320) in the unlimited band; audio goes past the qdisc through AF_PACKET (`CAP_NET_RAW` on the engine). Without it 300 kB of TCP from the box read 650 µs at a receiver and no counter of ours saw it. Read a receiver's Latency tab, not the box's counters;
- **`idle`:** C6, C8 and C10 off at every boot (a receiver read 490 µs against 240 µs with them on, no counter of ours sees it; the state numbers are the N100's);
- **`display`:** the display poll off at every boot;
- `iem-dante.service` disabled at boot (the engine and the server are the deploy's to enable); the sudoers rule.

Then reboot once (`# airuleset:destructive-ok <reason>` on the command) and run verify again: every check 1, `realtime_running` 1 (`uname -r` ends `-realtime`, `/sys/devices/system/cpu/isolated` the last two CPUs).

## 4. The device config (ops)

Copy the shape of `iemmixer-ops/site/gen3-device.toml`: the device name, RX/TX channel names, and the timing proven on the first box (build 3b18f30: `soak/gen3` 2 h with 19 listen gaps counted, `hil/gen3` and `live/gen3` green; numbers on #72, 2026-10-29):

```
latency_ns = 250000      # P: the RX playout (0.25 ms, 24 samples; the stage's worst packet 13 late)
block = 16               # B (167 us); W must be at least the block
work_margin = 16         # W in samples (the longest block 88 us)
tx_latency_ns = 250000   # what our TX advertises: receivers play us at it (the IEM card's 0.25 ms)
tx_margin_ns = 30000     # a packet leaves 30 us after its last sample
tx_fpp_max = 8           # the most frames a receiver may ask per packet
arc_rx_fpp = 4           # frames per packet asked of the stage devices (ARC-resolved): 16 left P at 36
pump_cpu = <N-1>         # N = nproc --all: the two CPUs provisioning isolated
io_cpu = <N-2>
```

D = B + P + W = 16 + 24 + 16 = 56 samples; mic to ear with the rack's 24 is 80 samples, 0.83 ms at 96 kHz (phase F's 32 + 36 + 48 was 140, 1.46 ms).

- `pump_cpu`/`io_cpu` must be exactly the isolated pair, or the engine runs on housekeeping cores and the timing breaks.
- The config refuses `tx_latency_ns` shorter than `tx_fpp_max` frames + `tx_margin_ns` + 50 µs.
- `block` is 16 or 32 (left out: 32); 16 halves B (167 µs a block) and lets `work_margin` go down to 16, a W below the block is refused. Back to 32 is a config change and a restart, no build; a build before `block` refuses a config that has it.
- On different hardware, prove the timing again before the band hears it (section 7): the values are measured, not universal.

## 5. Deploy

`scripts/gen3/deploy.sh --host <user@host> --bundle <CI's gen3-bundle-<sha>> --sha <sha> --config <ops device config> --iface <Dante NIC> --new-box --start engine`, plus `--site-file <ops site.toml> --roster-file <ops roster.toml>` for the band, or `--pins` for the synthetic test band. The checkout must be exactly `<sha>`, clean. Two slots; `--back` returns to the other.

The box that serves the band: `iempc gen3-deploy --bundle <gen3-bundle-<sha>> --sha <sha>` (`.claude/rules/gen3.md`). Without `--new-box`, deploy.sh needs the band's site and roster and enables the units.

A provision needs a short stop on the box that serves: it refuses a running `iem-engine` or `iem-server`. Stop both, run it, start both; its `stack` step leaves them enabled (it disables only the retired `iem-dante.service`), so a reboot still serves the band and no deploy has to follow. A change to `sudo-lines.txt` reaches the box only this way.

## 6. The band's data and URL

- `iempc gen3-new-box` carries the band's files and the pepper from the running box or from a backup, read back by sha256.
- `scripts/gen3/install-cloudflared.sh --host ... --version <X.Y.Z> --sha256 <deb sha256>`: the box's tunnel connector, installed and not enabled (the spare's takeover above does that).

## 7. Prove it (before the band)

A build the box serves is proven by the box's three gates, run from the dev box in dev time (each refused with `EVENT-NOW`). `deploy.sh`, so `iempc gen3-deploy` too, closes with one line naming them. Each posts a commit status on the box's build (its engine's `--version` SHA). Phase G's confirmation needs `soak/gen3`, `hil/gen3` and `live/gen3` all green on the build the box runs.

1. `iempc gen3-gate soak [--minutes 10|30|60|120]` (default 120), right after the deploy: `iem-soakclient` on the box beside the dev box's reads of `ctl status`, health and the stack's units. No HIL job and no engine restart. It proves the engine as it serves runs an event's length clean: the pump never late, missed, overrun or re-bootstrapped, the clock ready with no holdover or re-anchor, no late RX packet, no TX hole, the listen stream complete with no gap and no reconnect (#88). Status `soak/gen3`.
2. `iempc gen3-gate hil`, at least ten minutes after the deploy's start. A HIL job (the engine restarted with `--test-signal --fault-injection`): the builds, health, the RX flows back, the D5(b) loop through our own device, one -30 dBFS burst on the spare outputs then silence, the alarm path's reach, then the panic (the engine's own injected RT fault, systemd's one restart, healthy again, the alarm unit not started), then the job's end. Three engine starts. Status `hil/gen3`.
3. `iempc gen3-gate live`, at least ten minutes after the HIL gate ended (its three starts span its run, its end's restart the last). The live E2E specs in a real browser through the band's public URL, a 30 s burst every 60 s, the client-error marker, the push count. Two engine starts. Status `live/gen3`.

The waits are the engine's restart budget: the unit starts it at most three times in ten minutes (`StartLimitBurst=3`), and the gates keep their own starts and `iempc gen3-deploy`'s in iempc's `gen3-gate.json` and refuse a run that would pass it, naming the seconds to wait (a deploy.sh run by hand is not counted). During a burst every in-ear is silent (every mix TX is written as zero), so the HIL and live gates are dev time only. Their rules: `.claude/rules/gen3.md`, the box's HIL, live and soak gates.

Besides the gates, on new hardware or after a timing change:

- `scripts/gen3/live_guard.py --rate 96000 --seconds <n>` as a background task for the whole session.
- 30 minutes with the stage on, reading `ctl status` each second: expect late / missed / overruns 0, the longest block well under one block's time (167 µs at block 16: 88 µs on the N100), every RX flow's worst `lateness` under P (13 against 24 at `arc_rx_fpp` 4).
- How to measure each part (RX lateness, block time, a receiver's flow latency through its ARC 0x3200, our TX send spread with `tcpdump -p --time-stamp-precision=nano`, never without `-p`): `.claude/rules/gen3.md`, "Measuring latency on the box".

**Reboot and sudden loss (#72).** After the three gates: `iempc gen3-gate reboot`, then `iempc gen3-gate reset` (magic SysRq `b` under 20 Hz level writes, no sync). Each restarts the box for real and fails on a late or wide clock, a failed unit, a filesystem error, a lost state, a changed file. The first boot of a new box has no `/etc/dantesync/drift.json`: run dantesync locked for 10 minutes first (it saves every 10 minutes), else `phase` fails with about -700 us until dsync#146.

**The BIOS must power the box up by itself** after a power cut (the setting's name differs by board; look for restore on AC power, Power On). Set it before the box serves: a box that stays off is an event without in-ears. Software cannot read it, so a real power cut proves it.

## Gotchas that cost time once

- At most two `iem-engine` restarts in ten minutes: a third hits the start limit and runs the alarm unit (which notifies people only while the box serves the band). `reset-failed` before a deliberate start resets that count, as deploy.sh and the gates do; the gates' own budget still waits ten minutes between runs (section 7).
- Measure with the stage devices on: otherwise those flows come from other devices and P is judged on the wrong sources.
- Dante receivers play our flows at our advertised latency, never their own lower setting: a 1 ms `latency_ns` used for TX cost 0.75 ms at the rack.
- The display poll cannot be tested by setting `drm_kms_helper.poll` back to Y at runtime (it does not re-arm); test it with a boot.
- systemd gives `MONITOR_*` only to ExecStartPre=/ExecStart=, never to ExecCondition=.
- `iem-dante-dev health` stays degraded (`restarted`) after any automatic restart of a unit until its next deliberate start (`reset-failed` and `restart`, or a deploy): a check right after a crash or a panic reads degraded, not failed.
- A timing key changed on the box by hand is checked against `device.rs`'s bounds first: a device config the engine cannot load keeps it down (exit 2, no restart) and the alarm notifies people (2026-10-28 22:26: `work_margin = 24` with the block at 32, 3 min 41 s down). Keep the previous config and put it back at once when the engine is not healthy within seconds.
- `tcpdump` on the Dante NIC only with `-p`: entering promiscuous mode on the r8169 stalled RX once for ~2.7 ms, every flow late at the same moment (2026-10-28: 63 packets at fpp 4, 17 at fpp 16; the kernel's "entered promiscuous mode" line); with `-p` the capture added no late packet.
- dantesync steps the box's date by +1.6 s at 03:00 every day (the fleet's coordinated step): the engine holds its clock ~5.6 s around it (`holdover`, then back) with no pump fault, but a soak spanning 03:00 counts that holdover and fails. Run the gates outside it (#72, 2026-10-29).
- The soak's listen `gaps` (#88, low priority) come with the dev box's ssh reads beside it (measured on #88: more reads, more gaps; the server's own sends stall); the in-ear path is untouched by them (the 2 h soak with 19 gaps: pump, RX and TX counts all 0). The gate counts them and never judges them; a run with gaps and every other check clean is a pass.
