//! The predecessor's presets, snapshots and customizations re-keyed from
//! REAPER track numbers to stable ids (S4 design note §3.4). A key is read in
//! the era of the item's time (`eras.toml`); it must name a source the member
//! can send: an input with a send into the member's bus or stems bus, or
//! another bus with a send into the member's bus.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use iem_core::band::{MixSend, Preset, Snapshot};
use iem_core::{Customization, EqBand as LegacyBand, MixSnapshot, PresetEntry};
use iem_engine_proto::{
    BandKind as EqKind, BusId, DB_OFF, Eq as EqSettings, EqBand, InputId, SendId, Source,
};

use crate::aliases::{Aliases, Eras, MemberAlias};
use crate::import::{Problems, lin_to_db};
use crate::topology::Topology;

/// What the re-keying needs for one predecessor member.
pub struct Ctx<'a> {
    pub topology: &'a Topology,
    pub aliases: &'a Aliases,
    pub eras: &'a Eras,
    /// The predecessor's member id (file name).
    pub legacy_member: &'a str,
    pub member: &'a MemberAlias,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub items: usize,
    pub sends: usize,
    /// Input EQs kept as metadata.
    pub input_eqs: usize,
    /// EQs of other members' buses (mix viewers' snapshots), dropped.
    pub dropped_bus_eqs: usize,
    /// Items whose time fell between two eras.
    pub between_eras: usize,
}

impl std::ops::AddAssign for Stats {
    fn add_assign(&mut self, o: Self) {
        self.items += o.items;
        self.sends += o.sends;
        self.input_eqs += o.input_eqs;
        self.dropped_bus_eqs += o.dropped_bus_eqs;
        self.between_eras += o.between_eras;
    }
}

/// Legacy UI pan 0…1 → −1…1.
pub fn pan(p: f32) -> Result<f64, String> {
    if (0.0..=1.0).contains(&p) {
        Ok(f64::from(p) * 2.0 - 1.0)
    } else {
        Err(format!("pan {p} is outside 0…1"))
    }
}

/// A legacy EQ (5 bands with display values) as engine EQ settings.
pub fn legacy_eq(bands: &[LegacyBand]) -> Result<EqSettings, String> {
    let mut out = Vec::with_capacity(bands.len());
    for b in bands {
        let kind = match b.band_type.as_str() {
            "highpass" => EqKind::HighPass,
            "lowshelf" => EqKind::LowShelf,
            "band" => EqKind::Peak,
            "highshelf" => EqKind::HighShelf,
            other => return Err(format!("EQ band type {other:?} is not supported")),
        };
        let gain = f64::from(b.gain_db);
        out.push(EqBand {
            kind,
            enabled: b.enabled,
            freq_hz: f64::from(b.freq_hz),
            gain_db: if gain <= DB_OFF { DB_OFF } else { gain },
            bw_oct: f64::from(b.bw),
        });
    }
    let n = out.len();
    let bands: [EqBand; 5] = out
        .try_into()
        .map_err(|_| format!("EQ has {n} bands (5 are supported)"))?;
    Ok(EqSettings {
        gain_db: 0.0,
        bands,
    })
}

type KeyMap = BTreeMap<usize, Source>;

