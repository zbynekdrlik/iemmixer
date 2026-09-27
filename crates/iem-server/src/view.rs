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

/// `eq` with one band value set; `param` is freq_hz, gain_db, bw_oct or enabled.
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
        "gain_db" => b.gain_db = v,
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
mod tests {
    use super::*;
    use crate::site_view::tests::test_view;
    use iem_engine_proto::{
        EngineMsg, GroupId, Level, Limiter, MixGroup, MixId, MixOut, MixState, Transient,
    };

    fn page(v: &SiteView, id: &str) -> Page {
        v.page(id).unwrap()
    }

    fn member(sub: &str) -> Viewer {
        Viewer {
            sub: sub.into(),
            engineer: false,
        }
    }

    fn engineer() -> Viewer {
        Viewer {
            sub: "engineer".into(),
            engineer: true,
        }
    }

    fn mirror_with(f: impl FnOnce(&mut MixState, &mut Transient)) -> Mirror {
        let mut state = MixState::default();
        let mut transient = Transient::default();
        f(&mut state, &mut transient);
        let mut m = Mirror::default();
        m.apply(&EngineMsg::State {
            rev: 1,
            state,
            transient,
        });
        m
    }

    fn input(id: &str) -> Source {
        Source::Input(InputId::new(id))
    }

    fn mix(id: &str) -> MixId {
        MixId::new(id)
    }

    #[test]
    fn db_and_pan_convert_at_the_edges() {
        assert_eq!(ui_db(DB_OFF), -60.0);
        assert_eq!(ui_db(-60.0), -60.0);
        assert_eq!(ui_db(-59.9), -59.9);
        assert_eq!(ui_db(0.0), 0.0);
        assert_eq!(ui_db(12.04), 12.0);
        assert_eq!(ui_db(f64::NAN), -60.0);
        assert_eq!(engine_db(-60.0), Some(DB_OFF));
        assert_eq!(engine_db(-75.0), Some(DB_OFF));
        assert_eq!(engine_db(-59.8), Some(f64::from(-59.8f32)));
        assert_eq!(engine_db(6.0), Some(6.0));
        assert_eq!(engine_db(20.0), Some(12.0));
        assert_eq!(engine_db(f32::NAN), None);
        assert_eq!(engine_db(f32::INFINITY), None);
        assert_eq!(ui_pan(-1.0), 0.0);
        assert_eq!(ui_pan(0.0), 0.5);
        assert_eq!(ui_pan(1.0), 1.0);
        assert_eq!(ui_pan(3.0), 1.0);
        assert_eq!(engine_pan(0.0), Some(-1.0));
        assert_eq!(engine_pan(0.5), Some(0.0));
        assert_eq!(engine_pan(1.0), Some(1.0));
        assert_eq!(engine_pan(0.75), Some(0.5));
        assert_eq!(engine_pan(1.5), None);
        assert_eq!(engine_pan(f32::NAN), None);
        assert_eq!(limit_db(0.0), -6.0);
        assert_eq!(limit_db(0.5), -3.0);
        assert_eq!(limit_db(1.0), 0.0);
        assert_eq!(limit_norm(-6.0), 0.0);
        assert_eq!(limit_norm(-3.0), 0.5);
        assert_eq!(limit_norm(0.0), 1.0);
        assert_eq!(limit_norm(3.0), 1.0);
    }

