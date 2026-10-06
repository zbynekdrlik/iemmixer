# S1c — Windows tuning for a stable 32-sample buffer: design note

**Ticket:** #15 (program #1). **Spec:** program spec I2, I3, I8, P5, P6, P10, R3, D2, G1–G7, §2.1. **Builds on:** S1a (#3: the spike, `spike_window.py`, the baseline at 64/32). **Owned later by:** S6 (#9, the guard). **Plan:** `docs/superpowers/plans/2026-09-27-s1c-windows-tuning.md`. **Site values** (device instances, adapter name, process and task names, the REAPER-mode plan, file paths) live only in `~/.config/iemmixer/pc-tuning.json` and the ops runbook `docs/s1c-pc-runbook.md`.

## 1. Goal and acceptance

iemmixer runs at **B = 32 at 96 kHz** (period 333.3 µs) on the IEM PC, which is an appliance for it (I2, P10). The owner allows admin changes to Windows. REAPER must stay exactly as it is.

Acceptance (#15):

1. **≥ 8 h at B = 32 with 0 missed periods under realistic load** (engine + server + stream).
2. **DPC/ISR latency measured and documented before and after.**
3. **Tuning as a versioned, idempotent, reversible script with read-back,** wired into install and the guard.

Operational definitions:

- **A stable run** is the S1a verdict: 0 missed callbacks (interval ≥ 2 periods), 0 overruns (callback longer than a period), 0 sample-position gaps, and no driver reset, overload or buffer-size message. Late callbacks (> 1.5 periods) are reported with a target of 0 but do not fail a run: a late start still meets the deadline when the work fits in the rest of the period.
- **The S1c gate** (tuning done) is three consecutive stable 30-min runs at 32 under proxy load (§4.3), then one stable 8 h proxy soak.
- **The ticket's acceptance** is the 8 h soak with the real engine, server and a stream on the same tuning version. It needs S6's ASIO backend; until then the first box of #15 stays open (§8, W6).

## 2. Constraints

- **REAPER mode is a contract, not a hope (§5).** Everything REAPER mode depends on is read back identical in event mode (the *fingerprint*). Global changes are a closed, declared list, each with a harmlessness argument and a check after the reboot that activates it. Anything REAPER-sensitive is either applied only while iemmixer holds the card (*mode levers*) or not applied without an owner decision.
- **D2 and EVENT-NOW:** the PC is touched only in dev time, after the owner's "event skončil". `~/.config/iemmixer/EVENT-NOW` pre-empts every wait within 2 s (S1a machinery). The pre-emption also stops a running trace and reverts the mode levers before REAPER starts.
- **The owner's signal is the only gate (#38, owner decision 2026-10-06).** No step measures the stage: `to-dev` saves and quits REAPER without reading its meters, and no input level stops a spike run or a measurement (the spike's band guard and its `band-activity` outcome are gone). Other devices on the Dante network feed the card's inputs, so a level says nothing about the band; the inputs' loudest levels stay in the reports as information.
- **I8 and G4:** nothing is force-killed. Services stop gracefully (no `-Force`), processes are never ended, reboots are graceful (never `/f`).
- **Reboots need the owner's approval.** All reboot-bound levers are grouped into **one** approved reboot. That approval also covers one revert reboot if the checks after it fail (§6.4).
- **I2 and I3:** the buffer is the driver's preferred size, written by S1a's `set-buffer` and restored with read-back before REAPER starts. One ASIO host at a time.
- **Dante:** no Dante setting and no device other than the PC's own card is touched. On the card's driver only the preferred buffer changes (S1a). The DMA transaction size stays 32 unless the owner decides otherwise (§6.5).
- **G7:** the predecessor's code, config and deployment are never changed. Its app and its CI runner are at most placed on housekeeping CPUs (§6.2 L4), never stopped by the tuning.
- **Security:** nothing lowers security without an owner decision. That covers VBS/memory integrity, Defender real-time protection and firmware TPM. The one exception is Windows Update: disabling its service is declared (§6.3 G2) because it only makes the owner's existing no-auto-update policy effective, and Microsoft's soft real-time guidance requires it.
- **P5, P6, Tier 0:** tooling comes only from reviewed `dev` pushes (the S1a bundle). Site values stay private. Rust is compiled in hosted CI only.

