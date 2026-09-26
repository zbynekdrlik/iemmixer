//! Cross-check of the predecessor's newest backup JSON (v1) against an
//! imported project (S4 design note §3.2, #20 design note §7). Names must be
//! the project's tracks; differing values are reported, never applied: the
//! project saved at switch time is newer.

use iem_core::{EqBandBackup, MixerBackup};
use iem_engine_proto::{Level, Source, db_to_lin};

use crate::import::{Imported, Place, Problems};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Check {
    /// Values compared.
    pub compared: usize,
    /// Values that differ, one line each.
    pub differing: Vec<String>,
}

/// f32 precision (the predecessor stores f32 values).
fn same_f32(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * a.abs().max(b.abs()).max(1.0)
}

fn eq_type(kind: &str) -> Option<iem_engine_proto::BandKind> {
    use iem_engine_proto::BandKind as K;
    match kind {
        "highpass" => Some(K::HighPass),
        "lowshelf" => Some(K::LowShelf),
        "band" => Some(K::Peak),
        "highshelf" => Some(K::HighShelf),
        _ => None,
    }
}

/// A group strip's link into its mix: unity, centred, unmuted.
const LINK: Level = Level {
    gain_db: 0.0,
    pan: 0.0,
    muted: false,
};

struct Run<'a> {
    imp: &'a Imported,
    check: Check,
    unknown: Vec<String>,
}

impl Run<'_> {
    fn place(&mut self, name: &str) -> Option<Place> {
        let p = self.imp.place(name).cloned();
        if p.is_none() && !self.unknown.iter().any(|u| u == name) {
            self.unknown.push(name.to_owned());
        }
        p
    }

    fn note(
        &mut self,
        who: &str,
        what: &str,
        same: bool,
        backup: impl std::fmt::Display,
        project: impl std::fmt::Display,
    ) {
        self.check.compared += 1;
        if !same {
            self.check.differing.push(format!(
                "{who}: {what} {backup} in the backup, {project} in the project"
            ));
        }
    }

    fn eq_bands(&mut self, who: &str, eq: &iem_engine_proto::Eq, bands: &[EqBandBackup]) {
        for b in bands {
            let Some(p) = eq.bands.get(usize::from(b.band)) else {
                self.check
                    .differing
                    .push(format!("{who}: EQ band {} does not exist", b.band + 1));
                continue;
            };
            let w = format!("{who} EQ band {}", b.band + 1);
            let kind = eq_type(&b.band_type);
            self.note(
                &w,
                "type",
                kind == Some(p.kind),
                &b.band_type,
                format!("{:?}", p.kind),
            );
            self.note(&w, "enabled", b.enabled == p.enabled, b.enabled, p.enabled);
            let hz = f64::from(b.freq_hz);
            self.note(
                &w,
                "Hz",
                (hz - p.freq_hz).abs() <= 0.005 * p.freq_hz.abs(),
                hz,
                p.freq_hz,
            );
            let g = f64::from(b.gain_db);
            let same_gain = (g <= -150.0 && p.gain_db <= -150.0) || (g - p.gain_db).abs() <= 0.05;
            self.note(&w, "dB", same_gain, g, p.gain_db);
            let bw = f64::from(b.bw_oct);
            self.note(&w, "octaves", (bw - p.bw_oct).abs() <= 0.01, bw, p.bw_oct);
        }
    }
}

/// The project names and what they became, as `who` for reports.
fn who(p: &Place) -> String {
    match p {
        Place::Input(id) => format!("input {id}"),
        Place::Mix(id) => format!("mix {id}"),
        Place::Group { group, mix } => format!("mix {mix} group {group}"),
    }
}