impl Ctx<'_> {
    fn bus(&self) -> BusId {
        BusId::new(self.member.bus.clone())
    }

    fn stems(&self) -> BusId {
        BusId::new(self.member.stems.clone())
    }

    fn sends_to(&self, src: &Source, dst: BusId) -> bool {
        self.topology.has_send(&SendId {
            src: src.clone(),
            dst,
        })
    }

    /// The source REAPER track `key` (1-based) names in era `era`, if this
    /// member can send it.
    pub fn source(&self, era: usize, key: usize) -> Option<Source> {
        let name = self.eras.era.get(era)?.tracks.get(key.checked_sub(1)?)?;
        let id = self.aliases.tracks.get(name)?;
        let input = Source::Input(InputId::new(id.clone()));
        if self.topology.input(&InputId::new(id.clone())).is_some() {
            let ok = self.sends_to(&input, self.bus()) || self.sends_to(&input, self.stems());
            return ok.then_some(input);
        }
        let bus = Source::Bus(BusId::new(id.clone()));
        self.sends_to(&bus, self.bus()).then_some(bus)
    }

    fn map_in(&self, era: usize, keys: &BTreeSet<usize>) -> Result<KeyMap, Vec<usize>> {
        let mut map = KeyMap::new();
        let mut bad = Vec::new();
        for &k in keys {
            match self.source(era, k) {
                Some(s) => {
                    map.insert(k, s);
                }
                None => bad.push(k),
            }
        }
        if bad.is_empty() { Ok(map) } else { Err(bad) }
    }

    /// Maps every key of an item saved at `t`; returns the map and whether
    /// the time fell between eras.
    fn map_keys(
        &self,
        t: i64,
        keys: &BTreeSet<usize>,
        label: &str,
    ) -> Result<(KeyMap, bool), String> {
        let candidates = self.eras.candidates(t);
        let mut maps: Vec<KeyMap> = Vec::new();
        let mut failures = Vec::new();
        for &era in &candidates {
            match self.map_in(era, keys) {
                Ok(m) => maps.push(m),
                Err(bad) => failures.push(format!("era {}: keys {bad:?}", era + 1)),
            }
        }
        let Some(first) = maps.first() else {
            return Err(format!(
                "{label}: unmappable for member {} ({})",
                self.legacy_member,
                failures.join("; ")
            ));
        };
        if maps.iter().any(|m| m != first) {
            return Err(format!(
                "{label}: ambiguous between eras {:?} for member {}",
                candidates.iter().map(|e| e + 1).collect::<Vec<_>>(),
                self.legacy_member
            ));
        }
        Ok((first.clone(), candidates.len() > 1))
    }

    fn archive(&self) -> (bool, Option<String>) {
        let a = self.member.archived;
        (a, a.then(|| self.legacy_member.to_owned()))
    }

    fn eqs(
        &self,
        eq: Option<&HashMap<usize, Vec<LegacyBand>>>,
        map: &KeyMap,
        stats: &mut Stats,
    ) -> Result<BTreeMap<InputId, EqSettings>, String> {
        let mut out = BTreeMap::new();
        let mut entries: Vec<(&usize, &Vec<LegacyBand>)> = eq.into_iter().flatten().collect();
        entries.sort_by_key(|(k, _)| **k);
        for (k, bands) in entries {
            match map.get(k) {
                Some(Source::Input(id)) => {
                    out.insert(
                        id.clone(),
                        legacy_eq(bands).map_err(|e| format!("key {k}: {e}"))?,
                    );
                    stats.input_eqs += 1;
                }
                Some(Source::Bus(_)) => stats.dropped_bus_eqs += 1,
                None => return Err(format!("EQ key {k} has no source")),
            }
        }
        Ok(out)
    }
}

fn keys<V, W>(channels: &HashMap<usize, V>, eq: Option<&HashMap<usize, W>>) -> BTreeSet<usize> {
    channels
        .keys()
        .chain(eq.into_iter().flat_map(HashMap::keys))
        .copied()
        .collect()
}

/// A member's presets (the predecessor's file: name → preset), by name.
pub fn rekey_presets(
    entries: &HashMap<String, PresetEntry>,
    ctx: &Ctx<'_>,
) -> Result<(Vec<Preset>, Stats), Problems> {
    let mut names: Vec<&String> = entries.keys().collect();
    names.sort();
    let mut out = Vec::new();
    let mut stats = Stats::default();
    let mut problems = Vec::new();
    for name in names {
        let Some(e) = entries.get(name) else { continue };
        let label = format!("preset {name:?}");
        let mut one = || -> Result<Preset, String> {
            let (map, between) = ctx.map_keys(
                e.updated_at,
                &keys(&e.channels, e.eq_bands.as_ref()),
                &label,
            )?;
            let mut st = Stats {
                items: 1,
                between_eras: usize::from(between),
                ..Stats::default()
            };
            let mut sends = Vec::new();
            let mut ch: Vec<_> = e.channels.iter().collect();
            ch.sort_by_key(|(k, _)| **k);
            for (k, c) in ch {
                let src = map
                    .get(k)
                    .cloned()
                    .ok_or_else(|| format!("{label}: key {k}"))?;
                sends.push(MixSend {
                    src,
                    gain_db: f64::from(c.vol),
                    pan: pan(c.pan).map_err(|x| format!("{label}: key {k}: {x}"))?,
                    muted: c.mute,
                });
            }
            st.sends = sends.len();
            let input_eq = ctx
                .eqs(e.eq_bands.as_ref(), &map, &mut st)
                .map_err(|x| format!("{label}: {x}"))?;
            let (archived, legacy_member) = ctx.archive();
            stats += st;
            Ok(Preset {
                name: e.name.clone(),
                created_at: e.created_at,
                updated_at: e.updated_at,
                sends,
                stems_fader_db: e.stems_level_db.map(f64::from),
                input_eq,
                archived,
                legacy_member,
            })
        };
        match one() {
            Ok(p) => out.push(p),
            Err(x) => problems.push(x),
        }
    }
    if problems.is_empty() {
        Ok((out, stats))
    } else {
        Err(Problems(problems))
    }
}

