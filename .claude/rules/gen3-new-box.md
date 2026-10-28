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

`scripts/gen3/provision.sh --host <user@host> --ntp-from <ops>/site/gen3-dantesync.json` (stack stopped). It does, idempotently, with a read-back and `verify_box.py` at the end:

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
work_margin = 48         # W in samples (0.5 ms)
tx_latency_ns = 250000   # what our TX advertises: receivers play us at it (the IEM card's 0.25 ms)
tx_margin_ns = 30000     # a packet leaves 30 us after its last sample
tx_fpp_max = 8           # the most frames a receiver may ask per packet
pump_cpu = <N-1>         # N = nproc --all: the two CPUs provisioning isolated
io_cpu = <N-2>
```

- `pump_cpu`/`io_cpu` must be exactly the isolated pair, or the engine runs on housekeeping cores and the timing breaks.
- The config refuses `tx_latency_ns` shorter than `tx_fpp_max` frames + `tx_margin_ns` + 50 µs.
- On different hardware, prove the timing again before the band hears it (section 7): the values are measured, not universal.

## 5. Deploy

`scripts/gen3/deploy.sh --host <user@host> --bundle <CI's gen3-bundle-<sha>> --sha <sha> --config <ops device config> --iface <Dante NIC> --start engine`, plus `--site-file <ops site.toml> --roster-file <ops roster.toml>` for the band, or `--pins` for the synthetic test band. The checkout must be exactly `<sha>`, clean. Two slots; `--back` returns to the other.

Once the box has been deployed (and while 3.x serves, always): `iempc gen3-deploy --bundle <gen3-bundle-<sha>> --sha <sha>` (deploy.sh alone refuses while 3.x serves; `.claude/rules/gen3.md`, "Deploy while 3.x serves").

## 6. The band's data and URL

- `iempc gen3-data --pepper` (from the PC side): the band's files and the pepper onto the box, read back by sha256.
- `scripts/gen3/install-cloudflared.sh --host ... --version <X.Y.Z> --sha256 <deb sha256>`, then `iempc gen3-tunnel token`: the box's tunnel connector, never enabled by hand (the switch does it).

## 7. Prove it (before the band)

- `scripts/gen3/live_guard.py --rate 96000 --seconds <n>` as a background task for the whole session.
- 30 minutes with the stage on and the RX mirrored (`iempc gen3-mirror`), reading `ctl status` each second: expect late / missed / overruns 0, the longest block well under 333 µs (175–235 µs on the N100), every RX flow's worst `lateness` under P (14–27 against 36).
- The loop through the IEM card (`iempc gen3-loop --offsets 116,153 --bursts 5 --slot <spare HIL output>`, the PC in maintenance): round trips equal within a burst set, base 153 at `tx_latency_ns` 0.25 ms.
- How to measure each part (RX lateness, block time, a receiver's flow latency through its ARC 0x3200, our TX send spread with `tcpdump --time-stamp-precision=nano`): `.claude/rules/gen3.md`, "Measuring latency on the box".

## Gotchas that cost time once

- At most two `iem-engine` restarts in ten minutes: a third hits the start limit and runs the alarm unit (which notifies people only while the box serves the band).
- Measure with the stage devices on: otherwise the mirrored flows come from other devices and P is judged on the wrong sources.
- Dante receivers play our flows at our advertised latency, never their own lower setting: a 1 ms `latency_ns` used for TX cost 0.75 ms at the rack.
- The display poll cannot be tested by setting `drm_kms_helper.poll` back to Y at runtime (it does not re-arm); test it with a boot.
- systemd gives `MONITOR_*` only to ExecStartPre=/ExecStart=, never to ExecCondition=.
