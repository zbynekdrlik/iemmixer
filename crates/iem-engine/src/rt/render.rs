//! The processor's pipeline, one segment at a time (I7: nothing here
//! allocates, locks, logs or makes a syscall): the inputs, every mix in
//! declaration order, HIL's spare outputs and the D5(b) loopback probe,
//! with the per-sample gain helpers they share. `Processor::render` in
//! rt.rs calls them in that order.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use iem_audio_io::Block;
use iem_dsp::pan::StereoGain;
use iem_dsp::ramp::Ramp;
use rtrb::Producer;

use super::{MixRt, Processor};
use crate::TEST_CAP;
use crate::latency;
use crate::probe;

fn scale(x: &mut [f64], g: f64) {
    if g == 0.0 {
        x.fill(0.0);
    } else if g != 1.0 {
        for v in x {
            *v *= g;
        }
    }
}

/// Applies a mono gain ramp to both channels.
fn ramp_gain(ramp: &mut Ramp, l: &mut [f64], r: &mut [f64]) {
    if !ramp.is_moving() {
        let g = ramp.value();
        scale(l, g);
        scale(r, g);
        return;
    }
    for (a, b) in l.iter_mut().zip(r.iter_mut()) {
        let g = ramp.tick();
        *a *= g;
        *b *= g;
    }
}

/// Applies a fader/send gain (A5) in place.
fn stereo_gain(g: &mut StereoGain, l: &mut [f64], r: &mut [f64]) {
    if let Some((gl, gr)) = g.steady() {
        scale(l, gl);
        scale(r, gr);
        return;
    }
    for (a, b) in l.iter_mut().zip(r.iter_mut()) {
        let (gl, gr) = g.tick();
        *a *= gl;
        *b *= gr;
    }
}

/// Adds `g · src` to `dst` (a level).
fn accumulate(g: &mut StereoGain, src: (&[f64], &[f64]), dst: (&mut [f64], &mut [f64])) {
    let ((sl, sr), (dl, dr)) = (src, dst);
    if let Some((gl, gr)) = g.steady() {
        if gl != 0.0 {
            for (d, s) in dl.iter_mut().zip(sl) {
                *d += gl * s;
            }
        }
        if gr != 0.0 {
            for (d, s) in dr.iter_mut().zip(sr) {
                *d += gr * s;
            }
        }
        return;
    }
    for ((a, b), (x, y)) in dl.iter_mut().zip(dr.iter_mut()).zip(sl.iter().zip(sr)) {
        let (gl, gr) = g.tick();
        *a += gl * x;
        *b += gr * y;
    }
}

/// Copies `src` into `dst` element by element (equal lengths by construction).
fn copy(dst: &mut [f64], src: &[f64]) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d = *s;
    }
}

/// Pushes a segment of a listen tap (X3) as interleaved stereo, or as many
/// silent frames when `silent` (a HIL signal runs: the tap keeps its
/// cadence, and the test sine never reaches a web listener; #9,
/// 2026-09-28).
fn push_tap(
    p: &mut Producer<f32>,
    (l, r): (&[f64], &[f64]),
    silent: bool,
    scratch: &mut [f32],
    overruns: &AtomicU64,
) {
    let mut used = 0;
    for (pair, (a, b)) in scratch
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .zip(l.iter().zip(r))
    {
        *pair = if silent {
            [0.0, 0.0]
        } else {
            [*a as f32, *b as f32]
        };
        used += 2;
    }
    let (_, rest) = p.push_partial_slice(scratch.get(..used).unwrap_or_default());
    if !rest.is_empty() {
        overruns.fetch_add(1, Ordering::Relaxed);
    }
}

