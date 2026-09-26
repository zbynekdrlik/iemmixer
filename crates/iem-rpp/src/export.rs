//! State → a new project for the rollback (S4 design note §3.3, program spec
//! §4.3). The original project is the template: only mix values are patched,
//! and a value equal to the template's keeps the template's text, so an
//! unchanged state exports byte-identical.

use iem_engine_proto::{BusKind, DB_OFF, Eq as EqSettings, MixState, SendId, Source, db_to_lin};

use crate::aliases::Aliases;
use crate::import::{Problems, TrackRef, close, compare, db_close, import, rea_kind};
use crate::legacy::{LegacyProject, Plugin, PluginKind};
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

/// Writes `state` into the template `p` (see the module docs).
pub fn export(p: &LegacyProject, aliases: &Aliases, state: &MixState) -> Result<String, Problems> {
    let template = import(p, aliases)?;
    let old = &template.state;
    let mut x = Patch {
        doc: p.doc.clone(),
        p,
        problems: Vec::new(),
    };
    for (ti, (t, r)) in p.tracks.iter().zip(&template.tracks).enumerate() {
        match r {
            TrackRef::Input(id) => {
                let (Some(new), Some(was)) = (state.inputs.get(id), old.inputs.get(id)) else {
                    x.problems.push(format!("the state has no input {id}"));
                    continue;
                };
                x.gain(t.volpan, 1, new.fader_db, was.fader_db);
                x.value(t.volpan, 2, new.pan, was.pan, false);
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
            TrackRef::Bus(id) => {
                let (Some(new), Some(was)) = (state.buses.get(id), old.buses.get(id)) else {
                    x.problems.push(format!("the state has no bus {id}"));
                    continue;
                };
                x.gain(t.volpan, 1, new.fader_db, was.fader_db);
                x.value(t.volpan, 2, new.pan, was.pan, false);
                x.flag(t.mutesolo, 1, new.muted, was.muted);
                let kind = template.topology.bus(id).map(|b| b.kind);
                if matches!(kind, Some(BusKind::Output | BusKind::Stems))
                    && let Some(eq) = x.plugin(ti, PluginKind::ReaEq)
                {
                    x.eq(eq, &new.eq, &was.eq);
                }
                if kind == Some(BusKind::Output)
                    && let Some(lim) = x.plugin(ti, PluginKind::Limiter)
                {
                    let line = Some(lim.block.head + 1);
                    x.value(line, 0, new.limiter.limit_db, was.limiter.limit_db, true);
                    x.value(line, 3, new.limiter.limit_db, was.limiter.limit_db, true);
                    x.flag(
                        lim.bypass_line,
                        1,
                        !new.limiter.enabled,
                        !was.limiter.enabled,
                    );
                }
                for rc in &t.receives {
                    let Some(src) = template.tracks.get(rc.src) else {
                        continue;
                    };
                    let id = SendId {
                        src: match src {
                            TrackRef::Input(i) => Source::Input(i.clone()),
                            TrackRef::Bus(b) => Source::Bus(b.clone()),
                        },
                        dst: id.clone(),
                    };
                    let find = |s: &MixState| s.sends.iter().find(|e| e.id == id).map(|e| e.state);
                    let (Some(new), Some(was)) = (find(state), find(old)) else {
                        x.problems.push(format!("the state has no send {id}"));
                        continue;
                    };
                    x.gain(Some(rc.line), 3, new.gain_db, was.gain_db);
                    x.value(Some(rc.line), 4, new.pan, was.pan, false);
                    x.flag(Some(rc.line), 5, new.muted, was.muted);
                }
            }
        }
    }
    let master = iem_engine_proto::BusId::new(aliases.master.clone());
    match (state.buses.get(&master), old.buses.get(&master)) {
        (Some(new), Some(was)) => {
            let m = &p.master;
            x.gain(Some(m.volume), 1, new.fader_db, was.fader_db);
            x.value(Some(m.volume), 2, new.pan, was.pan, false);
            if new.muted != was.muted
                && let Some(line) = m.mutesolo
            {
                let flags = (m.mute_flags & !1) | i64::from(new.muted);
                x.token(line, 1, Ok(flags.to_string()));
            }
        }
        _ => x.problems.push(format!("the state has no bus {master}")),
    }
    if x.problems.is_empty() {
        Ok(x.doc.render())
    } else {
        Err(Problems(x.problems))
    }
}

/// [`export`] of a template text, checked before it is returned: the result
/// re-imports to the same topology and to `state` within 1e-9 dB.
pub fn export_checked(
    template: &str,
    aliases: &Aliases,
    state: &MixState,
) -> Result<String, Problems> {
    let p = LegacyProject::parse(template)?;
    let before = import(&p, aliases)?;
    let text = export(&p, aliases, state)?;
    let back = import(&LegacyProject::parse(&text)?, aliases)?;
    let mut problems: Vec<String> = Vec::new();
    if back.topology != before.topology {
        problems.push("self-check: the export changed the topology".into());
    }
    problems.extend(
        compare(&before.topology, &back.state, state, SAME_DB)
            .into_iter()
            .map(|d| format!("self-check: {d}")),
    );
    if problems.is_empty() {
        Ok(text)
    } else {
        Err(Problems(problems))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use iem_engine_proto::{BusId, InputId};

    use super::*;
    use crate::aliases::parse_aliases;
    use crate::sitegen::{aliases_toml, project, sample_state, synthetic_site, track_name};

    fn setup(seed: u64) -> (String, Aliases, MixState) {
        let topo = synthetic_site();
        let state = sample_state(&topo, seed);
        let text = project(&topo, &state, &track_name).unwrap();
        let aliases = parse_aliases(&aliases_toml(&topo, &BTreeMap::new())).unwrap();
        let imported = import(&LegacyProject::parse(&text).unwrap(), &aliases)
            .unwrap()
            .state;
        (text, aliases, imported)
    }

    fn changed_lines(a: &str, b: &str) -> usize {
        a.lines().zip(b.lines()).filter(|(x, y)| x != y).count()
    }

    #[test]
    fn unchanged_state_exports_byte_identical() {
        let (text, aliases, state) = setup(21);
        let p = LegacyProject::parse(&text).unwrap();
        assert_eq!(export(&p, &aliases, &state).unwrap(), text);
        assert_eq!(export_checked(&text, &aliases, &state).unwrap(), text);
        let crlf = text.replace('\n', "\r\n");
        assert_eq!(export_checked(&crlf, &aliases, &state).unwrap(), crlf);
    }

    #[test]
    fn edited_state_reimports_within_1e_9_db() {
        let (text, aliases, _) = setup(21);
        let topo = synthetic_site();
        let other = sample_state(&topo, 99);
        let out = export_checked(&text, &aliases, &other).unwrap();
        assert_ne!(out, text);
        assert_eq!(out.lines().count(), text.lines().count());
        let back = import(&LegacyProject::parse(&out).unwrap(), &aliases).unwrap();
        assert_eq!(
            compare(&topo, &back.state, &other, 1e-9),
            Vec::<String>::new()
        );
    }

    #[test]
    fn one_edit_changes_one_line() {
        let (text, aliases, state) = setup(22);
        fn input(s: &mut MixState, id: &str) -> &mut iem_engine_proto::InputState {
            s.inputs.get_mut(&InputId::new(id)).unwrap()
        }
        fn bus(s: &mut MixState, id: &str) -> &mut iem_engine_proto::BusState {
            s.buses.get_mut(&BusId::new(id)).unwrap()
        }
        let edits: [fn(&mut MixState); 15] = [
            |s: &mut MixState| s.sends[5].state.gain_db += 1.5,
            |s: &mut MixState| {
                let p = &mut s.sends[5].state.pan;
                *p = if *p > 0.0 { -0.5 } else { 0.5 };
            },
            |s: &mut MixState| s.sends[5].state.muted ^= true,
            |s: &mut MixState| input(s, "mic1").trim_db += 2.0,
            |s: &mut MixState| input(s, "mic2").processing ^= true,
            |s: &mut MixState| input(s, "mic3").muted ^= true,
            |s: &mut MixState| input(s, "mic4").eq.bands[0].freq_hz = 99.5,
            |s: &mut MixState| input(s, "mic4").eq.gain_db = 3.25,
            |s: &mut MixState| bus(s, "member1").limiter.limit_db = -1.5,
            |s: &mut MixState| bus(s, "member2").limiter.enabled ^= true,
            |s: &mut MixState| bus(s, "member3").fader_db = 6.0,
            |s: &mut MixState| bus(s, "member3.stems").eq.bands[4].enabled ^= true,
            |s: &mut MixState| bus(s, "master").muted ^= true,
            |s: &mut MixState| bus(s, "master").pan = 0.25,
            |s: &mut MixState| bus(s, "translator").fader_db = -3.0,
        ];
        let want = [1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1];
        for (i, edit) in edits.iter().enumerate() {
            let mut s = state.clone();
            edit(&mut s);
            let out =
                export_checked(&text, &aliases, &s).unwrap_or_else(|e| panic!("edit {i}: {e}"));
            let n = changed_lines(&text, &out);
            // A limiter ceiling is two sliders on one line; an EQ value is one
            // base64 line (unless it straddles two).
            assert!(n >= want[i] && n <= 2, "edit {i}: {n} lines changed");
        }
    }

    #[test]
    fn values_a_project_cannot_hold_are_ignored() {
        let (text, aliases, state) = setup(23);
        let mut s = state;
        s.buses
            .get_mut(&BusId::new("translator"))
            .unwrap()
            .eq
            .gain_db = 9.0;
        s.buses
            .get_mut(&BusId::new("member1.stems"))
            .unwrap()
            .limiter
            .limit_db = -3.0;
        s.buses
            .get_mut(&BusId::new("master"))
            .unwrap()
            .limiter
            .enabled = false;
        assert_eq!(export_checked(&text, &aliases, &s).unwrap(), text);
    }

    #[test]
    fn missing_state_entries_fail() {
        let (text, aliases, state) = setup(24);
        let p = LegacyProject::parse(&text).unwrap();
        let mut s = state.clone();
        s.inputs.remove(&InputId::new("mic1"));
        s.buses.remove(&BusId::new("member1"));
        s.buses.remove(&BusId::new("master"));
        let first = s.sends.remove(0).id;
        let err = export(&p, &aliases, &s).unwrap_err().0;
        for want in [
            "the state has no input mic1".to_owned(),
            "the state has no bus member1".to_owned(),
            "the state has no bus master".to_owned(),
            format!("the state has no send {first}"),
        ] {
            assert!(err.contains(&want), "{want:?} not in {err:#?}");
        }
    }

    #[test]
    fn master_mute_keeps_the_other_flag_bits() {
        let (text, aliases, state) = setup(25);
        let master = BusId::new("master");
        let muted = state.buses[&master].muted;
        let with_solo = text.replacen(
            &format!("MASTERMUTESOLO {}", u8::from(muted)),
            &format!("MASTERMUTESOLO {}", 2 | u8::from(muted)),
            1,
        );
        let mut s = state;
        s.buses.get_mut(&master).unwrap().muted = !muted;
        let out = export_checked(&with_solo, &aliases, &s).unwrap();
        assert!(out.contains(&format!("\n  MASTERMUTESOLO {}\n", 2 | u8::from(!muted))));
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
        let lim_state = &mut s.buses.get_mut(&BusId::new("member1")).unwrap().limiter;
        lim_state.enabled = true;
        assert!(export_checked(&stripped, &aliases, &s).is_ok());
        s.buses
            .get_mut(&BusId::new("member1"))
            .unwrap()
            .limiter
            .enabled = false;
        let err = export_checked(&stripped, &aliases, &s).unwrap_err().0;
        assert!(
            err.iter()
                .any(|e| e.starts_with("self-check: bus member1: limiter")),
            "{err:#?}"
        );
    }
}
