//! Cross-check of the predecessor's newest backup JSON (v1) against an
//! imported project (S4 design note §3.2). Names must map; differing values
//! are reported, never applied: the project saved at switch time is newer.

use iem_core::{EqBandBackup, MixerBackup};
use iem_engine_proto::{BusId, InputId, SendId, Source, db_to_lin};

use crate::aliases::Aliases;
use crate::import::{Imported, Problems};

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

fn source(imp: &Imported, aliases: &Aliases, name: &str) -> Option<Source> {
    let id = aliases.tracks.get(name)?;
    let input = InputId::new(id.clone());
    if imp.topology.input(&input).is_some() {
        return Some(Source::Input(input));
    }
    let bus = BusId::new(id.clone());
    imp.topology.bus(&bus).map(|_| Source::Bus(bus))
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

struct Run<'a> {
    imp: &'a Imported,
    aliases: &'a Aliases,
    check: Check,
    unknown: Vec<String>,
}

impl Run<'_> {
    fn source(&mut self, name: &str) -> Option<Source> {
        let s = source(self.imp, self.aliases, name);
        if s.is_none() && !self.unknown.iter().any(|u| u == name) {
            self.unknown.push(name.to_owned());
        }
        s
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

/// Compares `backup` with `imp` (both through `aliases`).
pub fn cross_check(
    backup: &MixerBackup,
    imp: &Imported,
    aliases: &Aliases,
) -> Result<Check, Problems> {
    let mut r = Run {
        imp,
        aliases,
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
        let (Some(src), Some(dst)) = (r.source(&s.src_name), r.source(&s.dest_name)) else {
            continue;
        };
        let Source::Bus(dst) = dst else {
            missing.push(format!(
                "send {:?} → {:?}: the destination is an input",
                s.src_name, s.dest_name
            ));
            continue;
        };
        let id = SendId { src, dst };
        let Some(st) = imp.state.sends.iter().find(|e| e.id == id).map(|e| e.state) else {
            missing.push(format!("send {id} is not in the project"));
            continue;
        };
        let who = format!("send {id}");
        let lin = db_to_lin(st.gain_db);
        r.note(&who, "volume", same_f32(s.vol, lin), s.vol, lin);
        r.note(&who, "pan", same_f32(s.pan, st.pan), s.pan, st.pan);
        r.note(&who, "mute", s.mute == st.muted, s.mute, st.muted);
    }
    let mut mutes: Vec<_> = backup.track_mutes.iter().collect();
    mutes.sort();
    for (name, muted) in mutes {
        match r.source(name) {
            Some(Source::Input(id)) => {
                let v = imp.state.inputs.get(&id).map(|s| s.muted);
                r.note(
                    &format!("input {id}"),
                    "mute",
                    v == Some(*muted),
                    muted,
                    v.unwrap_or_default(),
                );
            }
            Some(Source::Bus(id)) => {
                let v = imp.state.buses.get(&id).map(|s| s.muted);
                r.note(
                    &format!("bus {id}"),
                    "mute",
                    v == Some(*muted),
                    muted,
                    v.unwrap_or_default(),
                );
            }
            None => {}
        }
    }
    let mut volumes: Vec<_> = backup.track_volumes.iter().collect();
    volumes.sort_by_key(|a| a.0);
    for (name, vol) in volumes {
        match r.source(name) {
            Some(Source::Bus(id)) => {
                let lin = imp
                    .state
                    .buses
                    .get(&id)
                    .map_or(0.0, |s| db_to_lin(s.fader_db));
                r.note(
                    &format!("bus {id}"),
                    "volume",
                    same_f32(*vol, lin),
                    vol,
                    lin,
                );
            }
            Some(Source::Input(id)) => missing.push(format!(
                "track volume of input {id} (only buses are captured)"
            )),
            None => {}
        }
    }
    let mut eqs: Vec<_> = backup.eq.iter().collect();
    eqs.sort_by_key(|a| a.0);
    for (name, bands) in eqs {
        match r.source(name) {
            Some(Source::Input(id)) => {
                if let Some(s) = imp.state.inputs.get(&id) {
                    let eq = s.eq;
                    r.eq_bands(&format!("input {id}"), &eq, bands);
                }
            }
            Some(Source::Bus(id)) => {
                if let Some(s) = imp.state.buses.get(&id) {
                    let eq = s.eq;
                    r.eq_bands(&format!("bus {id}"), &eq, bands);
                }
            }
            None => {}
        }
    }
    let mut limiters: Vec<_> = backup.limiter.iter().collect();
    limiters.sort_by_key(|a| a.0);
    for (name, lim) in limiters {
        match r.source(name) {
            Some(Source::Bus(id)) => {
                let Some(s) = imp.state.buses.get(&id) else {
                    continue;
                };
                let (limit, enabled) = (s.limiter.limit_db, s.limiter.enabled);
                let who = format!("bus {id}");
                let l = f64::from(lim.limit_db);
                r.note(&who, "limit dB", (l - limit).abs() <= 0.01, l, limit);
                r.note(
                    &who,
                    "limiter",
                    lim.enabled == enabled,
                    lim.enabled,
                    enabled,
                );
            }
            Some(Source::Input(id)) => missing.push(format!("limiter on input {id}")),
            None => {}
        }
    }
    let mut problems = missing;
    if !r.unknown.is_empty() {
        let names: Vec<String> = r.unknown.iter().map(|n| format!("{n:?}")).collect();
        problems.insert(
            0,
            format!(
                "backup names missing from the aliases: {}",
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

    use super::*;
    use crate::aliases::parse_aliases;
    use crate::import::import;
    use crate::legacy::LegacyProject;
    use crate::sitegen::{aliases_toml, project, sample_state, synthetic_site, track_name};

    fn setup() -> (Imported, Aliases) {
        let topo = synthetic_site();
        let text = project(&topo, &sample_state(&topo, 31), &track_name).unwrap();
        let aliases = parse_aliases(&aliases_toml(&topo, &BTreeMap::new())).unwrap();
        (
            import(&LegacyProject::parse(&text).unwrap(), &aliases).unwrap(),
            aliases,
        )
    }

    /// The first send that is not off.
    fn on_send(imp: &Imported) -> usize {
        imp.state
            .sends
            .iter()
            .position(|e| e.state.gain_db > -100.0)
            .unwrap()
    }

    /// A backup that agrees with the import (at f32 precision).
    fn agreeing(imp: &Imported) -> MixerBackup {
        let mut b = MixerBackup {
            version: 1,
            ..MixerBackup::default()
        };
        let e = &imp.state.sends[on_send(imp)];
        b.sends.push(SendBackup {
            src_name: track_name(&e.id.src.to_string()),
            dest_name: track_name(&e.id.dst.0),
            vol: f64::from(db_to_lin(e.state.gain_db) as f32),
            pan: f64::from(e.state.pan as f32),
            mute: e.state.muted,
        });
        let m1 = &imp.state.buses[&BusId::new("member1")];
        b.track_mutes.insert(track_name("member1"), m1.muted);
        b.track_mutes.insert(
            track_name("mic1"),
            imp.state.inputs[&InputId::new("mic1")].muted,
        );
        b.track_volumes.insert(
            track_name("member1"),
            f64::from(db_to_lin(m1.fader_db) as f32),
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
        let (imp, aliases) = setup();
        let c = cross_check(&agreeing(&imp), &imp, &aliases).unwrap();
        assert_eq!(c.differing, Vec::<String>::new());
        assert_eq!(c.compared, 3 + 2 + 1 + 2 + 5);
    }

    #[test]
    fn changed_values_are_reported_not_applied() {
        let (imp, aliases) = setup();
        let mut b = agreeing(&imp);
        b.sends[0].mute ^= true;
        b.sends[0].vol *= 2.0;
        let name = track_name("member1");
        *b.track_mutes.get_mut(&name).unwrap() ^= true;
        *b.track_volumes.get_mut(&name).unwrap() *= 0.5;
        b.limiter.get_mut(&name).unwrap().enabled ^= true;
        b.eq.get_mut(&name).unwrap()[0].band_type = "band".into();
        b.eq.get_mut(&name).unwrap()[0].gain_db += 1.0;
        let c = cross_check(&b, &imp, &aliases).unwrap();
        assert_eq!(c.differing.len(), 7, "{:#?}", c.differing);
        let id = &imp.state.sends[on_send(&imp)].id;
        assert!(
            c.differing
                .iter()
                .any(|d| d.starts_with(&format!("send {id}: mute ")))
        );
        assert!(
            c.differing
                .iter()
                .any(|d| d.starts_with("bus member1 EQ band 2: type band in the backup"))
        );
    }

    #[test]
    fn unknown_names_and_missing_sends_fail() {
        let (imp, aliases) = setup();
        let mut b = agreeing(&imp);
        b.track_mutes.insert("GONE".into(), false);
        b.sends.push(SendBackup {
            src_name: track_name("member1"),
            dest_name: track_name("member2"),
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
        let err = cross_check(&b, &imp, &aliases).unwrap_err().0;
        assert_eq!(err[0], "backup names missing from the aliases: \"GONE\"");
        for want in [
            "send member1>member2 is not in the project",
            "the destination is an input",
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
        let (imp, aliases) = setup();
        let mut r = Run {
            imp: &imp,
            aliases: &aliases,
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
    }

    #[test]
    fn each_unknown_name_is_listed_once() {
        let (imp, aliases) = setup();
        let mut b = agreeing(&imp);
        b.track_mutes.insert("GONE".into(), false);
        b.track_volumes.insert("GONE".into(), 1.0);
        b.eq.insert("LOST".into(), Vec::new());
        let err = cross_check(&b, &imp, &aliases).unwrap_err().0;
        assert_eq!(
            err,
            vec!["backup names missing from the aliases: \"GONE\", \"LOST\"".to_owned()]
        );
    }
}