/// A member's snapshots (the predecessor's file: a list), in file order.
pub fn rekey_snapshots(
    list: &[MixSnapshot],
    ctx: &Ctx<'_>,
) -> Result<(Vec<Snapshot>, Stats), Problems> {
    let mut out = Vec::new();
    let mut stats = Stats::default();
    let mut problems = Vec::new();
    for (i, s) in list.iter().enumerate() {
        let label = format!("snapshot {} ({:?}, {})", i + 1, s.label, s.timestamp);
        let mut one = || -> Result<Snapshot, String> {
            let (map, between) =
                ctx.map_keys(s.timestamp, &keys(&s.channels, s.eq_bands.as_ref()), &label)?;
            let mut st = Stats {
                items: 1,
                between_eras: usize::from(between),
                ..Stats::default()
            };
            let mut sends = Vec::new();
            let mut ch: Vec<_> = s.channels.iter().collect();
            ch.sort_by_key(|(k, _)| **k);
            for (k, c) in ch {
                let src = map
                    .get(k)
                    .cloned()
                    .ok_or_else(|| format!("{label}: key {k}"))?;
                sends.push(MixSend {
                    src,
                    gain_db: lin_to_db(f64::from(c.vol)),
                    pan: pan(c.pan).map_err(|x| format!("{label}: key {k}: {x}"))?,
                    muted: c.mute,
                });
            }
            st.sends = sends.len();
            let input_eq = ctx
                .eqs(s.eq_bands.as_ref(), &map, &mut st)
                .map_err(|x| format!("{label}: {x}"))?;
            let (archived, legacy_member) = ctx.archive();
            stats += st;
            Ok(Snapshot {
                timestamp: s.timestamp,
                label: s.label.clone(),
                pinned: s.pinned,
                sends,
                stems_fader_db: None,
                input_eq,
                archived,
                legacy_member,
            })
        };
        match one() {
            Ok(x) => out.push(x),
            Err(x) => problems.push(x),
        }
    }
    if problems.is_empty() {
        Ok((out, stats))
    } else {
        Err(Problems(problems))
    }
}

/// A member's pins and hides, in the newest era (customizations carry no time).
pub fn rekey_customization(
    c: &Customization,
    ctx: &Ctx<'_>,
) -> Result<(Vec<Source>, Vec<Source>), Problems> {
    let Some(era) = ctx.eras.newest() else {
        return Err(Problems(vec!["no eras".into()]));
    };
    let mut bad = Vec::new();
    let mut map = |list: &[usize]| -> Vec<Source> {
        list.iter()
            .filter_map(|k| {
                let s = ctx.source(era, *k);
                if s.is_none() {
                    bad.push(*k);
                }
                s
            })
            .collect()
    };
    let pinned = map(c.pinned.as_slice());
    let hidden = map(c.hidden.as_slice());
    if bad.is_empty() {
        Ok((pinned, hidden))
    } else {
        Err(Problems(vec![format!(
            "customization of member {}: keys {bad:?} are unmappable in the newest era",
            ctx.legacy_member
        )]))
    }
}

#[cfg(test)]
mod tests {
    use iem_core::{ChannelPreset, ChannelSnapshot};