    #[test]
    fn channels_follow_the_topology_then_the_heard_mixes() {
        let v = test_view();
        let m = Mirror::default();
        let chs = channels(&v, &m, &page(&v, "member1"), &member("member1"));
        assert_eq!(chs.len(), 24 + 8);
        assert_eq!(chs[0].id, "mic1");
        assert_eq!(chs[0].name, "MEMBER1 mic");
        assert!(chs[0].own && chs[0].eq);
        assert!(!chs[1].own && !chs[1].eq);
        assert_eq!(chs[23].id, "bgvs");
        assert_eq!(chs[23].category, "stems");
        assert_eq!(chs[24].id, "member2");
        assert_eq!(chs[24].name, "Member2");
        assert_eq!(chs[24].category, "mixes");
        assert!(!chs[24].eq);
        // Engine defaults: levels off (−60 on the fader), centred.
        assert!(
            chs.iter()
                .all(|c| c.level_db == -60.0 && c.pan == 0.5 && !c.muted)
        );
        assert_eq!(
            channels(&v, &m, &page(&v, "member2"), &member("member2")).len(),
            24
        );
        let eng = channels(&v, &m, &page(&v, "engineer"), &engineer());
        assert_eq!(eng.len(), 24 + 9);
        assert!(eng.iter().all(|c| c.eq));
        assert!(eng.iter().any(|c| c.own && c.id == "eng_mic"));
        // The engineer on a member's page: that member's own channel, every EQ.
        let on4 = channels(&v, &m, &page(&v, "member4"), &engineer());
        let own: Vec<&str> = on4
            .iter()
            .filter(|c| c.own)
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(own, ["mic4"]);
        assert!(on4.iter().all(|c| c.eq));
        // Member4 owns two inputs: both EQs, one own channel.
        let m4 = channels(&v, &m, &page(&v, "member4"), &member("member4"));
        let eqs: Vec<&str> = m4.iter().filter(|c| c.eq).map(|c| c.id.as_str()).collect();
        assert_eq!(eqs, ["mic4", "mic5"]);
        // The translator page has no own channel.
        let t = channels(&v, &m, &page(&v, "translator"), &engineer());
        assert_eq!(t.len(), 24);
        assert!(t.iter().all(|c| !c.own));
    }

    #[test]
    fn levels_pans_and_the_solo_mask_show_on_the_channels() {
        let v = test_view();
        let m = mirror_with(|s, t| {
            let mx = s.mixes.entry(mix("member1")).or_default();
            mx.inputs.insert(
                InputId::new("mic2"),
                Level {
                    gain_db: -6.0,
                    pan: -0.5,
                    muted: false,
                },
            );
            mx.inputs.insert(
                InputId::new("keys"),
                Level {
                    gain_db: 3.0,
                    pan: 0.0,
                    muted: true,
                },
            );
            mx.mixes.insert(
                mix("member2"),
                Level {
                    gain_db: -1.0,
                    pan: 0.0,
                    muted: false,
                },
            );
            t.solo.push(iem_engine_proto::Solo {
                mix: mix("member1"),
                sources: vec![input("keys"), Source::Mix(mix("member2"))],
            });
        });
        let p = page(&v, "member1");
        let chs = channels(&v, &m, &p, &member("member1"));
        let get = |id: &str| chs.iter().find(|c| c.id == id).unwrap();
        assert_eq!((get("mic2").level_db, get("mic2").pan), (-6.0, 0.25));
        assert!(get("mic2").muted, "masked by the solo");
        assert!(get("keys").muted, "soloed but muted itself");
        assert!(!get("member2").muted, "soloed");
        assert!(get("mic1").muted, "masked");
        // Another page is not masked.
        let other = channels(&v, &m, &page(&v, "member2"), &member("member2"));
        assert!(other.iter().all(|c| !c.muted));
    }

