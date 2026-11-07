//! Test helpers for the DSP baseline: the reader of the committed goldens
//! (`goldens/s1b`) and the deterministic stimulus the limiter's goldens were
//! rendered from. A dev-dependency of `iem-dsp` and `iem-limiter-mga` only;
//! nothing the box runs links it.

pub mod golden;
pub mod stimulus;