impl Processor {
    pub(super) fn render_inputs(&mut self, block: &Block<'_>, off: usize, n: usize) {
        let topo = Arc::clone(&self.topo);
        if self.talkback_input.is_some() {
            self.read_talkback(n);
        }
        for (i, spec) in topo.inputs.iter().enumerate() {
            let mut tripped = false;
            let Some(node) = self.inputs.get_mut(i) else {
                continue;
            };
            let (l, r) = node.p.get_mut(n);
            for (dst, ch) in [(&mut *l, spec.rx[0]), (&mut *r, spec.rx[1])] {
                match block.input(ch).get(off..off + n) {
                    Some(src) => copy(dst, src),
                    None => dst.fill(0.0),
                }
            }
            // X1 on the card input.
            if node.trips.check([&mut *l, &mut *r]) {
                node.eq.reset();
                tripped = true;
            }
            if self.test.as_ref().is_some_and(|t| t.input == i) {
                let sine = self.test_buf.get(..n).unwrap_or_default();
                copy(l, sine);
                copy(r, sine);
            }
            let mix = &mut node.proc_mix;
            let dry = !mix.is_moving() && mix.value() == 0.0;
            if !dry {
                let fading = mix.is_moving();
                let (dl, dr) = self.dry.get_mut(n);
                if fading {
                    copy(dl, l);
                    copy(dr, r);
                }
                ramp_gain(&mut node.trim, l, r);
                if !node.eq.is_identity() {
                    node.eq.process([&mut *l, &mut *r]);
                }
                if fading {
                    for ((a, b), (x, y)) in
                        l.iter_mut().zip(r.iter_mut()).zip(dl.iter().zip(dr.iter()))
                    {
                        // At the ends the result is exactly wet or dry, as in a
                        // segment that starts after the fade (block-size invariance).
                        let m = mix.tick();
                        if m != 1.0 {
                            *a = x + m * (*a - x);
                            *b = y + m * (*b - y);
                        }
                    }
                }
            }
            if Some(i) == self.talkback_input {
                let tb = self.talk_buf.get(..n).unwrap_or_default();
                for ((a, b), t) in l.iter_mut().zip(r.iter_mut()).zip(tb) {
                    *a += t;
                    *b += t;
                }
            }
            ramp_gain(&mut node.gate, l, r);
            // X1 after the node.
            if node.trips.check([&mut *l, &mut *r]) {
                node.eq.reset();
                tripped = true;
            }
            node.peak.observe([&*l, &*r]);
            if tripped {
                self.trip();
            }
        }
    }

    pub(super) fn render_mixes(&mut self, block: &mut Block<'_>, off: usize, n: usize) {
        let topo = Arc::clone(&self.topo);
        let Self {
            inputs,
            mixes,
            taps,
            tap_buf,
            group_buf,
            tx,
            listen,
            listen_lim,
            listen_buf,
            fade_buf,
            status,
            test,
            test_buf,
            probes,
            ..
        } = self;
        // While a HIL signal runs no mix's TX and no listen tap carries
        // anything: it sounds only on HIL's spare outputs (`render_hil`) and,
        // with `listen`, the listened slots' probe taps (S7, for the server's
        // `&hil=1` listeners only), never to a band member. The mixes still
        // render and meter, and the listen limiter still follows its mix.
        let hil = test.as_ref().is_some_and(|t| t.mask.is_some());
        let probing = test.as_ref().is_some_and(|t| t.mask.is_some() && t.listen);
        let sine = test_buf.get(..n).unwrap_or_default();
        let fade = fade_buf.get(..n).unwrap_or_default();
        let heard_from = topo.inputs.len();
        let mut trips = 0;
        for (m, spec) in topo.mixes.iter().enumerate() {
            let (done, rest) = mixes.split_at_mut(m);
            let Some((mix, _)) = rest.split_first_mut() else {
                continue;
            };
            let MixRt {
                sum,
                levels,
                groups,
                eq,
                limiter,
                fader,
                safety,
                cap,
                trips: mix_trips,
                peak,
            } = mix;
            let (l, r) = sum.get_mut(n);
            l.fill(0.0);
            r.fill(0.0);
            // The inputs in no group, at their levels.
            for &i in &topo.direct {
                if let (Some(input), Some(g)) = (inputs.get(i), levels.get_mut(i)) {
                    accumulate(g, input.p.get(n), (&mut *l, &mut *r));
                }
            }
            // Each group's strip: its inputs at their levels → EQ → fader → mute.
            for (group, strip) in topo.groups.iter().zip(groups.iter_mut()) {
                let (gl, gr) = group_buf.get_mut(n);
                gl.fill(0.0);
                gr.fill(0.0);
                for &i in &group.inputs {
                    if let (Some(input), Some(g)) = (inputs.get(i), levels.get_mut(i)) {
                        accumulate(g, input.p.get(n), (&mut *gl, &mut *gr));
                    }
                }
                if !strip.eq.is_identity() {
                    strip.eq.process([&mut *gl, &mut *gr]);
                }
                stereo_gain(&mut strip.fader, gl, gr);
                if strip.trips.check([&mut *gl, &mut *gr]) {
                    strip.eq.reset();
                    trips += 1;
                }
                strip.peak.observe([&*gl, &*gr]);
                for ((a, c), (x, y)) in l.iter_mut().zip(r.iter_mut()).zip(gl.iter().zip(gr.iter()))
                {
                    *a += x;
                    *c += y;
                }
            }
            // The mixes it hears, after their mute and unclipped (A9).
            for (k, &s) in spec.mixes.iter().enumerate() {
                if let (Some(src), Some(g)) = (done.get(s), levels.get_mut(heard_from + k)) {
                    accumulate(g, src.sum.get(n), (&mut *l, &mut *r));
                }
            }
            if !eq.is_identity() {
                eq.process([&mut *l, &mut *r]);
            }
            limiter.process(l, r);
            if listen[0] == Some(m) {
                push_tap(&mut taps[0], (&*l, &*r), hil, tap_buf, &status.tap_overruns);
                if probing {
                    probe::push_probe(&mut probes[0], sine, fade, tap_buf, &status.tap_overruns);
                }
            }
            stereo_gain(fader, l, r);
            if mix_trips.check([&mut *l, &mut *r]) {
                eq.reset();
                limiter.lim.reset();
                trips += 1;
            }
            peak.observe([&*l, &*r]);
            if listen[1] == Some(m) {
                let (ll, lr) = listen_buf.get_mut(n);
                copy(ll, l);
                copy(lr, r);
                listen_lim.process(ll, lr);
                push_tap(
                    &mut taps[1],
                    (&*ll, &*lr),
                    hil,
                    tap_buf,
                    &status.tap_overruns,
                );
                if probing {
                    probe::push_probe(&mut probes[1], sine, fade, tap_buf, &status.tap_overruns);
                }
            }
            let (tl, tr) = tx.get_mut(n);
            for (((a, c), (x, y)), f) in tl
                .iter_mut()
                .zip(tr.iter_mut())
                .zip(l.iter().zip(r.iter()))
                .zip(fade)
            {
                let (inl, inr) = if spec.mono {
                    ((x + y) * 0.5, 0.0)
                } else {
                    (*x, *y)
                };
                let (yl, yr) = safety.tick(inl, inr);
                *a = yl.clamp(-*cap, *cap) * f;
                *c = yr.clamp(-*cap, *cap) * f;
            }
            for (ch, src) in spec.tx.iter().zip([&*tl, &*tr]) {
                let Some(ch) = *ch else {
                    continue;
                };
                if let Some(out) = block.output(ch).get_mut(off..off + n) {
                    if hil {
                        out.fill(0.0);
                    } else {
                        copy(out, src);
                    }
                }
            }
        }
        if trips > 0 {
            self.trips += trips;
            self.status.trips.fetch_add(trips, Ordering::Relaxed);
        }
    }