    #[test]
    fn the_state_message_names_the_mix_and_the_stems_strip() {
        let v = test_view();
        let m = mirror_with(|s, _| {
            let mx = s.mixes.entry(mix("member3")).or_default();
            mx.out = MixOut {
                volume_db: -4.0,
                muted: true,
                ..MixOut::default()
            };
            mx.groups.insert(
                GroupId::new("stems"),
                MixGroup {
                    gain_db: 2.0,
                    muted: true,
                    ..MixGroup::default()
                },
            );
        });
        let ServerMsg::State {
            channels,
            connected,
            global_level_db,
            global_muted,
            mix: mx,
            stems_level_db,
            stems_muted,
            group,
        } = state_msg(&v, &m, &page(&v, "member3"), &member("member3"), true)
        else {
            panic!("a State");
        };
        assert_eq!(channels.len(), 24);
        assert!(connected);
        assert_eq!((global_level_db, global_muted), (Some(-4.0), Some(true)));
        assert_eq!(mx.as_deref(), Some("member3"));
        assert_eq!((stems_level_db, stems_muted), (Some(2.0), Some(true)));
        assert_eq!(group.as_deref(), Some("stems"));
        // Without groups the stems fields are absent.
        let mut nogroup = v.clone();
        nogroup.group = None;
        let ServerMsg::State {
            stems_level_db,
            group,
            connected,
            ..
        } = state_msg(
            &nogroup,
            &m,
            &page(&v, "member3"),
            &member("member3"),
            false,
        )
        else {
            panic!("a State");
        };
        assert_eq!((stems_level_db, group, connected), (None, None, false));
    }

    #[test]
    fn changes_become_updates_for_their_page_only() {
        let v = test_view();
        let p1 = page(&v, "member1");
        let level = Level {
            gain_db: -3.0,
            pan: 1.0,
            muted: true,
        };
        let m = mirror_with(|s, _| {
            s.mixes
                .entry(mix("member1"))
                .or_default()
                .inputs
                .insert(InputId::new("mic3"), level);
        });
        let changes = vec![
            Change::Level {
                mix: mix("member1"),
                source: input("mic3"),
                level,
            },
            Change::Level {
                mix: mix("member2"),
                source: input("mic3"),
                level,
            },
            Change::MixOut {
                mix: mix("member1"),
                out: MixOut {
                    volume_db: DB_OFF,
                    muted: false,
                    ..MixOut::default()
                },
            },
            Change::Group {
                mix: mix("member1"),
                group: GroupId::new("stems"),
                state: MixGroup {
                    gain_db: -9.0,
                    muted: true,
                    ..MixGroup::default()
                },
            },
            Change::Group {
                mix: mix("member1"),
                group: GroupId::new("other"),
                state: MixGroup::default(),
            },
            Change::Input {
                id: InputId::new("mic3"),
                state: iem_engine_proto::InputState::default(),
            },
            Change::LimiterStatsReset {
                mix: mix("member1"),
            },
        ];
        assert_eq!(
            updates_for(&v, &m, &p1, &changes),
            vec![
                ServerMsg::ChannelUpdate {
                    id: "mic3".into(),
                    level_db: -3.0,
                    muted: true,
                    pan: 1.0
                },
                ServerMsg::GlobalVolumeUpdate {
                    level_db: -60.0,
                    muted: false
                },
                ServerMsg::StemsVolumeUpdate {
                    level_db: -9.0,
                    muted: true
                },
            ]
        );
        assert!(updates_for(&v, &m, &page(&v, "member3"), &changes).is_empty());
        // A heard mix the page does not hear is not shown.
        let heard = [Change::Level {
            mix: mix("member2"),
            source: Source::Mix(mix("member3")),
            level,
        }];
        assert!(updates_for(&v, &m, &page(&v, "member2"), &heard).is_empty());
    }

