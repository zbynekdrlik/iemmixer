---
paths:
  - "crates/iem-audio-io/src/asio.rs"
  - "crates/iem-audio-io/src/format.rs"
  - "crates/iem-audio-io/src/telemetry.rs"
  - "crates/iem-audio-io/examples/asio_spike.rs"
  - "scripts/asio-spike/**"
---

# ASIO host and the S1a spike (#3)

- `asio.rs` is the crate's only unsafe code (`deny(unsafe_code)` at the root, `allow` on the module). Every driver call stays on the thread that created `Host` (`!Send`, COM STA; that thread pumps messages). A stream is freed only after `STREAM` is cleared and `IN_FLIGHT` is 0.
- The host never calls `set_sample_rate`, `set_clock_source` or `open_control_panel` (integrity scan, I2) and streams only at 96 kHz and the driver's preferred buffer (`format::admit`). Outputs are zeroed before `start()` and on every callback (A1).
- Decisions live in `format.rs` and `telemetry.rs` (portable, tested, mutated); `asio.rs` and the example are excluded from mutation and build only on the Windows jobs (`windows`, `asio-spike`). Avoid `>`/`>=` on values where both branches agree (equivalent mutants).
- The driver's preferred buffer changes only through `spike_window.py set-buffer` and is restored with read-back before REAPER starts (whenever a write was recorded, even to the original value); `Invoke-SpikeBringBack` reads it again and refuses unless it is the original, or while `asio_spike` or its task runs. REAPER keeps its own value (owner, #3).
- A spike may be starting when "ide event" comes: unwind always writes the stop file while the card is free, `Stop-SpikeGracefully` waits for the spike and its task, `Invoke-SpikeRun` and the spike itself refuse on an existing stop file.
- PC windows only in dev time. `~/.config/iemmixer/EVENT-NOW` = an event is on (created on "ide event", removed on "event skončil"); every wait of `spike_window.py` sees it within 2 s and pre-empts. Nothing is killed; a spike that does not stop is an owner alarm.
- Only the `asio-spike-<sha>` artifact of a green `dev` push reaches the PC (`fetch-bundle`; `SHA256SUMS` checked on the dev box and on the PC).
- `SpikePc.psm1` imports `GoldenPc.psm1` from its own folder (the bundle layout); run `Test-SpikePc.ps1` on Windows PowerShell 5.1 (CI `asio-spike`) after any PowerShell change.
- Site values (driver name, registry key, task names, handover values) only in `~/.config/iemmixer/asio-spike.env` and the ops runbook `docs/s1a-pc-runbook.md`.