/// Compares `backup` with `imp` by the project's track names.
pub fn cross_check(backup: &MixerBackup, imp: &Imported) -> Result<Check, Problems> {
    let mut r = Run {
        imp,
        check: Check::default(),
        unknown: Vec::new(),
    };
    let mut missing = Vec::new();
    let mut sends: Vec<_> = backup.sends.iter().collect();
    sends.sort_by(|a, b| {
        a.src_name
            .cmp(&b.src_name)
            .then_with(|| a.dest_name.cmp(&b.dest_name))
    });
    for s in sends {
        let (Some(src), Some(dst)) = (r.place(&s.src_name), r.place(&s.dest_name)) else {
            continue;
        };
        let (source, mix) = match (&src, &dst) {
            (Place::Input(i), Place::Mix(m) | Place::Group { mix: m, .. }) => {
                (Source::Input(i.clone()), m.clone())
            }
            (Place::Mix(h), Place::Mix(m)) => (Source::Mix(h.clone()), m.clone()),
            (Place::Group { mix: into, .. }, Place::Mix(m)) if into == m => {
                let w = format!("{} link", who(&src));
                r.note(&w, "volume", same_f32(s.vol, 1.0), s.vol, 1.0);
                r.note(&w, "pan", same_f32(s.pan, LINK.pan), s.pan, LINK.pan);
                r.note(&w, "mute", s.mute == LINK.muted, s.mute, LINK.muted);
                continue;
            }
            _ => {
                missing.push(format!(
                    "send {:?} → {:?}: not a level of the engine's model",
                    s.src_name, s.dest_name
                ));
                continue;
            }
        };
        let Some(l) = imp.state.mixes.get(&mix).and_then(|x| x.level(&source)) else {
            missing.push(format!("level {source} in {mix} is not in the project"));
            continue;
        };
        let w = format!("mix {mix} level {source}");
        let lin = db_to_lin(l.gain_db);
        r.note(&w, "volume", same_f32(s.vol, lin), s.vol, lin);
        r.note(&w, "pan", same_f32(s.pan, l.pan), s.pan, l.pan);
        r.note(&w, "mute", s.mute == l.muted, s.mute, l.muted);
    }
    let mut mutes: Vec<_> = backup.track_mutes.iter().collect();
    mutes.sort();
    for (name, muted) in mutes {
        let Some(p) = r.place(name) else { continue };
        let v = match &p {
            Place::Input(id) => imp.state.inputs.get(id).map(|s| s.muted),
            Place::Mix(id) => imp.state.mixes.get(id).map(|m| m.out.muted),
            Place::Group { group, mix } => imp
                .state
                .mixes
                .get(mix)
                .and_then(|m| m.groups.get(group))
                .map(|g| g.muted),
        };
        r.note(
            &who(&p),
            "mute",
            v == Some(*muted),
            muted,
            v.unwrap_or_default(),
        );
    }
    let mut volumes: Vec<_> = backup.track_volumes.iter().collect();
    volumes.sort_by_key(|a| a.0);
    for (name, vol) in volumes {
        let Some(p) = r.place(name) else { continue };
        let db = match &p {
            Place::Mix(id) => imp.state.mixes.get(id).map(|m| m.out.volume_db),
            Place::Group { group, mix } => imp
                .state
                .mixes
                .get(mix)
                .and_then(|m| m.groups.get(group))
                .map(|g| g.gain_db),
            Place::Input(id) => {
                missing.push(format!(
                    "track volume of input {id} (only mixes and group strips are captured)"
                ));
                continue;
            }
        };
        let lin = db.map_or(0.0, db_to_lin);
        r.note(&who(&p), "volume", same_f32(*vol, lin), vol, lin);
    }
    let mut eqs: Vec<_> = backup.eq.iter().collect();
    eqs.sort_by_key(|a| a.0);
    for (name, bands) in eqs {
        let Some(p) = r.place(name) else { continue };
        let eq = match &p {
            Place::Input(id) => imp.state.inputs.get(id).map(|s| s.eq),
            Place::Mix(id) => imp.state.mixes.get(id).map(|m| m.out.eq),
            Place::Group { group, mix } => imp
                .state
                .mixes
                .get(mix)
                .and_then(|m| m.groups.get(group))
                .map(|g| g.eq),
        };
        if let Some(eq) = eq {
            r.eq_bands(&who(&p), &eq, bands);
        }
    }
    let mut limiters: Vec<_> = backup.limiter.iter().collect();
    limiters.sort_by_key(|a| a.0);
    for (name, lim) in limiters {
        let Some(p) = r.place(name) else { continue };
        let Place::Mix(id) = &p else {
            missing.push(format!("limiter on {}", who(&p)));
            continue;
        };
        let Some(m) = imp.state.mixes.get(id) else {
            continue;
        };
        let (limit, enabled) = (m.out.limiter.limit_db, m.out.limiter.enabled);
        let w = who(&p);
        let l = f64::from(lim.limit_db);
        r.note(&w, "limit dB", (l - limit).abs() <= 0.01, l, limit);
        r.note(&w, "limiter", lim.enabled == enabled, lim.enabled, enabled);
    }
    let mut problems = missing;
    if !r.unknown.is_empty() {
        let names: Vec<String> = r.unknown.iter().map(|n| format!("{n:?}")).collect();
        problems.insert(
            0,
            format!(
                "backup names missing from the project: {}",
                names.join(", ")
            ),
        );
    }
    if problems.is_empty() {
        Ok(r.check)
    } else {
        Err(Problems(problems))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use iem_core::{LimiterBackup, SendBackup};
    use iem_engine_proto::{InputId, MixId};

    use super::*;
    use crate::aliases::parse_aliases;
    use crate::import::import;
    use crate::legacy::LegacyProject;
    use crate::sitegen::{
        aliases_toml, instance_name, project, sample_state, synthetic_routing, synthetic_site,
        track_name,
    };

    fn setup() -> Imported {
        let topo = synthetic_site();
        let routing = synthetic_routing(&topo);
        let text = project(
            &topo,
            &routing,
            &sample_state(&topo, &routing, 31),
            &track_name,
        )
        .unwrap();
        let aliases = parse_aliases(&aliases_toml(&topo, &routing, &BTreeMap::new())).unwrap();
        import(&LegacyProject::parse(&text).unwrap(), &aliases).unwrap()
    }

    /// The first level of member1 that is not off.
    fn on_level(imp: &Imported) -> (InputId, Level) {
        imp.state.mixes[&MixId::new("member1")]
            .inputs
            .iter()
            .find(|(_, l)| l.gain_db > -100.0)
            .map(|(i, l)| (i.clone(), *l))
            .unwrap()
    }

    /// A backup that agrees with the import (at f32 precision).
    fn agreeing(imp: &Imported) -> MixerBackup {
        let mut b = MixerBackup {
            version: 1,
            ..MixerBackup::default()
        };
        let (i, l) = on_level(imp);
        let grouped = imp.topology.group_of(&i).is_some();
        let dest = if grouped {
            instance_name(
                &MixId::new("member1"),
                &iem_engine_proto::GroupId::new("stems"),
            )
        } else {
            "member1".to_owned()
        };
        b.sends.push(SendBackup {
            src_name: track_name(&i.0),
            dest_name: track_name(&dest),
            vol: f64::from(db_to_lin(l.gain_db) as f32),
            pan: f64::from(l.pan as f32),
            mute: l.muted,
        });
        let m1 = &imp.state.mixes[&MixId::new("member1")].out;
        b.track_mutes.insert(track_name("member1"), m1.muted);
        b.track_mutes.insert(
            track_name("mic1"),
            imp.state.inputs[&InputId::new("mic1")].muted,
        );
        b.track_volumes.insert(
            track_name("member1"),
            f64::from(db_to_lin(m1.volume_db) as f32),
        );
        b.limiter.insert(
            track_name("member1"),
            LimiterBackup {
                limit_db: m1.limiter.limit_db as f32,
                limit_norm: 0.0,
                enabled: m1.limiter.enabled,
            },
        );
        let band = m1.eq.bands[1];
        b.eq.insert(
            track_name("member1"),
            vec![EqBandBackup {
                band: 1,
                band_type: "lowshelf".into(),
                freq_norm: 0.0,
                gain_norm: 0.0,
                bw_norm: 0.0,
                freq_hz: band.freq_hz as f32,
                gain_db: band.gain_db as f32,
                bw_oct: band.bw_oct as f32,
                enabled: band.enabled,
            }],
        );
        b
    }

    #[test]
    fn an_agreeing_backup_compares_clean() {
        let imp = setup();
        let c = cross_check(&agreeing(&imp), &imp).unwrap();
        assert_eq!(c.differing, Vec::<String>::new());
        assert_eq!(c.compared, 3 + 2 + 1 + 2 + 5);
    }

    #[test]
    fn changed_values_are_reported_not_applied() {
        let imp = setup();
        let mut b = agreeing(&imp);
        b.sends[0].mute ^= true;
        b.sends[0].vol *= 2.0;
        let name = track_name("member1");
        *b.track_mutes.get_mut(&name).unwrap() ^= true;
        *b.track_volumes.get_mut(&name).unwrap() *= 0.5;
        b.limiter.get_mut(&name).unwrap().enabled ^= true;
        b.eq.get_mut(&name).unwrap()[0].band_type = "band".into();
        b.eq.get_mut(&name).unwrap()[0].gain_db += 1.0;
        let c = cross_check(&b, &imp).unwrap();
        assert_eq!(c.differing.len(), 7, "{:#?}", c.differing);
        let (i, _) = on_level(&imp);
        assert!(
            c.differing
                .iter()
                .any(|d| d.starts_with(&format!("mix member1 level {i}: mute ")))
        );
        assert!(
            c.differing
                .iter()
                .any(|d| d.starts_with("mix member1 EQ band 2: type band in the backup"))
        );
    }

    #[test]
    fn group_strips_and_their_links_compare() {
        let imp = setup();
        let strip = track_name(&instance_name(
            &MixId::new("member3"),
            &iem_engine_proto::GroupId::new("stems"),
        ));
        let g = imp.state.mixes[&MixId::new("member3")].groups
            [&iem_engine_proto::GroupId::new("stems")];
        let mut b = MixerBackup::default();
        b.track_volumes
            .insert(strip.clone(), f64::from(db_to_lin(g.gain_db) as f32));
        b.track_mutes.insert(strip.clone(), !g.muted);
        b.sends.push(SendBackup {
            src_name: strip.clone(),
            dest_name: track_name("member3"),
            vol: 1.0,
            pan: 0.0,
            mute: false,
        });
        let c = cross_check(&b, &imp).unwrap();
        assert_eq!(c.compared, 1 + 1 + 3);
        assert_eq!(c.differing.len(), 1, "{:#?}", c.differing);
        assert!(c.differing[0].starts_with("mix member3 group stems: mute "));
        b.sends[0].vol = 0.5;
        let c = cross_check(&b, &imp).unwrap();
        assert!(
            c.differing
                .iter()
                .any(|d| d.starts_with("mix member3 group stems link: volume 0.5")),
            "{:#?}",
            c.differing
        );
    }

    #[test]
    fn unknown_names_and_sends_outside_the_model_fail() {
        let imp = setup();
        let mut b = agreeing(&imp);
        b.track_mutes.insert("GONE".into(), false);
        b.sends.push(SendBackup {
            src_name: track_name("member2"),
            dest_name: track_name("member3"),
            vol: 1.0,
            pan: 0.0,
            mute: false,
        });
        b.sends.push(SendBackup {
            src_name: track_name("member1"),
            dest_name: track_name("mic1"),
            vol: 1.0,
            pan: 0.0,
            mute: false,
        });
        b.track_volumes.insert(track_name("mic2"), 1.0);
        b.limiter.insert(
            track_name("mic2"),
            LimiterBackup {
                limit_db: 0.0,
                limit_norm: 0.0,
                enabled: true,
            },
        );
        let err = cross_check(&b, &imp).unwrap_err().0;
        assert_eq!(err[0], "backup names missing from the project: \"GONE\"");
        for want in [
            "level member2 in member3 is not in the project",
            "not a level of the engine's model",
            "track volume of input mic2",
            "limiter on input mic2",
        ] {
            assert!(
                err.iter().any(|e| e.contains(want)),
                "{want:?} not in {err:#?}"
            );
        }
    }

    #[test]
    fn band_types_map_to_kinds() {
        use iem_engine_proto::BandKind as K;
        assert_eq!(eq_type("highpass"), Some(K::HighPass));
        assert_eq!(eq_type("lowshelf"), Some(K::LowShelf));
        assert_eq!(eq_type("band"), Some(K::Peak));
        assert_eq!(eq_type("highshelf"), Some(K::HighShelf));
        assert_eq!(eq_type("notch"), None);
    }

    #[test]
    fn f32_precision_scales_with_the_magnitude() {
        assert!(same_f32(1000.0, 1000.0005));
        assert!(!same_f32(1000.0, 1000.002));
        assert!(same_f32(0.5, 0.500_000_5));
        assert!(!same_f32(0.5, 0.500_002));
    }

    #[test]
    fn eq_band_gains_and_frequencies_compare_within_their_tolerances() {
        let imp = setup();
        let mut r = Run {
            imp: &imp,
            check: Check::default(),
            unknown: Vec::new(),
        };
        // (backup dB, project dB, backup Hz, project Hz, the difference reported)
        let cases: [(f32, f64, f32, f64, Option<&str>); 6] = [
            (-200.0, -150.0, 1000.0, 1004.0, None),
            (
                3.0,
                -150.0,
                1000.0,
                1000.0,
                Some("dB 3 in the backup, -150 in the project"),
            ),
            (-150.0, -160.0, 1000.0, 1000.0, None),
            (
                -150.0,
                3.0,
                1000.0,
                1000.0,
                Some("dB -150 in the backup, 3 in the project"),
            ),
            (1.0, 1.04, 1000.0, 1000.0, None),
            (
                0.0,
                0.0,
                1000.0,
                1010.0,
                Some("Hz 1000 in the backup, 1010 in the project"),
            ),
        ];
        for (i, (bg, pg, bhz, phz, want)) in cases.into_iter().enumerate() {
            r.check = Check::default();
            let mut eq = iem_engine_proto::Eq::default();
            eq.bands[2] = iem_engine_proto::EqBand {
                kind: iem_engine_proto::BandKind::Peak,
                enabled: true,
                freq_hz: phz,
                gain_db: pg,
                bw_oct: 1.0,
            };
            let band = EqBandBackup {
                band: 2,
                band_type: "band".into(),
                freq_norm: 0.0,
                gain_norm: 0.0,
                bw_norm: 0.0,
                freq_hz: bhz,
                gain_db: bg,
                bw_oct: 1.0,
                enabled: true,
            };
            r.eq_bands("x", &eq, &[band]);
            assert_eq!(r.check.compared, 5, "case {i}");
            let want: Vec<String> = want
                .map(|w| format!("x EQ band 3: {w}"))
                .into_iter()
                .collect();
            assert_eq!(r.check.differing, want, "case {i}");
        }
        let short = iem_engine_proto::Eq::default();
        r.check = Check::default();
        let band = EqBandBackup {
            band: 7,
            band_type: "band".into(),
            freq_norm: 0.0,
            gain_norm: 0.0,
            bw_norm: 0.0,
            freq_hz: 1.0,
            gain_db: 0.0,
            bw_oct: 1.0,
            enabled: true,
        };
        r.eq_bands("y", &short, &[band]);
        assert_eq!(
            r.check.differing,
            vec!["y: EQ band 8 does not exist".to_owned()]
        );
    }

    #[test]
    fn each_unknown_name_is_listed_once() {
        let imp = setup();
        let mut b = agreeing(&imp);
        b.track_mutes.insert("GONE".into(), false);
        b.track_volumes.insert("GONE".into(), 1.0);
        b.eq.insert("LOST".into(), Vec::new());
        let err = cross_check(&b, &imp).unwrap_err().0;
        assert_eq!(
            err,
            vec!["backup names missing from the project: \"GONE\", \"LOST\"".to_owned()]
        );
    }
}
