//! Predecessor project → engine topology and `MixState` (S4 design note
//! §3.2). Every rule that cannot be mapped fails the import, and all problems
//! are reported together.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use iem_engine_proto::{
    BandKind as EqKind, BusId, BusKind, BusState, DB_OFF, Eq as EqSettings, EqBand, InputId,
    InputState, Limiter, MixState, SendEntry, SendId, SendState, Source, Tap,
};

use crate::aliases::Aliases;
use crate::legacy::{self, LegacyProject, Plugin, PluginKind, Track};
use crate::read;
use crate::reaeq::{BandKind as ReaKind, EqBlob, ReaEq};
use crate::rpp::RppError;
use crate::topology::{Counts, TopoBus, TopoInput, Topology};

/// Everything that stops an import or export, all at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problems(pub Vec<String>);

impl fmt::Display for Problems {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} problem(s):", self.0.len())?;
        for p in &self.0 {
            write!(f, "\n  - {p}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Problems {}

impl From<RppError> for Problems {
    fn from(e: RppError) -> Self {
        Self(vec![e.to_string()])
    }
}

/// What a project track became.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackRef {
    Input(InputId),
    Bus(BusId),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Imported {
    pub topology: Topology,
    pub state: MixState,
    pub counts: Counts,
    /// Things ignored on purpose (bypassed plug-ins outside the mix path).
    pub notes: Vec<String>,
    /// One entry per project track, in project order.
    pub tracks: Vec<TrackRef>,
}

/// `REC` input and `HWOUT` values from here on address a stereo pair…
pub const STEREO_FROM: i64 = 1024;
/// …or a mono channel (`HWOUT`), up to here.
pub const CHANNEL_LIMIT: i64 = 2048;
/// The limiter settings the engine reproduces (A13).
pub const LIMITER_RELEASE_MS: f64 = 50.0;
pub const LIMITER_LINK_PCT: f64 = 75.0;

/// Linear gain → dB; 0 (and anything below the floor) is [`DB_OFF`].
pub fn lin_to_db(v: f64) -> f64 {
    if v > 0.0 {
        (20.0 * v.log10()).max(DB_OFF)
    } else {
        DB_OFF
    }
}

/// Two dB values are the same within `tol` (both off counts as the same).
pub fn db_close(a: f64, b: f64, tol: f64) -> bool {
    (a <= DB_OFF && b <= DB_OFF) || (a - b).abs() <= tol
}

/// Two plain values (pan, Hz, octaves) are the same up to rounding.
pub fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(1.0)
}

fn channel(n: i64) -> Option<u16> {
    u16::try_from(n).ok().filter(|c| *c >= 1)
}

fn pair(first: i64) -> Option<Vec<u16>> {
    let a = channel(first)?;
    Some(vec![a, a.checked_add(1)?])
}

/// Card RX channels of a `REC` input field.
pub fn rx_of(input: i64) -> Option<Vec<u16>> {
    match input {
        0..STEREO_FROM => Some(vec![channel(input + 1)?]),
        STEREO_FROM..CHANNEL_LIMIT => pair(input - (STEREO_FROM - 1)),
        _ => None,
    }
}

/// Bus kind and TX channels of a track's hardware outputs.
pub fn bus_of(hwout: &[i64]) -> Result<(BusKind, Vec<u16>), String> {
    let bad = |n: i64| format!("hardware output {n} is not a card channel");
    match hwout {
        [] => Ok((BusKind::Stems, Vec::new())),
        [n @ 0..STEREO_FROM] => Ok((BusKind::Output, pair(n + 1).ok_or_else(|| bad(*n))?)),
        [n @ STEREO_FROM..CHANNEL_LIMIT] => Ok((
            BusKind::Translator,
            vec![channel(n - (STEREO_FROM - 1)).ok_or_else(|| bad(*n))?],
        )),
        [n] => Err(bad(*n)),
        _ => Err(format!(
            "{} hardware outputs (one is supported)",
            hwout.len()
        )),
    }
}

fn master_tx(hwout: &[i64]) -> Result<Vec<u16>, String> {
    match hwout {
        [n @ 0..STEREO_FROM] => pair(n + 1).ok_or_else(|| format!("MASTERHWOUT {n}")),
        [] => Err("no MASTERHWOUT".into()),
        _ => Err(format!("MASTERHWOUT {hwout:?} is not one stereo pair")),
    }
}

pub const fn eq_kind(k: ReaKind) -> EqKind {
    match k {
        ReaKind::HighPass => EqKind::HighPass,
        ReaKind::LowShelf => EqKind::LowShelf,
        ReaKind::Band => EqKind::Peak,
        ReaKind::HighShelf => EqKind::HighShelf,
    }
}

pub const fn rea_kind(k: EqKind) -> ReaKind {
    match k {
        EqKind::HighPass => ReaKind::HighPass,
        EqKind::LowShelf => ReaKind::LowShelf,
        EqKind::Peak => ReaKind::Band,
        EqKind::HighShelf => ReaKind::HighShelf,
    }
}

pub fn eq_from_reaeq(r: &ReaEq) -> Result<EqSettings, String> {
    let bands: Vec<EqBand> = r
        .bands
        .iter()
        .map(|b| EqBand {
            kind: eq_kind(b.kind),
            enabled: b.enabled,
            freq_hz: b.freq_hz,
            gain_db: lin_to_db(b.gain_lin),
            bw_oct: b.bw_oct,
        })
        .collect();
    let n = bands.len();
    let bands: [EqBand; 5] = bands
        .try_into()
        .map_err(|_| format!("ReaEQ has {n} bands (5 are supported)"))?;
    Ok(EqSettings {
        gain_db: lin_to_db(r.global_gain),
        bands,
    })
}

fn sliders(p: &LegacyProject, x: &Plugin) -> Vec<String> {
    read::tokens(p.body(x).first().copied().unwrap_or_default())
}

fn slider(t: &[String], k: usize, x: &Plugin) -> Result<f64, String> {
    legacy::float(t, k, x.block.head + 1).map_err(|e| e.to_string())
}

fn trim_db(p: &LegacyProject, x: &Plugin) -> Result<f64, String> {
    let t = sliders(p, x);
    let db = slider(&t, 0, x)?;
    if slider(&t, 1, x)? != 0.0 || slider(&t, 2, x)? != 0.0 {
        return Err("TRIM sliders 2 and 3 must be 0".into());
    }
    Ok(db)
}

fn limiter(p: &LegacyProject, x: &Plugin) -> Result<Limiter, String> {
    let t = sliders(p, x);
    let (threshold, release, link, ceiling) = (
        slider(&t, 0, x)?,
        slider(&t, 1, x)?,
        slider(&t, 2, x)?,
        slider(&t, 3, x)?,
    );
    if threshold != ceiling || release != LIMITER_RELEASE_MS || link != LIMITER_LINK_PCT {
        return Err(format!(
            "limiter {threshold}/{release}/{link}/{ceiling}: threshold must equal the ceiling, \
             release 50 ms, link 75 %"
        ));
    }
    Ok(Limiter {
        enabled: !x.bypassed,
        limit_db: ceiling,
    })
}

fn eq_of(p: &LegacyProject, x: &Plugin) -> Result<EqSettings, String> {
    let blob = EqBlob::decode(&p.body(x)).map_err(|e| e.to_string())?;
    eq_from_reaeq(&blob.eq)
}

/// Checks the chain is `base` (plus `optional` at its end); returns whether
/// the optional plug-in is there.
fn chain(
    active: &[&Plugin],
    base: &[PluginKind],
    optional: Option<PluginKind>,
) -> Result<bool, String> {
    let kinds: Vec<PluginKind> = active.iter().map(|x| x.kind).collect();
    let with_optional = optional.map(|o| {
        let mut v = base.to_vec();
        v.push(o);
        v
    });
    let has_optional = if kinds == base {
        false
    } else if with_optional.as_ref() == Some(&kinds) {
        true
    } else {
        let names: Vec<&str> = active.iter().map(|x| x.name.as_str()).collect();
        return Err(format!("FX chain {names:?} is not {base:?}"));
    };
    if let Some(x) = active
        .iter()
        .find(|x| x.bypassed && x.kind != PluginKind::Limiter)
    {
        return Err(format!("{:?} is bypassed", x.name));
    }
    Ok(has_optional)
}

fn check_fader(t: &Track) -> Result<(), String> {
    if t.vol < 0.0 || !(-1.0..=1.0).contains(&t.pan) {
        return Err(format!("volume {} / pan {} out of range", t.vol, t.pan));
    }
    if t.soloed {
        return Err("soloed in REAPER".into());
    }
    Ok(())
}

fn input_track(
    p: &LegacyProject,
    t: &Track,
    id: &InputId,
    active: &[&Plugin],
) -> Result<(TopoInput, InputState), String> {
    check_fader(t)?;
    let input = t.rec.map(|r| r.1);
    let rx = input
        .and_then(rx_of)
        .ok_or_else(|| format!("record input {input:?} is not a mono or stereo card input"))?;
    if !t.receives.is_empty() || !t.hwout.is_empty() {
        return Err("an input has no receives and no hardware output".into());
    }
    let talkback = chain(
        active,
        &[PluginKind::Trim, PluginKind::ReaEq],
        Some(PluginKind::Talkback),
    )?;
    let (trim, eq) = match active {
        [trim, eq, ..] => (trim_db(p, trim)?, eq_of(p, eq)?),
        _ => return Err("TRIM and ReaEQ missing".into()),
    };
    Ok((
        TopoInput {
            id: id.clone(),
            rx,
            talkback,
        },
        InputState {
            trim_db: trim,
            muted: t.muted,
            processing: t.fx_on,
            fader_db: lin_to_db(t.vol),
            pan: t.pan,
            eq,
        },
    ))
}

fn bus_track(
    p: &LegacyProject,
    t: &Track,
    id: &BusId,
    active: &[&Plugin],
) -> Result<(TopoBus, BusState, bool), String> {
    check_fader(t)?;
    if !t.fx_on {
        return Err("the FX chain of a bus is switched off".into());
    }
    let (kind, tx) = bus_of(&t.hwout)?;
    let mut state = BusState {
        fader_db: lin_to_db(t.vol),
        pan: t.pan,
        muted: t.muted,
        ..BusState::default()
    };
    let mut listen = false;
    match kind {
        BusKind::Output => {
            listen = chain(
                active,
                &[PluginKind::ReaEq, PluginKind::Limiter],
                Some(PluginKind::Listen),
            )?;
            if let [eq, lim, ..] = active {
                state.eq = eq_of(p, eq)?;
                state.limiter = limiter(p, lim)?;
            }
        }
        BusKind::Stems => {
            chain(active, &[PluginKind::ReaEq], None)?;
            if let [eq] = active {
                state.eq = eq_of(p, eq)?;
            }
        }
        BusKind::Translator | BusKind::Master => {
            chain(active, &[], None)?;
        }
    }
    Ok((
        TopoBus {
            id: id.clone(),
            kind,
            tx,
        },
        state,
        listen,
    ))
}

/// Imports a project through the aliases.
pub fn import(p: &LegacyProject, aliases: &Aliases) -> Result<Imported, Problems> {
    let mut problems = Vec::new();
    let mut notes = Vec::new();
    let mut refs: Vec<Option<TrackRef>> = Vec::with_capacity(p.tracks.len());
    let mut unknown = Vec::new();
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for t in &p.tracks {
        let Some(id) = aliases.tracks.get(&t.name) else {
            unknown.push(format!("{:?}", t.name));
            refs.push(None);
            continue;
        };
        if let Some(other) = seen.insert(id, &t.name) {
            problems.push(format!(
                "tracks {other:?} and {:?} both map to {id}",
                t.name
            ));
        }
        if *id == aliases.master {
            problems.push(format!("track {:?} maps to the master's id {id}", t.name));
        }
        let armed = matches!(t.rec, Some((true, _)));
        refs.push(Some(if armed {
            TrackRef::Input(InputId::new(id.clone()))
        } else {
            TrackRef::Bus(BusId::new(id.clone()))
        }));
    }
    if !unknown.is_empty() {
        problems.push(format!(
            "track names missing from the aliases: {}",
            unknown.join(", ")
        ));
    }

    let mut topology = Topology::default();
    let mut state = MixState::default();
    let mut talkback = Vec::new();
    let mut listen = Vec::new();
    for (t, r) in p.tracks.iter().zip(&refs) {
        let Some(r) = r else { continue };
        let mut active = Vec::new();
        for x in &t.plugins {
            if x.kind == PluginKind::Other && x.bypassed {
                notes.push(format!(
                    "track {:?}: bypassed plug-in {:?} ignored",
                    t.name, x.name
                ));
            } else {
                active.push(x);
            }
        }
        match r {
            TrackRef::Input(id) => match input_track(p, t, id, &active) {
                Ok((topo, s)) => {
                    if topo.talkback {
                        talkback.push(id.clone());
                    }
                    topology.inputs.push(topo);
                    state.inputs.insert(id.clone(), s);
                }
                Err(e) => problems.push(format!("input {:?}: {e}", t.name)),
            },
            TrackRef::Bus(id) => match bus_track(p, t, id, &active) {
                Ok((topo, s, is_listen)) => {
                    if is_listen {
                        listen.push(id.clone());
                    }
                    topology.buses.push(topo);
                    state.buses.insert(id.clone(), s);
                }
                Err(e) => problems.push(format!("bus {:?}: {e}", t.name)),
            },
        }
    }

    let mut sends = BTreeSet::new();
    for (t, r) in p.tracks.iter().zip(&refs) {
        let Some(TrackRef::Bus(dst)) = r else {
            continue;
        };
        for rc in &t.receives {
            let from = p.tracks.get(rc.src).map_or("?", |s| s.name.as_str());
            let what = format!("send {from:?} → {:?}", t.name);
            let (src, tap) = match (refs.get(rc.src), rc.mode) {
                (None, _) => {
                    problems.push(format!(
                        "{what}: source track {} does not exist",
                        rc.src + 1
                    ));
                    continue;
                }
                (Some(None), _) => continue,
                (Some(Some(TrackRef::Input(i))), 3) => (Source::Input(i.clone()), Tap::Pre),
                (Some(Some(TrackRef::Bus(b))), 0) => (Source::Bus(b.clone()), Tap::Post),
                (Some(Some(_)), m) => {
                    problems.push(format!(
                        "{what}: mode {m} is not supported from this source (inputs 3, buses 0)"
                    ));
                    continue;
                }
            };
            if !rc.plain {
                problems.push(format!(
                    "{what}: channel mapping, mono sum or phase is not supported"
                ));
                continue;
            }
            if rc.vol < 0.0 || !(-1.0..=1.0).contains(&rc.pan) {
                problems.push(format!(
                    "{what}: volume {} / pan {} out of range",
                    rc.vol, rc.pan
                ));
                continue;
            }
            let id = SendId {
                src,
                dst: dst.clone(),
            };
            if !sends.insert(id.clone()) {
                problems.push(format!("{what}: a second send for {id}"));
                continue;
            }
            state.sends.push(SendEntry {
                id: id.clone(),
                state: SendState {
                    gain_db: lin_to_db(rc.vol),
                    pan: rc.pan,
                    muted: rc.mute,
                },
            });
            topology.sends.push((id, tap));
        }
    }
    state.sends.sort_by(|a, b| a.id.cmp(&b.id));

    let m = &p.master;
    let master = BusId::new(aliases.master.clone());
    match master_tx(&m.hwout) {
        Ok(tx) => topology.buses.push(TopoBus {
            id: master.clone(),
            kind: BusKind::Master,
            tx,
        }),
        Err(e) => problems.push(format!("master: {e}")),
    }
    if m.active_fx > 0 {
        problems.push("master: plug-ins in the master chain are not supported".into());
    }
    if m.vol < 0.0 || !(-1.0..=1.0).contains(&m.pan) {
        problems.push(format!(
            "master: volume {} / pan {} out of range",
            m.vol, m.pan
        ));
    }
    state.buses.insert(
        master,
        BusState {
            fader_db: lin_to_db(m.vol),
            pan: m.pan,
            muted: m.mute_flags & 1 != 0,
            ..BusState::default()
        },
    );

    if talkback.len() > 1 {
        problems.push(format!(
            "talkback injector on {} inputs (one allowed)",
            talkback.len()
        ));
    }
    if listen.len() > 1 {
        problems.push(format!(
            "listen tap on {} buses (one allowed)",
            listen.len()
        ));
    }
    topology.engineer = listen.into_iter().next();

    if !problems.is_empty() {
        return Err(Problems(problems));
    }
    let tracks = refs.into_iter().flatten().collect();
    Ok(Imported {
        counts: topology.counts(),
        topology,
        state,
        notes,
        tracks,
    })
}

/// The part of a state a project can hold: inputs whole; output buses whole;
/// stems buses without the limiter; translator and master fader, pan and
/// mute; the topology's sends.
pub fn project(topo: &Topology, s: &MixState) -> MixState {
    let mut out = MixState::default();
    for i in &topo.inputs {
        if let Some(x) = s.inputs.get(&i.id) {
            out.inputs.insert(i.id.clone(), *x);
        }
    }
    let d = BusState::default();
    for b in &topo.buses {
        if let Some(x) = s.buses.get(&b.id) {
            let v = match b.kind {
                BusKind::Output => *x,
                BusKind::Stems => BusState {
                    limiter: d.limiter,
                    ..*x
                },
                BusKind::Translator | BusKind::Master => BusState {
                    eq: d.eq,
                    limiter: d.limiter,
                    ..*x
                },
            };
            out.buses.insert(b.id.clone(), v);
        }
    }
    out.sends = s
        .sends
        .iter()
        .filter(|e| topo.has_send(&e.id))
        .cloned()
        .collect();
    out.sends.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

struct Cmp {
    out: Vec<String>,
    tol_db: f64,
}

impl Cmp {
    fn db(&mut self, who: &str, what: &str, x: f64, y: f64) {
        if !db_close(x, y, self.tol_db) {
            self.out.push(format!("{who}: {what} {x} vs {y}"));
        }
    }

    fn val(&mut self, who: &str, what: &str, x: f64, y: f64) {
        if !close(x, y) {
            self.out.push(format!("{who}: {what} {x} vs {y}"));
        }
    }

    fn flag(&mut self, who: &str, what: &str, x: bool, y: bool) {
        if x != y {
            self.out.push(format!("{who}: {what} {x} vs {y}"));
        }
    }

    fn eq(&mut self, who: &str, x: &EqSettings, y: &EqSettings) {
        self.db(who, "EQ gain", x.gain_db, y.gain_db);
        for (i, (p, q)) in x.bands.iter().zip(&y.bands).enumerate() {
            let w = format!("{who} band {}", i + 1);
            if p.kind != q.kind {
                self.out
                    .push(format!("{w}: kind {:?} vs {:?}", p.kind, q.kind));
            }
            self.flag(&w, "enabled", p.enabled, q.enabled);
            self.val(&w, "Hz", p.freq_hz, q.freq_hz);
            self.db(&w, "gain", p.gain_db, q.gain_db);
            self.val(&w, "octaves", p.bw_oct, q.bw_oct);
        }
    }
}

/// Differences between two states over what `topo`'s project can hold:
/// dB values within `tol_db`, other numbers up to rounding, flags exactly.
pub fn compare(topo: &Topology, a: &MixState, b: &MixState, tol_db: f64) -> Vec<String> {
    let (a, b) = (project(topo, a), project(topo, b));
    let mut c = Cmp {
        out: Vec::new(),
        tol_db,
    };
    let ids: BTreeSet<&InputId> = a.inputs.keys().chain(b.inputs.keys()).collect();
    for id in ids {
        let who = format!("input {id}");
        match (a.inputs.get(id), b.inputs.get(id)) {
            (Some(x), Some(y)) => {
                c.db(&who, "trim", x.trim_db, y.trim_db);
                c.flag(&who, "muted", x.muted, y.muted);
                c.flag(&who, "processing", x.processing, y.processing);
                c.db(&who, "fader", x.fader_db, y.fader_db);
                c.val(&who, "pan", x.pan, y.pan);
                c.eq(&who, &x.eq, &y.eq);
            }
            _ => c.out.push(format!("{who}: in one state only")),
        }
    }
    let ids: BTreeSet<&BusId> = a.buses.keys().chain(b.buses.keys()).collect();
    for id in ids {
        let who = format!("bus {id}");
        match (a.buses.get(id), b.buses.get(id)) {
            (Some(x), Some(y)) => {
                c.db(&who, "fader", x.fader_db, y.fader_db);
                c.val(&who, "pan", x.pan, y.pan);
                c.flag(&who, "muted", x.muted, y.muted);
                c.eq(&who, &x.eq, &y.eq);
                c.flag(&who, "limiter", x.limiter.enabled, y.limiter.enabled);
                c.db(&who, "limit", x.limiter.limit_db, y.limiter.limit_db);
            }
            _ => c.out.push(format!("{who}: in one state only")),
        }
    }
    let xs: BTreeMap<&SendId, &SendState> = a.sends.iter().map(|e| (&e.id, &e.state)).collect();
    let ys: BTreeMap<&SendId, &SendState> = b.sends.iter().map(|e| (&e.id, &e.state)).collect();
    let ids: BTreeSet<&SendId> = xs.keys().chain(ys.keys()).copied().collect();
    for id in ids {
        let who = format!("send {id}");
        match (xs.get(id), ys.get(id)) {
            (Some(x), Some(y)) => {
                c.db(&who, "gain", x.gain_db, y.gain_db);
                c.val(&who, "pan", x.pan, y.pan);
                c.flag(&who, "muted", x.muted, y.muted);
            }
            _ => c.out.push(format!("{who}: in one state only")),
        }
    }
    c.out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::aliases::parse_aliases;
    use crate::reaeq::Band;
    use crate::sitegen::{
        aliases_toml, project as write, sample_state, synthetic_site, track_name,
    };

    struct Fixture {
        text: String,
        aliases: Aliases,
    }

    fn fixture() -> Fixture {
        let topo = synthetic_site();
        let text = write(&topo, &sample_state(&topo, 11), &track_name).unwrap();
        let aliases = parse_aliases(&aliases_toml(&topo, &BTreeMap::new())).unwrap();
        Fixture { text, aliases }
    }

    fn problems(text: &str, aliases: &Aliases) -> Vec<String> {
        import(&LegacyProject::parse(text).unwrap(), aliases)
            .unwrap_err()
            .0
    }

    fn fails_with(text: &str, aliases: &Aliases, needle: &str) {
        let p = problems(text, aliases);
        assert!(
            p.iter().any(|x| x.contains(needle)),
            "{needle:?} not in {p:#?}"
        );
    }

    /// `text` with the first `from` after the line naming `track` replaced.
    fn after(text: &str, track: &str, from: &str, to: &str) -> String {
        let anchor = format!("NAME \"{}\"", track_name(track));
        let at = text.find(&anchor).unwrap();
        let (head, tail) = text.split_at(at);
        assert!(tail.contains(from), "{from:?} after {track}");
        format!("{head}{}", tail.replacen(from, to, 1))
    }

    #[test]
    fn unknown_and_clashing_names_fail_together() {
        let f = fixture();
        let mut a = f.aliases.clone();
        a.tracks.remove(&track_name("mic2"));
        a.tracks.remove(&track_name("keys"));
        let p = problems(&f.text, &a);
        assert_eq!(
            p,
            vec![format!(
                "track names missing from the aliases: {:?}, {:?}",
                track_name("mic2"),
                track_name("keys")
            )]
        );
        let mut a = f.aliases.clone();
        a.tracks.insert(track_name("mic2"), "mic1".into());
        fails_with(&f.text, &a, "both map to mic1");
        let mut a = f.aliases;
        a.tracks.insert(track_name("mic2"), "master".into());
        fails_with(&f.text, &a, "maps to the master's id master");
    }

    #[test]
    fn inputs_need_a_card_input_and_nothing_else() {
        let f = fixture();
        let a = &f.aliases;
        fails_with(
            &after(&f.text, "mic1", "REC 1 100 ", "REC 1 5000 "),
            a,
            "record input Some(5000)",
        );
        fails_with(
            &after(
                &f.text,
                "mic1",
                "    NCHAN 2\n",
                "    NCHAN 2\n    HWOUT 3 0 1 0 0 0 0 -1:U -1\n",
            ),
            a,
            "no receives and no hardware output",
        );
        fails_with(
            &after(&f.text, "mic1", " 0 0\n    IPHASE", " 1 0\n    IPHASE"),
            a,
            "soloed in REAPER",
        );
        fails_with(
            &f.text.replacen("    VOLPAN ", "    VOLPAN -1 ", 1),
            a,
            "out of range",
        );
        fails_with(
            &after(&f.text, "mic1", " 0 0 - ", " 1 0 - "),
            a,
            "TRIM sliders 2 and 3 must be 0",
        );
        fails_with(
            &f.text.replacen("BYPASS 1 0 0", "BYPASS 0 0 0", 1),
            a,
            "synthesis/tonegenerator",
        );
        fails_with(
            &after(&f.text, "mic1", "BYPASS 0 0 0", "BYPASS 1 0 0"),
            a,
            "\"utility/volume_pan\" is bypassed",
        );
    }

    #[test]
    fn a_second_talkback_input_fails() {
        let mut topo = synthetic_site();
        topo.inputs[0].talkback = true;
        let f = fixture();
        let text = write(&topo, &sample_state(&topo, 1), &track_name).unwrap();
        fails_with(&text, &f.aliases, "talkback injector on 2 inputs");
    }

    #[test]
    fn buses_need_one_hardware_output_and_their_chain() {
        let f = fixture();
        let a = &f.aliases;
        fails_with(
            &after(&f.text, "member1", "FX 1", "FX 0"),
            a,
            "switched off",
        );
        fails_with(
            &after(&f.text, "member1", "HWOUT 70 ", "HWOUT 5000 "),
            a,
            "hardware output 5000",
        );
        fails_with(
            &after(
                &f.text,
                "member1",
                "HWOUT 70 0 1 0 0 0 0 -1:U -1\n",
                "HWOUT 70 0 1 0 0 0 0 -1:U -1\n    HWOUT 72 0 1 0 0 0 0 -1:U -1\n",
            ),
            a,
            "2 hardware outputs",
        );
        fails_with(
            &after(&f.text, "member1", "BYPASS 0 0 0", "BYPASS 1 0 0"),
            a,
            "\"VST: ReaEQ (Cockos)\" is bypassed",
        );
        fails_with(
            &after(&f.text, "member1", " 50 75 ", " 40 75 "),
            a,
            "release 50 ms",
        );
        fails_with(
            &after(&f.text, "member1", " 50 75 ", " 50 70 "),
            a,
            "link 75 %",
        );
        let no_listen = after(&f.text, "engineer", "(Test)", "(Test) x");
        assert!(import(&LegacyProject::parse(&no_listen).unwrap(), a).is_ok());
        fails_with(
            &after(&f.text, "engineer", "VBAN IEM", "Other Thing"),
            a,
            "Other Thing",
        );
    }

    #[test]
    fn sends_need_the_supported_modes_and_routing() {
        let f = fixture();
        let a = &f.aliases;
        fails_with(
            &after(&f.text, "member1", "AUXRECV 0 3 ", "AUXRECV 0 0 "),
            a,
            "mode 0 is not supported",
        );
        fails_with(
            &after(&f.text, "member1", "AUXRECV 0 3 ", "AUXRECV 99 3 "),
            a,
            "source track 100 does not exist",
        );
        fails_with(
            &after(
                &f.text,
                "member1",
                " 0 0 0 0 -1:U 0 -1 ''",
                " 0 0 0 1024 -1:U 0 -1 ''",
            ),
            a,
            "channel mapping",
        );
        let at = f.text.find("    AUXRECV 0 3 ").unwrap();
        let line_end = at + f.text[at..].find('\n').unwrap() + 1;
        let dup = format!("{}{}", &f.text[..line_end], &f.text[at..]);
        fails_with(&dup, a, "a second send for mic1>member1");
        let topo = synthetic_site();
        let mut s = sample_state(&topo, 11);
        s.sends[0].state.pan = 1.5;
        fails_with(&write(&topo, &s, &track_name).unwrap(), a, "out of range");
    }

    #[test]
    fn the_master_needs_a_stereo_output_and_no_plugins() {
        let f = fixture();
        let a = &f.aliases;
        let without = f.text.replacen("  MASTERHWOUT 88 0 1 0 0 0 0 -1\n", "", 1);
        assert_ne!(without, f.text);
        fails_with(&without, a, "master: no MASTERHWOUT");
        fails_with(
            &f.text.replacen("MASTERHWOUT 88", "MASTERHWOUT 2000", 1),
            a,
            "not one stereo pair",
        );
        let fx = f.text.replacen(
            "  MASTER_NCH 2 2\n",
            "  MASTER_NCH 2 2\n  <MASTERFXLIST\n    BYPASS 0 0 0\n    <JS x/y \"\"\n      0\n    >\n  >\n",
            1,
        );
        fails_with(&fx, a, "master chain");
        fails_with(
            &f.text.replacen("MASTER_VOLUME ", "MASTER_VOLUME -1 ", 1),
            a,
            "master: volume -1",
        );
        let muted = f
            .text
            .replacen("MASTERMUTESOLO 0", "MASTERMUTESOLO 3", 1)
            .replacen("MASTERMUTESOLO 1", "MASTERMUTESOLO 3", 1);
        let imp = import(&LegacyProject::parse(&muted).unwrap(), a).unwrap();
        assert!(imp.state.buses[&BusId::new("master")].muted);
    }

    #[test]
    fn a_clean_import_counts_and_notes() {
        let f = fixture();
        let imp = import(&LegacyProject::parse(&f.text).unwrap(), &f.aliases).unwrap();
        assert_eq!(imp.counts, synthetic_site().counts());
        assert_eq!(imp.tracks.len(), 45);
        assert_eq!(imp.tracks[0], TrackRef::Input(InputId::new("mic1")));
        assert_eq!(imp.tracks[24], TrackRef::Bus(BusId::new("member1")));
        assert_eq!(imp.topology.engineer, Some(BusId::new("engineer")));
        assert_eq!(imp.notes.len(), 1);
    }

    #[test]
    fn channel_fields_map_to_card_channels() {
        assert_eq!(rx_of(0), Some(vec![1]));
        assert_eq!(rx_of(1023), Some(vec![1024]));
        assert_eq!(rx_of(1024), Some(vec![1, 2]));
        assert_eq!(rx_of(1044), Some(vec![21, 22]));
        assert_eq!(rx_of(2047), Some(vec![1024, 1025]));
        assert_eq!(rx_of(2048), None);
        assert_eq!(rx_of(-1), None);
        assert_eq!(bus_of(&[]), Ok((BusKind::Stems, vec![])));
        assert_eq!(bus_of(&[0]), Ok((BusKind::Output, vec![1, 2])));
        assert_eq!(bus_of(&[1023]), Ok((BusKind::Output, vec![1024, 1025])));
        assert_eq!(bus_of(&[1024]), Ok((BusKind::Translator, vec![1])));
        assert_eq!(bus_of(&[1058]), Ok((BusKind::Translator, vec![35])));
        assert_eq!(bus_of(&[2047]), Ok((BusKind::Translator, vec![1024])));
        assert!(bus_of(&[2048]).unwrap_err().contains("2048"));
        assert!(bus_of(&[-1]).unwrap_err().contains("-1"));
        assert!(bus_of(&[1, 2]).unwrap_err().contains("2 hardware outputs"));
        assert_eq!(master_tx(&[0]), Ok(vec![1, 2]));
        assert!(master_tx(&[1024]).is_err());
    }

    #[test]
    fn gains_and_eq_convert() {
        assert_eq!(lin_to_db(0.0), DB_OFF);
        assert_eq!(lin_to_db(-1.0), DB_OFF);
        assert_eq!(lin_to_db(1e-20), DB_OFF);
        assert_eq!(lin_to_db(1.0), 0.0);
        assert!((lin_to_db(3.981072) - 12.000_000_7).abs() < 1e-6);
        assert!(db_close(-150.0, -200.0, 0.0));
        assert!(!db_close(-149.0, -150.0, 0.5));
        assert!(db_close(1.0, 1.0 + 1e-10, 1e-9));
        assert!(close(1000.0, 1000.0 + 1e-7));
        assert!(!close(1000.0, 1000.1));
        let mut r = ReaEq::standard_flat();
        r.bands[3] = Band {
            kind: ReaKind::Band,
            enabled: true,
            freq_hz: 1000.0,
            gain_lin: 0.0,
            bw_oct: 0.5,
        };
        r.global_gain = 2.0;
        let e = eq_from_reaeq(&r).unwrap();
        assert_eq!(e.bands[3].kind, EqKind::Peak);
        assert_eq!(e.bands[3].gain_db, DB_OFF);
        assert_eq!(e.bands[0].kind, EqKind::HighPass);
        assert_eq!(e.bands[4].kind, EqKind::HighShelf);
        assert_eq!(e.bands[1].kind, EqKind::LowShelf);
        assert!((e.gain_db - 6.020_599_913_279_624).abs() < 1e-12);
        for k in [
            EqKind::HighPass,
            EqKind::LowShelf,
            EqKind::Peak,
            EqKind::HighShelf,
        ] {
            assert_eq!(eq_kind(rea_kind(k)), k);
        }
        r.bands.pop();
        assert_eq!(
            eq_from_reaeq(&r),
            Err("ReaEQ has 4 bands (5 are supported)".into())
        );
    }

    #[test]
    fn compare_names_each_difference() {
        let topo = synthetic_site();
        let a = sample_state(&topo, 5);
        assert!(compare(&topo, &a, &a, 0.0).is_empty());
        let mut b = a.clone();
        let mic1 = InputId::new("mic1");
        let m1 = BusId::new("member1");
        b.inputs.get_mut(&mic1).unwrap().trim_db += 1.0;
        b.inputs.get_mut(&mic1).unwrap().eq.bands[1].freq_hz += 1.0;
        b.inputs.get_mut(&mic1).unwrap().eq.bands[1].kind = EqKind::Peak;
        b.buses.get_mut(&m1).unwrap().limiter.enabled ^= true;
        b.sends[3].state.pan = 0.123;
        b.buses
            .get_mut(&BusId::new("translator"))
            .unwrap()
            .eq
            .gain_db = 5.0;
        b.buses.remove(&BusId::new("member2"));
        let d = compare(&topo, &a, &b, 1e-9);
        assert!(
            d.iter().any(|x| x.starts_with("input mic1: trim ")),
            "{d:#?}"
        );
        assert!(
            d.iter().any(|x| x.starts_with("input mic1 band 2: Hz ")),
            "{d:#?}"
        );
        assert!(
            d.iter()
                .any(|x| x.starts_with("input mic1 band 2: kind LowShelf vs Peak")),
            "{d:#?}"
        );
        assert!(
            d.iter().any(|x| x.starts_with("bus member1: limiter ")),
            "{d:#?}"
        );
        assert!(
            d.iter().any(|x| x == "bus member2: in one state only"),
            "{d:#?}"
        );
        assert!(
            d.iter()
                .any(|x| x.starts_with(&format!("send {}: pan ", b.sends[3].id))),
            "{d:#?}"
        );
        assert_eq!(d.len(), 6, "translator EQ is not part of a project: {d:#?}");
    }

    #[test]
    fn problems_display_every_line() {
        let p = Problems(vec!["a".into(), "b".into()]);
        assert_eq!(p.to_string(), "2 problem(s):\n  - a\n  - b");
        let e: Problems = RppError::Invalid("x".into()).into();
        assert_eq!(e.0, vec!["invalid project: x".to_owned()]);
    }

    #[test]
    fn faders_need_a_non_negative_volume_and_a_pan_in_range() {
        let track = |vol: f64, pan: f64| Track {
            name: "t".into(),
            volpan: None,
            vol,
            pan,
            mutesolo: None,
            muted: false,
            soloed: false,
            rec: None,
            fx: None,
            fx_on: true,
            hwout: Vec::new(),
            receives: Vec::new(),
            plugins: Vec::new(),
        };
        assert_eq!(check_fader(&track(0.0, 0.0)), Ok(()));
        assert_eq!(check_fader(&track(1.0, -1.0)), Ok(()));
        assert_eq!(
            check_fader(&track(-0.5, 0.0)),
            Err("volume -0.5 / pan 0 out of range".to_owned())
        );
        assert_eq!(
            check_fader(&track(1.0, 1.5)),
            Err("volume 1 / pan 1.5 out of range".to_owned())
        );
    }

    #[test]
    fn a_bad_slider_names_its_line() {
        let f = fixture();
        let text = after(&f.text, "mic1", " 0 0 - ", " x 0 - ");
        let at = text
            .find(&format!("NAME \"{}\"", track_name("mic1")))
            .unwrap();
        let slider = at + text[at..].find(" x 0 - ").unwrap();
        let line = text[..slider].matches('\n').count() + 1;
        fails_with(
            &text,
            &f.aliases,
            &format!("line {line}: field 1 \"x\" is not a finite number"),
        );
    }

    #[test]
    fn a_silent_master_imports() {
        let topo = synthetic_site();
        let mut s = sample_state(&topo, 11);
        s.buses.get_mut(&BusId::new("master")).unwrap().fader_db = DB_OFF;
        let text = write(&topo, &s, &track_name).unwrap();
        assert!(text.contains("\n  MASTER_VOLUME 0 "));
        let f = fixture();
        let imp = import(&LegacyProject::parse(&text).unwrap(), &f.aliases).unwrap();
        assert_eq!(imp.state.buses[&BusId::new("master")].fader_db, DB_OFF);
    }

    #[test]
    fn a_project_without_a_listen_tap_imports() {
        let mut topo = synthetic_site();
        topo.engineer = None;
        let text = write(&topo, &sample_state(&topo, 11), &track_name).unwrap();
        assert!(!text.contains("VBAN IEM"));
        let f = fixture();
        let imp = import(&LegacyProject::parse(&text).unwrap(), &f.aliases).unwrap();
        assert_eq!(imp.topology.engineer, None);
    }
}