## 3. What is known, and what is not

From the read-only inventory of 2026-09 (private research notes; nothing on the PC was changed):

| Item | Value | Relevance |
|---|---|---|
| CPU | 8 cores / 16 logical processors (Zen 3, one core complex), SMT siblings `2k`/`2k+1` | Core layout (§6.1); C-state exit latency |
| OS | Windows 11 IoT Enterprise LTSC 24H2 | Microsoft's soft real-time guidance targets this edition [1] |
| Card | PCIe Gen1 x4 behind the chipset switch; MSI-capable (4 messages); DMA transaction 32 samples | 3000 transfers/s at 96 kHz; interrupt mode and target CPU are **UNVERIFIED** |
| Power | Process Lasso switches to its "Highest Performance" plan while `reaper.exe` runs. That plan has minimum processor state 100 % and PCIe link-state power management off, but USB selective suspend on and the display off after 900 s | The plan while REAPER does *not* run is **UNKNOWN**. IdleSaver is on by default [2] and could switch plans in iemmixer mode |
| Process Lasso | Performance mode triggered by `reaper.exe`. ProBalance ignores non-normal-priority processes by default [3] | Where REAPER's High priority class comes from is **UNKNOWN** (REAPER or a Process Lasso rule) |
| MMCSS | `SystemResponsiveness = 0` (clamped to 20 by Windows [4]), `NetworkThrottlingIndex = 0xFFFFFFFF` | REAPER-mode setting; left alone |
| Updates | No-auto-update policy set; the update service is manual-start | Scans can still run |
| NIC | Onboard 2.5GbE, **linked at 100 Mbps**, power saving on, EEE off. Same network as the Dante control plane; a packet-capture filter driver feeds the PTP clock-sync service | NDIS/filter DPC load; link speed is a physical issue |
| Background | Tunnel service, remote-desktop service, remote-control agent, Audinate services, the predecessor app and CI runner, vendor updater tasks | Placement or scheduling (§6.2, §6.3) |
| GPU | Discrete AMD GPU plus a USB virtual display adapter | Graphics DPCs; display power transitions |
| Firmware | BIOS from 2022; fTPM state **UNKNOWN** | AMD PA-410: fTPM SPI-flash transactions stall the system [5] |
| VBS / memory integrity | **UNKNOWN** | Hypervisor overhead on interrupts [6] |
| S1a results (32/48/64) | **Pending** (window not run yet) | The baseline starts from them |

Also open: what degraded the "original 32" the owner remembers. The inventory (§4.2) reconstructs a timeline of updates, driver installs, new services and tools. That timeline is compared with the owner's recollection only if the measurements leave a regression unexplained.

## 4. Measurement first

No lever is applied before it can be judged: every tier is measured the same way before and after.

### 4.1 Instruments

