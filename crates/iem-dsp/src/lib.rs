//! iemmixer DSP kernels (S2; program spec §3.3–3.5, design note
//! `docs/superpowers/specs/2026-09-26-s2-dsp-design.md`):
//!
//! - [`eq`]: the ReaEQ-parity EQ, a TPT state-variable filter fed RBJ
//!   parameters (A12), and the response function the UI shares (F11);
//! - [`pan`]: the measured pan law, send gains and the mono downmix (A5, A10);
//! - [`ramp`]: linear smoothers (X15);
//! - [`meter`]: peak meters and dB/seconds helpers (F9, X14);
//! - [`sanitize`]: the node sanitiser (X1).
//!
//! f64 throughout (I5). Process paths allocate nothing, lock nothing and cannot
//! panic (I7): fixed-size state, slices in and out, no indexing.

#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic
    )
)]

pub mod meter;
pub mod pan;
pub mod ramp;
pub mod sanitize;
