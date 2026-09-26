# S1b — Golden renders from REAPER: design note

**Ticket:** #4 (program #1). **Spec:** `2026-09-24-iemmixer-gen2-program.md` §3.3–3.5, §4, D7. **Plan:** `docs/superpowers/plans/2026-09-26-s1b-golden-renders.md`. **Site values** (PC paths, host, task names, the predecessor's data folders) live only in the private ops runbook `14-s1b-pc-runbook-private.md`.

## 1. Goal

Measure the REAPER behaviour that documentation does not settle, before S2 freezes the DSP. Done means:

- **Measured laws**, each with its source render and residual:
  - send and track pan taper;
  - shelf bandwidth semantics and HPF band gain;
  - mono downmix into a mono destination, and mono media level;
  - FX-chain and single-FX bypass;
  - trim, send volume and summing.
- **Golden vectors** committed (≤ 20 MB) for S2:
  - the ReaEQ matrix at 44.1, 48 and 96 kHz;
  - the 44 anonymised site EQs;
  - limiter outputs on hot material.
- **Evidence on #4** that every render window restored the IEM PC bit-identically.

## 2. Constraints

- **D7:** only the IEM PC's REAPER 7.65, only in dev time; full backup first, verified bit-identical restore last; offline renders of generated projects, no ASIO card, no Dante; the original project is never touched by the render work; the owner is not needed at the PC.
- **D2, §4.3:** save and quit REAPER, stop the predecessor app only through its graceful exit, never force-kill (P4, I8); "ide event" pre-empts everything.
- **P6:** the public repo gets only synthetic projects, anonymised EQ values and measured laws.
- **Tier 0:** the generator is built and run in hosted CI.
- **P5:** only a bundle from a `push` run on `dev` whose SHA equals the reviewed commit reaches the PC; the PC re-checks every hash; plug-ins are allowlisted in CI and on the PC.

## 3. Architecture

1. **Generator** — crate `iem-rpp` (S4 adds the importer and exporter): RPP writer, ReaEQ chunk encoder (byte-exact against a REAPER-written chunk), JSFX/FX-chain blocks, 64-bit float WAV stimuli and the case catalogue. `iem-rpp-gen` writes a **bundle** (`projects/*.rpp`, `stimuli/*.wav`, `bundle.json` with hashes and case metadata); CI job `golden-bundle` builds it on push runs only.
2. **Window procedure.**
   - `scripts/golden/golden_window.py` (dev box) runs one subcommand per step and keeps a state file, so a pre-emption knows what to undo.
   - `scripts/golden/GoldenPc.psm1` (PC) does manifests, backup, verify/restore, staging, the allowlist and the render queue.
   - One Interactive task of ours (`iemmixer-golden`) runs `golden-task.ps1` in the console session, so REAPER's dialogs stay visible and dismissible. Session 0 over ssh cannot show them.
   - GUI-only steps use the PC's remote-desktop MCP server: the predecessor's tray Exit, dialogs, and the audio-device calibration.
3. **Analysis.**
   - `scripts/golden/analyze.py` (Python + numpy) turns the fetched renders into `goldens/s1b/laws.json`, binary vectors and `goldens/s1b/README.md`.
   - Its tests run in CI on synthetic renders made from known filters.

## 4. The render window