    #[test]
    fn a_solo_change_sends_the_solo_and_every_channel_again() {
        let v = test_view();
        let m = mirror_with(|_, t| {
            t.solo.push(iem_engine_proto::Solo {
                mix: mix("member2"),
                sources: vec![input("mic2")],
            });
        });
        let p = page(&v, "member2");
        let ups = updates_for(
            &v,
            &m,
            &p,
            &[Change::Solo {
                mix: mix("member2"),
                sources: vec![input("mic2")],
            }],
        );
        assert_eq!(
            ups[0],
            ServerMsg::SoloUpdate {
                soloed: vec!["mic2".into()]
            }
        );
        assert_eq!(ups.len(), 1 + 24);
        let muted: Vec<bool> = ups[1..]
            .iter()
            .map(|u| match u {
                ServerMsg::ChannelUpdate { muted, .. } => *muted,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(muted.iter().filter(|m| !**m).count(), 1);
    }

    #[test]
    fn another_mixs_solo_changes_nothing_on_the_page() {
        let v = test_view();
        let m = mirror_with(|_, t| {
            t.solo.push(iem_engine_proto::Solo {
                mix: mix("member2"),
                sources: vec![input("mic2")],
            });
        });
        let solo = [Change::Solo {
            mix: mix("member2"),
            sources: vec![input("mic2")],
        }];
        assert!(updates_for(&v, &m, &page(&v, "member3"), &solo).is_empty());
    }

    #[test]
    fn ui_commands_become_engine_commands_on_the_page_mix() {
        let v = test_view();
        let m = Mirror::default();
        let p = page(&v, "member1");
        let me = member("member1");
        let one = |msg: ClientMsg| {
            let mut c = command(&v, &m, &p, &me, &msg).unwrap();
            assert_eq!(c.len(), 1);
            c.remove(0)
        };
        assert_eq!(
            one(ClientMsg::SetLevel {
                id: "mic2".into(),
                level_db: -60.0
            }),
            Cmd::SetLevel {
                mix: mix("member1"),
                source: input("mic2"),
                gain_db: Some(DB_OFF),
                pan: None,
                muted: None
            }
        );
        assert_eq!(
            one(ClientMsg::SetMute {
                id: "member2".into(),
                muted: true
            }),
            Cmd::SetLevel {
                mix: mix("member1"),
                source: Source::Mix(mix("member2")),
                gain_db: None,
                pan: None,
                muted: Some(true)
            }
        );
        assert_eq!(
            one(ClientMsg::SetPan {
                id: "keys".into(),
                pan: 0.25
            }),
            Cmd::SetLevel {
                mix: mix("member1"),
                source: input("keys"),
                gain_db: None,
                pan: Some(-0.5),
                muted: None
            }
        );
        assert_eq!(
            one(ClientMsg::SetGlobalLevel { level_db: -3.0 }),
            Cmd::SetMix {
                mix: mix("member1"),
                volume_db: Some(-3.0),
                muted: None
            }
        );
        assert_eq!(
            one(ClientMsg::SetGlobalMute { muted: true }),
            Cmd::SetMix {
                mix: mix("member1"),
                volume_db: None,
                muted: Some(true)
            }
        );
        assert_eq!(
            one(ClientMsg::SetStemsLevel { level_db: 1.0 }),
            Cmd::SetGroup {
                mix: mix("member1"),
                group: GroupId::new("stems"),
                gain_db: Some(1.0),
                muted: None
            }
        );
        assert_eq!(
            one(ClientMsg::SetStemsMute { muted: false }),
            Cmd::SetGroup {
                mix: mix("member1"),
                group: GroupId::new("stems"),
                gain_db: None,
                muted: Some(false)
            }
        );
        assert_eq!(
            one(ClientMsg::SetSolo {
                soloed: vec!["mic1".into(), "member3".into()]
            }),
            Cmd::SetSolo {
                mix: mix("member1"),
                sources: vec![input("mic1"), Source::Mix(mix("member3"))]
            }
        );
        assert_eq!(
            one(ClientMsg::SetLimiterParam {
                param: "limit".into(),
                value: 0.5
            }),
            Cmd::SetLimiter {
                mix: mix("member1"),
                enabled: None,
                limit_db: Some(-3.0)
            }
        );
        assert_eq!(
            one(ClientMsg::SetLimiterEnabled { enabled: false }),
            Cmd::SetLimiter {
                mix: mix("member1"),
                enabled: Some(false),
                limit_db: None
            }
        );
        assert_eq!(
            one(ClientMsg::ResetLimiterActivity),
            Cmd::ResetLimiterStats {
                mix: mix("member1")
            }
        );
        let Cmd::SetEq { target, eq } = one(ClientMsg::SetEqBand {
            target: "mic1".into(),
            band: 2,
            param: "gain_db".into(),
            value: 4.5,
        }) else {
            panic!("SetEq");
        };
        assert_eq!(target, EqTarget::Input(InputId::new("mic1")));
        assert_eq!(eq.bands[2].gain_db, 4.5);
    }

    #[test]
    fn bad_values_unknown_ids_and_foreign_targets_are_refused() {
        let v = test_view();
        let m = Mirror::default();
        let p2 = page(&v, "member2");
        let me = member("member2");
        let err = |msg: ClientMsg| command(&v, &m, &p2, &me, &msg).unwrap_err();
        assert_eq!(
            err(ClientMsg::SetLevel {
                id: "member3".into(),
                level_db: 0.0
            }),
            ViewError::UnknownId("member3".into()),
            "member2 hears no other mix"
        );
        assert_eq!(
            err(ClientMsg::SetMute {
                id: "stems".into(),
                muted: true
            }),
            ViewError::UnknownId("stems".into())
        );
        assert!(matches!(
            err(ClientMsg::SetLevel {
                id: "mic1".into(),
                level_db: f32::NAN
            }),
            ViewError::BadValue(_)
        ));
        assert!(matches!(
            err(ClientMsg::SetPan {
                id: "mic1".into(),
                pan: -0.5
            }),
            ViewError::BadValue(_)
        ));
        assert!(matches!(
            err(ClientMsg::SetGlobalLevel {
                level_db: f32::INFINITY
            }),
            ViewError::BadValue(_)
        ));
        assert!(matches!(
            err(ClientMsg::SetSolo {
                soloed: vec!["mic1".into(), "nope".into()]
            }),
            ViewError::UnknownId(_)
        ));
        assert!(matches!(
            err(ClientMsg::SetLimiterParam {
                param: "release".into(),
                value: 0.5
            }),
            ViewError::BadValue(_)
        ));
        assert!(matches!(
            err(ClientMsg::SetLimiterParam {
                param: "limit".into(),
                value: 1.5
            }),
            ViewError::BadValue(_)
        ));
        assert_eq!(
            err(ClientMsg::SetEqBand {
                target: "mic1".into(),
                band: 0,
                param: "gain_db".into(),
                value: 1.0
            }),
            ViewError::Forbidden("an input's EQ is its owner's or the engineer's")
        );
        assert_eq!(
            err(ClientMsg::SetInput {
                input: "mic2".into(),
                trim_db: Some(1.0),
                muted: None,
                processing: None
            }),
            ViewError::Forbidden("input controls are the engineer's")
        );
        assert_eq!(
            err(ClientMsg::ResetLimiterStats {
                mix: "member2".into()
            }),
            ViewError::Forbidden("another mix's limiter is the engineer's")
        );
        assert_eq!(err(ClientMsg::GetConsole), ViewError::NotACommand);
        assert_eq!(err(ClientMsg::TalkStart), ViewError::NotACommand);
        let mut nogroup = v.clone();
        nogroup.group = None;
        assert_eq!(
            command(
                &nogroup,
                &m,
                &p2,
                &me,
                &ClientMsg::SetStemsMute { muted: true }
            ),
            Err(ViewError::NoGroup)
        );
    }

    #[test]
    fn engineer_only_commands_work_for_the_engineer() {
        let v = test_view();
        let m = Mirror::default();
        let p = page(&v, "engineer");
        let e = engineer();
        assert_eq!(
            command(
                &v,
                &m,
                &p,
                &e,
                &ClientMsg::SetInput {
                    input: "keys".into(),
                    trim_db: Some(-3.0),
                    muted: Some(true),
                    processing: Some(false)
                }
            ),
            Ok(vec![Cmd::SetInput {
                input: InputId::new("keys"),
                trim_db: Some(-3.0),
                muted: Some(true),
                processing: Some(false)
            }])
        );
        assert!(matches!(
            command(
                &v,
                &m,
                &p,
                &e,
                &ClientMsg::SetInput {
                    input: "keys".into(),
                    trim_db: Some(f32::NAN),
                    muted: None,
                    processing: None
                }
            ),
            Err(ViewError::BadValue(_))
        ));
        assert_eq!(
            command(
                &v,
                &m,
                &p,
                &e,
                &ClientMsg::SetInput {
                    input: "nope".into(),
                    trim_db: None,
                    muted: None,
                    processing: None
                }
            ),
            Err(ViewError::UnknownId("nope".into()))
        );
        assert_eq!(
            command(
                &v,
                &m,
                &p,
                &e,
                &ClientMsg::ResetLimiterStats {
                    mix: "translator".into()
                }
            ),
            Ok(vec![Cmd::ResetLimiterStats {
                mix: mix("translator")
            }])
        );
        assert_eq!(
            command(
                &v,
                &m,
                &p,
                &e,
                &ClientMsg::ResetLimiterStats { mix: "x".into() }
            ),
            Err(ViewError::UnknownId("x".into()))
        );
    }

    #[test]
    fn eq_targets_follow_x7() {
        let v = test_view();
        let p1 = page(&v, "member1");
        let me = member("member1");
        assert_eq!(
            eq_target(&v, &p1, &me, "member1"),
            Ok((EqTarget::Mix(mix("member1")), "IEM VOL".into()))
        );
        assert_eq!(
            eq_target(&v, &p1, &me, "stems"),
            Ok((
                EqTarget::Group {
                    mix: mix("member1"),
                    group: GroupId::new("stems")
                },
                "STEMS".into()
            ))
        );
        assert_eq!(
            eq_target(&v, &p1, &me, "mic1"),
            Ok((EqTarget::Input(InputId::new("mic1")), "MEMBER1 mic".into()))
        );
        assert!(matches!(
            eq_target(&v, &p1, &me, "mic2"),
            Err(ViewError::Forbidden(_))
        ));
        assert!(matches!(
            eq_target(&v, &p1, &me, "member2"),
            Err(ViewError::Forbidden(_))
        ));
        assert_eq!(
            eq_target(&v, &p1, &engineer(), "member2"),
            Ok((EqTarget::Mix(mix("member2")), "Member2".into()))
        );
        assert_eq!(
            eq_target(&v, &p1, &engineer(), "mic2").map(|t| t.0),
            Ok(EqTarget::Input(InputId::new("mic2")))
        );
        assert_eq!(
            eq_target(&v, &p1, &me, "engineer"),
            Err(ViewError::UnknownId("engineer".into()))
        );
    }

    #[test]
    fn eq_edits_compose_and_map_to_the_ui_bands() {
        let eq = Eq::default();
        let a = apply_band(&eq, 3, "freq_hz", 2500.0).unwrap();
        let b = apply_band(&a, 3, "gain_db", -4.0).unwrap();
        let c = apply_band(&b, 3, "bw_oct", 0.5).unwrap();
        let d = apply_band(&c, 3, "enabled", 1.0).unwrap();
        assert_eq!(
            (
                d.bands[3].freq_hz,
                d.bands[3].gain_db,
                d.bands[3].bw_oct,
                d.bands[3].enabled
            ),
            (2500.0, -4.0, 0.5, true)
        );
        assert!(!apply_band(&d, 3, "enabled", 0.49).unwrap().bands[3].enabled);
        assert_eq!(d.bands[2], eq.bands[2]);
        assert!(apply_band(&eq, 5, "gain_db", 0.0).is_err());
        assert!(apply_band(&eq, 0, "gain", 0.25).is_err());
        assert!(apply_band(&eq, 0, "gain_db", f32::NAN).is_err());
        let ui = eq_bands(&d);
        let kinds: Vec<&str> = ui.iter().map(|b| b.band_type.as_str()).collect();
        assert_eq!(kinds, ["highpass", "lowshelf", "band", "band", "highshelf"]);
        assert_eq!(
            (ui[3].freq_hz, ui[3].gain_db, ui[3].bw, ui[3].enabled),
            (2500.0, -4.0, 0.5, true)
        );
        let mut off = eq;
        off.bands[1].gain_db = -1000.0;
        assert_eq!(eq_bands(&off)[1].gain_db, -150.0);
    }

    #[test]
    fn eq_and_limiter_messages_read_the_mirror() {
        let v = test_view();
        let m = mirror_with(|s, _| {
            let mx = s.mixes.entry(mix("member1")).or_default();
            mx.out.limiter = Limiter {
                enabled: false,
                limit_db: -1.5,
            };
            mx.out.eq.bands[0].enabled = true;
            s.inputs.entry(InputId::new("mic1")).or_default().eq.bands[4].gain_db = 2.0;
            mx.groups.entry(GroupId::new("stems")).or_default().eq.bands[1].freq_hz = 150.0;
        });
        let p = page(&v, "member1");
        let me = member("member1");
        let ServerMsg::EqParams {
            target,
            track_name,
            bands,
        } = eq_params_msg(&v, &m, &p, &me, "mic1").unwrap()
        else {
            panic!("EqParams");
        };
        assert_eq!(
            (target.as_str(), track_name.as_str()),
            ("mic1", "MEMBER1 mic")
        );
        assert_eq!(bands[4].gain_db, 2.0);
        let ServerMsg::EqParams { bands, .. } = eq_params_msg(&v, &m, &p, &me, "member1").unwrap()
        else {
            panic!("EqParams");
        };
        assert!(bands[0].enabled);
        let ServerMsg::EqParams { bands, .. } = eq_params_msg(&v, &m, &p, &me, "stems").unwrap()
        else {
            panic!("EqParams");
        };
        assert_eq!(bands[1].freq_hz, 150.0);
        assert!(eq_params_msg(&v, &m, &p, &me, "mic9").is_err());
        assert_eq!(
            limiter_msg(&m, &p, 12.5),
            ServerMsg::LimiterParams {
                mix: "member1".into(),
                track_name: "IEM VOL".into(),
                limit_db: -1.5,
                limit_norm: 0.75,
                enabled: false,
                active_seconds: 12.5
            }
        );
        // The current EQ of a group and a heard mix come from their entities.
        assert_eq!(
            current_eq(
                &m,
                &EqTarget::Group {
                    mix: mix("member1"),
                    group: GroupId::new("stems")
                }
            )
            .bands[1]
                .freq_hz,
            150.0
        );
        assert_eq!(
            current_eq(&m, &EqTarget::Mix(mix("member2"))),
            Eq::default()
        );
    }

    #[test]
    fn mute_all_mutes_every_level_of_the_page_mix() {
        let v = test_view();
        let Cmd::Batch { ops } = mute_all(&v, &page(&v, "engineer")) else {
            panic!("a batch");
        };
        assert_eq!(ops.len(), 24 + 9);
        assert!(ops.iter().all(|c| matches!(
            c,
            Cmd::SetLevel { mix, muted: Some(true), gain_db: None, pan: None, .. } if mix.0 == "engineer"
        )));
        assert!(ops.contains(&Cmd::SetLevel {
            mix: mix("engineer"),
            source: Source::Mix(mix("member1")),
            gain_db: None,
            pan: None,
            muted: Some(true)
        }));
        let Cmd::Batch { ops } = mute_all(&v, &page(&v, "member5")) else {
            panic!("a batch");
        };
        assert_eq!(ops.len(), 24);
    }
}
