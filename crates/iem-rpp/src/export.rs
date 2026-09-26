//! State → a new project for the rollback (S4 design note §3.3, program spec
//! §4.3, #20 design note §7). The original project is the template: only mix
//! values are patched, and a value equal to the template's keeps the
//! template's text, so an unchanged state exports byte-identical. Values the
//! project cannot hold (a level without a receive, a mono mix's EQ or
//! limiter, a group strip without an instance) are not carried back: they
//! are reported (iemmixer-only changes, §4.3).

use iem_engine_proto::{
    DB_OFF, Eq as EqSettings, Mix, MixGroup, MixId, MixOut, MixState, Source, db_to_lin,
};

use crate::aliases::Aliases;
use crate::import::{
    Imported, Place, Problems, close, compare, db_close, import, project, rea_kind,
};
use crate::legacy::{LegacyProject, Plugin, PluginKind, Track};
use crate::read::Doc;
use crate::reaeq::{Band, EqBlob, ReaEq};
use crate::rpp::{RppError, num};

/// dB values closer than this are "unchanged" (the template's text is kept).
pub const SAME_DB: f64 = 1e-9;

struct Patch<'a> {
    doc: Doc,
    p: &'a LegacyProject,
    problems: Vec<String>,
}

impl<'a> Patch<'a> {
    fn token(&mut self, line: usize, k: usize, text: Result<String, RppError>) {
        let done = text.and_then(|t| self.doc.set_token(line, k, &t));
        if let Err(e) = done {
            self.problems.push(e.to_string());
        }
    }

    /// A linear gain field holding `new` dB where the template holds `old`.
    fn gain(&mut self, line: Option<usize>, k: usize, new: f64, old: f64) {
        if let Some(line) = line
            && !db_close(new, old, SAME_DB)
        {
            self.token(line, k, num(db_to_lin(new)));
        }
    }

    /// A plain number field (pan, dB slider).
    fn value(&mut self, line: Option<usize>, k: usize, new: f64, old: f64, db: bool) {
        let same = if db {
            db_close(new, old, SAME_DB)
        } else {
            close(new, old)
        };
        let v = if db { new.max(DB_OFF) } else { new };
        if let Some(line) = line
            && !same
        {
            self.token(line, k, num(v));
        }
    }

    fn flag(&mut self, line: Option<usize>, k: usize, new: bool, old: bool) {
        if let Some(line) = line
            && new != old
        {
            self.token(line, k, Ok(u8::from(new).to_string()));
        }
    }

    fn eq(&mut self, x: &Plugin, new: &EqSettings, old: &EqSettings) {
        let p = self.p;
        let lines = x.block.direct();
        let body: Vec<&str> = lines.iter().map(|i| p.doc.content(*i)).collect();
        let blob = match EqBlob::decode(&body) {
            Ok(b) => b,
            Err(e) => {
                self.problems.push(e.to_string());
                return;
            }
        };
        let mut eq: ReaEq = blob.eq.clone();
        if !db_close(new.gain_db, old.gain_db, SAME_DB) {
            eq.global_gain = db_to_lin(new.gain_db);
        }
        for ((band, n), o) in eq.bands.iter_mut().zip(&new.bands).zip(&old.bands) {
            let kind = rea_kind(n.kind);
            *band = Band {
                kind: if n.kind == o.kind { band.kind } else { kind },
                enabled: n.enabled,
                freq_hz: if close(n.freq_hz, o.freq_hz) {
                    band.freq_hz
                } else {
                    n.freq_hz
                },
                gain_lin: if db_close(n.gain_db, o.gain_db, SAME_DB) {
                    band.gain_lin
                } else {
                    db_to_lin(n.gain_db)
                },
                bw_oct: if close(n.bw_oct, o.bw_oct) {
                    band.bw_oct
                } else {
                    n.bw_oct
                },
            };
        }
        match blob.lines_for(&eq) {
            Ok(text) => {
                for (i, t) in lines.iter().zip(&text) {
                    if let Err(e) = self.doc.set_content(*i, t) {
                        self.problems.push(e.to_string());
                    }
                }
            }
            Err(e) => self.problems.push(e.to_string()),
        }
    }

    fn plugin(&self, t: usize, kind: PluginKind) -> Option<&'a Plugin> {
        let p: &'a LegacyProject = self.p;
        p.tracks.get(t)?.plugins.iter().find(|x| x.kind == kind)
    }
}

/// A written project and the values it could not hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exported {
    pub text: String,
    /// Values of the state that are not carried back, one line each.
    pub dropped: Vec<String>,
}

