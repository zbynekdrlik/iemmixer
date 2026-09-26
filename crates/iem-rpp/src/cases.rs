//! The S1b case catalogue (design note §6). Every rendered track is one
//! case; its metadata tells scripts/golden/analyze.py what it measures.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::fx::{Fx, FxSlot};
use crate::oracle::{Tap, post_fader_taps};
use crate::project::{Item, Project, RenderFormat, Send, SendMode, Track};
use crate::reaeq::{Band, BandKind, ReaEq};
use crate::stimulus::{hot_material, impulse, log_sweep};

pub const FAMILIES: [&str; 10] = [
    "cal", "pan", "mute", "sum", "downmix", "mono", "bypass", "eq", "site-eq", "lim",
];
pub const RATES: [u32; 3] = [44_100, 48_000, 96_000];
/// Impulse amplitude (headroom for +12 dB gains in float renders).
pub const AMP: f64 = 0.5;
/// Seed of the limiter material (S2 regenerates it with the same seed).
pub const HOT_SEED: u64 = 27;

#[derive(Debug, Clone, PartialEq)]
pub struct Stimulus {
    pub file: String,
    pub rate: u32,
    pub channels: Vec<Vec<f64>>,
    /// Impulse taps (None for sweeps and hot material).
    pub taps: Option<Vec<Tap>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CaseMeta {
    pub track: String,
    pub family: String,
    pub stimulus: Option<String>,
    pub position: u64,
    pub params: Value,
    pub expect: Option<Vec<Tap>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Case {
    pub project: Project,
    pub meta: Vec<CaseMeta>,
}

pub struct Catalogue {
    pub projects: Vec<Case>,
    pub stimuli: Vec<Stimulus>,
}

#[derive(Debug, Error)]
pub enum CatalogueError {
    #[error("unknown family: {0}")]
    Family(String),
    #[error("site EQ list: {0}")]
    SiteEq(#[from] serde_json::Error),
}

pub fn db(x: f64) -> f64 {
    10f64.powf(x / 20.0)
}

fn k0(rate: u32) -> u64 {
    u64::from(rate) / 100
}

fn k2(rate: u32) -> u64 {
    u64::from(rate) / 50
}

fn samples(rate: u32, ms: u64) -> u64 {
    u64::from(rate) * ms / 1000
}

pub fn stimuli() -> Vec<Stimulus> {
    let mut out = Vec::new();
    for rate in RATES {
        #[allow(clippy::cast_possible_truncation)]
        let (n, a, b) = (
            samples(rate, 500) as usize,
            k0(rate) as usize,
            k2(rate) as usize,
        );
        let (ta, tb) = (k0(rate), k2(rate));
        out.push(Stimulus {
            file: format!("imp-dm-{rate}.wav"),
            rate,
            channels: vec![impulse(n, a, AMP), impulse(n, a, AMP)],
            taps: Some(vec![Tap {
                at: ta,
                l: AMP,
                r: AMP,
            }]),
        });
        out.push(Stimulus {
            file: format!("imp-st-{rate}.wav"),
            rate,
            channels: vec![impulse(n, a, AMP), impulse(n, b, AMP)],
            taps: Some(vec![
                Tap {
                    at: ta,
                    l: AMP,
                    r: 0.0,
                },
                Tap {
                    at: tb,
                    l: 0.0,
                    r: AMP,
                },
            ]),
        });
        // Hypothesis under test (A2): mono media plays as L = R = x.
        out.push(Stimulus {
            file: format!("imp-mono-{rate}.wav"),
            rate,
            channels: vec![impulse(n, a, AMP)],
            taps: Some(vec![Tap {
                at: ta,
                l: AMP,
                r: AMP,
            }]),
        });
        let [l, r] = hot_material(rate, HOT_SEED);
        out.push(Stimulus {
            file: format!("hot-{rate}.wav"),
            rate,
            channels: vec![l, r],
            taps: None,
        });
    }
    let s = log_sweep(96_000, 20.0, 20_000.0, 2.0, 0.25);
    out.push(Stimulus {
        file: "sweep-96000.wav".into(),
        rate: 96_000,
        channels: vec![s.clone(), s],
        taps: None,
    });
    out
}

fn source(name: &str, stimulus: &str, rate: u32, ms: u64) -> Track {
    let mut t = Track::new(name);
    t.render = false;
    t.item = Some(Item {
        stimulus: stimulus.into(),
        position: 0,
        length: samples(rate, ms),
    });
    t
}

fn bus(name: &str, receives: Vec<Send>) -> Track {
    let mut t = Track::new(name);
    t.receives = receives;
    t
}

fn send(src: usize, mode: SendMode, vol: f64, pan: f64) -> Send {
    let mut s = Send::new(src, mode);
    s.vol = vol;
    s.pan = pan;
    s
}

struct Builder {
    id: String,
    rate: u32,
    format: RenderFormat,
    tracks: Vec<Track>,
    meta: Vec<CaseMeta>,
}

impl Builder {
    fn new(id: impl Into<String>, rate: u32) -> Self {
        Self {
            id: id.into(),
            rate,
            format: RenderFormat::Float64,
            tracks: Vec::new(),
            meta: Vec::new(),
        }
    }

    fn push(&mut self, t: Track) -> usize {
        self.tracks.push(t);
        self.tracks.len() - 1
    }

    fn case(&mut self, mut t: Track, family: &str, params: Value) -> usize {
        t.render = true;
        self.meta.push(CaseMeta {
            track: t.name.clone(),
            family: family.into(),
            stimulus: t.item.as_ref().map(|i| i.stimulus.clone()),
            position: t.item.as_ref().map_or(0, |i| i.position),
            params,
            expect: None,
        });
        self.push(t)
    }

    fn finish(self, stimuli: &[Stimulus]) -> Case {
        let project = Project {
            id: self.id,
            rate: self.rate,
            format: self.format,
            tracks: self.tracks,
        };
        let rate = project.rate;
        let lookup = |file: &str| {
            stimuli
                .iter()
                .find(|s| s.file == file && s.rate == rate)
                .and_then(|s| s.taps.clone())
        };
        let taps = post_fader_taps(&project, &lookup);
        let mut meta = self.meta;
        for m in &mut meta {
            if let Some(i) = project.tracks.iter().position(|t| t.name == m.track) {
                m.expect.clone_from(&taps[i]);
            }
        }
        Case { project, meta }
    }
}

fn cal(stimuli: &[Stimulus]) -> Vec<Case> {
    let r = 96_000;
    [RenderFormat::Float64, RenderFormat::Float32]
        .into_iter()
        .map(|format| {
            let bits = format.bits();
            let mut b = Builder::new(format!("cal-96000-f{bits}"), r);
            b.format = format;
            for (case, stim) in [
                ("identity", "imp-dm-96000.wav"),
                ("stereo", "imp-st-96000.wav"),
                ("mono", "imp-mono-96000.wav"),
            ] {
                b.case(
                    source(&format!("cal{bits}-{case}"), stim, r, 500),
                    "cal",
                    json!({"case": case, "bits": bits}),
                );
            }
            let mut trim = source(&format!("cal{bits}-trim6"), "imp-dm-96000.wav", r, 500);
            trim.fx.push(FxSlot::active(Fx::Trim { db: 6.0 }));
            b.case(
                trim,
                "cal",
                json!({"case": "trim6", "bits": bits, "trim_db": 6.0}),
            );
            let band = Band::new(BandKind::Band, 1000.0, db(6.0), 1.0);
            let mut peak = source(&format!("cal{bits}-peak"), "imp-dm-96000.wav", r, 500);
            peak.fx.push(FxSlot::active(Fx::ReaEq(ReaEq::single(band))));
            b.case(
                peak,
                "cal",
                json!({"case": "peak", "bits": bits, "band": band}),
            );
            b.finish(stimuli)
        })
        .collect()
}

pub fn pan_points() -> Vec<f64> {
    let mut p: Vec<f64> = (-20..=20).map(|i| f64::from(i) / 20.0).collect();
    p.extend([0.86, -0.4, 0.04]);
    p
}

fn pan(stimuli: &[Stimulus]) -> Case {
    let r = 96_000;
    let mut b = Builder::new("pan-96000", r);
    for (i, p) in pan_points().into_iter().enumerate() {
        for (kind, stim) in [("dm", "imp-dm-96000.wav"), ("st", "imp-st-96000.wav")] {
            let src = b.push(source(&format!("src-send-{kind}-{i:02}"), stim, r, 500));
            b.case(
                bus(
                    &format!("pan-send-{kind}-{i:02}"),
                    vec![send(src, SendMode::PreFader, 1.0, p)],
                ),
                "pan",
                json!({"what": "send_pan", "source": kind, "pan": p}),
            );
        }
        let mut t = source(&format!("pan-track-st-{i:02}"), "imp-st-96000.wav", r, 500);
        t.pan = p;
        b.case(
            t,
            "pan",
            json!({"what": "track_pan", "source": "st", "pan": p}),
        );
    }
    for (i, (tp, sp)) in [
        (-0.5, -0.5),
        (-0.5, 0.0),
        (-0.5, 0.5),
        (0.5, -0.5),
        (0.5, 0.0),
        (0.5, 0.5),
    ]
    .into_iter()
    .enumerate()
    {
        let mut src = source(&format!("src-post-{i}"), "imp-st-96000.wav", r, 500);
        src.vol = 0.5;
        src.pan = tp;
        let src = b.push(src);
        b.case(
            bus(
                &format!("pan-post-{i}"),
                vec![send(src, SendMode::PostFader, 1.0, sp)],
            ),
            "pan",
            json!({"what": "post_fader", "track_vol": 0.5, "track_pan": tp, "pan": sp}),
        );
    }
    for (i, v) in [0.000803, 0.5, 1.0, 2.0, 3.981, 4.0]
        .into_iter()
        .enumerate()
    {
        let src = b.push(source(&format!("src-vol-{i}"), "imp-dm-96000.wav", r, 500));
        b.case(
            bus(
                &format!("pan-vol-{i}"),
                vec![send(src, SendMode::PreFader, v, 0.0)],
            ),
            "pan",
            json!({"what": "send_vol", "vol": v}),
        );
    }
    b.finish(stimuli)
}

fn mute(stimuli: &[Stimulus]) -> Case {
    let r = 96_000;
    let mut b = Builder::new("mute-96000", r);
    for (name, track_mute, mode, send_mute) in [
        ("mute-track-pre", true, SendMode::PreFader, false),
        ("mute-track-post", true, SendMode::PostFader, false),
        ("mute-send", false, SendMode::PreFader, true),
        ("mute-control", false, SendMode::PreFader, false),
    ] {
        let mut src = source(&format!("src-{name}"), "imp-dm-96000.wav", r, 500);
        src.mute = track_mute;
        let src = b.push(src);
        let mut s = Send::new(src, mode);
        s.mute = send_mute;
        b.case(
            bus(name, vec![s]),
            "mute",
            json!({"track_mute": track_mute, "mode": mode.code(), "send_mute": send_mute}),
        );
    }
    for (name, bus_mute) in [("mute-bus-tap", true), ("mute-bus-tap-control", false)] {
        let src = b.push(source(&format!("src-{name}"), "imp-dm-96000.wav", r, 500));
        let mut mid = bus(
            &format!("mid-{name}"),
            vec![Send::new(src, SendMode::PreFader)],
        );
        mid.render = false;
        mid.mute = bus_mute;
        let mid = b.push(mid);
        b.case(
            bus(name, vec![Send::new(mid, SendMode::PostFader)]),
            "mute",
            json!({"bus_mute": bus_mute, "mode": 0}),
        );
    }
    b.finish(stimuli)
}

fn sum(stimuli: &[Stimulus]) -> Case {
    let r = 96_000;
    let mut b = Builder::new("sum-96000", r);
    let mut in1 = source("in1", "imp-dm-96000.wav", r, 500);
    in1.vol = 0.25;
    let mut in2 = source("in2", "imp-st-96000.wav", r, 500);
    if let Some(item) = in2.item.as_mut() {
        item.position = 3 * k0(r);
    }
    let mut in3 = source("in3", "imp-dm-96000.wav", r, 500);
    in3.pan = -0.3;
    if let Some(item) = in3.item.as_mut() {
        item.position = 6 * k0(r);
    }
    let (i1, i2, i3) = (b.push(in1), b.push(in2), b.push(in3));
    let mut stems = bus(
        "sum-stems",
        vec![
            send(i1, SendMode::PreFader, 0.5, 0.0),
            send(i2, SendMode::PreFader, 0.25, 0.5),
            send(i3, SendMode::PostFader, 1.0, 0.0),
        ],
    );
    stems.vol = 0.5;
    let st = b.case(stems, "sum", json!({"node": "stems"}));
    let mut out = bus(
        "sum-out",
        vec![
            send(st, SendMode::PostFader, 1.0, 0.0),
            send(i1, SendMode::PreFader, 2.0, 0.0),
        ],
    );
    out.vol = 2.0;
    let o = b.case(out, "sum", json!({"node": "output"}));
    b.case(
        bus("sum-elevated", vec![send(o, SendMode::PostFader, 0.5, 0.0)]),
        "sum",
        json!({"node": "elevated"}),
    );
    b.finish(stimuli)
}

fn downmix(stimuli: &[Stimulus]) -> Case {
    let r = 96_000;
    let mut b = Builder::new("downmix-96000", r);
    for (name, kind, vol, pan) in [
        ("dmx-st-p0", "st", 1.0, 0.0),
        ("dmx-st-pl", "st", 1.0, -0.5),
        ("dmx-st-pr", "st", 1.0, 0.5),
        ("dmx-st-v05", "st", 0.5, 0.0),
        ("dmx-dm-p0", "dm", 1.0, 0.0),
    ] {
        let src = b.push(source(
            &format!("src-{name}"),
            &format!("imp-{kind}-96000.wav"),
            r,
            500,
        ));
        let mut s = send(src, SendMode::PreFader, vol, pan);
        s.dst_mono = true;
        b.case(
            bus(name, vec![s]),
            "downmix",
            json!({"source": kind, "vol": vol, "pan": pan, "k": [k0(r), k2(r)]}),
        );
    }
    b.finish(stimuli)
}

fn mono(stimuli: &[Stimulus]) -> Case {
    let r = 96_000;
    let mut b = Builder::new("mono-96000", r);
    for (name, stim, pan) in [
        ("mono-item-p0", "imp-mono-96000.wav", 0.0),
        ("mono-item-p05", "imp-mono-96000.wav", 0.5),
        ("mono-dm-p0", "imp-dm-96000.wav", 0.0),
    ] {
        let mut t = source(name, stim, r, 500);
        t.pan = pan;
        b.case(t, "mono", json!({"file": stim, "pan": pan}));
    }
    b.finish(stimuli)
}

fn bypass(stimuli: &[Stimulus]) -> Case {
    let r = 96_000;
    let mut b = Builder::new("bypass-96000", r);
    let band = Band::new(BandKind::Band, 1000.0, db(12.0), 1.0);
    let eq = || FxSlot::active(Fx::ReaEq(ReaEq::single(band)));
    let trim = || FxSlot::active(Fx::Trim { db: 6.0 });
    let mut chain_off = source("byp-chain", "imp-dm-96000.wav", r, 500);
    chain_off.fx = vec![trim(), eq()];
    chain_off.fx_enabled = false;
    b.case(
        chain_off,
        "bypass",
        json!({"what": "chain_off", "expect": "identity"}),
    );
    let mut slot = source("byp-slot", "imp-dm-96000.wav", r, 500);
    let mut off = eq();
    off.bypassed = true;
    slot.fx = vec![trim(), off];
    b.case(
        slot,
        "bypass",
        json!({"what": "eq_slot_bypassed", "expect": "trim_only", "trim_db": 6.0}),
    );
    let mut control = source("byp-control", "imp-dm-96000.wav", r, 500);
    control.fx = vec![trim(), eq()];
    b.case(
        control,
        "bypass",
        json!({"what": "control", "expect": "trim_and_eq", "trim_db": 6.0, "band": band}),
    );
    b.finish(stimuli)
}

const EQ_FREQS: [f64; 5] = [20.0, 80.0, 1000.0, 8000.0, 20_000.0];
const EQ_GAINS_DB: [f64; 4] = [-12.0, -3.0, 3.0, 12.0];
const EQ_BWS: [f64; 6] = [0.01, 0.4, 0.8, 1.5, 2.0, 4.0];
const HP_BWS: [f64; 4] = [0.4, 1.0, 2.0, 4.0];

fn eq_track(name: &str, stimulus: &str, rate: u32, ms: u64, eq: ReaEq) -> Track {
    let mut t = source(name, stimulus, rate, ms);
    t.fx.push(FxSlot::active(Fx::ReaEq(eq)));
    t
}

fn eq(stimuli: &[Stimulus], rate: u32) -> Case {
    let mut b = Builder::new(format!("eq-{rate}"), rate);
    let imp = format!("imp-dm-{rate}.wav");
    // Case names carry the rate (eq44, eq48, eq96): every case name is unique across the bundle.
    let khz = rate / 1000;
    for (kind, tag) in [
        (BandKind::LowShelf, "ls"),
        (BandKind::HighShelf, "hs"),
        (BandKind::Band, "pk"),
    ] {
        for (fi, f) in EQ_FREQS.into_iter().enumerate() {
            for (gi, g) in EQ_GAINS_DB.into_iter().enumerate() {
                for (wi, w) in EQ_BWS.into_iter().enumerate() {
                    let band = Band::new(kind, f, db(g), w);
                    b.case(
                        eq_track(
                            &format!("eq{khz}-{tag}-f{fi}-g{gi}-w{wi}"),
                            &imp,
                            rate,
                            500,
                            ReaEq::single(band),
                        ),
                        "eq",
                        json!({"band": band, "global_gain": 1.0}),
                    );
                }
            }
        }
    }
    for (fi, f) in EQ_FREQS.into_iter().enumerate() {
        for (gi, g) in [1.0, 0.5, db(9.15)].into_iter().enumerate() {
            for (wi, w) in HP_BWS.into_iter().enumerate() {
                let band = Band::new(BandKind::HighPass, f, g, w);
                b.case(
                    eq_track(
                        &format!("eq{khz}-hp-f{fi}-g{gi}-w{wi}"),
                        &imp,
                        rate,
                        500,
                        ReaEq::single(band),
                    ),
                    "eq",
                    json!({"band": band, "global_gain": 1.0}),
                );
            }
        }
    }
    let mut disabled = ReaEq::single(Band::new(BandKind::Band, 1000.0, db(12.0), 1.0));
    disabled.bands[2].enabled = false;
    let mut global = ReaEq::standard_flat();
    global.global_gain = 0.5;
    let mut cascade = ReaEq::standard_flat();
    cascade.bands[1] = Band::new(BandKind::LowShelf, 200.0, db(6.0), 2.0);
    cascade.bands[4] = Band::new(BandKind::HighShelf, 8000.0, db(-6.0), 2.0);
    for (name, e) in [
        (
            "edge-gain0",
            ReaEq::single(Band::new(BandKind::Band, 1000.0, 0.0, 1.0)),
        ),
        (
            "edge-bw0",
            ReaEq::single(Band::new(BandKind::Band, 1000.0, db(6.0), 0.0)),
        ),
        (
            "edge-top",
            ReaEq::single(Band::new(BandKind::Band, 24_000.0, db(6.0), 1.0)),
        ),
        ("edge-disabled", disabled),
        ("edge-global", global),
        ("edge-cascade", cascade),
    ] {
        let params = json!({"eq": &e});
        b.case(
            eq_track(&format!("eq{khz}-{name}"), &imp, rate, 500, e),
            "eq-edge",
            params,
        );
    }
    b.finish(stimuli)
}

#[derive(Deserialize)]
struct SiteEq {
    id: String,
    eq: ReaEq,
}

pub fn site_eqs() -> Result<Vec<(String, ReaEq)>, serde_json::Error> {
    let list: Vec<SiteEq> = serde_json::from_str(include_str!("../cases/site-eq.json"))?;
    Ok(list.into_iter().map(|s| (s.id, s.eq)).collect())
}

fn site_eq(stimuli: &[Stimulus], eqs: &[(String, ReaEq)]) -> Case {
    let r = 96_000;
    let mut b = Builder::new("site-eq-96000", r);
    for (id, e) in eqs {
        let lid = id.to_ascii_lowercase();
        b.case(
            eq_track(
                &format!("seq-{lid}-imp"),
                "imp-dm-96000.wav",
                r,
                500,
                e.clone(),
            ),
            "site-eq",
            json!({"id": id, "eq": e, "stimulus": "impulse"}),
        );
        b.case(
            eq_track(
                &format!("seq-{lid}-sweep"),
                "sweep-96000.wav",
                r,
                2000,
                e.clone(),
            ),
            "site-eq",
            json!({"id": id, "eq": e, "stimulus": "sweep"}),
        );
    }
    b.finish(stimuli)
}

fn lim(stimuli: &[Stimulus]) -> Vec<Case> {
    [
        (96_000, &[("m6", -6.0), ("m3", -3.0), ("0", 0.0)][..]),
        (48_000, &[("m6", -6.0)][..]),
        (44_100, &[("m6", -6.0)][..]),
    ]
    .into_iter()
    .map(|(rate, limits)| {
        let mut b = Builder::new(format!("lim-{rate}"), rate);
        for (tag, l) in limits {
            let mut t = source(
                &format!("lim-{rate}-{tag}"),
                &format!("hot-{rate}.wav"),
                rate,
                1000,
            );
            t.fx.push(FxSlot::active(Fx::Limiter { limit_db: *l }));
            b.case(
                t,
                "lim",
                json!({"limit_db": l, "release_ms": 50, "link_pct": 75, "seed": HOT_SEED}),
            );
        }
        b.finish(stimuli)
    })
    .collect()
}

pub fn catalogue(only: &[String]) -> Result<Catalogue, CatalogueError> {
    if let Some(bad) = only.iter().find(|f| !FAMILIES.contains(&f.as_str())) {
        return Err(CatalogueError::Family(bad.clone()));
    }
    let want = |f: &str| only.is_empty() || only.iter().any(|o| o == f);
    let stimuli = stimuli();
    let mut projects = Vec::new();
    if want("cal") {
        projects.extend(cal(&stimuli));
    }
    for (name, build) in [
        ("pan", pan as fn(&[Stimulus]) -> Case),
        ("mute", mute),
        ("sum", sum),
        ("downmix", downmix),
        ("mono", mono),
        ("bypass", bypass),
    ] {
        if want(name) {
            projects.push(build(&stimuli));
        }
    }
    if want("eq") {
        projects.extend(RATES.into_iter().map(|rate| eq(&stimuli, rate)));
    }
    if want("site-eq") {
        let eqs = site_eqs()?;
        if !eqs.is_empty() {
            projects.push(site_eq(&stimuli, &eqs));
        }
    }
    if want("lim") {
        projects.extend(lim(&stimuli));
    }
    Ok(Catalogue { projects, stimuli })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::ALLOWED_FX_HEADS;

    fn all() -> Catalogue {
        catalogue(&[]).unwrap()
    }

    fn case<'a>(c: &'a Catalogue, track: &str) -> &'a CaseMeta {
        c.projects
            .iter()
            .flat_map(|p| &p.meta)
            .find(|m| m.track == track)
            .unwrap()
    }

    #[test]
    fn every_project_renders_and_track_names_are_globally_unique() {
        let c = all();
        let mut seen = std::collections::BTreeSet::new();
        for p in &c.projects {
            p.project.to_rpp().unwrap();
            for m in &p.meta {
                assert!(seen.insert(m.track.clone()), "duplicate case {}", m.track);
            }
        }
    }

    #[test]
    fn family_sizes_are_as_designed() {
        let c = all();
        let count = |id: &str| {
            c.projects
                .iter()
                .find(|p| p.project.id == id)
                .unwrap()
                .meta
                .len()
        };
        assert_eq!(count("cal-96000-f64"), 5);
        assert_eq!(count("pan-96000"), 44 * 3 + 6 + 6);
        assert_eq!(count("mute-96000"), 6);
        assert_eq!(count("sum-96000"), 3);
        assert_eq!(count("downmix-96000"), 5);
        assert_eq!(count("eq-96000"), 360 + 60 + 6);
        assert_eq!(count("lim-96000"), 3);
    }

    #[test]
    fn oracle_expectations_follow_the_hypotheses() {
        let c = all();
        let p = pan_points().iter().position(|p| *p == 0.5).unwrap();
        assert_eq!(
            case(&c, &format!("pan-send-dm-{p:02}")).expect,
            Some(vec![Tap {
                at: 960,
                l: 0.25,
                r: 0.5
            }])
        );
        assert_eq!(case(&c, "mute-track-pre").expect, Some(vec![]));
        assert_eq!(case(&c, "mute-bus-tap").expect, Some(vec![]));
        assert!(
            case(&c, "mute-control")
                .expect
                .as_ref()
                .is_some_and(|t| !t.is_empty())
        );
        assert_eq!(case(&c, "dmx-st-p0").expect, None);
        assert_eq!(case(&c, "cal64-trim6").expect, None);
    }

    #[test]
    fn sum_topology_matches_the_hand_computed_taps() {
        let c = all();
        let got = case(&c, "sum-elevated").expect.clone().unwrap();
        let want = [
            (960, 1.125, 1.125),
            (3_840, 0.03125, 0.0),
            (4_800, 0.0, 0.0625),
            (6_720, 0.25, 0.175),
        ];
        assert_eq!(got.len(), want.len());
        for (g, (at, l, r)) in got.iter().zip(want) {
            assert_eq!(g.at, at);
            assert!((g.l - l).abs() < 1e-15 && (g.r - r).abs() < 1e-15, "{g:?}");
        }
    }

    #[test]
    fn only_allowlisted_plugins_appear() {
        for p in &all().projects {
            for line in p.project.to_rpp().unwrap().lines() {
                let t = line.trim();
                if t.starts_with("<VST") || t.starts_with("<JS") {
                    assert!(ALLOWED_FX_HEADS.contains(&&t[1..]), "{t}");
                }
            }
        }
    }

    #[test]
    fn unknown_family_is_refused_and_filters_work() {
        assert!(matches!(
            catalogue(&["nope".into()]),
            Err(CatalogueError::Family(_))
        ));
        let cal_only = catalogue(&["cal".into()]).unwrap();
        assert_eq!(cal_only.projects.len(), 2);
        assert_eq!(cal_only.stimuli.len(), 13);
    }

    #[test]
    fn site_eq_file_parses() {
        let eqs = site_eqs().unwrap();
        assert!(
            eqs.iter()
                .all(|(id, e)| id.starts_with("EQ-") && e.bands.len() == 5)
        );
    }
}
