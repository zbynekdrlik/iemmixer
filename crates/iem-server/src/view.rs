//! The UI's view of the engine (S5 design note §5, §6): channels of a mixer
//! page from the mirror, the UI updates a change produces for a page, and
//! the engine commands a UI command becomes — with the server's permissions
//! (the engine does no authorisation, it only caps values). Pure: no I/O.

use iem_core::{Channel, ClientMsg, EqBand, ServerMsg, is_valid_ui_pan};
use iem_engine_proto::{BandKind, Change, Cmd, DB_OFF, Eq, EqTarget, InputId, Source};

use crate::engine::mirror::Mirror;
use crate::site_view::{Page, SiteView};

/// The fader's bottom (−∞ on the UI, off in the engine) and top (F5, F7).
pub const FADER_MIN_DB: f32 = -60.0;
pub const FADER_MAX_DB: f32 = 12.0;
/// The limiter slider's range (F12): 0…1 → −6…0 dB.
pub const LIMIT_MIN_DB: f64 = -6.0;
/// Title of the page mix's EQ and limiter modals (IEM VOL, F7).
pub const OUT_NAME: &str = "IEM VOL";
/// Title of the stems strip's EQ modal.
pub const GROUP_NAME: &str = "STEMS";

/// Who is looking: the JWT subject and whether it is the engineer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    pub sub: String,
    pub engineer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ViewError {
    #[error("unknown id {0:?}")]
    UnknownId(String),
    #[error("bad value: {0}")]
    BadValue(String),
    #[error("not allowed: {0}")]
    Forbidden(&'static str),
    #[error("this site has no stems group")]
    NoGroup,
    #[error("not an engine command")]
    NotACommand,
}

/// Engine dB → the fader's dB: at or below −60 (off included) is −60, capped at +12.
pub fn ui_db(db: f64) -> f32 {
    if db > f64::from(FADER_MIN_DB) {
        db.min(f64::from(FADER_MAX_DB)) as f32
    } else {
        FADER_MIN_DB
    }
}

/// The fader's dB → engine dB: −60 and below are off; `None` when not finite.
pub fn engine_db(ui: f32) -> Option<f64> {
    if !ui.is_finite() {
        None
    } else if ui <= FADER_MIN_DB {
        Some(DB_OFF)
    } else {
        Some(f64::from(ui.min(FADER_MAX_DB)))
    }
}

/// Engine pan −1…1 → UI pan 0…1.
pub fn ui_pan(p: f64) -> f32 {
    ((p + 1.0) / 2.0).clamp(0.0, 1.0) as f32
}

/// UI pan 0…1 → engine pan −1…1; `None` outside 0…1.
pub fn engine_pan(u: f32) -> Option<f64> {
    is_valid_ui_pan(u).then(|| f64::from(u) * 2.0 - 1.0)
}

/// The source a channel id names on `page`: an input, or a mix the page's
/// mix hears.
pub fn source(view: &SiteView, page: &Page, id: &str) -> Option<Source> {
    if view.input(id).is_some() {
        return Some(Source::Input(InputId::new(id)));
    }
    view.mix(&page.mix)?
        .hears
        .iter()
        .find(|m| m.0 == id)
        .map(|m| Source::Mix(m.clone()))
}

/// Whether a solo on the page's mix silences `s` (X2).
fn masked(mirror: &Mirror, page: &Page, s: &Source) -> bool {
    let solo = mirror.solo(&page.mix);
    !solo.is_empty() && !solo.contains(s)
}

fn channel(mirror: &Mirror, page: &Page, s: &Source) -> (f32, bool, f32) {
    let l = mirror.level(&page.mix, s);
    (
        ui_db(l.gain_db),
        l.muted || masked(mirror, page, s),
        ui_pan(l.pan),
    )
}

/// Every channel of `page` for `viewer`: the inputs in topology order, then
/// the mixes the page's mix hears (the Mixes tab).
pub fn channels(view: &SiteView, mirror: &Mirror, page: &Page, viewer: &Viewer) -> Vec<Channel> {
    let own = page.member.as_deref().and_then(|m| view.own_input(m));
    let mut out: Vec<Channel> = view
        .inputs
        .iter()
        .map(|i| {
            let (level_db, muted, pan) = channel(mirror, page, &Source::Input(i.id.clone()));
            Channel {
                id: i.id.0.clone(),
                name: i.name.clone(),
                level_db,
                pan,
                muted,
                category: i.category.clone(),
                eq: viewer.engineer || i.owner.as_deref() == Some(viewer.sub.as_str()),
                own: own == Some(&i.id),
            }
        })
        .collect();
    if let Some(mv) = view.mix(&page.mix) {
        for h in &mv.hears {
            let (level_db, muted, pan) = channel(mirror, page, &Source::Mix(h.clone()));
            out.push(Channel {
                id: h.0.clone(),
                name: view.mix_name(h),
                level_db,
                pan,
                muted,
                category: "mixes".into(),
                eq: viewer.engineer,
                own: false,
            });
        }
    }
    out
}

/// The page's full state (on connect and after a resync).
pub fn state_msg(
    view: &SiteView,
    mirror: &Mirror,
    page: &Page,
    viewer: &Viewer,
    connected: bool,
) -> ServerMsg {
    let out = mirror.out(&page.mix);
    let strip = view.group.as_ref().map(|g| mirror.group(&page.mix, g));
    ServerMsg::State {
        channels: channels(view, mirror, page, viewer),
        connected,
        global_level_db: Some(ui_db(out.volume_db)),
        global_muted: Some(out.muted),
        mix: Some(page.mix.0.clone()),
        stems_level_db: strip.map(|s| ui_db(s.gain_db)),
        stems_muted: strip.map(|s| s.muted),
        group: view.group.as_ref().map(|g| g.0.clone()),
    }
}

/// The UI updates `changes` (already applied to `mirror`) produce for `page`.
pub fn updates_for(
    view: &SiteView,
    mirror: &Mirror,
    page: &Page,
    changes: &[Change],
) -> Vec<ServerMsg> {
    let mut out = Vec::new();
    let shown = |s: &Source| match s {
        Source::Input(id) => view.input(&id.0).is_some(),
        Source::Mix(id) => view.mix(&page.mix).is_some_and(|m| m.hears.contains(id)),
    };
    for c in changes {
        match c {
            Change::Level { mix, source, .. } if *mix == page.mix && shown(source) => {
                let (level_db, muted, pan) = channel(mirror, page, source);
                out.push(ServerMsg::ChannelUpdate {
                    id: source.to_string(),
                    level_db,
                    muted,
                    pan,
                });
            }
            Change::MixOut { mix, out: o } if *mix == page.mix => {
                out.push(ServerMsg::GlobalVolumeUpdate {
                    level_db: ui_db(o.volume_db),
                    muted: o.muted,
                });
            }
            Change::Group { mix, group, state }
                if *mix == page.mix && view.group.as_ref() == Some(group) =>
            {
                out.push(ServerMsg::StemsVolumeUpdate {
                    level_db: ui_db(state.gain_db),
                    muted: state.muted,
                });
            }
            Change::Solo { mix, sources } if *mix == page.mix => {
                out.push(ServerMsg::SoloUpdate {
                    soloed: sources.iter().map(ToString::to_string).collect(),
                });
                // The mask shows as each channel's mute (as the predecessor's
                // REAPER mutes did): every channel is sent again.
                for ch in channels(view, mirror, page, &Viewer::nobody()) {
                    out.push(ServerMsg::ChannelUpdate {
                        id: ch.id,
                        level_db: ch.level_db,
                        muted: ch.muted,
                        pan: ch.pan,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

impl Viewer {
    /// A viewer without rights (for values that do not depend on the viewer).
    pub fn nobody() -> Self {
        Self {
            sub: String::new(),
            engineer: false,
        }
    }
}

fn db(v: f32) -> Result<f64, ViewError> {
    engine_db(v).ok_or_else(|| ViewError::BadValue(format!("{v} dB")))
}

/// The engine commands one UI command becomes on `page` (the connection is
/// already allowed on the page: its own member or the engineer).
pub fn command(
    view: &SiteView,
    mirror: &Mirror,
    page: &Page,
    viewer: &Viewer,
    msg: &ClientMsg,
) -> Result<Vec<Cmd>, ViewError> {
    let src = |id: &str| source(view, page, id).ok_or_else(|| ViewError::UnknownId(id.into()));
    let group = || view.group.clone().ok_or(ViewError::NoGroup);
    let engineer = |what: &'static str| {
        if viewer.engineer {
            Ok(())
        } else {
            Err(ViewError::Forbidden(what))
        }
    };
    let cmd = match msg {
        ClientMsg::SetLevel { id, level_db } => Cmd::SetLevel {
            mix: page.mix.clone(),
            source: src(id)?,
            gain_db: Some(db(*level_db)?),
            pan: None,
            muted: None,
        },
        ClientMsg::SetMute { id, muted } => Cmd::SetLevel {
            mix: page.mix.clone(),
            source: src(id)?,
            gain_db: None,
            pan: None,
            muted: Some(*muted),
        },
        ClientMsg::SetPan { id, pan } => Cmd::SetLevel {
            mix: page.mix.clone(),
            source: src(id)?,
            gain_db: None,
            pan: Some(engine_pan(*pan).ok_or_else(|| ViewError::BadValue(format!("pan {pan}")))?),
            muted: None,
        },
        ClientMsg::SetGlobalLevel { level_db } => Cmd::SetMix {
            mix: page.mix.clone(),
            volume_db: Some(db(*level_db)?),
            muted: None,
        },
        ClientMsg::SetGlobalMute { muted } => Cmd::SetMix {
            mix: page.mix.clone(),
            volume_db: None,
            muted: Some(*muted),
        },
        ClientMsg::SetStemsLevel { level_db } => Cmd::SetGroup {
            mix: page.mix.clone(),
            group: group()?,
            gain_db: Some(db(*level_db)?),
            muted: None,
        },
        ClientMsg::SetStemsMute { muted } => Cmd::SetGroup {
            mix: page.mix.clone(),
            group: group()?,
            gain_db: None,
            muted: Some(*muted),
        },
        ClientMsg::SetSolo { soloed } => Cmd::SetSolo {
            mix: page.mix.clone(),
            sources: soloed.iter().map(|id| src(id)).collect::<Result<_, _>>()?,
        },
        ClientMsg::SetLimiterParam { param, value } => {
            if param != "limit" {
                return Err(ViewError::BadValue(format!("limiter parameter {param:?}")));
            }
            if !(0.0..=1.0).contains(value) {
                return Err(ViewError::BadValue(format!("limit {value}")));
            }
            Cmd::SetLimiter {
                mix: page.mix.clone(),
                enabled: None,
                limit_db: Some(limit_db(*value)),
            }
        }
        ClientMsg::SetLimiterEnabled { enabled } => Cmd::SetLimiter {
            mix: page.mix.clone(),
            enabled: Some(*enabled),
            limit_db: None,
        },
        ClientMsg::ResetLimiterActivity => Cmd::ResetLimiterStats {
            mix: page.mix.clone(),
        },
        ClientMsg::SetEqBand {
            target,
            band,
            param,
            value,
        } => {
            let (target, _) = eq_target(view, page, viewer, target)?;
            let eq = apply_band(&current_eq(mirror, &target), *band, param, *value)?;
            Cmd::SetEq { target, eq }
        }
        ClientMsg::SetInput {
            input,
            trim_db,
            muted,
            processing,
        } => {
            engineer("input controls are the engineer's")?;
            if view.input(input).is_none() {
                return Err(ViewError::UnknownId(input.clone()));
            }
            if trim_db.is_some_and(|t| !t.is_finite()) {
                return Err(ViewError::BadValue("trim".into()));
            }
            Cmd::SetInput {
                input: InputId::new(input.clone()),
                trim_db: trim_db.map(f64::from),
                muted: *muted,
                processing: *processing,
            }
        }
        ClientMsg::ResetLimiterStats { mix } => {
            engineer("another mix's limiter is the engineer's")?;
            let id = iem_engine_proto::MixId::new(mix.clone());
            if view.mix(&id).is_none() {
                return Err(ViewError::UnknownId(mix.clone()));
            }
            Cmd::ResetLimiterStats { mix: id }
        }
        _ => return Err(ViewError::NotACommand),
    };
    Ok(vec![cmd])
}

/// The slider position 0…1 as a limit in dB (−6…0).
pub fn limit_db(norm: f32) -> f64 {
    LIMIT_MIN_DB + f64::from(norm) * -LIMIT_MIN_DB
}

/// A limit in dB as the slider position.
pub fn limit_norm(db: f64) -> f32 {
    ((db - LIMIT_MIN_DB) / -LIMIT_MIN_DB).clamp(0.0, 1.0) as f32
}

/// The EQ a UI target names on `page`, if the viewer may edit it (X7): the
/// page mix's output, the page's stems strip, an input (its owner or the
/// engineer) or a heard mix's output (the engineer).
pub fn eq_target(
    view: &SiteView,
    page: &Page,
    viewer: &Viewer,
    target: &str,
) -> Result<(EqTarget, String), ViewError> {
    if target == page.mix.0 {
        return Ok((EqTarget::Mix(page.mix.clone()), OUT_NAME.into()));
    }
    if let Some(g) = view.group.as_ref().filter(|g| g.0 == target) {
        return Ok((
            EqTarget::Group {
                mix: page.mix.clone(),
                group: g.clone(),
            },
            GROUP_NAME.into(),
        ));
    }
    if let Some(i) = view.input(target) {
        if viewer.engineer || i.owner.as_deref() == Some(viewer.sub.as_str()) {
            return Ok((EqTarget::Input(i.id.clone()), i.name.clone()));
        }
        return Err(ViewError::Forbidden(
            "an input's EQ is its owner's or the engineer's",
        ));
    }
    match source(view, page, target) {
        Some(Source::Mix(m)) if viewer.engineer => {
            let name = view.mix_name(&m);
            Ok((EqTarget::Mix(m), name))
        }
        Some(_) => Err(ViewError::Forbidden("another mix's EQ is the engineer's")),
        None => Err(ViewError::UnknownId(target.into())),
    }
}

/// The EQ the mirror holds for `target`.
pub fn current_eq(mirror: &Mirror, target: &EqTarget) -> Eq {
    match target {
        EqTarget::Input(id) => mirror.input(id).eq,
        EqTarget::Mix(m) => mirror.out(m).eq,
        EqTarget::Group { mix, group } => mirror.group(mix, group).eq,
    }
}

fn kind_name(k: BandKind) -> &'static str {
    match k {
        BandKind::HighPass => "highpass",
        BandKind::LowShelf => "lowshelf",
        BandKind::Peak => "band",
        BandKind::HighShelf => "highshelf",
    }
}

/// The UI's bands of an EQ.
pub fn eq_bands(eq: &Eq) -> Vec<EqBand> {
    eq.bands
        .iter()
        .map(|b| EqBand {
            band_type: kind_name(b.kind).into(),
            freq_hz: b.freq_hz as f32,
            gain_db: b.gain_db.max(DB_OFF) as f32,
            bw: b.bw_oct as f32,
            enabled: b.enabled,
        })
        .collect()
}

/// `eq` with one band value set; `param` is freq_hz, gain_db, bw_oct or
/// enabled. A gain change also switches a band with a gain on, as ReaEQ did
/// under the predecessor (FG-2, P9: the band must not notice the switch); the
/// high-pass has no gain, so its switch stays as it is (no low cut from a
/// gain drag or Reset).
pub fn apply_band(eq: &Eq, band: u8, param: &str, value: f32) -> Result<Eq, ViewError> {
    if !value.is_finite() {
        return Err(ViewError::BadValue(format!("{param} {value}")));
    }
    let mut out = *eq;
    let b = out
        .bands
        .get_mut(usize::from(band))
        .ok_or_else(|| ViewError::BadValue(format!("band {band}")))?;
    let v = f64::from(value);
    match param {
        "freq_hz" => b.freq_hz = v,
        "gain_db" => {
            b.gain_db = v;
            if b.kind != BandKind::HighPass {
                b.enabled = true;
            }
        }
        "bw_oct" => b.bw_oct = v,
        "enabled" => b.enabled = value >= 0.5,
        other => return Err(ViewError::BadValue(format!("EQ parameter {other:?}"))),
    }
    Ok(out)
}

/// The EQ modal's data for `target` on `page`.
pub fn eq_params_msg(
    view: &SiteView,
    mirror: &Mirror,
    page: &Page,
    viewer: &Viewer,
    target: &str,
) -> Result<ServerMsg, ViewError> {
    let (t, name) = eq_target(view, page, viewer, target)?;
    Ok(ServerMsg::EqParams {
        target: target.into(),
        track_name: name,
        bands: eq_bands(&current_eq(mirror, &t)),
    })
}

/// The limiter modal's data for the page's mix (F12); `active_seconds` from
/// the latest meters (X14).
pub fn limiter_msg(mirror: &Mirror, page: &Page, active_seconds: f64) -> ServerMsg {
    let l = mirror.out(&page.mix).limiter;
    ServerMsg::LimiterParams {
        mix: page.mix.0.clone(),
        track_name: OUT_NAME.into(),
        limit_db: l.limit_db as f32,
        limit_norm: limit_norm(l.limit_db),
        enabled: l.enabled,
        active_seconds,
    }
}

/// Mute All (F15): one batch muting every level of the page's mix.
pub fn mute_all(view: &SiteView, page: &Page) -> Cmd {
    let heard = view
        .mix(&page.mix)
        .map(|m| m.hears.clone())
        .unwrap_or_default();
    let sources = view
        .inputs
        .iter()
        .map(|i| Source::Input(i.id.clone()))
        .chain(heard.into_iter().map(Source::Mix));
    Cmd::Batch {
        ops: sources
            .map(|source| Cmd::SetLevel {
                mix: page.mix.clone(),
                source,
                gain_db: None,
                pan: None,
                muted: Some(true),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests;