    use super::*;
    use crate::aliases::{parse_aliases, parse_eras};
    use crate::sitegen::{aliases_toml, synthetic_site, track_name};

    struct Env {
        topology: Topology,
        aliases: Aliases,
        eras: Eras,
        member: MemberAlias,
        viewer: MemberAlias,
    }

    fn member(n: &str) -> MemberAlias {
        MemberAlias {
            id: n.into(),
            bus: n.into(),
            stems: format!("{n}.stems"),
            archived: false,
        }
    }

    /// Era 1 (t 100…200): tracks mic1, mic2, drums, member2.
    /// Era 2 (t 300…400): mic2, mic1, drums, member2 (first two swapped).
    /// Era 3 (t 500…600): mic1, spare, drums, member2 (spare is not aliased).
    fn env() -> Env {
        let topology = synthetic_site();
        let aliases = parse_aliases(&aliases_toml(&topology, &BTreeMap::new())).unwrap();
        let n = |id: &str| format!("{:?}", track_name(id));
        let eras = parse_eras(&format!(
            "[[era]]\nfirst_seen = 100\nlast_seen = 200\ntracks = [{}, {}, {}, {}]\n\
             [[era]]\nfirst_seen = 300\nlast_seen = 400\ntracks = [{}, {}, {}, {}]\n\
             [[era]]\nfirst_seen = 500\nlast_seen = 600\ntracks = [{}, \"SPARE\", {}, {}]\n",
            n("mic1"),
            n("mic2"),
            n("drums"),
            n("member2"),
            n("mic2"),
            n("mic1"),
            n("drums"),
            n("member2"),
            n("mic1"),
            n("drums"),
            n("member2"),
        ))
        .unwrap();
        Env {
            topology,
            aliases,
            eras,
            member: member("member3"),
            viewer: member("member1"),
        }
    }

    fn ctx<'a>(e: &'a Env, m: &'a MemberAlias) -> Ctx<'a> {
        Ctx {
            topology: &e.topology,
            aliases: &e.aliases,
            eras: &e.eras,
            legacy_member: "legacy3",
            member: m,
        }
    }

    fn band(kind: &str, gain: f32) -> LegacyBand {
        LegacyBand {
            band_type: kind.into(),
            freq_hz: 1000.0,
            gain_db: gain,
            bw: 1.0,
            freq_norm: 0.0,
            gain_norm: 0.0,
            bw_norm: 0.0,
            gain_db_min: -150.0,
            gain_db_max: 12.0,
            enabled: true,
        }
    }

    fn five() -> Vec<LegacyBand> {
        vec![
            band("highpass", 0.0),
            band("lowshelf", 2.5),
            band("band", -150.0),
            band("band", 1.0),
            band("highshelf", -3.0),
        ]
    }

    fn snap(t: i64, ch: &[(usize, f32)]) -> MixSnapshot {
        MixSnapshot {
            timestamp: t,
            label: "auto".into(),
            pinned: false,
            channels: ch
                .iter()
                .map(|(k, v)| {
                    (
                        *k,
                        ChannelSnapshot {
                            vol: *v,
                            mute: false,
                            pan: 0.75,
                        },
                    )
                })
                .collect(),
            eq_bands: None,
        }
    }

    fn src_in(id: &str) -> Source {
        Source::Input(InputId::new(id))
    }

    #[test]
    fn keys_follow_the_era_of_the_item() {
        let e = env();
        let c = ctx(&e, &e.member);
        let (s, st) = rekey_snapshots(
            &[
                snap(150, &[(1, 1.0), (2, 0.5)]),
                snap(350, &[(1, 1.0), (2, 0.0)]),
            ],
            &c,
        )
        .unwrap();
        assert_eq!(st.items, 2);
        assert_eq!(st.sends, 4);
        assert_eq!(s[0].sends[0].src, src_in("mic1"));
        assert_eq!(s[0].sends[1].src, src_in("mic2"));
        assert_eq!(s[1].sends[0].src, src_in("mic2"), "swapped in era 2");
        assert_eq!(s[0].sends[0].gain_db, 0.0);
        assert!((s[0].sends[1].gain_db + 6.020_599_913_279_624).abs() < 1e-9);
        assert_eq!(s[1].sends[1].gain_db, DB_OFF);
        assert!((s[0].sends[0].pan - 0.5).abs() < 1e-12);
        assert_eq!(s[0].label, "auto");
        assert!(!s[0].archived && s[0].legacy_member.is_none());
        assert_eq!(st.between_eras, 0, "both items fall inside an era");
    }

