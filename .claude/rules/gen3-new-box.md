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

`scripts/gen3/install-dantesync.sh --host <user@host> --version <vX.Y.Z> --sha256 <asset sha256> --config <ops follower config.json>`: the box's one PTP owner, a follower only. Nothing else may own time or PTP (provision refuses another PTP daemon and disables NTP).

## 3. Provisioning, then one reboot

`scripts/gen3/provision.sh --host <user@host> --ntp-from <ops>/site/gen3-dantesync.json --band-roster <ops>/site/roster.toml` (stack stopped; `--band-roster` puts the band's ids into the sudo rule's `{member}` beside the test roster's: the live gate mints a band member's token). It does, idempotently, with a read-back and `verify_box.py` at the end:

- service user `iemmixer`, folders, state ownership, the PIN pepper credential (`systemd-creds`, never in clear);
- journald on disk; apt: kernel meta packages held, unattended upgrades masked; sleep masked; swap;
- dantesync as the only clock;
- **`realtime`:** `linux-image-realtime` (Ubuntu's PREEMPT_RT, in the normal archive) as grub's saved default, the generic kernel kept as a menu entry; the kernel command line isolates the last two CPUs (`isolcpus=domain,managed_irq`, `nohz_full`, `rcu_nocbs`, `irqaffinity` on the rest); unbound kernel work off them (tmpfiles); `update-grub` again whenever `grub.cfg` lacks the line or the entry. Check `/etc/default/grub.d/*.cfg` sets neither key (it would win);
- **`napi`:** the Dante NIC's packet priority at every boot (`iem-napi.service`): threaded NAPI at SCHED_FIFO 60 on core 0, its interrupt on core 0 (it waits up to 20 s for the interface to be opened), **EEE off**;
- **`display`:** the display poll off at every boot;
- the stack's units never enabled at boot; the sudoers rule.

Then reboot once (`# airuleset:destructive-ok <reason>` on the command) and run verify again: every check 1, `realtime_running` 1 (`uname -r` ends `-realtime`, `/sys/devices/system/cpu/isolated` the last two CPUs).

## 4. The device config (ops)

Copy the shape of `iemmixer-ops/site/gen3-device.toml`: the device name, RX/TX channel names, and the timing proven in phase F:

```
latency_ns = 375000      # P: the RX playout (0.375 ms)
block = 32               # 16 halves B; W must be at least the block
work_margin = 48         # W in samples (0.5 ms)
tx_latency_ns = 250000   # what our TX advertises: receivers play us at it (the IEM card's 0.25 ms)
tx_margin_ns = 30000     # a packet leaves 30 us after its last sample
tx_fpp_max = 8           # the most frames a receiver may ask per packet
arc_rx_fpp = 16          # frames per packet asked of the stage devices (ARC-resolved); lower cuts RX lateness, prove it first
pump_cpu = <N-1>         # N = nproc --all: the two CPUs provisioning isolated
io_cpu = <N-2>
```

- `pump_cpu`/`io_cpu` must be exactly the isolated pair, or the engine runs on housekeeping cores and the timing breaks.
- The config refuses `tx_latency_ns` shorter than `tx_fpp_max` frames + `tx_margin_ns` + 50 µs.
- `block` is 16 or 32 (left out: 32); 16 halves B (167 µs a block) and lets `work_margin` go down to 16, a W below the block is refused. Back to 32 is a config change and a restart, no build; a build before `block` refuses a config that has it.
- On different hardware, prove the timing again before the band hears it (section 7): the values are measured, not universal.

## 5. Deploy

`scripts/gen3/deploy.sh --host <user@host> --bundle <CI's gen3-bundle-<sha>> --sha <sha> --config <ops device config> --iface <Dante NIC> --start engine`, plus `--site-file <ops site.toml> --roster-file <ops roster.toml>` for the band, or `--pins` for the synthetic test band. The checkout must be exactly `<sha>`, clean. Two slots; `--back` returns to the other.

Once the box has been deployed (and while 3.x serves, always): `iempc gen3-deploy --bundle <gen3-bundle-<sha>> --sha <sha>` (deploy.sh alone refuses while 3.x serves; `.claude/rules/gen3.md`, "Deploy while 3.x serves").

While 3.x serves, a provision is followed at once by `iempc gen3-deploy`: provision's `stack` step disables `iem-engine` and `iem-server`, so a reboot in between would not serve the band (first box, 2026-10-28: stop, provision, deploy took 25 s of silence).

## 6. The band's data and URL

- `iempc gen3-data --pepper` (from the PC side): the band's files and the pepper onto the box, read back by sha256.
- `scripts/gen3/install-cloudflared.sh --host ... --version <X.Y.Z> --sha256 <deb sha256>`, then `iempc gen3-tunnel token`: the box's tunnel connector, never enabled by hand (the switch does it).

## 7. Prove it (before the band)

A build the box serves is proven by the box's three gates, run from the dev box in dev time while 3.x serves (each refused with `EVENT-NOW` or while 2.x serves). `deploy.sh`, so `iempc gen3-deploy` too, closes with one line naming them. Each posts a commit status on the box's build (its engine's `--version` SHA). Phase G's confirmation needs `soak/gen3`, `hil/gen3` and `live/gen3` all green on the build the box runs.

1. `iempc gen3-gate soak [--minutes 10|30|60|120]` (default 120), right after the deploy: `iem-soakclient` on the box beside the dev box's reads of `ctl status`, health and the stack's units. No HIL job and no engine restart. It proves the engine as it serves runs an event's length clean: the pump never late, missed, overrun or re-bootstrapped, the clock ready with no holdover or re-anchor, no late RX packet, no TX hole, the listen stream whole. Status `soak/gen3`.
2. `iempc gen3-gate hil`, at least ten minutes after the deploy's start. A HIL job (the engine restarted with `--test-signal --fault-injection`): the builds, health, the RX flows back, the D5(b) loop through our own device, one -30 dBFS burst on the spare outputs then silence, the alarm path's reach, then the panic (the engine's own injected RT fault, systemd's one restart, healthy again, the alarm unit not started), then the job's end. Three engine starts. Status `hil/gen3`.
3. `iempc gen3-gate live`, at least ten minutes after the HIL gate ended (its three starts span its run, its end's restart the last). The live E2E specs in a real browser through the band's public URL, a 30 s burst every 60 s, the client-error marker, the push count. Two engine starts. Status `live/gen3`.

The waits are the engine's restart budget: the unit starts it at most three times in ten minutes (`StartLimitBurst=3`), and the gates keep their own starts, `iempc gen3-deploy`'s and `gen3-switch to-3`'s in iempc's `gen3-gate.json` and refuse a run that would pass it, naming the seconds to wait (a deploy.sh run by hand is not counted). During a burst every in-ear is silent (every mix TX is written as zero), so the HIL and live gates are dev time only. Their rules: `.claude/rules/gen3.md`, the box's HIL, live and soak gates.

Besides the gates, on new hardware or after a timing change:

- `scripts/gen3/live_guard.py --rate 96000 --seconds <n>` as a background task for the whole session.
- 30 minutes with the stage on and the RX mirrored (`iempc gen3-mirror`), reading `ctl status` each second: expect late / missed / overruns 0, the longest block well under 333 µs (175–235 µs on the N100), every RX flow's worst `lateness` under P (14–27 against 36).
- The loop through the IEM card (`iempc gen3-loop --offsets 116,153 --bursts 5 --slot <spare HIL output>`, the PC in maintenance): round trips equal within a burst set, base 153 at `tx_latency_ns` 0.25 ms.
- How to measure each part (RX lateness, block time, a receiver's flow latency through its ARC 0x3200, our TX send spread with `tcpdump -p --time-stamp-precision=nano`, never without `-p`): `.claude/rules/gen3.md`, "Measuring latency on the box".

## Gotchas that cost time once

- At most two `iem-engine` restarts in ten minutes: a third hits the start limit and runs the alarm unit (which notifies people only while the box serves the band). `reset-failed` before a deliberate start resets that count, as deploy.sh and the gates do; the gates' own budget still waits ten minutes between runs (section 7).
- Measure with the stage devices on: otherwise the mirrored flows come from other devices and P is judged on the wrong sources.
- Dante receivers play our flows at our advertised latency, never their own lower setting: a 1 ms `latency_ns` used for TX cost 0.75 ms at the rack.
- The display poll cannot be tested by setting `drm_kms_helper.poll` back to Y at runtime (it does not re-arm); test it with a boot.
- systemd gives `MONITOR_*` only to ExecStartPre=/ExecStart=, never to ExecCondition=.
- `iem-dante-dev health` stays degraded (`restarted`) after any automatic restart of a unit until its next deliberate start (`reset-failed` and `restart`, or a deploy): a check right after a crash or a panic reads degraded, not failed.
- `tcpdump` on the Dante NIC only with `-p`: entering promiscuous mode on the r8169 stalled RX once for ~2.7 ms, every flow late at the same moment (2026-10-28: 63 packets at fpp 4, 17 at fpp 16; the kernel's "entered promiscuous mode" line); with `-p` the capture added no late packet.