/// The state's values the template cannot hold and that differ from what
/// its import reads there.
fn dropped(template: &Imported, state: &MixState) -> Vec<String> {
    let held = project(&template.topology, &template.routing, state);
    let mut out = Vec::new();
    for (id, mix) in &state.mixes {
        let Some(kept) = held.mixes.get(id) else {
            continue;
        };
        if kept.out != mix.out {
            out.push(format!(
                "mix {id}: EQ and limiter (a mono mix holds only volume and mute)"
            ));
        }
        let sources = mix
            .inputs
            .iter()
            .map(|(i, l)| (Source::Input(i.clone()), l))
            .chain(mix.mixes.iter().map(|(h, l)| (Source::Mix(h.clone()), l)));
        for (src, l) in sources {
            if kept.level(&src).is_none() && l.gain_db > DB_OFF && !l.muted {
                out.push(format!(
                    "mix {id} level {src}: {:.2} dB (no receive in the project)",
                    l.gain_db
                ));
            }
        }
        for (g, strip) in &mix.groups {
            if !kept.groups.contains_key(g) && *strip != MixGroup::default() {
                out.push(format!("mix {id} group {g}: no instance in the project"));
            }
        }
    }
    out
}

fn mix_of<'s>(state: &'s MixState, id: &MixId, problems: &mut Vec<String>) -> Option<&'s Mix> {
    let m = state.mixes.get(id);
    if m.is_none() {
        problems.push(format!("the state has no mix {id}"));
    }
    m
}

/// Writes `state` into the template `p` (see the module docs).
pub fn export(
    p: &LegacyProject,
    aliases: &Aliases,
    state: &MixState,
) -> Result<Exported, Problems> {
    let template = import(p, aliases)?;
    let old = &template.state;
    let mut x = Patch {
        doc: p.doc.clone(),
        p,
        problems: Vec::new(),
    };
    for (ti, (t, (_, place))) in p.tracks.iter().zip(&template.tracks).enumerate() {
        match place {
            Place::Input(id) => {
                let (Some(new), Some(was)) = (state.inputs.get(id), old.inputs.get(id)) else {
                    x.problems.push(format!("the state has no input {id}"));
                    continue;
                };
                x.flag(t.mutesolo, 1, new.muted, was.muted);
                x.flag(t.fx, 1, new.processing, was.processing);
                if let Some(trim) = x.plugin(ti, PluginKind::Trim) {
                    let line = trim.block.head + 1;
                    x.value(Some(line), 0, new.trim_db, was.trim_db, true);
                }
                if let Some(eq) = x.plugin(ti, PluginKind::ReaEq) {
                    x.eq(eq, &new.eq, &was.eq);
                }
            }
            Place::Mix(id) => {
                let (Some(new), Some(was)) =
                    (mix_of(state, id, &mut x.problems), old.mixes.get(id))
                else {
                    continue;
                };
                let (n, w): (&MixOut, &MixOut) = (&new.out, &was.out);
                x.gain(t.volpan, 1, n.volume_db, w.volume_db);
                x.flag(t.mutesolo, 1, n.muted, w.muted);
                if let Some(eq) = x.plugin(ti, PluginKind::ReaEq) {
                    x.eq(eq, &n.eq, &w.eq);
                }
                if let Some(lim) = x.plugin(ti, PluginKind::Limiter) {
                    let line = Some(lim.block.head + 1);
                    x.value(line, 0, n.limiter.limit_db, w.limiter.limit_db, true);
                    x.value(line, 3, n.limiter.limit_db, w.limiter.limit_db, true);
                    x.flag(lim.bypass_line, 1, !n.limiter.enabled, !w.limiter.enabled);
                }
                x.receives(t, &template, new, was, id);
            }
            Place::Group { group, mix } => {
                let (Some(new), Some(was)) =
                    (mix_of(state, mix, &mut x.problems), old.mixes.get(mix))
                else {
                    continue;
                };
                let (Some(n), Some(w)) = (new.groups.get(group), was.groups.get(group)) else {
                    x.problems
                        .push(format!("the state has no group strip {group} in {mix}"));
                    continue;
                };
                x.gain(t.volpan, 1, n.gain_db, w.gain_db);
                x.flag(t.mutesolo, 1, n.muted, w.muted);
                if let Some(eq) = x.plugin(ti, PluginKind::ReaEq) {
                    x.eq(eq, &n.eq, &w.eq);
                }
                x.receives(t, &template, new, was, mix);
            }
        }
    }
    if x.problems.is_empty() {
        Ok(Exported {
            text: x.doc.render(),
            dropped: dropped(&template, state),
        })
    } else {
        Err(Problems(x.problems))
    }
}