    #[test]
    fn between_eras_the_one_that_maps_wins_or_both_agree() {
        let e = env();
        let c = ctx(&e, &e.member);
        // Keys 3 (drums, a stems-group input → the stems bus) and 4 (another
        // member's bus: member3 does not receive it) — neither era maps key 4.
        let err = rekey_snapshots(&[snap(250, &[(4, 1.0)])], &c).unwrap_err();
        assert!(err.0[0].contains("unmappable for member legacy3"), "{err}");
        assert!(
            err.0[0].contains("era 1: keys [4]; era 2: keys [4]"),
            "{err}"
        );
        // Key 3 is drums in both eras: they agree.
        let (s, st) = rekey_snapshots(&[snap(250, &[(3, 1.0)])], &c).unwrap();
        assert_eq!(s[0].sends[0].src, src_in("drums"));
        assert_eq!(st.between_eras, 1);
        // Key 1 differs between era 1 and 2: ambiguous.
        let err = rekey_snapshots(&[snap(250, &[(1, 1.0)])], &c).unwrap_err();
        assert!(err.0[0].contains("ambiguous between eras [1, 2]"), "{err}");
        // Key 2 exists in era 3 only as the unaliased spare: era 2 wins.
        let (s, _) = rekey_snapshots(&[snap(450, &[(2, 1.0)])], &c).unwrap();
        assert_eq!(s[0].sends[0].src, src_in("mic1"));
        // Out of range keys never map.
        assert!(rekey_snapshots(&[snap(150, &[(0, 1.0)])], &c).is_err());
        assert!(rekey_snapshots(&[snap(150, &[(9, 1.0)])], &c).is_err());
    }

    #[test]
    fn mix_viewers_map_other_members_buses() {
        let e = env();
        let c = ctx(&e, &e.viewer);
        let mut s = snap(150, &[(1, 1.0), (4, 1.0)]);
        let mut eq = HashMap::new();
        eq.insert(1, five());
        eq.insert(4, five());
        s.eq_bands = Some(eq);
        let (out, st) = rekey_snapshots(&[s], &c).unwrap();
        assert_eq!(out[0].sends[1].src, Source::Bus(BusId::new("member2")));
        assert_eq!(st.input_eqs, 1);
        assert_eq!(st.dropped_bus_eqs, 1);
        let eq = &out[0].input_eq[&InputId::new("mic1")];
        assert_eq!(eq.bands[1].kind, EqKind::LowShelf);
        assert_eq!(eq.bands[1].gain_db, 2.5);
        assert_eq!(eq.bands[2].gain_db, DB_OFF);
        assert_eq!(eq.bands[4].kind, EqKind::HighShelf);
    }

    #[test]
    fn presets_keep_db_and_stems_and_mark_archived_members() {
        let e = env();
        let mut old = e.member.clone();
        old.archived = true;
        let c = ctx(&e, &old);
        let mut channels = HashMap::new();
        channels.insert(
            3,
            ChannelPreset {
                vol: -12.5,
                mute: true,
                pan: 0.25,
            },
        );
        let mut entries = HashMap::new();
        entries.insert(
            "b".to_owned(),
            PresetEntry {
                name: "b".into(),
                channels: channels.clone(),
                created_at: 90,
                updated_at: 160,
                stems_level_db: Some(-4.0),
                eq_bands: None,
            },
        );
        entries.insert(
            "a".to_owned(),
            PresetEntry {
                name: "a".into(),
                channels,
                created_at: 90,
                updated_at: 550,
                stems_level_db: None,
                eq_bands: None,
            },
        );
        let (p, st) = rekey_presets(&entries, &c).unwrap();
        assert_eq!(st.items, 2);
        assert_eq!(p[0].name, "a");
        assert_eq!(p[1].name, "b");
        assert_eq!(p[1].sends[0].src, src_in("drums"));
        assert_eq!(p[1].sends[0].gain_db, -12.5);
        assert!(p[1].sends[0].muted);
        assert!((p[1].sends[0].pan + 0.5).abs() < 1e-12);
        assert_eq!(p[1].stems_fader_db, Some(-4.0));
        assert_eq!(p[0].stems_fader_db, None);
        assert!(p[0].archived);
        assert_eq!(p[0].legacy_member.as_deref(), Some("legacy3"));
    }

