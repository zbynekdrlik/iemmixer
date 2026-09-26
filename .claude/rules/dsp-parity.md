---
paths:
  - "crates/iem-dsp/**"
  - "crates/iem-limiter-mga/**"
  - "crates/iem-rpp/src/golden.rs"
---

# DSP and limiter parity (S2)

- Parity is proven against `goldens/s1b` only (S2 design note §2): EQ ≤ 1e-9 and ≤ 0.01 dB, limiter ≤ 1e-12, block sizes ≤ 1e-12. A miss is a design bug — never loosen a tolerance, never special-case a golden.
- Golden vectors are regenerated with `scripts/golden/analyze.py` from the raw renders in `~/.local/share/iemmixer/golden-raw/<window>`, never edited by hand; `committed_s1b_vectors_are_complete_and_non_blank` guards against the S1b zero-vector bug.
- ReaEQ laws live in `eq::design` (warp cap at π/2, shelf `S = min(1/bw², 1.2)`, HPF gain ignored, `f0 ≤ 0.49·fs`, `bw ≥ 0.01`, gain 0 = notch). Prototype any change in Python against the goldens first (Tier 0: CI is the only compiler).
- Steady state must be bit-exact: ramps land on their target and the last step designs from the exact target. New smoothing steps per sample (never per block), or `eq_output_does_not_depend_on_the_block_size` breaks. A moving band is redesigned every `DESIGN_EVERY` (16) samples of its own ramp counter (S3: per-sample redesign of 220 bands cost twice the 32-sample period).
- `iem-limiter-mga` is GPL-3.0-or-later (D1): only the engine links it; `iem-dsp` never depends on it. The fixture `fixtures/MGA_JSLimiterST` stays byte-exact (CRLF, `-text` in `.gitattributes`); `tests/golden.rs` pins the literal translation to it.
- RT: library code denies indexing, `unwrap`, `expect`, `panic` (crate attributes); every process path is exercised in `tests/rt.rs` under `assert_no_alloc` (its own test binary, because it installs the global allocator).
- Clippy traps: `approx_constant` (write `SQRT_2`, not its digits), `excessive_precision` (paste Python `repr` values), `should_implement_trait` (no method named `next`).
