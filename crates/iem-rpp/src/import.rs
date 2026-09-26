//! Predecessor project → the engine topology and `MixState` (S4 design note
//! §3.2, #20 design note §7). A record-armed track is an input, a track with
//! a hardware output is a mix, any other track is a group's instance (a
//! stems bus) in the one mix it feeds. Every rule that cannot be mapped fails
//! the import, and all problems are reported together.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use iem_engine_proto::{
    BandKind as EqKind, DB_OFF, Eq as EqSettings, EqBand, GroupId, InputId, InputState, Level,
    Limiter, Mix, MixGroup, MixId, MixOut, MixState, Source,
};

use crate::aliases::Aliases;
use crate::legacy::{self, LegacyProject, Plugin, PluginKind, Track};
use crate::read;
use crate::reaeq::{BandKind as ReaKind, EqBlob, ReaEq};
use crate::rpp::RppError;
use crate::topology::{Counts, Routing, TopoGroup, TopoInput, TopoMix, Topology};

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
pub enum Place {
    Input(InputId),
    Mix(MixId),
    /// The instance (a stems bus) of `group` in `mix`.
    Group {
        group: GroupId,
        mix: MixId,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Imported {
    pub topology: Topology,
    /// The levels and group strips the project holds.
    pub routing: Routing,
    pub state: MixState,
    pub counts: Counts,
    /// Things ignored on purpose (bypassed plug-ins outside the mix path,
    /// input faders that feed only the muted master).
    pub notes: Vec<String>,
    /// Every project track's name and what it became, in project order.
    pub tracks: Vec<(String, Place)>,
}

impl Imported {
    /// What the track called `name` became.
    pub fn place(&self, name: &str) -> Option<&Place> {
        self.tracks.iter().find(|(n, _)| n == name).map(|(_, p)| p)
    }
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

/// TX channels of a mix's hardware output: a stereo pair (below 1024) or one
/// channel (1024 and up, the mono downmix).
pub fn tx_of(hwout: &[i64]) -> Result<Vec<u16>, String> {
    let bad = |n: i64| format!("hardware output {n} is not a card channel");
    match hwout {
        [n @ 0..STEREO_FROM] => pair(n + 1).ok_or_else(|| bad(*n)),
        [n @ STEREO_FROM..CHANNEL_LIMIT] => {
            Ok(vec![channel(n - (STEREO_FROM - 1)).ok_or_else(|| bad(*n))?])
        }
        [n] => Err(bad(*n)),
        _ => Err(format!(
            "{} hardware outputs (one is supported)",
            hwout.len()
        )),
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

/// Mixes and group strips have no pan in iemmixer (no GUI sets one).
fn no_pan(t: &Track) -> Result<(), String> {
    if t.pan == 0.0 {
        Ok(())
    } else {
        Err(format!(
            "pan {}: iemmixer mixes and group strips have no pan",
            t.pan
        ))
    }
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
            eq,
        },
    ))
}

/// A mix: stereo with ReaEQ and the limiter (and the listen tap on the
/// engineer's), or mono without plug-ins (a flat EQ and no limiter).
fn mix_track(
    p: &LegacyProject,
    t: &Track,
    id: &MixId,
    active: &[&Plugin],
) -> Result<(TopoMix, MixOut, bool), String> {
    check_fader(t)?;
    no_pan(t)?;
    if !t.fx_on {
        return Err("the FX chain of a mix is switched off".into());
    }
    let tx = tx_of(&t.hwout)?;
    let mut out = MixOut {
        volume_db: lin_to_db(t.vol),
        muted: t.muted,
        ..MixOut::default()
    };
    let mut listen = false;
    if tx.len() == 2 {
        listen = chain(
            active,
            &[PluginKind::ReaEq, PluginKind::Limiter],
            Some(PluginKind::Listen),
        )?;
        if let [eq, lim, ..] = active {
            out.eq = eq_of(p, eq)?;
            out.limiter = limiter(p, lim)?;
        }
    } else {
        chain(active, &[], None)?;
        out.limiter.enabled = false;
    }
    Ok((
        TopoMix {
            id: id.clone(),
            tx,
            mixes: Vec::new(),
        },
        out,
        listen,
    ))
}

/// A group's instance: ReaEQ only, no pan.
fn group_track(p: &LegacyProject, t: &Track, active: &[&Plugin]) -> Result<MixGroup, String> {
    check_fader(t)?;
    no_pan(t)?;
    if !t.fx_on {
        return Err("the FX chain of a group strip is switched off".into());
    }
    chain(active, &[PluginKind::ReaEq], None)?;
    let eq = match active {
        [eq] => eq_of(p, eq)?,
        _ => EqSettings::default(),
    };
    Ok(MixGroup {
        gain_db: lin_to_db(t.vol),
        muted: t.muted,
        eq,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Input,
    Mix,
    Group,
}

impl Role {
    const fn name(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Mix => "mix",
            Self::Group => "group",
        }
    }
}

fn level_of(rc: &legacy::Receive) -> Level {
    Level {
        gain_db: lin_to_db(rc.vol),
        pan: rc.pan,
        muted: rc.mute,
    }
}

/// Imports a project through the aliases.
pub fn import(p: &LegacyProject, aliases: &Aliases) -> Result<Imported, Problems> {
    let mut problems = Vec::new();
    let mut notes = Vec::new();

    // Roles and names: inputs and mixes map to one track each; a group id
    // names every instance of the group.
    let mut roles: Vec<Option<(Role, String)>> = Vec::with_capacity(p.tracks.len());
    let mut unknown = Vec::new();
    let mut seen: BTreeMap<String, (Role, String)> = BTreeMap::new();
    for t in &p.tracks {
        let Some(id) = aliases.tracks.get(&t.name) else {
            unknown.push(format!("{:?}", t.name));
            roles.push(None);
            continue;
        };
        let role = if matches!(t.rec, Some((true, _))) {
            Role::Input
        } else if t.hwout.is_empty() {
            Role::Group
        } else {
            Role::Mix
        };
        match seen.entry(id.clone()) {
            Entry::Vacant(v) => {
                v.insert((role, t.name.clone()));
            }
            Entry::Occupied(o) => {
                let (r, other) = o.get();
                if !(*r == Role::Group && role == Role::Group) {
                    problems.push(format!(
                        "tracks {other:?} ({}) and {:?} ({}) both map to {id}",
                        r.name(),
                        t.name,
                        role.name()
                    ));
                }
            }
        }
        roles.push(Some((role, id.clone())));
    }
    if !unknown.is_empty() {
        problems.push(format!(
            "track names missing from the aliases: {}",
            unknown.join(", ")
        ));
    }

    let mut topology = Topology::default();
    let mut state = MixState::default();
    let mut counts = Counts::default();
    let mut strips: BTreeMap<usize, MixGroup> = BTreeMap::new();
    let mut talkback = Vec::new();
    let mut listen = Vec::new();
    let mut faders = Vec::new();
    for (ti, (t, r)) in p.tracks.iter().zip(&roles).enumerate() {
        let Some((role, id)) = r else { continue };
        counts.tracks += 1;
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
        match role {
            Role::Input => {
                let id = InputId::new(id.clone());
                match input_track(p, t, &id, &active) {
                    Ok((topo, s)) => {
                        if topo.talkback {
                            talkback.push(id.clone());
                        }
                        if t.vol != 1.0 || t.pan != 0.0 {
                            faders.push(format!("{id} {:.2} dB pan {}", lin_to_db(t.vol), t.pan));
                        }
                        counts.trims += 1;
                        counts.eqs += 1;
                        topology.inputs.push(topo);
                        state.inputs.insert(id, s);
                    }
                    Err(e) => problems.push(format!("input {:?}: {e}", t.name)),
                }
            }
            Role::Mix => {
                let id = MixId::new(id.clone());
                match mix_track(p, t, &id, &active) {
                    Ok((topo, out, is_listen)) => {
                        if is_listen {
                            listen.push(id.clone());
                        }
                        if topo.tx.len() == 2 {
                            counts.eqs += 1;
                            counts.limiters += 1;
                        }
                        topology.mixes.push(topo);
                        state.mixes.insert(
                            id,
                            Mix {
                                out,
                                ..Mix::default()
                            },
                        );
                    }
                    Err(e) => problems.push(format!("mix {:?}: {e}", t.name)),
                }
            }
            Role::Group => match group_track(p, t, &active) {
                Ok(strip) => {
                    counts.eqs += 1;
                    strips.insert(ti, strip);
                }
                Err(e) => problems.push(format!("group strip {:?}: {e}", t.name)),
            },
        }
    }
    if !faders.is_empty() {
        notes.push(format!(
            "input faders and pans feed only the muted master and are ignored: {}",
            faders.join(", ")
        ));
    }

    let name = |i: usize| p.tracks.get(i).map_or("?", |t| t.name.as_str());
    let role = |i: usize| roles.get(i).and_then(Option::as_ref);
    let what = |src: usize, dst: usize| format!("send {:?} → {:?}", name(src), name(dst));

    // The mix each group instance feeds: exactly one unity, centred,
    // unmuted mode-0 send.
    let mut instance_mix: BTreeMap<usize, MixId> = BTreeMap::new();
    for (di, t) in p.tracks.iter().enumerate() {
        let Some((Role::Mix, dst)) = role(di) else {
            continue;
        };
        for rc in &t.receives {
            let Some((Role::Group, group)) = role(rc.src) else {
                continue;
            };
            let w = what(rc.src, di);
            if rc.mode != 0 || rc.vol != 1.0 || rc.pan != 0.0 || rc.mute || !rc.plain {
                problems.push(format!(
                    "{w}: a group strip feeds its mix at unity, centred and unmuted (mode 0)"
                ));
            }
            let dst = MixId::new(dst.clone());
            if let Some(other) = instance_mix.get(&rc.src) {
                problems.push(format!("{w}: the group strip already feeds {other}"));
                continue;
            }
            let twice = instance_mix.iter().any(|(k, m)| {
                *m == dst && role(*k).map(|(_, g)| g.as_str()) == Some(group.as_str())
            });
            if twice {
                problems.push(format!("{w}: {dst} has a second instance of {group}"));
            }
            instance_mix.insert(rc.src, dst);
        }
    }
    for ti in strips.keys() {
        if !instance_mix.contains_key(ti) {
            problems.push(format!("group strip {:?} feeds no mix", name(*ti)));
        }
    }

    // Group membership: the inputs that send into a group's instances.
    let mut member_of: BTreeMap<usize, String> = BTreeMap::new();
    for (di, t) in p.tracks.iter().enumerate() {
        let Some((Role::Group, group)) = role(di) else {
            continue;
        };
        for rc in &t.receives {
            if let Some((Role::Input, input)) = role(rc.src) {
                match member_of.get(&rc.src).cloned() {
                    Some(g) if g != *group => problems.push(format!(
                        "input {input} sends into the groups {g} and {group}"
                    )),
                    _ => {
                        member_of.insert(rc.src, group.clone());
                    }
                }
            }
        }
    }

    // Levels: every other receive.
    let mut routing = Routing::default();
    for (di, t) in p.tracks.iter().enumerate() {
        let Some((dst_role, dst_id)) = role(di) else {
            continue;
        };
        let into = match dst_role {
            Role::Mix => MixId::new(dst_id.clone()),
            Role::Group => match instance_mix.get(&di) {
                Some(m) => m.clone(),
                None => continue,
            },
            Role::Input => continue,
        };
        for rc in &t.receives {
            let w = what(rc.src, di);
            let src = match (role(rc.src), dst_role) {
                _ if rc.src >= p.tracks.len() => {
                    problems.push(format!("{w}: source track {} does not exist", rc.src + 1));
                    continue;
                }
                (None, _) => continue,
                (Some((Role::Group, _)), Role::Mix) => {
                    counts.sends += 1;
                    continue;
                }
                (Some((Role::Input, i)), Role::Mix) if rc.mode == 3 => {
                    if let Some(g) = member_of.get(&rc.src) {
                        problems.push(format!(
                            "{w}: input {i} is in the group {g} and reaches mixes only through it"
                        ));
                        continue;
                    }
                    Source::Input(InputId::new(i.clone()))
                }
                (Some((Role::Input, i)), Role::Group) if rc.mode == 3 => {
                    Source::Input(InputId::new(i.clone()))
                }
                (Some((Role::Mix, m)), Role::Mix) if rc.mode == 0 => {
                    Source::Mix(MixId::new(m.clone()))
                }
                (Some(_), _) => {
                    problems.push(format!(
                        "{w}: mode {} is not supported here (inputs send with mode 3, mixes \
                         with mode 0 into mixes)",
                        rc.mode
                    ));
                    continue;
                }
            };
            if !rc.plain {
                problems.push(format!(
                    "{w}: channel mapping, mono sum or phase is not supported"
                ));
                continue;
            }
            if rc.vol < 0.0 || !(-1.0..=1.0).contains(&rc.pan) {
                problems.push(format!(
                    "{w}: volume {} / pan {} out of range",
                    rc.vol, rc.pan
                ));
                continue;
            }
            if !routing.levels.insert((into.clone(), src.clone())) {
                problems.push(format!("{w}: a second send for {src} in {into}"));
                continue;
            }
            counts.sends += 1;
            let mix = state.mixes.entry(into.clone()).or_default();
            match &src {
                Source::Input(i) => {
                    mix.inputs.insert(i.clone(), level_of(rc));
                }
                Source::Mix(m) => {
                    mix.mixes.insert(m.clone(), level_of(rc));
                    if let Some(tm) = topology.mixes.iter_mut().find(|x| x.id == into) {
                        tm.mixes.push(m.clone());
                    }
                }
            }
        }
    }

    // Groups: in the order of their first instance; members in project order.
    for (ti, strip) in &strips {
        let (Some((_, group)), Some(mix)) = (role(*ti), instance_mix.get(ti)) else {
            continue;
        };
        let group = GroupId::new(group.clone());
        if !topology.groups.iter().any(|g| g.id == group) {
            let inputs = member_of
                .iter()
                .filter(|(_, g)| g.as_str() == group.0.as_str())
                .filter_map(|(i, _)| role(*i).map(|(_, id)| InputId::new(id.clone())))
                .collect();
            topology.groups.push(TopoGroup {
                id: group.clone(),
                inputs,
            });
        }
        routing.strips.insert((mix.clone(), group.clone()));
        state
            .mixes
            .entry(mix.clone())
            .or_default()
            .groups
            .insert(group, *strip);
    }

    if p.master.mute_flags & 1 == 0 {
        problems.push(
            "the master is not muted: iemmixer has no master, so its hardware output would \
             fall silent"
                .into(),
        );
    }
    if talkback.len() > 1 {
        problems.push(format!(
            "talkback injector on {} inputs (one allowed)",
            talkback.len()
        ));
    }
    if listen.len() > 1 {
        problems.push(format!(
            "listen tap on {} mixes (one allowed)",
            listen.len()
        ));
    }
    topology.engineer = listen.into_iter().next();

    if !problems.is_empty() {
        return Err(Problems(problems));
    }
    let tracks = p
        .tracks
        .iter()
        .enumerate()
        .filter_map(|(ti, t)| {
            let (r, id) = role(ti)?;
            let place = match r {
                Role::Input => Place::Input(InputId::new(id.clone())),
                Role::Mix => Place::Mix(MixId::new(id.clone())),
                Role::Group => Place::Group {
                    group: GroupId::new(id.clone()),
                    mix: instance_mix.get(&ti)?.clone(),
                },
            };
            Some((t.name.clone(), place))
        })
        .collect();
    Ok(Imported {
        topology,
        routing,
        state,
        counts,
        notes,
        tracks,
    })
}

/// The part of a state a project can hold: inputs whole; each mix's volume
/// and mute, and its EQ and limiter when it is stereo (a mono mix reads back
/// flat, without limiter); the levels and group strips `routing` holds.
pub fn project(topo: &Topology, routing: &Routing, s: &MixState) -> MixState {
    let mut out = MixState::default();
    for i in &topo.inputs {
        if let Some(x) = s.inputs.get(&i.id) {
            out.inputs.insert(i.id.clone(), *x);
        }
    }
    for m in &topo.mixes {
        let Some(x) = s.mixes.get(&m.id) else {
            continue;
        };
        let held = if m.tx.len() == 2 {
            x.out
        } else {
            MixOut {
                volume_db: x.out.volume_db,
                muted: x.out.muted,
                eq: EqSettings::default(),
                limiter: Limiter {
                    enabled: false,
                    ..Limiter::default()
                },
            }
        };
        let mix = Mix {
            out: held,
            inputs: x
                .inputs
                .iter()
                .filter(|(i, _)| routing.has_level(&m.id, &Source::Input((*i).clone())))
                .map(|(i, l)| (i.clone(), *l))
                .collect(),
            groups: x
                .groups
                .iter()
                .filter(|(g, _)| routing.has_strip(&m.id, g))
                .map(|(g, v)| (g.clone(), *v))
                .collect(),
            mixes: x
                .mixes
                .iter()
                .filter(|(h, _)| routing.has_level(&m.id, &Source::Mix((*h).clone())))
                .map(|(h, l)| (h.clone(), *l))
                .collect(),
        };
        out.mixes.insert(m.id.clone(), mix);
    }
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

    fn level(&mut self, who: &str, x: &Level, y: &Level) {
        self.db(who, "gain", x.gain_db, y.gain_db);
        self.val(who, "pan", x.pan, y.pan);
        self.flag(who, "muted", x.muted, y.muted);
    }

    fn levels<K: Ord + fmt::Display>(
        &mut self,
        mix: &MixId,
        a: &BTreeMap<K, Level>,
        b: &BTreeMap<K, Level>,
    ) {
        let ids: BTreeSet<&K> = a.keys().chain(b.keys()).collect();
        for id in ids {
            let who = format!("mix {mix} level {id}");
            match (a.get(id), b.get(id)) {
                (Some(x), Some(y)) => self.level(&who, x, y),
                _ => self.out.push(format!("{who}: in one state only")),
            }
        }
    }
}

/// Differences between two states over what `topo`'s project can hold
/// (`routing`): dB values within `tol_db`, other numbers up to rounding,
/// flags exactly.
pub fn compare(
    topo: &Topology,
    routing: &Routing,
    a: &MixState,
    b: &MixState,
    tol_db: f64,
) -> Vec<String> {
    let (a, b) = (project(topo, routing, a), project(topo, routing, b));
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
                c.eq(&who, &x.eq, &y.eq);
            }
            _ => c.out.push(format!("{who}: in one state only")),
        }
    }
    let ids: BTreeSet<&MixId> = a.mixes.keys().chain(b.mixes.keys()).collect();
    for id in ids {
        let who = format!("mix {id}");
        let (Some(x), Some(y)) = (a.mixes.get(id), b.mixes.get(id)) else {
            c.out.push(format!("{who}: in one state only"));
            continue;
        };
        c.db(&who, "volume", x.out.volume_db, y.out.volume_db);
        c.flag(&who, "muted", x.out.muted, y.out.muted);
        c.eq(&who, &x.out.eq, &y.out.eq);
        c.flag(
            &who,
            "limiter",
            x.out.limiter.enabled,
            y.out.limiter.enabled,
        );
        c.db(
            &who,
            "limit",
            x.out.limiter.limit_db,
            y.out.limiter.limit_db,
        );
        c.levels(id, &x.inputs, &y.inputs);
        c.levels(id, &x.mixes, &y.mixes);
        let groups: BTreeSet<&GroupId> = x.groups.keys().chain(y.groups.keys()).collect();
        for g in groups {
            let who = format!("mix {id} group {g}");
            match (x.groups.get(g), y.groups.get(g)) {
                (Some(p), Some(q)) => {
                    c.db(&who, "gain", p.gain_db, q.gain_db);
                    c.flag(&who, "muted", p.muted, q.muted);
                    c.eq(&who, &p.eq, &q.eq);
                }
                _ => c.out.push(format!("{who}: in one state only")),
            }
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
        aliases_toml, instance_name, project as write, sample_state, synthetic_routing,
        synthetic_site, track_name,
    };

    struct Fixture {
        text: String,
        aliases: Aliases,
    }

    fn fixture() -> Fixture {
        let topo = synthetic_site();
        let routing = synthetic_routing(&topo);
        let text = write(
            &topo,
            &routing,
            &sample_state(&topo, &routing, 11),
            &track_name,
        )
        .unwrap();
        let aliases = parse_aliases(&aliases_toml(&topo, &routing, &BTreeMap::new())).unwrap();
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

    /// `want`, word for word, is one of the problems of importing `text`.
    fn has_problem(text: &str, aliases: &Aliases, want: &str) {
        let p = problems(text, aliases);
        assert!(p.iter().any(|x| x == want), "{want:?} not in {p:#?}");
    }

    /// `text` with the first `from` after the line naming `track` replaced.
    fn after(text: &str, track: &str, from: &str, to: &str) -> String {
        let anchor = format!("NAME \"{}\"", track_name(track));
        let at = text.find(&anchor).unwrap();
        let (head, tail) = text.split_at(at);
        assert!(tail.contains(from), "{from:?} after {track}");
        format!("{head}{}", tail.replacen(from, to, 1))
    }

    fn m1_stems() -> String {
        instance_name(&MixId::new("member1"), &GroupId::new("stems"))
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
        has_problem(
            &f.text,
            &a,
            &format!(
                "tracks {:?} (input) and {:?} (input) both map to mic1",
                track_name("mic1"),
                track_name("mic2")
            ),
        );
        // A group id names only group strips: not an input, not a mix.
        let mut a = f.aliases.clone();
        a.tracks.insert(track_name("member2"), "stems".into());
        let engineer_stems = instance_name(&MixId::new("engineer"), &GroupId::new("stems"));
        has_problem(
            &f.text,
            &a,
            &format!(
                "tracks {:?} (mix) and {:?} (group) both map to stems",
                track_name("member2"),
                track_name(&engineer_stems)
            ),
        );
        let mut a = f.aliases;
        a.tracks.insert(track_name(&m1_stems()), "mic3".into());
        has_problem(
            &f.text,
            &a,
            &format!(
                "tracks {:?} (input) and {:?} (group) both map to mic3",
                track_name("mic3"),
                track_name(&m1_stems())
            ),
        );
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
    fn input_faders_feed_only_the_master_and_are_ignored_with_a_note() {
        let f = fixture();
        let text = after(&f.text, "hand1", "VOLPAN 1 0 ", "VOLPAN 0.5 -0.25 ");
        let imp = import(&LegacyProject::parse(&text).unwrap(), &f.aliases).unwrap();
        let note = imp
            .notes
            .iter()
            .find(|n| n.contains("input faders"))
            .unwrap();
        assert!(note.contains("hand1 -6.02 dB pan -0.25"), "{note}");
        let clean = import(&LegacyProject::parse(&f.text).unwrap(), &f.aliases).unwrap();
        assert_eq!(clean.state, imp.state, "the fader changes nothing");
        assert!(!clean.notes.iter().any(|n| n.contains("input faders")));
        // A pan alone, or a fader alone, is noted as well.
        for (volpan, fader) in [
            ("VOLPAN 1 -0.25 ", "hand1 0.00 dB pan -0.25"),
            ("VOLPAN 0.5 0 ", "hand1 -6.02 dB pan 0"),
        ] {
            let text = after(&f.text, "hand1", "VOLPAN 1 0 ", volpan);
            let imp = import(&LegacyProject::parse(&text).unwrap(), &f.aliases).unwrap();
            let want = format!(
                "input faders and pans feed only the muted master and are ignored: {fader}"
            );
            assert!(imp.notes.contains(&want), "{want:?} not in {:?}", imp.notes);
        }
    }

    #[test]
    fn a_second_talkback_input_fails() {
        let mut topo = synthetic_site();
        topo.inputs[0].talkback = true;
        let routing = synthetic_routing(&topo);
        let f = fixture();
        let text = write(
            &topo,
            &routing,
            &sample_state(&topo, &routing, 1),
            &track_name,
        )
        .unwrap();
        fails_with(&text, &f.aliases, "talkback injector on 2 inputs");
    }

    #[test]
    fn mixes_need_one_hardware_output_their_chain_and_no_pan() {
        let f = fixture();
        let a = &f.aliases;
        fails_with(
            &after(&f.text, "member1", "FX 1", "FX 0"),
            a,
            "the FX chain of a mix is switched off",
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
        let at = f
            .text
            .find(&format!("NAME \"{}\"", track_name("member1")))
            .unwrap();
        let volpan = at + f.text[at..].find("VOLPAN ").unwrap();
        let line_end = volpan + f.text[volpan..].find('\n').unwrap();
        let mut fields: Vec<&str> = f.text[volpan..line_end].split(' ').collect();
        fields[2] = "0.5";
        let panned = format!(
            "{}{}{}",
            &f.text[..volpan],
            fields.join(" "),
            &f.text[line_end..]
        );
        fails_with(
            &panned,
            a,
            "pan 0.5: iemmixer mixes and group strips have no pan",
        );
        let no_listen = after(&f.text, "engineer", "(Test)", "(Test) x");
        assert!(import(&LegacyProject::parse(&no_listen).unwrap(), a).is_ok());
        fails_with(
            &after(&f.text, "engineer", "VBAN IEM", "Other Thing"),
            a,
            "Other Thing",
        );
        // A mono mix carries no plug-ins.
        let text = after(
            &f.text,
            "translator",
            "    <FXCHAIN\n",
            "    <FXCHAIN\n      BYPASS 0 0 0\n      <JS x/y \"\"\n        0\n      >\n",
        );
        fails_with(&text, a, "FX chain [\"x/y\"] is not []");
    }

    #[test]
    fn a_mono_mix_imports_flat_without_a_limiter() {
        let f = fixture();
        let imp = import(&LegacyProject::parse(&f.text).unwrap(), &f.aliases).unwrap();
        let tr = &imp.state.mixes[&MixId::new("translator")];
        assert_eq!(tr.out.eq, EqSettings::default());
        assert!(!tr.out.limiter.enabled);
        assert_eq!(tr.inputs.len(), 1);
        assert!(tr.inputs.contains_key(&InputId::new("hand1")));
        assert_eq!(
            imp.topology.mix(&MixId::new("translator")).unwrap().tx,
            vec![93]
        );
    }

    #[test]
    fn group_strips_feed_one_mix_at_unity_and_hold_their_inputs() {
        let f = fixture();
        let a = &f.aliases;
        let imp = import(&LegacyProject::parse(&f.text).unwrap(), a).unwrap();
        assert_eq!(imp.topology.groups.len(), 1);
        let stems = &imp.topology.groups[0];
        assert_eq!(stems.id, GroupId::new("stems"));
        assert_eq!(
            stems.inputs,
            ["click", "guide", "drums", "bass", "inst", "other", "bgvs"]
                .map(InputId::new)
                .to_vec()
        );
        let m1 = MixId::new("member1");
        assert!(imp.routing.has_strip(&m1, &GroupId::new("stems")));
        assert_eq!(imp.state.mixes[&m1].groups.len(), 1);
        assert_eq!(imp.state.mixes[&m1].inputs.len(), 24);
        assert_eq!(
            imp.place(&track_name(&m1_stems())),
            Some(&Place::Group {
                group: GroupId::new("stems"),
                mix: m1.clone()
            })
        );
        // The link must be unity, centred and unmuted.
        let stems_link = |from: &str, to: &str| {
            let at = f
                .text
                .find(&format!("NAME \"{}\"", track_name("member1")))
                .unwrap();
            let link = at + f.text[at..].find(" 0 1 0 0 0 0 0 0 -1:U 0 -1 ''").unwrap();
            let start = f.text[..link].rfind("AUXRECV").unwrap();
            let line_end = link + f.text[link..].find('\n').unwrap();
            let line = &f.text[start..line_end];
            format!(
                "{}{}{}",
                &f.text[..start],
                line.replacen(from, to, 1),
                &f.text[line_end..]
            )
        };
        fails_with(
            &stems_link(" 0 1 0 0 ", " 0 0.5 0 0 "),
            a,
            "at unity, centred and unmuted",
        );
        fails_with(
            &stems_link(" 0 1 0 0 ", " 0 1 0 1 "),
            a,
            "at unity, centred and unmuted",
        );
        // A strip must not be panned, and it holds only ReaEQ.
        fails_with(
            &after(&f.text, &m1_stems(), " 0 -1 -1 1\n", " 0.5 -1 -1 1\n"),
            a,
            "pan 0.5",
        );
        fails_with(
            &after(&f.text, &m1_stems(), "FX 1", "FX 0"),
            a,
            "the FX chain of a group strip is switched off",
        );
    }

    #[test]
    fn a_strip_that_feeds_no_mix_or_two_fails() {
        let f = fixture();
        let a = &f.aliases;
        // Drop member1's link to its stems strip.
        let at = f
            .text
            .find(&format!("NAME \"{}\"", track_name("member1")))
            .unwrap();
        let link = at + f.text[at..].find(" 0 1 0 0 0 0 0 0 -1:U 0 -1 ''").unwrap();
        let start = f.text[..link].rfind("    AUXRECV").unwrap();
        let line_end = link + f.text[link..].find('\n').unwrap() + 1;
        let line = f.text[start..line_end].to_owned();
        let dropped = format!("{}{}", &f.text[..start], &f.text[line_end..]);
        fails_with(&dropped, a, "feeds no mix");
        // The same strip also into member2: two mixes.
        let at2 = f
            .text
            .find(&format!("NAME \"{}\"", track_name("member2")))
            .unwrap();
        let hw = at2 + f.text[at2..].find("    HWOUT").unwrap();
        let twice = format!("{}{line}{}", &f.text[..hw], &f.text[hw..]);
        fails_with(&twice, a, "the group strip already feeds");
    }

    #[test]
    fn a_mix_holds_one_instance_of_a_group() {
        let f = fixture();
        // member2's stems strip also linked into member1 (ahead of member1's
        // own): member1 then has two instances of stems.
        let at2 = f
            .text
            .find(&format!("NAME \"{}\"", track_name("member2")))
            .unwrap();
        let link = at2 + f.text[at2..].find(" 0 1 0 0 0 0 0 0 -1:U 0 -1 ''").unwrap();
        let start = f.text[..link].rfind("    AUXRECV").unwrap();
        let line_end = link + f.text[link..].find('\n').unwrap() + 1;
        let line = &f.text[start..line_end];
        let at1 = f
            .text
            .find(&format!("NAME \"{}\"", track_name("member1")))
            .unwrap();
        let hw = at1 + f.text[at1..].find("    HWOUT").unwrap();
        let text = format!("{}{line}{}", &f.text[..hw], &f.text[hw..]);
        has_problem(
            &text,
            &f.aliases,
            &format!(
                "send {:?} → {:?}: member1 has a second instance of stems",
                track_name(&m1_stems()),
                track_name("member1")
            ),
        );
    }

    #[test]
    fn an_input_belongs_to_one_group() {
        // The stems split in two groups: every stereo mix has an instance
        // of each, and that imports.
        let mut topo = synthetic_site();
        let beat = topo.groups[0].inputs.split_off(3);
        topo.groups.push(TopoGroup {
            id: GroupId::new("beat"),
            inputs: beat,
        });
        let routing = synthetic_routing(&topo);
        let text = write(
            &topo,
            &routing,
            &sample_state(&topo, &routing, 11),
            &track_name,
        )
        .unwrap();
        let aliases = parse_aliases(&aliases_toml(&topo, &routing, &BTreeMap::new())).unwrap();
        let imp = import(&LegacyProject::parse(&text).unwrap(), &aliases).unwrap();
        let members = |g: &str| {
            imp.topology
                .groups
                .iter()
                .find(|x| x.id == GroupId::new(g))
                .map(|x| x.inputs.clone())
        };
        assert_eq!(
            members("stems"),
            Some(["click", "guide", "drums"].map(InputId::new).to_vec())
        );
        assert_eq!(
            members("beat"),
            Some(["bass", "inst", "other", "bgvs"].map(InputId::new).to_vec())
        );
        assert_eq!(imp.state.mixes[&MixId::new("member1")].groups.len(), 2);
        // bass (in beat) also sends into member1's stems strip.
        let bass = topo
            .inputs
            .iter()
            .position(|i| i.id == InputId::new("bass"))
            .unwrap();
        let strip = instance_name(&MixId::new("member1"), &GroupId::new("stems"));
        let text = after(
            &text,
            &strip,
            "    NCHAN 2\n",
            &format!("    NCHAN 2\n    AUXRECV {bass} 3 1 0 0 0 0 0 0 -1:U 0 -1 ''\n"),
        );
        has_problem(
            &text,
            &aliases,
            "input bass sends into the groups beat and stems",
        );
    }

    #[test]
    fn grouped_inputs_reach_mixes_only_through_their_group() {
        let f = fixture();
        let a = &f.aliases;
        let topo = synthetic_site();
        let drums_index = topo
            .inputs
            .iter()
            .position(|i| i.id == InputId::new("drums"))
            .unwrap();
        let direct = format!("    AUXRECV {drums_index} 3 1 0 0 0 0 0 0 -1:U 0 -1 ''\n");
        let at = f
            .text
            .find(&format!("NAME \"{}\"", track_name("member2")))
            .unwrap();
        let hw = at + f.text[at..].find("    HWOUT").unwrap();
        let text = format!("{}{direct}{}", &f.text[..hw], &f.text[hw..]);
        fails_with(
            &text,
            a,
            "is in the group stems and reaches mixes only through it",
        );
    }

    #[test]
    fn sends_need_the_supported_modes_and_routing() {
        let f = fixture();
        let a = &f.aliases;
        fails_with(
            &after(&f.text, "member1", "AUXRECV 0 3 ", "AUXRECV 0 0 "),
            a,
            "mode 0 is not supported here",
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
        fails_with(&dup, a, "a second send for mic1 in member1");
        // Into a group strip an input sends with mode 3 only…
        let topo = synthetic_site();
        let click = topo
            .inputs
            .iter()
            .position(|i| i.id == InputId::new("click"))
            .unwrap();
        has_problem(
            &after(
                &f.text,
                &m1_stems(),
                &format!("AUXRECV {click} 3 "),
                &format!("AUXRECV {click} 0 "),
            ),
            a,
            &format!(
                "send {:?} → {:?}: mode 0 is not supported here (inputs send with mode 3, mixes \
                 with mode 0 into mixes)",
                track_name("click"),
                track_name(&m1_stems())
            ),
        );
        // …and a mix into a mix with mode 0 only.
        let member2 = topo.inputs.len() + 1;
        has_problem(
            &after(
                &f.text,
                "member1",
                &format!("AUXRECV {member2} 0 "),
                &format!("AUXRECV {member2} 3 "),
            ),
            a,
            &format!(
                "send {:?} → {:?}: mode 3 is not supported here (inputs send with mode 3, mixes \
                 with mode 0 into mixes)",
                track_name("member2"),
                track_name("member1")
            ),
        );
        let routing = synthetic_routing(&topo);
        let mut s = sample_state(&topo, &routing, 11);
        s.mixes
            .get_mut(&MixId::new("member2"))
            .unwrap()
            .inputs
            .get_mut(&InputId::new("mic1"))
            .unwrap()
            .pan = 1.5;
        fails_with(
            &write(&topo, &routing, &s, &track_name).unwrap(),
            a,
            "out of range",
        );
    }

    #[test]
    fn the_master_must_be_muted() {
        let f = fixture();
        let a = &f.aliases;
        fails_with(
            &f.text.replacen("MASTERMUTESOLO 1", "MASTERMUTESOLO 0", 1),
            a,
            "the master is not muted",
        );
        fails_with(
            &f.text.replacen("MASTERMUTESOLO 1", "MASTERMUTESOLO 2", 1),
            a,
            "the master is not muted",
        );
        let other_bits = f.text.replacen("MASTERMUTESOLO 1", "MASTERMUTESOLO 3", 1);
        assert!(import(&LegacyProject::parse(&other_bits).unwrap(), a).is_ok());
        // Its output, volume and plug-ins no longer matter.
        let loud = f
            .text
            .replacen("MASTER_VOLUME 1 ", "MASTER_VOLUME 4 ", 1)
            .replacen("  MASTERHWOUT 88 0 1 0 0 0 0 -1\n", "", 1);
        assert!(import(&LegacyProject::parse(&loud).unwrap(), a).is_ok());
    }

    #[test]
    fn a_clean_import_counts_places_and_notes() {
        let f = fixture();
        let imp = import(&LegacyProject::parse(&f.text).unwrap(), &f.aliases).unwrap();
        assert_eq!(
            imp.counts,
            Counts {
                tracks: 45,
                sends: 268,
                eqs: 44,
                limiters: 10,
                trims: 24
            }
        );
        assert_eq!(imp.tracks.len(), 45);
        assert_eq!(imp.tracks[0].1, Place::Input(InputId::new("mic1")));
        assert_eq!(imp.tracks[24].1, Place::Mix(MixId::new("member1")));
        assert_eq!(
            imp.place(&track_name("engineer")),
            Some(&Place::Mix(MixId::new("engineer")))
        );
        assert_eq!(imp.place("nope"), None);
        assert_eq!(imp.topology.engineer, Some(MixId::new("engineer")));
        assert_eq!(imp.notes.len(), 1);
        // Heard mixes land on the hearing mix, in receive order.
        let m1 = imp.topology.mix(&MixId::new("member1")).unwrap();
        assert_eq!(m1.mixes.len(), 8);
        assert_eq!(m1.mixes[0], MixId::new("member2"));
        assert_eq!(imp.state.mixes[&MixId::new("engineer")].mixes.len(), 9);
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
        assert_eq!(tx_of(&[0]), Ok(vec![1, 2]));
        assert_eq!(tx_of(&[1023]), Ok(vec![1024, 1025]));
        assert_eq!(tx_of(&[1024]), Ok(vec![1]));
        assert_eq!(tx_of(&[1058]), Ok(vec![35]));
        assert_eq!(tx_of(&[2047]), Ok(vec![1024]));
        assert!(tx_of(&[2048]).unwrap_err().contains("2048"));
        assert!(tx_of(&[-1]).unwrap_err().contains("-1"));
        assert!(tx_of(&[1, 2]).unwrap_err().contains("2 hardware outputs"));
        assert!(tx_of(&[]).unwrap_err().contains("0 hardware outputs"));
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
        let routing = synthetic_routing(&topo);
        let a = sample_state(&topo, &routing, 5);
        assert!(compare(&topo, &routing, &a, &a, 0.0).is_empty());
        let mut b = a.clone();
        let mic1 = InputId::new("mic1");
        let m1 = MixId::new("member1");
        b.inputs.get_mut(&mic1).unwrap().trim_db += 1.0;
        b.inputs.get_mut(&mic1).unwrap().eq.bands[1].freq_hz += 1.0;
        b.inputs.get_mut(&mic1).unwrap().eq.bands[1].kind = EqKind::Peak;
        let x = b.mixes.get_mut(&m1).unwrap();
        x.out.limiter.enabled ^= true;
        x.inputs.get_mut(&InputId::new("mic3")).unwrap().pan = 0.123;
        x.groups.get_mut(&GroupId::new("stems")).unwrap().muted ^= true;
        x.mixes.remove(&MixId::new("member2"));
        let tr = b.mixes.get_mut(&MixId::new("translator")).unwrap();
        tr.out.eq.gain_db = 5.0;
        tr.out.limiter.enabled = true;
        tr.inputs.insert(mic1.clone(), Level::default());
        b.mixes.remove(&MixId::new("member2"));
        let d = compare(&topo, &routing, &a, &b, 1e-9);
        for want in [
            "input mic1: trim ",
            "input mic1 band 2: Hz ",
            "input mic1 band 2: kind LowShelf vs Peak",
            "mix member1: limiter ",
            "mix member1 level mic3: pan ",
            "mix member1 group stems: muted ",
            "mix member1 level member2: in one state only",
            "mix member2: in one state only",
        ] {
            assert!(d.iter().any(|x| x.starts_with(want)), "{want:?} in {d:#?}");
        }
        assert_eq!(
            d.len(),
            8,
            "the translator's EQ, limiter and unrouted levels are not part of a project: {d:#?}"
        );
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
        assert_eq!(no_pan(&track(1.0, 0.0)), Ok(()));
        assert!(no_pan(&track(1.0, -0.01)).unwrap_err().contains("-0.01"));
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
    fn a_project_without_a_listen_tap_imports() {
        let mut topo = synthetic_site();
        topo.engineer = None;
        let routing = synthetic_routing(&topo);
        let text = write(
            &topo,
            &routing,
            &sample_state(&topo, &routing, 11),
            &track_name,
        )
        .unwrap();
        assert!(!text.contains("VBAN IEM"));
        let f = fixture();
        let imp = import(&LegacyProject::parse(&text).unwrap(), &f.aliases).unwrap();
        assert_eq!(imp.topology.engineer, None);
    }

    #[test]
    fn project_keeps_what_a_project_can_hold() {
        let topo = synthetic_site();
        let routing = synthetic_routing(&topo);
        let mut s = sample_state(&topo, &routing, 5);
        let tr = MixId::new("translator");
        let x = s.mixes.get_mut(&tr).unwrap();
        x.out.eq.gain_db = 3.0;
        x.out.limiter = Limiter::default();
        x.inputs.insert(InputId::new("mic1"), Level::default());
        x.groups.insert(GroupId::new("stems"), MixGroup::default());
        s.inputs
            .insert(InputId::new("ghost"), InputState::default());
        s.mixes.insert(MixId::new("ghost"), Mix::default());
        let held = project(&topo, &routing, &s);
        let t = &held.mixes[&tr];
        assert_eq!(t.out.eq, EqSettings::default());
        assert!(!t.out.limiter.enabled);
        assert_eq!(t.inputs.len(), 1);
        assert!(t.groups.is_empty());
        assert!(!held.inputs.contains_key(&InputId::new("ghost")));
        assert!(!held.mixes.contains_key(&MixId::new("ghost")));
        let m1 = &held.mixes[&MixId::new("member1")];
        assert_eq!(
            (m1.inputs.len(), m1.mixes.len(), m1.groups.len()),
            (24, 8, 1)
        );
    }
}