    #[test]
    fn bad_items_fail_with_their_label() {
        let e = env();
        let c = ctx(&e, &e.member);
        let mut channels = HashMap::new();
        channels.insert(
            1,
            ChannelPreset {
                vol: 0.0,
                mute: false,
                pan: 1.5,
            },
        );
        let mut entries = HashMap::new();
        entries.insert(
            "x".to_owned(),
            PresetEntry {
                name: "x".into(),
                channels,
                created_at: 0,
                updated_at: 150,
                stems_level_db: None,
                eq_bands: None,
            },
        );
        let err = rekey_presets(&entries, &c).unwrap_err();
        assert_eq!(
            err.0,
            vec!["preset \"x\": key 1: pan 1.5 is outside 0…1".to_owned()]
        );
        let mut s = snap(150, &[(1, 1.0)]);
        let mut eq = HashMap::new();
        eq.insert(1, five()[..4].to_vec());
        s.eq_bands = Some(eq);
        let err = rekey_snapshots(&[s.clone()], &c).unwrap_err();
        assert!(err.0[0].contains("EQ has 4 bands"), "{err}");
        let mut bad = five();
        bad[0].band_type = "notch".into();
        s.eq_bands = Some(HashMap::from([(1, bad)]));
        let err = rekey_snapshots(&[s], &c).unwrap_err();
        assert!(err.0[0].contains("\"notch\" is not supported"), "{err}");
    }

    #[test]
    fn customizations_use_the_newest_era() {
        let e = env();
        let c = ctx(&e, &e.member);
        let (p, h) = rekey_customization(
            &Customization {
                pinned: vec![1, 3],
                hidden: vec![],
            },
            &c,
        )
        .unwrap();
        assert_eq!(p, vec![src_in("mic1"), src_in("drums")]);
        assert!(h.is_empty());
        let err = rekey_customization(
            &Customization {
                pinned: vec![],
                hidden: vec![2, 4],
            },
            &c,
        )
        .unwrap_err();
        assert!(err.0[0].contains("keys [2, 4]"), "{err}");
        let none = Eras { era: vec![] };
        let c2 = Ctx {
            eras: &none,
            ..ctx(&e, &e.member)
        };
        assert!(rekey_customization(&Customization::default(), &c2).is_err());
    }

    #[test]
    fn pans_and_stats_add_up() {
        assert_eq!(pan(0.0), Ok(-1.0));
        assert_eq!(pan(1.0), Ok(1.0));
        assert!(pan(-0.1).is_err());
        let mut a = Stats {
            items: 1,
            sends: 2,
            input_eqs: 3,
            dropped_bus_eqs: 4,
            between_eras: 5,
        };
        let b = a;
        a += b;
        assert_eq!(
            a,
            Stats {
                items: 2,
                sends: 4,
                input_eqs: 6,
                dropped_bus_eqs: 8,
                between_eras: 10
            }
        );
    }

    #[test]
    fn presets_count_the_ones_between_eras() {
        let e = env();
        let c = ctx(&e, &e.member);
        let entry = |name: &str, updated_at: i64| PresetEntry {
            name: name.into(),
            channels: HashMap::from([(
                3,
                ChannelPreset {
                    vol: -6.0,
                    mute: false,
                    pan: 0.5,
                },
            )]),
            created_at: 90,
            updated_at,
            stems_level_db: None,
            eq_bands: None,
        };
        let entries = HashMap::from([
            ("inside".to_owned(), entry("inside", 160)),
            ("between".to_owned(), entry("between", 250)),
        ]);
        let (p, st) = rekey_presets(&entries, &c).unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(st.items, 2);
        assert_eq!(st.between_eras, 1);
        for x in &p {
            assert_eq!(x.sends[0].src, src_in("drums"), "{}", x.name);
        }
    }
}