1. **Precondition:** the owner's "event skončil" arrived after the last "ide event". The agent never infers it.
2. **Preflight (read-only):** REAPER 7.65, no render instance, only REAPER holds the ASIO module (`tasklist /m`), ≥ 10 GB free.
3. **Interlock:** input meters are read over REAPER's HTTP surface for 60 s. Any peak above −50 dBFS aborts and alarms (§4.2).
4. **Save and quit REAPER:** save (action 40026, project file changed; the D2 switch step, before any backup), quit (40004), and within 30 s REAPER is gone and nothing holds the ASIO module.
5. **Stop the predecessor app** via its tray Exit (MCP), its only graceful path.
6. **Backup** the trees in the private trees file (REAPER resource folder, project folder, the app's data; about 0.7 GB) plus three registry keys and the start task's XML. The sha256 manifests of sources and copies must be equal.
7. **Stage:** upload the bundle; verify hashes and the allowlist; substitute the `@@JOB@@`/`@@OUT@@` tokens; seed a fresh render resource folder (minimal `reaper.ini`, two JSFX and the licence files, all copied from the PC's REAPER and never leaving the PC).
8. **Render:**
   - The task runs `reaper.exe -newinst -nosplash -ignoreerrors -cfgfile <ini> -renderproject <copy>` once per project, with a bounded wait and never a kill.
   - The dev box polls the ASIO module holders every 0.5 s. Any render process there stops the queue and alarms.
9. **Fetch** the renders to the dev box (outside the repo) and check their hashes.
10. **Verify and restore:**
    - Re-hash every tree against the backup manifest.
    - Copy changed or missing files back; move extra files to quarantine, never delete them. Compare the registry exports.
    - The window succeeds only on an identical re-hash. Any difference is a finding on #4, and no further window runs until it is explained.
11. **Bring back:** REAPER through the predecessor's own start task, then the app's executable; the runbook's handover checks (with no band playing: `UNCONFIRMED-AUDIO`, reported as such).

**End state:** the pre-window state, so a late "ide event" finds REAPER and the app running. **"ide event" mid-window** runs `preempt`: stop file, wait for the current render, verify and restore, bring back, confirm to the owner.

**A hung render instance** (modal dialog) is dismissed via MCP or closed with `CloseMainWindow()` (graceful). The main REAPER never starts while any `reaper.exe` exists, since its start task would hand the project to that instance. Last resort: the owner's reboot, which comes back in event mode (G1).

## 5. Keeping REAPER off the card

Verified CLI facts (REAPER `whatsnew.txt`; ReaTeam's transcript of `reaper -h`):

- `-renderproject` renders and quits, hiding the main window and splash.
- `-cfgfile` makes the ini's folder the resource path on Windows (5.23) and needs a full path.
- `-newinst`, `-nosplash` and `-ignoreerrors` do what they say.
- `-peaktest` exists since 7.62, `-close…:exit` since 7.29, but `-fxoffline` only from 7.74.
- Command-line renders ignore "bring FX online" (7.55).

**The safety argument needs no unverified value:**

- The live `REAPER.ini` selects ASIO as `mode=3`, and only ASIO can open the card, which has no WDM, KS or DirectSound endpoint.
- The render ini has no ASIO keys and a `mode` other than 3. The stager and the tests refuse `mode=3`.
- `tasklist /m` proves at every render that no render process loaded the driver.
- The "Dummy Audio" `mode` value is confirmed once, in window 1, by starting the render instance with `-audiocfg` and reading the dialog through MCP.

## 6. Case catalogue

| Family | kHz | Cases | Yields |
|---|---|---|---|
| `cal` | 96 | Identity, mono item, trim +6 dB, peak EQ; 64-bit and 32-bit float configs | Format, bit-exact passthrough, FX loaded |
| `pan` | 96 | Send pan at 41 points plus site-like values (dual-mono and stereo sources); track pan; track pan × mode-0 send pan; send volume 0.000803…4.0 | Taper |
| `mute` | 96 | Track mute vs mode 3 and mode 0; send mute; bus mute before a mode-0 tap | A3, A9 |
| `sum` | 96 | Inputs → stems bus → output bus → elevated bus | A6, A7, A9 |
| `downmix` | 96 | Stereo and dual-mono sources into a mono destination, with pan and volume | Downmix law |
| `mono` | 96 | Mono media on a stereo track, pan 0 and 0.5 | Mono level (media) |
| `bypass` | 96 | `FX 0` chain; single-FX `BYPASS` | Identity |
| `eq` | 44.1/48/96 | LS/HS/Band × 5 f × 4 gains × 6 BW; HPF gain 0.5/1/+9.15 dB; edge cases | Shelf α(BW), HPF gain, h[0..256] |
| `site-eq` | 96 | 44 anonymised EQs, impulse and log sweep | h[0..2048], linearity |
| `lim` | 96/48/44.1 | MGA limiter at −6/−3/0 dB on hot material | Output vectors |

## 7. What offline renders cannot prove

Residuals for Method B/C (S7/S8), never claimed: live input duplication; the hardware-output mono downmix (the send-to-mono law stands in); FX on muted tracks; live delay compensation; REAPER's ramps.

## 8. Decisions and rejected alternatives

- **Synthetic projects, not a site-project copy:** they cover every send, volume, pan and FX field the site uses and stay public. Importer fidelity is S4/S8 work.
- **Each window restores the pre-window state**, which is safer against a late signal (R4).
- **The licence file is copied into the render folder on the same PC**, so no nag dialog blocks the render. It never leaves the PC.
- **The app starts from its executable**, because its launcher force-kills (I8).
- **Rejected:**
  - rendering next to the live REAPER (competes with live audio; its ini keeps changing);
  - leaving the app running (its logs change);
  - a VM or ReaPlugs (D7 fallbacks, not needed).

## 9. Risks and open items

- **UNVERIFIED encodings:** `RENDER_CFG` for 64-bit float and `RENDER_STEMS 2`. The `cal` family settles both first.
- **Action 40004 = Quit** is M-confidence. If it fails, the window aborts before the backup.
- **No MCP server for the PC desktop is configured here yet.** Task 9 registers it; no window starts without it.
- **The predecessor's start task has a 72 h execution limit.** That is out of scope; the observation goes on #4.

## 10. Results

- Laws, formulas and vectors: `goldens/s1b/README.md` and `laws.json` (window `20260926T091333Z`, bundle `82716ea`); per-law lines and both restore records on #4.
- **Changes a program-spec assumption:** A5 — the pan law has the exact sine-taper direction with a tabulated magnitude, not the +0 dB balance law; A10 — a mono destination gets half the panned sum on channel 1. Recorded for S2 on #5.
- **Fills A12:** peak and HPF are RBJ with the octave warp capped at π/2, shelves use S = min(1/bw², 1.2), the HPF band gain is ignored.
- **Procedure findings:** `RENDER_STEMS 2` renders nothing in REAPER 7.65 (the generator uses 1); a fresh render resource folder scans every default VST3 folder (the scan cache is now copied); the Dummy Audio `mode` value stays unverified, and no render instance ever loaded the ASIO module.
