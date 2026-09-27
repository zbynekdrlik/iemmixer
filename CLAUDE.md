# iemmixer

Gen 2 of the band's in-ear-monitor mixer: a native Rust audio engine replacing REAPER. **Public repository** — site data never enters it (program spec P6).

- Program spec: `docs/superpowers/specs/2026-09-24-iemmixer-gen2-program.md` (approved 2026-09-24). Each sub-project gets a design note and a plan in `docs/superpowers/` before code.
- Tickets: this repository's issues (#1 program, one ticket per sub-project). Predecessor issues are written `reaperiem#N`; `#N` always means this repository.

## Playbook router

- Public-repo hygiene, denylist, scrubbing, imports from the predecessor → `.claude/rules/public-repo-hygiene.md`
- Login protection, PIN hashing, pepper, secrets, provisioning → `.claude/rules/security-baseline.md`
- Playwright E2E (console guard, env PINs, login budgets) → `.claude/rules/e2e.md`
- CI Rust toolchain, `--locked`, mutation gate → `.claude/rules/ci-rust-toolchain.md`
- Leptos `view!` gotchas and disposal safety → `.claude/rules/leptos-view-macro.md`
- Engine core, RT contract at B = 32, site `[engine]` table, protocol, persistence, pipes, rtsan/bench → `.claude/rules/engine.md`
- Server ↔ engine: ids, pan/dB domains, echoes, solo, talk lock → `.claude/rules/server-engine.md`
- Cloudflare tunnel watchdog, LAN URL / public host → `.claude/rules/tunnel-watchdog.md`
- ASIO host, the S1a spike, the PC window driver → `.claude/rules/asio-spike.md`
- Importer, exporter, band data migration (`iem-migrate`, iem-rpp import/export) → `.claude/rules/migration.md`
- DSP kernels, EQ/pan/limiter parity, golden tolerances → `.claude/rules/dsp-parity.md`
- Gen 1 test parity manifest (`docs/parity/gen1-tests.tsv`, its CI check, cutover gate) → `.claude/rules/parity.md`
- Golden renders on the IEM PC (generator, window driver, analysis) → `.claude/rules/golden-renders.md`
- Guard, iemmode, PC install → `.claude/rules/guard.md`

## Always-apply rules

**Owner event signals (D2).** "ide event" (an event is coming) → immediately stop everything iemmixer on the IEM PC, start REAPER and the predecessor app, verify the handover, and confirm back to the owner. "event skončil" (the event ended) → save and quit REAPER, stop the predecessor app gracefully, start iemmixer, continue development. Never switch on your own and never ask whether an event is running. A reboot always comes back in event mode. Nothing is ever force-killed.
**The switch: one rule, two phases.** Once the guard is installed on the PC (S6 plan Task 16 Step 3, recorded on #9): "ide event" → `python3 scripts/iem-pc/iempc.py event` (it writes `~/.config/iemmixer/EVENT-NOW` first, pre-empts an open S1a/S1c window, runs `iemmode event`, and `iemmode event --direct` when the guard is unreachable); "event skončil" → remove the flag, then `iempc.py dev`. **Before that** the interim switch applies: `scripts/asio-spike/spike_window.py` with the private `~/.config/iemmixer/asio-spike.env`: "ide event" → create `~/.config/iemmixer/EVENT-NOW`, then `preempt` if a window is open, then the event runbook's checks (`~/.config/iemmixer/event-runbook.md`); "event skončil" → remove the flag; `to-dev` / `to-event` switch only inside a window of a running task.

**Predecessor boundary.** Never push to `zbynekdrlik/reaperiem`, never change its code, config or deployment; read it only through `~/devel/reaperiem` at a pinned SHA.

**Site data (P6).** Never commit names, hosts, IPs, Dante channel numbers, real track names, PINs, keys or tokens. Real site values live only in the private `zbynekdrlik/iemmixer-ops` (credential strings not even there: only in `~/.config/iemmixer/` and the CI secret). Every clone installs the pre-push hook (gitleaks + private denylist + allowed commit identities); CI repeats all three.

**Dante.** Never change Dante subscriptions or any Dante device other than the IEM PC's own card.

**Builds.** Tier 0: no local cargo compilation — push `dev` and verify in CI. Local `cargo fmt`, `cargo metadata`, `cargo tree`, `cargo update -p` are fine; `cargo deny` and `cargo mutants` run in CI only.

**Versions.** One version for the workspace: `[workspace.package].version` in `Cargo.toml` (`2.0.0-dev.N` until cutover). `scripts/check_version.py` enforces consistency, and dev > main on PRs.
