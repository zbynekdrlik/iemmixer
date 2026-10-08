//! The listen probe (S7 design note §6; #10). While a HIL signal with
//! `listen` runs, each listened slot's probe tap carries the spare outputs'
//! own samples (`render_hil`'s expression: the sine clamped to `TEST_CAP`,
//! times the output fade), stereo with both channels equal, at the slot's
//! cadence: one stereo frame a sample, beside the slot's listen tap, which
//! stays silent. The media thread frames the probe taps as their own streams
//! (`media::stream::ENGINEER_PROBE`, `MEMBER_PROBE`), so the listen taps keep
//! their bytes. RT (I7): an index loop over the processor's scratch and one
//! `push_partial_slice` into a ring `Processor::with_hil` allocated.

use std::sync::atomic::{AtomicU64, Ordering};

use rtrb::Producer;

use crate::TEST_CAP;

/// Pushes one segment of a probe tap: `sine` (the test signal's samples of
/// the segment) clamped to `TEST_CAP`, times `fade` (the output fade), as
/// interleaved stereo with both channels equal, through `scratch` (two
/// values a sample). A full ring takes what fits and counts one overrun in
/// `overruns` (the listen taps' counter).
pub fn push_probe(
    p: &mut Producer<f32>,
    sine: &[f64],
    fade: &[f64],
    scratch: &mut [f32],
    overruns: &AtomicU64,
) {
    let mut used = 0;
    for (pair, (s, f)) in scratch
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .zip(sine.iter().zip(fade))
    {
        let y = (s.clamp(-TEST_CAP, TEST_CAP) * f) as f32;
        *pair = [y, y];
        used += 2;
    }
    let (_, rest) = p.push_partial_slice(scratch.get(..used).unwrap_or_default());
    if !rest.is_empty() {
        overruns.fetch_add(1, Ordering::Relaxed);
    }
}