impl Patch<'_> {
    /// The levels a track's receives hold (into `mix`); a group strip's link
    /// into its mix is kept as it is.
    fn receives(&mut self, t: &Track, template: &Imported, new: &Mix, was: &Mix, mix: &MixId) {
        for rc in &t.receives {
            let src = match template.tracks.get(rc.src).map(|(_, p)| p) {
                Some(Place::Input(i)) => Source::Input(i.clone()),
                Some(Place::Mix(m)) => Source::Mix(m.clone()),
                Some(Place::Group { .. }) | None => continue,
            };
            let (Some(n), Some(w)) = (new.level(&src), was.level(&src)) else {
                self.problems
                    .push(format!("the state has no level {src} in {mix}"));
                continue;
            };
            self.gain(Some(rc.line), 3, n.gain_db, w.gain_db);
            self.value(Some(rc.line), 4, n.pan, w.pan, false);
            self.flag(Some(rc.line), 5, n.muted, w.muted);
        }
    }
}

/// [`export`] of a template text, checked before it is returned: the result
/// re-imports to the same topology and routing, and to `state` within 1e-9 dB
/// wherever the project holds it.
pub fn export_checked(
    template: &str,
    aliases: &Aliases,
    state: &MixState,
) -> Result<Exported, Problems> {
    let p = LegacyProject::parse(template)?;
    let before = import(&p, aliases)?;
    let out = export(&p, aliases, state)?;
    let back = import(&LegacyProject::parse(&out.text)?, aliases)?;
    let problems = self_check(&before, &back, state);
    if problems.is_empty() {
        Ok(out)
    } else {
        Err(Problems(problems))
    }
}

