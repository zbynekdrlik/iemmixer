# S2 — DSP and limiter: design note

**Ticket:** #5 (program #1). **Spec:** `2026-09-24-iemmixer-gen2-program.md` §2.2, §2.5 (I5, I7), §3.3 (A5, A10, A12, A13), §3.4 (X1, X14, X15), §3.5, D1. **Plan:** `docs/superpowers/plans/2026-09-26-s2-dsp.md`. **Inputs:** the S1b goldens (`goldens/s1b`, window `20260926T091333Z`) and the hand-offs on #5.

## 1. Goal

Two libraries whose output equals REAPER's on the S1b goldens, ready for the S3 engine:

- `iem-dsp` (MIT OR Apache-2.0, no dependencies, WASM-safe): EQ, pan law and mono downmix, smoothers, peak meter, sanitiser, and the EQ response function the UI will share (F11).
- `iem-limiter-mga` (GPL-3.0-or-later, D1(a)): a faithful port of the MGA JS Limiter, with its JSFX source kept as a test fixture.

Done means: the §3.5 tolerances hold in hosted CI, the process paths are proven allocation-free, block-size invariance holds, and the mutation gate has room for the diff.

## 2. What S1b measured (inputs, not hypotheses)

- **EQ (A12):** ReaEQ's band and HPF are RBJ biquads with `α = sin w0 · sinh(ln2/2 · bw · k)`, `k = w0/sin w0` for `w0 ≤ π/2`, else `π/2`. Shelves are RBJ with `A = sqrt(gain)` and slope `S = min(1/bw², 1.2)`. The HPF band's gain is ignored.
- **EQ edges** (`eq_edge`, recovered from the vectors in S2): the frequency is capped at `0.49·fs` (the 24 kHz case renders identically at 44.1 and 48 kHz, `w0 = 0.98π`); the bandwidth has a floor of 0.01 oct (bw 0 gives exactly 0.01); a band at gain 0 (−∞ dB) is a **notch** with the band's bandwidth (`b = [1, −2c, 1]/(1+α)`); a disabled band is the identity; the global gain multiplies the output.
- **Pan (A5, replaces the spec's +0 dB balance):** the direction is exactly the sine taper, `atan2(gR, gL) = (p+1)·π/4`; the magnitude `sqrt(gL² + gR²)` falls from √2 (p = 0) to 1 (p = ±1) and is tabulated at 41 points. One law for send pan (mode 0 and 3), track pan, stereo and dual-mono sources and mono media.
- **Mono downmix (A10):** a mono destination gets `vol·(gL·L + gR·R)/2` on channel 1 and nothing on channel 2.
- **Limiter (A13):** the S1b vectors are REAPER's output of `loser/MGA_JSLimiterST` at threshold = ceiling = −6/−3/0 dB, release 50 ms, link 75 %, on `iem_rpp::stimulus::hot_material(rate, 27)`.

**Finding (recorded on #5):** the committed `goldens/s1b/*.f64` were almost all zeros, because `analyze.py` reopened each vector file with `"wb"` on every append. `laws.json` was unaffected. S2 fixes `analyze.py` (RED/GREEN) and regenerates the vectors from the raw renders of the same window; `laws.json`, `README.md` and `index.json` come out byte-identical.

## 3. Decisions

### 3.1 EQ: TPT state-variable filter fed RBJ parameters

- Each band is a Simper linear-trapezoidal SVF (`g = tan(π f/fs)`, `k = 1/Q`, mixing `m0, m1, m2`), fed the same `f0`, `Q` and `A` as the RBJ design. Both are bilinear transforms of the same analogue prototype, so the transfer functions are identical; only round-off differs.
- **Why not biquads with smoothed coefficients:** a direct-form state is tied to its coefficients (jumps click), and interpolated biquad coefficients can pass through unstable sets. The SVF state is the integrator state, so per-sample coefficient changes are glitch-free.
- **Measured before deciding** (Python prototype on the regenerated goldens): SVF vs REAPER ≤ 2.9e-13 absolute on all 1260 matrix cases at 44.1/48/96 kHz, ≤ 1.3e-14 on the edge cases once the three edge rules above are applied, ≤ 9.2e-15 on the 15 site EQs (2048 taps). The spec asks for 1e-9.
- **Designs** (`Q⁻¹` from the measured bandwidth laws):
  - band: `k = Q⁻¹/A`, `m = (1, k(A²−1), 0)`; gain 0: notch `k = Q⁻¹`, `m = (1, −k, 0)`;
  - high-pass: `k = Q⁻¹`, `m = (1, −k, −1)` (gain ignored);
  - low shelf: `g /= √A`, `m = (1, k(A−1), A²−1)`; high shelf: `g *= √A`, `m = (A², k(1−A)A, 1−A²)`;
  - clamps: `20 Hz ≤ f ≤ min(24 kHz, 0.49·fs)`, `0.01 ≤ bw ≤ 4`, `gain ≤ 4` (+12.04 dB, ReaEQ's top). A shelf at gain 0 was not measured; its gain has a floor of 1e-6 (−120 dB).
- **Smoothing (X15, 20 ms):** per band, `log2 f`, gain in dB (floor −120 dB while moving) and `log2 bw` ramp linearly; coefficients are recomputed per sample only while a band moves, and the last step designs from the exact target, so steady state is bit-for-bit the target design. Enabling or disabling a band crossfades it over 20 ms; a fully bypassed band is skipped and its state zeroed. The global gain ramps over 20 ms. A band whose kind changes (never in the product) restarts from zero state.
- **Existing crates (read before deciding):** `biquad` 0.6 designs RBJ biquads from a Q (`alpha = sin ω/(2Q)`) in direct form only; `fundsp` 0.23 `svf.rs` has exactly these Simper forms (`bell`, `lowshelf`, `highshelf`, `highpass` from cutoff, Q and gain) and serves as a cross-check of ours. Neither carries ReaEQ's laws (warp cap, shelf slope, clamps, the gain-0 notch) or our per-sample ramps, and `fundsp` would bring a large dependency tree into the engine (§5.2 allowlist). The coefficient code is ~40 lines, so `iem-dsp` stays dependency-free.
- **Stereo:** `Equalizer<CH>` shares coefficients across channels (`CH = 1` for mono inputs, 2 for buses).
- **Response function (F11):** `response_db` evaluates the same designs analytically (bilinear `s = j·tan(ω/2)/g`), so the UI shows exactly the engine's curve. It matches a 65 536-tap impulse response of the filter to ≤ 1.2e-12 dB over 20 Hz–20 kHz (prototype).

### 3.2 Pan law

- `gains(p) = m(|p|)·(sin((1−p)·π/4), sin((1+p)·π/4))`: the sine form makes p = ±1 exactly one-sided and `gains(−p)` exactly the mirror of `gains(p)`.
- `m` is Catmull-Rom over the 21 measured magnitudes (even extension at 0, linear ghost point past 1). It is exact on the 0.05 grid (≤ 2.3e-16 against the measured gains). The two off-grid measurements are within 2.7e-7 (p = 0.04) and 7.8e-6 (p = 0.86), i.e. ≤ 6e-5 dB.
- `send_gains(v, mute, p) = v·(1−m)·gains(p)`; `mono_downmix(L, R, gL, gR) = (gL·L + gR·R)/2`.
- A closed form was looked for and not found; the table is the law.

### 3.3 Smoothers (X15)

One linear `Ramp` (fixed sample count per change, value snaps to the target on the last step) for everything: gains and pan 10 ms, mute 5 ms, EQ 20 ms; presets (50 ms) and the engine fade-in (500 ms) reuse it in S3. Stepping is per sample, so output depends only on sample indices, never on block boundaries. `StereoGain` ramps `v·gL`, `v·gR` over 10 ms and the mute factor over 5 ms.

### 3.4 Limiter port (A13, X14)

- `Mga` is the faithful core with all four JSFX sliders (threshold, release, link, ceiling): hold `sr/128`, two staggered peak-hold windows, the asymmetric 75 % link, instant attack, `r = exp(−3/(sr·max(release, 0.05)))`, zero lookahead, and the GR meter with its `exp(1/sr)` recovery. The only addition is a per-sample flush of `env < 1e-30` to 0 (no effect on gain: `env ≤ thresh` there).
- `Limiter` is the product policy: threshold = ceiling = limit, clamped to −6…0 dB, release 50 ms, link 75 %; enabling applies at once (safety), disabling crossfades over 10 ms; the detector always runs.
- **X14:** a sample counts as active while the JSFX GR meter (the value REAPER showed as "GR", with its 8.7 dB/s recovery) is below −1 dB, and only while the limiter is enabled; GR reads 0 dB while disabled.
- **Proof:** a line-by-line EEL2 → Rust translation (test code, beside the `@sample` text of the fixture it translates) matches the port ≤ 1e-12 on random sliders and material, and the port matches REAPER's vectors ≤ 1e-12 (prototype: ≤ 3.3e-16, i.e. below −300 dBFS; the spec asks < −100 dBFS).
- The JSFX file is copied verbatim into `crates/iem-limiter-mga/fixtures/` with its licence header; the crate carries the GPL-3.0 text.

### 3.5 Meters, sanitiser

- `PeakMeter<CH>`: sample peak per channel since the last read (hold and decay stay in the UI, F9).
- `sanitize`: a block with a non-finite sample or one above 1e6 is zeroed and reported (X1); the caller counts, resets the node (`Equalizer::reset`, `Limiter::reset`) and alarms (S3).

## 4. Real-time safety (I7)

- No allocation, lock, syscall or log on any process path: fixed-size state (`[_; 5]` bands, const-generic channels), slices in and out.
- The process paths run under `assert_no_alloc` (warn mode, violation count asserted zero; a self-test proves the detector sees an allocation).
- Crate lints in library code: `forbid(unsafe_code)`, `deny(clippy::indexing_slicing, unwrap_used, expect_used, panic)`.
- **rtsan** needs a nightly toolchain and an RT thread to observe; it moves to S3 with the engine (hand-off on #6).
- **Fuzzing** (S0 hand-off): a stable-toolchain randomised harness (`tests/props.rs` in both crates, seeded, with finite/non-finite/huge inputs and random parameters and block splits) runs briefly in every test run and at length in a per-run `fuzz` CI job. Coverage-guided fuzzing of the engine's command parser belongs to S3.

## 5. Golden harness

- `iem_rpp::golden` reads `goldens/s1b` (`index.json` offsets in f64 values, `.f64` float64 LE, `laws.json`).
- `iem-dsp/tests/golden.rs`: every EQ vector (matrix, edges, site EQs) ≤ 1e-9 and the frequency check of §3.5; the pan table and the measured pan-dependent taps (`send_pan`, `track_pan`, `post_fader`, `mono_media`) and the downmix law.
- `iem-limiter-mga/tests/golden.rs`: the five limiter vectors ≤ 1e-12, ceiling respected, activity counts.
- A CI step prints the maximum error per category.

## 6. Not in S2

- The mix graph, send summing, taps, commands and the RT thread (S3); rtsan (S3); the UI switching to `response_db` (S5).
- The Q1 safety stage before the clamp, the listen-path limiter (X3) and the 96→48 kHz listen resampler (X4): graph nodes of S3, built from these kernels (a `Limiter` at 0 dB is the natural safety stage).
- Linear-mix goldens beyond the pan law and downmix (the S3 oracle).

## 7. Risks

- **Pan law off the grid:** interpolation error ≤ 6e-5 dB, measured at two points only. A later site value between grid points is within that error.
- **Unmeasured edges:** shelf at gain 0 (floored), gain between 0 and −120 dB at the notch boundary (a ramp towards gain 0 ends in a jump to the notch). Neither is reachable from the UI (±12 dB); only an import can carry gain 0.
- **Mutation budget:** DSP arithmetic yields many mutants; the shard matrix is resized from the `mutants-list` count.