1. **ASIO callback telemetry (the S1a spike, extended):**
   - existing: interval p50/p99/p99.9/max, missed/late/overruns, position gaps, driver messages, callback CPU time, drift;
   - new: a **glitch log** with the stream-clock time of each glitch and its QPC base (lock-free ring written by the callback, drained by the owner thread);
   - new: **which logical processor ran each callback** and the callback thread's id. The dev box reads that thread's base and current priority from outside, so the driver's thread is never touched;
   - new: a **trace marker per glitch** (`EventWriteString` on a fixed provider, written by the owner thread within 10 ms, payload = the glitch's QPC), so glitches appear on the kernel trace's timeline.
2. **Kernel trace — the LatencyMon-equivalent CLI:**
   - Windows Performance Toolkit `xperf` [7]: `PROC_THREAD+LOADER+DPC+INTERRUPT`, plus `CSWITCH+DISPATCHER` for short diagnostic runs, plus the marker provider in a user session;
   - `xperf -a dpcisr` gives per-module DPC and ISR duration histograms and per-CPU usage [8]; `latency_report.py` turns them into per-module maxima and counts above 100/250/333 µs;
   - installed on the PC in dev time with the ADK bootstrapper's WPT-only feature (Authenticode-verified Microsoft signature, §8 W1). It adds files and no services or drivers: a declared global change;
   - fallback if the install fails: counters, hwlat and the spike only. DPC/ISR attribution is then missing, and the owner is told.
3. **Per-core interrupt counts:**
   - `Win32_PerfRawData_PerfOS_Processor` per logical processor: interrupts, DPCs queued, % DPC time, % interrupt time, C1–C3 transitions;
   - `Win32_PerfFormattedData_Counters_ProcessorInformation`: frequency and % performance;
   - sampled over WMI every 10 s by the window's watch loop. WMI class and property names are language-neutral, unlike perfmon counter paths on a localized Windows;
   - rates are computed on the dev box.
4. **hwlat:**
   - a spike mode that spins one thread at `TIME_CRITICAL` (HIGH class), pinned to one logical processor, and records every gap ≥ 10 µs between two clock reads (histogram plus the 32 largest);
   - run 30 s per logical processor, it maps each CPU's noise, which guides the core layout;
   - gaps with no DPC/ISR in the trace point at SMIs or firmware (the fTPM case).
5. **Sentinels** during every run, sampled every 10 s:
   - the active power plan, the Process Lasso governor state and a light read-back of the mode levers;
   - after the run, the System log's warnings and errors since its start (WHEA, driver resets, power).
6. **Inventory M0** (§4.2) and the **REAPER-mode fingerprint** (§5.1).

### 4.2 Inventory M0 (read-only, first window, REAPER still running)

- **System:** OS build and edition, BIOS version and date, CPU topology (CPU Sets), TPM (`Get-Tpm`), Device Guard status, `bcdedit /enum {current}`, the current timer resolution (`NtQueryTimerResolution`).
- **Power:** plan list, the active plan and its full settings.
- **Devices** (the card, the NIC, GPU, USB controllers, storage): driver version and date, MSI registry values, affinity policy, allocated interrupts.
- **NIC:** advanced properties, RSS and power management.
- **Services and tasks:** every service's start type and state; every enabled scheduled task with its last run.
- **Defender:** exclusions, scan schedule, real-time state.
- **Process Lasso:** its config hash and the relevant lines (IdleSaver, ProBalance, performance mode, default priorities, affinities, CPU sets, SmartTrim).
- **REAPER's process:** priority class, affinity, CPU sets.
- **MMCSS keys.**
- **History:** installed updates, driver installs and new services over 12 months — the timeline behind "what degraded 32".

Raw output stays in the private raw directory. The public report carries numbers and generic names only.

### 4.3 Measurement set per step

In a dev window, card free, buffer 32:

1. hwlat on every logical processor, 30 s each (baseline and after Tier 3 only; ~8 min).
2. **Idle run:** duplex 10 min, no load, with the DPC/ISR trace.
3. **Proxy-load run:** duplex 10 min with the trace. The spike burns 40 µs per callback, about the engine's typical p99.9 (S3 bench: 37.7 µs on the hosted runner). Four normal-priority busy threads on the housekeeping CPUs stand in for the server and the Opus streams.
4. **Headroom run** (report only): 10 min at a 200 µs burn (the engine's worst-case p50).
5. **Diagnostic** (only when glitches stay unattributed): 2 min with `CSWITCH+DISPATCHER`. Near-glitch activity (DPC/ISR/context switches on the callback's and the card's CPUs in the 2 periods before each marker) is extracted from `xperf -a dumper`.

Each step yields one summary: verdict, glitches by kind, callback CPUs, the thread's priority, DPC/ISR per module, per-core rates, sentinel changes and System-log events. Gate runs (30 min) are traced only for DPC/ISR (overhead is low); the 8 h soak runs a 1 GB circular trace that is cut and restarted after each new glitch (at most 5 cuts).

### 4.4 Budgets at 333 µs

- **DPC/ISR:**
  - on the card's CPU and the audio CPU: max ≤ 100 µs. xperf reports power-of-two buckets, so this reads as nothing above the 64–128 µs bucket for a module that runs on those CPUs;
  - anywhere: none ≥ 333 µs (a full period);
  - a module above either limit is named in the step's summary.
- **Callback:**
  - CPU time p99.9 ≤ 50 % of the period under proxy load;
  - hwlat on the audio CPU: no gap ≥ 50 µs.
- **Keep or revert:** a lever stays when its step is not worse on these numbers. A Tier 1–2 lever that is neutral stays too, because it removes a disturbance source (P10). A lever that makes things worse is reverted in the same window.

## 5. REAPER mode

### 5.1 The fingerprint

Read in the first window while REAPER runs (read-only), and stored as the baseline. It is re-read after every `to-event` and after every reboot, and must equal the baseline:

- the active plan's GUID, and a SHA-256 of the full settings of the REAPER-mode plan (`powercfg /qh <plan>`: the plan iemmixer duplicates is never edited);
- the Process Lasso governor's state and start type, and a hash of its config file;
- REAPER's priority class, affinity mask and default CPU Sets;
- the MMCSS `SystemProfile` values and the `Pro Audio` task;
- `ReservedCpuSets` absent; a hash of `bcdedit /enum {current}`; the Device Guard services;
- the tuning journal says no mode lever is active.

If the Process Lasso config turns out to be rewritten by Process Lasso itself (two reads in W1 differ), the fingerprint keeps only its rule lines, selected by key.

### 5.2 Mode levers versus declared global changes

- **Mode levers (§6.2)** are applied by `Enter-IemTuningMode` after REAPER has quit and the card is free. `Exit-IemTuningMode` reverts them before REAPER starts, in `to-event`, in `preempt`, before an approved reboot, and later in the guard's `iemmode event` and at logon (G1).
  - Their before-values are journaled on the PC before the first write, so a crash or reboot in iemmixer mode is reconciled by the next exit.
  - Exit failures raise an owner alarm but never block REAPER. Only the buffer blocks (the S1a rule).
- **Declared global changes (§6.3, §6.4)** stay in both modes. Each has an argument why it cannot hurt REAPER, and each reboot-bound one is checked in event mode after the approved reboot: handover checks (REAPER streams, meters advance, app answers) plus the fingerprint.

## 6. Levers

In order of expected impact and risk. Every lever has a reader (read-back), an idempotent writer and a journaled before-value for revert. "Reboot" means effective only after a reboot.

### 6.1 Core layout (profile values; the W1 hwlat and ISR data may change them)

| Logical processors | Role |
|---|---|
| 0–1 (core 0) | Windows defaults: clock interrupt, unmoved devices |
| 2 (core 1) | The card's ISR/DPC; its sibling 3 stays unplaced |
| 4 (core 2) | NIC ISR/DPC and RSS (base 4, max 5) |
| 6–13 (cores 3–6) | Housekeeping: background processes, server, streams, stress |
| 14 (core 7) | The engine process (audio CPU Set); its sibling 15 stays unplaced |

In the profile this is `layout`: `housekeeping` 6–13, `card` 2, `nic` 4, `audio` 14 (a role is absent or a list of integers 0..63, one role per processor; #32 MINOR-6). The RSS range (`nic.rss.base`..`max`, here 4–5) starts on a `layout.nic` processor and past it may reach only processors of no role (here LP 5, the NIC core's unplaced sibling), never a card, audio or housekeeping processor; the tuning module refuses any other range before a write (#32 MINOR-5).

Three placements of the engine are compared (§8 W2), and the best one is kept:

- no CPU Set;
- LP 14 (a dedicated core);
- LP 3 (the card DPC's sibling).

### 6.2 Tier 1 — mode levers (no reboot; REAPER untouched by construction)

| Id | What and why | Apply / read-back / revert |
|---|---|---|
| L1 `buffer` | The driver's preferred buffer is 32 (I2, S1a). | S1a `set-buffer` / `restore-buffer`, read back. |
| L2 `plan` | An **iemmixer power plan**, duplicated from the REAPER-mode plan (so everything else carries over) and activated. Settings: processor min/max 100 %, core parking off (min cores 100 %), EPP 0, PCIe ASPM off, USB selective suspend off, display/disk/sleep/hibernate never. Idle is A/B-tested: default, `IDLESTATEMAX = 1` (C1 only) [9], or `IDLEDISABLE = 1` (Microsoft's soft-RT step 1 [1]); the choice watches frequency and % performance counters for thermal loss. Why: C-state exits, core unparking and USB suspend/resume are classic DPC-latency sources, and display power-off triggers graphics driver work. | `powercfg /duplicatescheme` (fixed GUID), `PowerWriteACValueIndex` / `PowerReadACValueIndex` (language-neutral), `PowerSetActiveScheme`; revert = re-activate the journaled plan. The plan stays defined but inactive. |
| L3 `governor` | **Pause Process Lasso's governor service** while iemmixer holds the card. Its config stays byte-identical, so REAPER mode keeps exactly its Process Lasso. Why: with no `reaper.exe`, IdleSaver (on by default [2]) may switch plans on idle, and its rules can act on our processes. Two owners of the power plan means drift. Rejected: editing its config (changes REAPER mode's tool); uninstalling it (may change REAPER's priority and plan). If the GUI restarts the governor, the sentinel shows it and it becomes an owner question. | `Stop-Service` / `Start-Service` (no `-Force`), wait bounded; read-back = status. |
| L4 `placement` | **Housekeeping placement:** the default CPU Sets of the listed background processes (tunnel, remote access, clock sync, Audinate, the predecessor's app and runner) are set to the housekeeping CPUs. Soft isolation without `ReservedCpuSets`. Protected processes that refuse are reported. | `SetProcessDefaultCpuSets` from the module (P/Invoke); revert = the journaled set (normally none); processes that exited are skipped. |
| L5 `engine` | **In-process** (spike now, engine in S6). HIGH priority class (spec §2.1; REALTIME would starve system threads without isolated cores). Power throttling off (`EXECUTION_SPEED`, `IGNORE_TIMER_RESOLUTION` [10]). Default CPU Set = the audio CPU (A/B, §6.1). It applies to every thread of the process, the driver's included, but only its **placement** changes, never its priority (spec §2.1). Busy threads go to the housekeeping CPUs. | Spike flags `--audio-cpus` / `--stress-cpus`; the report reads back the CPU Set IDs and the callback CPUs. |
| L6 `services-mode` (conditional) | Stop the PTP clock-sync service or Windows Audio only while iemmixer runs — only if traces attribute interference to them. Microsoft's soft-RT guidance disables Audiosrv [1]; the card is ASIO-only. | Stop / start with read-back. |

### 6.3 Tier 2 — declared global changes, no reboot

| Id | What and why | Why REAPER cannot suffer |
|---|---|---|
| G1 `services` | Disable SysMain, the Diagnostic Policy Service (both Microsoft soft-RT steps [1]), Windows Search and the telemetry service (DiagTrack). They cause background I/O and CPU bursts. | REAPER uses none of them. |
| G2 `updates` | The Windows Update service goes to Disabled; update-orchestrator tasks are disabled where Windows permits (refusals are recorded). Microsoft: "the Windows Update agent does not respect CPU core isolation" [1]. Maintenance procedure: revert G2 in a dev window, update, re-apply. | The no-auto-update policy is already set. Fewer scans during events. |
| G3 `maintenance` | Automatic Maintenance off (`MaintenanceDisabled = 1`). Disable the maintenance tasks: defrag, disk diagnostics, compatibility appraiser, CEIP, WinSAT, power-efficiency diagnostics. Disable the vendor updater tasks (private list): they can also change REAPER mode's software unasked. | Removes bursts in both modes. |
| G4 `defender` | Exclusions for iemmixer's directories and executables only, which are writable only by the owner account and administrators. Real-time protection and scans stay. | REAPER's scanning is unchanged. |

### 6.4 Tier 3 — declared global changes, the one approved reboot

| Id | What and why | Why REAPER cannot suffer |
|---|---|---|
| R1 `irq-card` | Card interrupt affinity → LP 2 (`Affinity Policy`: `DevicePolicy = 4` IrqPolicySpecifiedProcessors, `AssignmentSetOverride` = mask [11]). Applies only when the card already uses MSI; enabling MSI is Tier 4. The profile's instance path is checked against the hardware id before any write. | The ISR moves off core 0's crowd. REAPER's threads keep all CPUs. Verified by the post-boot handover checks (REAPER meters advance = the card delivers). |
| R2 `irq-nic` | NIC: RSS base/max → LP 4–5 (the base on `layout.nic`, the rest on it or on processors of no role, §6.1; MSI-X affinity only if its ISRs still land elsewhere); "power saving" and "allow the computer to turn off this device" off. Written as the adapter's driver-key values [15], which NDIS reads at initialization: no network drop before the reboot. | The web control plane and meters keep working; power saving off only reduces wake latency. |
| R3 `irq-others` (conditional) | Graphics, USB controller and storage interrupts away from LP 2/14 — only if their ISR/DPC land there in the traces. | As R1. |

Undo: each value is written back from the journal, which needs another reboot. That revert reboot is pre-approved in the same question.

### 6.5 Tier 4 — only with a named cause after Tiers 1–3, each an owner decision (❓)

| Id | Lever | Why it needs the owner |
|---|---|---|
| X1 | `ReservedCpuSets` (hard isolation at boot [12]) | REAPER loses the reserved CPUs (never measured). Microsoft warns that soft-RT core reservation without the full preparation can need reimaging [1]. |
| X2 | VBS / memory integrity off [6] | Lowers security. |
| X3 | Enable MSI on the card (if it uses line interrupts) [13] | The 2016 driver's MSI support is unknown: the card could fail at the next event boot. |
| X4 | DMA transaction 16 samples | REAPER runs at 32; the driver likely reads it only at load, so it cannot be mode-scoped. |
| X5 | fTPM off / BIOS update [5] | Firmware setting at the PC. |
| X6 | `bcdedit /set disabledynamictick yes` | Weak evidence; only if hwlat shows tick-periodic gaps; reboot. |
| X7 | Graphics driver change | REAPER mode's driver. |
| X8 | The 100 Mbps NIC link | Cable or switch port (physical). |

### 6.6 Considered and not a lever

- **Timer resolution:**
  - the callback is paced by the card's interrupt, not by timers;
  - since Windows 10 2004 `timeBeginPeriod` is per-process, and Windows 11 ignores it for hidden processes unless they opt out [14];
  - the engine opts out (L5); its helper threads use high-resolution waitable timers (S6);
  - the current resolution is recorded in M0.
- **HPET (`useplatformclock`):** the default (TSC) stays; M0 checks that it is not forced.
- **MMCSS registry:** stays as it is (a REAPER-mode setting; `SystemResponsiveness = 0` is already clamped to 20 [4]).
- **Engine helper threads (S6 rule):**
  - they run below the callback thread's measured priority;
  - MMCSS "Pro Audio" (23–26 [4]) only for a helper that stays below it; a helper above the driver's thread could pre-empt it;
  - the spike measures the priority.
- **Threaded DPCs off:** a debugging step in Microsoft's guidance [1], not a tuning lever.
- **Game Mode, GPU scheduling, page file, priority separation:** no mechanism that affects an interrupt-paced callback on an idle console.

## 7. The tuning tool

**PC side** (Windows PowerShell 5.1, shipped in the S1a bundle):

- `scripts/pc-tuning/IemTuning.psm1`: items with a reader, a writer and a revert, as data (kind + arguments). No closures: `GetNewClosure` loses the module's private functions.
  - **Kinds:** registry value, service start type, service state, scheduled task, plan existence, plan value, active plan, Defender exclusion, process CPU Sets.
  - The NIC levers are registry values in the adapter's driver key: its advanced properties, `*RssBaseProcNumber` / `*RssMaxProcNumber` and `PnPCapabilities`. NDIS reads them when the adapter initializes, so they take effect at the reboot, and the read-back is exact.
- **Journal** `%ProgramData%\iemmixer\tuning\journal.json`:
  - per item: kind, arguments, before-value, time, boot time, reboot flag;
  - a `global` section and a `mode` section;
  - the profile version that applied.
- **Commands:**
  - `Invoke-IemTuningApply -Tier 2|3 [-Only]`: writes only differing values, journals before-values once, reads back;
  - `Undo-IemTuning`: writes before-values back, reads back, drops the entries;
  - `Enter-IemTuningMode` / `Exit-IemTuningMode`: exit continues past failures and reports them together; both are idempotent;
  - `Get-IemTuningState`: desired, actual, before, pending-reboot (applied after the current boot), version drift;
  - `Get-IemReaperFingerprint` / `Compare-IemFingerprint`;
  - `Get-IemInventory`.
- **Device writes** refuse unless the instance's hardware id matches the profile. Reboot-bound items report `pending` until the next boot.
- `scripts/pc-tuning/IemMeasure.psm1`: xperf start/stop/analyze, marker session, per-CPU counter snapshots, System-log events, WPT install check.
- **Self-test** `Test-IemTuning.ps1` in CI (hosted `windows-2025`, PowerShell 5.1), against real backends on the ephemeral runner:
  - registry items under an HKCU test root;
  - a real service and a scheduled task;
  - a real duplicated power plan;
  - Defender exclusions;
  - CPU Sets of a child process.
  - It proves that the second apply writes nothing, and that undo and exit restore the original (absent values included).

**Dev box:**

- `scripts/pc-tuning/tuning_window.py`: `tuning-setup`, `inventory`, `fingerprint`, `wpt-install`, `enter`, `exit`, `apply`, `undo`, `state`, `measure`, `hwlat`, `reboot-prepare`, `reboot` (records the owner's quoted approval, then a graceful restart or `--by-owner`), `post-boot`.
- `scripts/pc-tuning/latency_report.py`: the summaries.
- `spike_window.py` gains two unwind steps. After `stop-spike`, the order is: `trace-stop` → `tuning-exit` → `restore-buffer` → `bring-back`. Each is recorded before its action, so `preempt` finds it.

**S6 ownership:**

- the guard calls the same module through an elevated task of its own design:
  - `Enter` after REAPER quits;
  - `Exit` in `iemmode event` and at logon (G1);
  - `Get-IemTuningState` at every `iemmode` call, raising a drift alarm on any difference;
- the install applies Tier 2 idempotently when the profile version rises;
- reboot-bound tiers are applied only in an owner-approved maintenance window, never by the guard on its own.

## 8. PC windows (dev time only, EVENT-NOW pre-empts every step)

| Window | Contents | Time |
|---|---|---|
| W1 | Inventory M0 and fingerprint baseline (REAPER running, read-only), the fingerprint read twice, WPT install; `to-dev`; baseline measurement set at 32 (§4.3) with no tuning. Needs S1a Task 12 done (or runs right after it). | ~2 h |
| W2 | Tier 1 A/B: plan idle variants, governor pause, placement, engine CPU Set. Then Tier 2 apply and measure. Then Tier 3 written (pending); `reboot-prepare` (unwind to free + buffer restored + mode exited). | ~2.5 h |
| Reboot | The owner approves (one ❓ covering the reboot and one revert reboot). Graceful restart; the PC comes back in event mode. `post-boot`: handover checks, fingerprint, nothing pending, card interrupts on LP 2. Development continues only after the owner's next "event skončil" (G1, D2). | ~15 min |
| W3 | Measurement set after Tier 3 (hwlat again); three 30-min gate runs; Tier 4 questions only with a named cause. | ~2.5 h |
| W4 | The 8 h proxy soak. "ide event" ends it; it restarts from zero in the next window. | 8 h+ |
| W5 | Report (§10). | — |
| W6 | With S6's backend: the 8 h real-load soak (engine at 32 on the card, server, one listen stream and one mixer client connected) on the same tuning version — the ticket's acceptance. | 8 h+ |

## 9. Risks

- **A global lever breaks REAPER at an event:** declared list; activation only through the approved reboot in dev time with post-boot checks; the revert reboot pre-approved; fingerprint and handover checks on every `to-event`.
- **Interrupt affinity on the 2016 driver is ignored or breaks the card:** the post-boot checks see it (no meters / no card interrupts on LP 2), then revert.
- **The governor restarts itself, or the fingerprint changes on its own:** sentinel; W1 double read; owner question with the evidence.
- **Soft CPU Sets are not enough:** Tier 4 X1 with numbers.
- **`IDLEDISABLE` heats the CPU and costs boost clock:** the frequency counters decide; C1-only is the middle option.
- **Tracing perturbs timing:** gate runs trace only DPC/ISR; `CSWITCH` only in 2-min diagnostics.
- **The soak collides with an event:** pre-emption; no partial credit.
- **WPT cannot be installed:** fallback per §4.1, owner informed.
- **The regression behind the "original 32" is unknown:** the M0 timeline plus A/B.
- **The xperf text formats differ from the documented samples:** the parsers are validated against the first real outputs (scrubbed fixtures, RED/GREEN).

## 10. Deliverables and hand-offs

- **Report on #15** (Slovak, numbers):
  - before/after per tier: verdicts, glitches, DPC/ISR maxima per module, per-core rates, hwlat maxima;
  - the chosen layout and plan variant;
  - the fingerprint result;
  - the soak verdict.
- **Results in this note:** `## 11. Results`.
- **S6 (#9):**
  - the guard owns the module: enter/exit/state, drift alarm, logon reconciliation, install of Tier 2;
  - the engine applies L5 via `iem_audio_io::os`;
  - the helper-thread priority rule (§6.6);
  - locked RT memory (`VirtualLock`) so trimmed pages never fault in the callback.
- **S7 (#10):** the real-load soak method (W6) and the per-run summary format.
- **After cutover (S8):** REAPER mode becomes rollback-only; Tier 4 items may then be re-weighed with the owner.

## Sources

1. Microsoft, *How to set up a Device for Real-Time Performance* (Windows IoT Enterprise soft real-time): https://learn.microsoft.com/en-us/windows/iot/iot-enterprise/soft-real-time/soft-real-time-device ; *Developing an Application for Real-Time Performance*: https://learn.microsoft.com/en-us/windows/iot/iot-enterprise/soft-real-time/soft-real-time-application
2. Bitsum, IdleSaver: https://bitsum.com/apps/process-lasso/docs/power/idlesaver/ ; Process Lasso automation: https://bitsum.com/automation/
3. Bitsum, ProBalance: https://bitsum.com/apps/process-lasso/docs/algorithms/probalance/ ; FAQ: https://bitsum.com/process-lasso-faq/
4. Microsoft, Multimedia Class Scheduler Service: https://learn.microsoft.com/en-us/windows/win32/procthread/multimedia-class-scheduler-service
5. AMD, PA-410 (fTPM stutter): https://www.amd.com/en/resources/support-articles/faqs/PA-410.html
6. Microsoft, Options to optimize gaming performance in Windows 11 (memory integrity, VMP): https://support.microsoft.com/en-us/windows/options-to-optimize-gaming-performance-in-windows-11-a255f612-2949-4373-a566-ff6f3f474613
7. Microsoft, Download and install the Windows ADK (ADK 10.1.26100.9457, WPT): https://learn.microsoft.com/en-us/windows-hardware/get-started/adk-install
8. Microsoft, xperf `dpcisr` action: https://learn.microsoft.com/en-us/windows-hardware/test/wpt/dpcisr ; example output: https://forums.guru3d.com/threads/simple-way-to-trace-dpcs-and-isrs.423884/
9. Microsoft, IDLESTATEMAX: https://learn.microsoft.com/en-us/previous-versions/mt422916(v=vs.85)
10. Microsoft, `timeBeginPeriod` (per-process and Windows 11 behaviour): https://learn.microsoft.com/en-us/windows/win32/api/timeapi/nf-timeapi-timebeginperiod ; CPU Sets: https://learn.microsoft.com/en-us/windows/win32/procthread/cpu-sets ; `SetProcessDefaultCpuSets`: https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-setprocessdefaultcpusets
11. Microsoft, Interrupt affinity: https://learn.microsoft.com/en-us/windows-hardware/drivers/kernel/interrupt-affinity-and-priority
12. `ReservedCpuSets` (the registry form of the IoT `SetRTCores` CSP): https://github.com/valleyofdoom/ReservedCpuSets
13. Microsoft, Enabling message-signaled interrupts in the registry: https://learn.microsoft.com/en-us/windows-hardware/drivers/kernel/enabling-message-signaled-interrupts-in-the-registry
14. B. Dawson, Windows timer resolution: the great rule change: https://randomascii.wordpress.com/2020/10/04/windows-timer-resolution-the-great-rule-change/
15. Microsoft, `Set-NetAdapterAdvancedProperty` (`-NoRestart`): https://learn.microsoft.com/en-us/powershell/module/netadapter/set-netadapteradvancedproperty ; `Set-NetAdapterRss`: https://learn.microsoft.com/en-us/powershell/module/netadapter/set-netadapterrss