/// What [`export_checked`] refuses: `back` (the export, re-imported) must
/// hold `before`'s topology and routing, and `state` within [`SAME_DB`]
/// wherever the project holds it.
fn self_check(before: &Imported, back: &Imported, state: &MixState) -> Vec<String> {
    let mut problems: Vec<String> = Vec::new();
    if back.topology != before.topology || back.routing != before.routing {
        problems.push("self-check: the export changed the topology".into());
    }
    problems.extend(
        compare(
            &before.topology,
            &before.routing,
            &back.state,
            state,
            SAME_DB,
        )
        .into_iter()
        .map(|d| format!("self-check: {d}")),
    );
    problems
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use iem_engine_proto::{BandKind, GroupId, InputId, InputState, Level};

    use super::*;
    use crate::aliases::parse_aliases;
    use crate::sitegen::{
        aliases_toml, project as write, sample_state, synthetic_routing, synthetic_site, track_name,
    };

    fn setup(seed: u64) -> (String, Aliases, MixState) {
        let topo = synthetic_site();
        let routing = synthetic_routing(&topo);
        let state = sample_state(&topo, &routing, seed);
        let text = write(&topo, &routing, &state, &track_name).unwrap();
        let aliases = parse_aliases(&aliases_toml(&topo, &routing, &BTreeMap::new())).unwrap();
        let imported = import(&LegacyProject::parse(&text).unwrap(), &aliases)
            .unwrap()
            .state;
        (text, aliases, imported)
    }

    fn changed_lines(a: &str, b: &str) -> usize {
        a.lines().zip(b.lines()).filter(|(x, y)| x != y).count()
    }

    fn input<'s>(s: &'s mut MixState, id: &str) -> &'s mut InputState {
        s.inputs.get_mut(&InputId::new(id)).unwrap()
    }

    fn mix<'s>(s: &'s mut MixState, id: &str) -> &'s mut Mix {
        s.mixes.get_mut(&MixId::new(id)).unwrap()
    }

    fn stems<'s>(s: &'s mut MixState, id: &str) -> &'s mut MixGroup {
        mix(s, id).groups.get_mut(&GroupId::new("stems")).unwrap()
    }

    #[test]
    fn unchanged_state_exports_byte_identical() {
        let (text, aliases, state) = setup(21);
        let p = LegacyProject::parse(&text).unwrap();
        let out = export(&p, &aliases, &state).unwrap();
        assert_eq!(out.text, text);
        assert!(out.dropped.is_empty(), "{:?}", out.dropped);
        assert_eq!(export_checked(&text, &aliases, &state).unwrap().text, text);
        let crlf = text.replace('\n', "\r\n");
        assert_eq!(export_checked(&crlf, &aliases, &state).unwrap().text, crlf);
    }

    #[test]
    fn edited_state_reimports_within_1e_9_db() {
        let (text, aliases, _) = setup(21);
        let topo = synthetic_site();
        let routing = synthetic_routing(&topo);
        let other = sample_state(&topo, &routing, 99);
        let out = export_checked(&text, &aliases, &other).unwrap().text;
        assert_ne!(out, text);
        assert_eq!(out.lines().count(), text.lines().count());
        let back = import(&LegacyProject::parse(&out).unwrap(), &aliases).unwrap();
        assert_eq!(
            compare(&topo, &routing, &back.state, &other, 1e-9),
            Vec::<String>::new()
        );
    }

    #[test]
    fn one_edit_changes_one_line() {
        let (text, aliases, state) = setup(22);
        let edits: [fn(&mut MixState); 16] = [
            |s: &mut MixState| {
                let l = mix(s, "member1")
                    .inputs
                    .get_mut(&InputId::new("mic2"))
                    .unwrap();
                l.gain_db = if l.gain_db > -100.0 {
                    l.gain_db + 1.5
                } else {
                    -3.0
                };
            },
            |s: &mut MixState| {
                let p = &mut mix(s, "member1")
                    .inputs
                    .get_mut(&InputId::new("mic2"))
                    .unwrap()
                    .pan;
                *p = if *p > 0.0 { -0.5 } else { 0.5 };
            },
            |s: &mut MixState| {
                mix(s, "member1")
                    .inputs
                    .get_mut(&InputId::new("mic2"))
                    .unwrap()
                    .muted ^= true;
            },
            |s: &mut MixState| input(s, "mic1").trim_db += 2.0,
            |s: &mut MixState| input(s, "mic2").processing ^= true,
            |s: &mut MixState| input(s, "mic3").muted ^= true,
            |s: &mut MixState| input(s, "mic4").eq.bands[0].freq_hz = 99.5,
            |s: &mut MixState| input(s, "mic4").eq.gain_db = 3.25,
            |s: &mut MixState| mix(s, "member1").out.limiter.limit_db = -1.5,
            |s: &mut MixState| mix(s, "member2").out.limiter.enabled ^= true,
            |s: &mut MixState| mix(s, "member3").out.volume_db = 6.0,
            |s: &mut MixState| stems(s, "member3").eq.bands[4].enabled ^= true,
            |s: &mut MixState| stems(s, "member4").gain_db = -3.0,
            |s: &mut MixState| {
                let l = mix(s, "member1")
                    .mixes
                    .get_mut(&MixId::new("member2"))
                    .unwrap();
                l.gain_db = if l.gain_db > -100.0 {
                    l.gain_db + 1.0
                } else {
                    -2.0
                };
            },
            |s: &mut MixState| mix(s, "translator").out.volume_db = -3.0,
            |s: &mut MixState| {
                mix(s, "member5")
                    .inputs
                    .get_mut(&InputId::new("drums"))
                    .unwrap()
                    .gain_db = -7.0;
            },
        ];
        for (i, edit) in edits.iter().enumerate() {
            let mut s = state.clone();
            edit(&mut s);
            assert_ne!(s, state, "edit {i} changes the state");
            let out = export_checked(&text, &aliases, &s)
                .unwrap_or_else(|e| panic!("edit {i}: {e}"))
                .text;
            let n = changed_lines(&text, &out);
            // A limiter ceiling is two sliders on one line; an EQ value is one
            // base64 line (unless it straddles two).
            assert!((1..=2).contains(&n), "edit {i}: {n} lines changed");
        }
    }

    #[test]
    fn values_a_project_cannot_hold_are_reported_not_written() {
        let (text, aliases, state) = setup(23);
        let mut s = state;
        let tr = mix(&mut s, "translator");
        tr.out.eq.gain_db = 9.0;
        tr.out.limiter.enabled = true;
        tr.inputs.insert(
            InputId::new("mic1"),
            Level {
                gain_db: -6.0,
                ..Level::default()
            },
        );
        // Off or muted levels change nothing audible: not reported.
        tr.inputs.insert(InputId::new("mic2"), Level::default());
        tr.inputs.insert(
            InputId::new("mic3"),
            Level {
                gain_db: 0.0,
                muted: true,
                ..Level::default()
            },
        );
        tr.groups.insert(
            GroupId::new("stems"),
            MixGroup {
                gain_db: -3.0,
                ..MixGroup::default()
            },
        );
        let out = export_checked(&text, &aliases, &s).unwrap();
        assert_eq!(out.text, text);
        assert_eq!(
            out.dropped,
            vec![
                "mix translator: EQ and limiter (a mono mix holds only volume and mute)".to_owned(),
                "mix translator level mic1: -6.00 dB (no receive in the project)".to_owned(),
                "mix translator group stems: no instance in the project".to_owned(),
            ]
        );
    }

    #[test]
    fn missing_state_entries_fail() {
        let (text, aliases, state) = setup(24);
        let p = LegacyProject::parse(&text).unwrap();
        let mut s = state.clone();
        s.inputs.remove(&InputId::new("mic1"));
        s.mixes.remove(&MixId::new("member1"));
        mix(&mut s, "member2").inputs.remove(&InputId::new("mic1"));
        mix(&mut s, "member3").groups.remove(&GroupId::new("stems"));
        mix(&mut s, "member4").inputs.remove(&InputId::new("drums"));
        let err = export(&p, &aliases, &s).unwrap_err().0;
        for want in [
            "the state has no input mic1",
            "the state has no mix member1",
            "the state has no level mic1 in member2",
            "the state has no group strip stems in member3",
            "the state has no level drums in member4",
        ] {
            assert!(err.contains(&want.to_owned()), "{want:?} not in {err:#?}");
        }
    }

    #[test]
    fn the_master_is_left_as_it_is() {
        let (text, aliases, _) = setup(25);
        let topo = synthetic_site();
        let other = sample_state(&topo, &synthetic_routing(&topo), 98);
        let out = export_checked(&text, &aliases, &other).unwrap().text;
        let master = |t: &str| -> Vec<String> {
            t.lines()
                .filter(|l| l.trim_start().starts_with("MASTER"))
                .map(str::to_owned)
                .collect()
        };
        assert_eq!(master(&out), master(&text));
        assert!(out.contains("\n  MASTERMUTESOLO 1\n"));
    }

    #[test]
    fn the_self_check_catches_a_value_that_was_not_written() {
        let (text, aliases, state) = setup(26);
        // A limiter without its BYPASS line: disabling it cannot be written.
        let at = text
            .find(&format!("NAME \"{}\"", track_name("member1")))
            .unwrap();
        let lim = at + text[at..].find("<JS loser/MGA_JSLimiterST").unwrap();
        let bypass = text[..lim].rfind("BYPASS").unwrap();
        let line_start = text[..bypass].rfind('\n').unwrap() + 1;
        let line_end = bypass + text[bypass..].find('\n').unwrap() + 1;
        let stripped = format!("{}{}", &text[..line_start], &text[line_end..]);
        let mut s = state;
        mix(&mut s, "member1").out.limiter.enabled = true;
        assert!(export_checked(&stripped, &aliases, &s).is_ok());
        mix(&mut s, "member1").out.limiter.enabled = false;
        let err = export_checked(&stripped, &aliases, &s).unwrap_err().0;
        assert!(
            err.iter()
                .any(|e| e.starts_with("self-check: mix member1: limiter")),
            "{err:#?}"
        );
    }

    #[test]
    fn the_self_check_refuses_a_changed_topology_or_routing() {
        let (text, aliases, state) = setup(21);
        let before = import(&LegacyProject::parse(&text).unwrap(), &aliases).unwrap();
        assert_eq!(self_check(&before, &before, &state), Vec::<String>::new());
        let changed = vec!["self-check: the export changed the topology".to_owned()];
        // The routing alone…
        let mut back = before.clone();
        back.routing.levels.pop_first().unwrap();
        assert_eq!(self_check(&before, &back, &state), changed);
        // …or the topology alone.
        let mut back = before.clone();
        back.topology.inputs[0].talkback ^= true;
        assert_eq!(self_check(&before, &back, &state), changed);
    }

    #[test]
    fn a_changed_band_kind_is_written() {
        let (text, aliases, state) = setup(22);
        let mic4 = InputId::new("mic4");
        let mut s = state;
        let band = &mut s.inputs.get_mut(&mic4).unwrap().eq.bands[1];
        assert_eq!(band.kind, BandKind::LowShelf);
        band.kind = BandKind::Peak;
        let out = export_checked(&text, &aliases, &s).unwrap().text;
        let back = import(&LegacyProject::parse(&out).unwrap(), &aliases).unwrap();
        assert_eq!(back.state.inputs[&mic4].eq.bands[1].kind, BandKind::Peak);
    }
}
