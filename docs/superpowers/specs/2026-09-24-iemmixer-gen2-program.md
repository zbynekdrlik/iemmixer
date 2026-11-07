# iemmixer principles

The program this file once planned, Gen 2 on a Windows PC beside REAPER, ended on 2026-11-01 (#86). Its plan is gone from the repository; what stands are the principles and invariants below, which the code and the rules cite by id. Their numbers keep their old gaps. The F, X, A, D and Q ids still seen in comments named the old plan's features and assumptions: labels only, nothing defines them any more. The current design is `2026-10-24-gen3-linux-dante-design.md`.

**MVP means limited scope, never lower quality.** Live-audio safety, hearing safety and public-repo security are non-negotiable.

## Principles

- **P6** Site data never enters the public repo: names, hosts, Dante channel numbers and real track names, PINs, keys. EQ and limiter values are not site data.
- **P7** The box is self-contained at runtime: no dependency on any other machine we run.
- **P8** Framework first; reuse predecessor app code that does not depend on REAPER.

## Invariants

- **I1** The engine opens no socket, links no codec, parses no browser data.
- **I2** iemmixer runs at a 32-sample buffer at 96 kHz (≈ 0.33 ms): mandatory (owner, 2026-09-26: in-ear system). The box is an appliance bound to iemmixer.
- **I4** The topology is compiled once per run from `site.toml` and validated (unique channels in the map, disjoint groups, every heard mix declared before the mix that hears it); topology change = controlled engine restart.
- **I5** f64, plain summing, no bus pan law, zero added latency, no delay-compensation emulation.
- **I6** The engine is the single writer; clients get revisioned `State`/`Delta`, and any gap forces a resync.
- **I7** RT contract: lock-free rings, atomics and fixed messages only; ≤ 512 commands per block; no allocation, lock, syscall or log; CI checks with `assert_no_alloc` and rtsan.
- **I8** Nothing is force-killed: a stop is a request and a bounded wait, never a kill.
- **I9** A server restart, crash or deploy never touches audio.
- **I10** Edits lost: none on a graceful restart, ≤ 5 s on a crash.
