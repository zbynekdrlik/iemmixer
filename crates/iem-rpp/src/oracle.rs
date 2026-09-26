//! Impulse-tap oracle for the linear cases, under the hypotheses the
//! goldens test (design note §6): +0 dB linear balance law, mono media
//! duplicated at unity, mode 3 = pre-fader post-FX, mode 0 = post-fader
//! post-pan, mute zeroes every tap. analyze.py compares renders with it.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::project::{Project, SendMode, Track};

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Tap {
    pub at: u64,
    pub l: f64,
    pub r: f64,
}

/// Stereo balance gains for pan p ∈ [-1, 1] at a +0 dB law.
pub fn balance(p: f64) -> (f64, f64) {
    (1.0 - p.max(0.0), 1.0 + p.min(0.0))
}

type Taps = BTreeMap<u64, (f64, f64)>;

fn add(into: &mut Taps, from: &Taps, gl: f64, gr: f64) {
    for (&at, &(l, r)) in from {
        let e = into.entry(at).or_insert((0.0, 0.0));
        e.0 += gl * l;
        e.1 += gr * r;
    }
}

fn track_taps(
    t: &Track,
    pre: &[Option<Taps>],
    post: &[Option<Taps>],
    stim: &dyn Fn(&str) -> Option<Vec<Tap>>,
) -> Option<(Taps, Taps)> {
    if !t.fx.is_empty() {
        return None;
    }
    let mut p = Taps::new();
    if let Some(item) = &t.item {
        for tap in stim(&item.stimulus)? {
            let e = p.entry(item.position + tap.at).or_insert((0.0, 0.0));
            e.0 += tap.l;
            e.1 += tap.r;
        }
    }
    for s in &t.receives {
        if s.dst_mono || s.mode == SendMode::PreFx {
            return None;
        }
        let from = (if s.mode == SendMode::PreFader {
            pre.get(s.src)?
        } else {
            post.get(s.src)?
        })
        .as_ref()?;
        if s.mute {
            continue;
        }
        let (gl, gr) = balance(s.pan);
        add(&mut p, from, s.vol * gl, s.vol * gr);
    }
    if t.mute {
        p.clear();
    }
    let (gl, gr) = balance(t.pan);
    let mut o = Taps::new();
    add(&mut o, &p, t.vol * gl, t.vol * gr);
    Some((p, o))
}

/// Expected post-fader taps per track (None where the oracle does not apply).
pub fn post_fader_taps(
    project: &Project,
    stim: &dyn Fn(&str) -> Option<Vec<Tap>>,
) -> Vec<Option<Vec<Tap>>> {
    let mut pre: Vec<Option<Taps>> = Vec::with_capacity(project.tracks.len());
    let mut post: Vec<Option<Taps>> = Vec::with_capacity(project.tracks.len());
    for t in &project.tracks {
        match track_taps(t, &pre, &post, stim) {
            Some((p, o)) => {
                pre.push(Some(p));
                post.push(Some(o));
            }
            None => {
                pre.push(None);
                post.push(None);
            }
        }
    }
    post.into_iter()
        .map(|m| m.map(|m| m.into_iter().map(|(at, (l, r))| Tap { at, l, r }).collect()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::{Fx, FxSlot};
    use crate::project::{Item, RenderFormat, Send};

    fn stim(file: &str) -> Option<Vec<Tap>> {
        (file == "dm").then(|| {
            vec![Tap {
                at: 10,
                l: 0.5,
                r: 0.5,
            }]
        })
    }

    fn src(name: &str) -> Track {
        let mut t = Track::new(name);
        t.item = Some(Item {
            stimulus: "dm".into(),
            position: 0,
            length: 100,
        });
        t
    }

    #[test]
    fn balance_is_the_plus_zero_db_linear_law() {
        assert_eq!(balance(0.0), (1.0, 1.0));
        assert_eq!(balance(0.5), (0.5, 1.0));
        assert_eq!(balance(-1.0), (1.0, 0.0));
    }

    #[test]
    fn pre_fader_ignores_the_source_fader_and_post_fader_follows_it() {
        let mut s = src("s");
        s.vol = 0.25;
        s.pan = 0.5;
        let mut pre_bus = Track::new("pre");
        pre_bus.receives.push(Send::new(0, SendMode::PreFader));
        let mut post_bus = Track::new("post");
        post_bus.receives.push(Send::new(0, SendMode::PostFader));
        let p = Project {
            id: "o".into(),
            rate: 96_000,
            format: RenderFormat::Float64,
            tracks: vec![s, pre_bus, post_bus],
        };
        let taps = post_fader_taps(&p, &stim);
        assert_eq!(
            taps[1],
            Some(vec![Tap {
                at: 10,
                l: 0.5,
                r: 0.5
            }])
        );
        assert_eq!(
            taps[2],
            Some(vec![Tap {
                at: 10,
                l: 0.0625,
                r: 0.125
            }])
        );
    }

    #[test]
    fn mute_zeroes_every_tap_and_fx_or_mono_leave_the_oracle() {
        let mut s = src("s");
        s.mute = true;
        let mut bus = Track::new("b");
        bus.receives.push(Send::new(0, SendMode::PreFader));
        let mut fx = src("f");
        fx.fx.push(FxSlot::active(Fx::Trim { db: 6.0 }));
        let mut mono = Track::new("m");
        let mut send = Send::new(0, SendMode::PreFader);
        send.dst_mono = true;
        mono.receives.push(send);
        let p = Project {
            id: "o".into(),
            rate: 96_000,
            format: RenderFormat::Float64,
            tracks: vec![s, bus, fx, mono],
        };
        let taps = post_fader_taps(&p, &stim);
        assert_eq!(taps[1], Some(vec![]));
        assert_eq!(taps[2], None);
        assert_eq!(taps[3], None);
    }
}