    /// HIL's spare outputs after the topology's TX (S6): while a HIL signal
    /// runs, the masked ones carry its sine, capped at the test-signal level
    /// and faded like every output; otherwise, and the others, zero (A1).
    /// Their peaks go to the meter frame.
    pub(super) fn render_hil(&mut self, block: &mut Block<'_>, off: usize, n: usize) {
        let first = self.topo.tx.len();
        let mask = self.test.as_ref().and_then(|t| t.mask.as_ref());
        let sine = self.test_buf.get(..n).unwrap_or_default();
        let fade = self.fade_buf.get(..n).unwrap_or_default();
        for (k, peak) in self.hil_peaks.iter_mut().enumerate() {
            let Some(out) = block.output(first + k).get_mut(off..off + n) else {
                continue;
            };
            if mask.is_some_and(|m| m.get(k).copied().unwrap_or(false)) {
                for ((y, s), f) in out.iter_mut().zip(sine).zip(fade) {
                    *y = s.clamp(-TEST_CAP, TEST_CAP) * f;
                }
            } else {
                out.fill(0.0);
            }
            peak.observe([&*out]);
        }
    }

    /// The D5(b) loopback round-trip (S6 test 5): records the first hil-output
    /// sample at or above the onset threshold as the emit, feeds every block
    /// of every loopback-return input (after the topology's rx) to the probe,
    /// which judges the echo and a busy return, and stores the delay in
    /// samples once measured. No-op unless the return is open.
    pub(super) fn probe_latency(&mut self, block: &mut Block<'_>, off: usize, n: usize) {
        if self.hil_rx == 0 {
            return;
        }
        let out_first = self.topo.tx.len();
        let mut emit: Option<usize> = None;
        for k in 0..self.hil_peaks.len() {
            let out = block.output(out_first + k);
            if let Some(seg) = out.get(off..off + n)
                && let Some(i) = seg.iter().position(|y| y.abs() >= latency::ONSET)
            {
                emit = Some(emit.map_or(i, |e| e.min(i)));
            }
        }
        if let Some(i) = emit {
            self.latency.emitted(self.time.saturating_add(i as u64));
        }
        let in_first = self.topo.rx.len();
        for j in 0..self.hil_rx {
            let ret = block.input(in_first + j);
            if let Some(seg) = ret.get(off..off + n) {
                self.latency.feed(seg, self.time);
            }
        }
        if let Some(samples) = self.latency.samples() {
            self.status
                .loopback_samples
                .store(samples, Ordering::Relaxed);
        }
    }
}
