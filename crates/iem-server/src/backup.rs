//! Backups v2 (F19) and restore with preview (F31; S5 design note §6): a
//! backup is the engine's state from the mirror plus every member's pins and
//! hides; the preview is a diff of the running state and the backup in the
//! UI's names; the restore is one `ImportState` (the engine applies it
//! atomically with its ramps) plus the pins and hides.

use iem_core::backup::{MixerBackup, RestoreCategory, RestoreChange, RestorePreview, SkippedEntry};
use iem_core::band::CustomizationFile;
use iem_engine_proto::{Eq, InputState, Level, MixState, Source};

use crate::band_store::BandStore;
use crate::engine::mirror::Mirror;
use crate::site_view::SiteView;
use crate::view::GROUP_NAME;

/// A backup of the mirror's state at `timestamp` (RFC 3339), refused when
/// a member's pins and hides cannot be read: never a partial backup.
pub fn capture(
    view: &SiteView,
    mirror: &Mirror,
    store: &BandStore,
    timestamp: String,
) -> Result<MixerBackup, String> {
    let mut b = MixerBackup::new(timestamp, mirror.rev, mirror.state.clone());
    for m in &view.members {
        let c = store
            .customization(&m.id)
            .map_err(|e| format!("the pins and hides of {} are unreadable: {e}", m.id))?;
        b.customizations.insert(m.id.clone(), c);
    }
    Ok(b)
}

fn db(v: f64) -> String {
    if v <= iem_engine_proto::DB_OFF {
        "off".into()
    } else {
        format!("{v:.1} dB")
    }
}

fn pan(p: f64) -> String {
    let pct = (p.abs() * 100.0).round();
    if pct < 1.0 {
        "C".into()
    } else if p < 0.0 {
        format!("L{pct:.0}")
    } else {
        format!("R{pct:.0}")
    }
}

fn level(l: &Level) -> String {
    let mut s = format!("{} {}", db(l.gain_db), pan(l.pan));
    if l.muted {
        s.push_str(" muted");
    }
    s
}

fn on(b: bool) -> String {
    if b { "on".into() } else { "off".into() }
}

/// The enabled bands of an EQ, e.g. "1000 Hz +3.0 dB 1.00 oct".
pub fn eq_summary(e: &Eq) -> String {
    let bands: Vec<String> = e
        .bands
        .iter()
        .filter(|b| b.enabled)
        .map(|b| {
            format!(
                "{:.0} Hz {:+.1} dB {:.2} oct",
                b.freq_hz, b.gain_db, b.bw_oct
            )
        })
        .collect();
    if bands.is_empty() {
        "flat".into()
    } else {
        bands.join(", ")
    }
}

struct Diff {
    p: RestorePreview,
}

impl Diff {
    fn value(&mut self, cat: RestoreCategory, what: String, cur: String, new: String) {
        if cur == new {
            self.p.unchanged_count += 1;
        } else {
            self.p.changes.push(RestoreChange {
                category: cat,
                description: what,
                current_value: cur,
                backup_value: new,
            });
        }
    }

    fn eq(&mut self, what: String, cur: &Eq, new: &Eq) {
        if cur == new {
            self.p.unchanged_count += 1;
        } else {
            self.p.changes.push(RestoreChange {
                category: RestoreCategory::Eq,
                description: what,
                current_value: eq_summary(cur),
                backup_value: eq_summary(new),
            });
        }
    }

    fn skip(&mut self, cat: RestoreCategory, what: String) {
        self.p.skipped.push(SkippedEntry {
            category: cat,
            description: what,
            reason: "not in the running topology".into(),
        });
    }
}

fn input_diff(d: &mut Diff, name: &str, cur: &InputState, new: &InputState) {
    d.value(
        RestoreCategory::Input,
        format!("{name}: trim"),
        db(cur.trim_db),
        db(new.trim_db),
    );
    d.value(
        RestoreCategory::Input,
        format!("{name}: processing"),
        on(cur.processing),
        on(new.processing),
    );
    d.value(
        RestoreCategory::Input,
        format!("{name}: mute"),
        on(cur.muted),
        on(new.muted),
    );
    d.eq(format!("{name} EQ"), &cur.eq, &new.eq);
}

/// What restoring `backup` over `current` would change.
pub fn preview(
    view: &SiteView,
    current: &MixState,
    current_customizations: &dyn Fn(&str) -> Option<CustomizationFile>,
    backup: &MixerBackup,
) -> RestorePreview {
    let mut d = Diff {
        p: RestorePreview::default(),
    };
    for (id, new) in &backup.state.inputs {
        let Some(i) = view.input(&id.0) else {
            d.skip(RestoreCategory::Input, id.0.clone());
            continue;
        };
        let cur = current.inputs.get(id).copied().unwrap_or_default();
        input_diff(&mut d, &i.name, &cur, new);
    }
    for (mix, new) in &backup.state.mixes {
        if view.mix(mix).is_none() {
            d.skip(RestoreCategory::Output, mix.0.clone());
            continue;
        }
        let name = view.mix_name(mix);
        let cur = current.mixes.get(mix).cloned().unwrap_or_default();
        d.value(
            RestoreCategory::Output,
            format!("{name}: IEM VOL"),
            db(cur.out.volume_db),
            db(new.out.volume_db),
        );
        d.value(
            RestoreCategory::Output,
            format!("{name}: IEM VOL mute"),
            on(cur.out.muted),
            on(new.out.muted),
        );
        d.eq(format!("{name}: IEM VOL EQ"), &cur.out.eq, &new.out.eq);
        let lim = |l: &iem_engine_proto::Limiter| format!("{} {}", on(l.enabled), db(l.limit_db));
        d.value(
            RestoreCategory::Limiter,
            format!("{name}: limiter"),
            lim(&cur.out.limiter),
            lim(&new.out.limiter),
        );
        for (src, l) in new
            .inputs
            .iter()
            .map(|(i, l)| (Source::Input(i.clone()), l))
            .chain(new.mixes.iter().map(|(m, l)| (Source::Mix(m.clone()), l)))
        {
            let src_name = match &src {
                Source::Input(i) => view.input(&i.0).map(|v| v.name.clone()),
                Source::Mix(m) => view.mix(m).map(|_| view.mix_name(m)),
            };
            let Some(src_name) = src_name else {
                d.skip(RestoreCategory::Level, format!("{src} → {name}"));
                continue;
            };
            let c = cur.level(&src).copied().unwrap_or_default();
            d.value(
                RestoreCategory::Level,
                format!("{src_name} → {name}"),
                level(&c),
                level(l),
            );
        }
        for (g, strip) in &new.groups {
            if view.group.as_ref() != Some(g) {
                d.skip(RestoreCategory::Group, format!("{g} → {name}"));
                continue;
            }
            let c = cur.groups.get(g).copied().unwrap_or_default();
            let fmt = |s: &iem_engine_proto::MixGroup| {
                let mut t = db(s.gain_db);
                if s.muted {
                    t.push_str(" muted");
                }
                t
            };
            d.value(
                RestoreCategory::Group,
                format!("{GROUP_NAME} → {name}"),
                fmt(&c),
                fmt(strip),
            );
            d.eq(format!("{GROUP_NAME} → {name} EQ"), &c.eq, &strip.eq);
        }
    }
    for (member, c) in &backup.customizations {
        let Some(m) = view.member(member) else {
            d.skip(RestoreCategory::Customization, member.clone());
            continue;
        };
        let cur = current_customizations(member).unwrap_or_default();
        let fmt =
            |f: &CustomizationFile| format!("{} pinned, {} hidden", f.pinned.len(), f.hidden.len());
        if cur.pinned == c.pinned && cur.hidden == c.hidden {
            d.p.unchanged_count += 1;
        } else {
            d.p.changes.push(RestoreChange {
                category: RestoreCategory::Customization,
                description: format!("{}: pins and hides", m.name),
                current_value: fmt(&cur),
                backup_value: fmt(c),
            });
        }
    }
    d.p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site_view::tests::test_view;
    use iem_engine_proto::{EngineMsg, GroupId, InputId, MixGroup, MixId, Transient};

    fn state_with(f: impl FnOnce(&mut MixState)) -> MixState {
        let mut s = MixState::default();
        f(&mut s);
        s
    }

    #[test]
    fn formats_are_readable() {
        assert_eq!(db(-6.0), "-6.0 dB");
        assert_eq!(db(iem_engine_proto::DB_OFF), "off");
        assert_eq!(pan(0.0), "C");
        assert_eq!(pan(-0.5), "L50");
        assert_eq!(pan(1.0), "R100");
        assert_eq!(pan(0.004), "C");
        assert_eq!(
            level(&Level {
                gain_db: 3.0,
                pan: 0.25,
                muted: true
            }),
            "3.0 dB R25 muted"
        );
        let mut e = Eq::default();
        assert_eq!(eq_summary(&e), "flat");
        e.bands[2].enabled = true;
        e.bands[2].gain_db = 3.0;
        assert_eq!(eq_summary(&e), "802 Hz +3.0 dB 1.00 oct");
        assert_eq!(on(true), "on");
    }

    #[test]
    fn a_pan_of_one_percent_is_off_centre() {
        assert_eq!(pan(0.01), "R1");
        assert_eq!(pan(-0.01), "L1");
        assert_eq!(pan(0.0049), "C");
    }

    #[test]
    fn preview_counts_every_value_that_already_matches() {
        let v = test_view();
        let current = state_with(|s| {
            s.inputs.entry(InputId::new("keys")).or_default().trim_db = 2.0;
        });
        let mut b = MixerBackup::new("t".into(), 1, current.clone());
        let pins =
            CustomizationFile::new("member2", vec![Source::Input(InputId::new("keys"))], vec![]);
        b.customizations.insert("member2".into(), pins.clone());
        let p = preview(
            &v,
            &current,
            &|m| (m == "member2").then(|| pins.clone()),
            &b,
        );
        assert!(p.changes.is_empty(), "{:?}", p.changes);
        assert!(p.skipped.is_empty());
        // KEYS: trim, processing, mute and EQ; member2's pins and hides.
        assert_eq!(p.unchanged_count, 4 + 1);
    }

    #[test]
    fn a_level_from_a_missing_source_is_skipped() {
        let v = test_view();
        let backup_state = state_with(|s| {
            let m = s.mixes.entry(MixId::new("member1")).or_default();
            m.inputs.insert(InputId::new("gone"), Level::default());
            m.inputs.insert(
                InputId::new("mic3"),
                Level {
                    gain_db: -3.0,
                    ..Level::default()
                },
            );
            m.mixes.insert(MixId::new("oldmix"), Level::default());
            m.groups
                .insert(GroupId::new("oldgroup"), MixGroup::default());
        });
        let b = MixerBackup::new("t".into(), 1, backup_state);
        let p = preview(&v, &MixState::default(), &|_| None, &b);
        let skipped: Vec<(RestoreCategory, &str, &str)> = p
            .skipped
            .iter()
            .map(|s| (s.category, s.description.as_str(), s.reason.as_str()))
            .collect();
        let why = "not in the running topology";
        assert_eq!(
            skipped,
            [
                (RestoreCategory::Level, "gone → Member1", why),
                (RestoreCategory::Level, "oldmix → Member1", why),
                (RestoreCategory::Group, "oldgroup → Member1", why),
            ]
        );
        // The source the topology has is compared; the others are not.
        let changed: Vec<&str> = p.changes.iter().map(|c| c.description.as_str()).collect();
        assert_eq!(changed, ["MEMBER3 mic → Member1"]);
    }

    #[test]
    fn a_preview_lists_what_the_backup_does_not_have() {
        let v = test_view();
        let ids =
            |list: &[&str]| -> Vec<InputId> { list.iter().map(|i| InputId::new(*i)).collect() };
        // The running state (the engine sends every id of its topology).
        let current = state_with(|s| {
            for i in ids(&["keys", "mic1", "mic2"]) {
                s.inputs.insert(i.clone(), InputState::default());
                for m in ["member1", "member2"] {
                    let mix = s.mixes.entry(MixId::new(m)).or_default();
                    mix.inputs.insert(i.clone(), Level::default());
                }
            }
            let m1 = s.mixes.entry(MixId::new("member1")).or_default();
            m1.mixes.insert(MixId::new("member2"), Level::default());
            m1.mixes.insert(MixId::new("member3"), Level::default());
            m1.groups.insert(GroupId::new("stems"), MixGroup::default());
            m1.groups.insert(GroupId::new("extra"), MixGroup::default());
            s.mixes.insert(MixId::new("member3"), Default::default());
        });
        // A backup from before keys and member3's mix were added, and before
        // member1 heard member2, got the stems strip and a level of mic2.
        let backup_state = state_with(|s| {
            for i in ids(&["mic1", "mic2"]) {
                s.inputs.insert(i.clone(), InputState::default());
                s.mixes
                    .entry(MixId::new("member2"))
                    .or_default()
                    .inputs
                    .insert(i, Level::default());
            }
            s.mixes
                .entry(MixId::new("member1"))
                .or_default()
                .inputs
                .insert(InputId::new("mic1"), Level::default());
        });
        let b = MixerBackup::new("t".into(), 1, backup_state);
        let p = preview(&v, &current, &|_| None, &b);
        let kept: Vec<(RestoreCategory, &str)> = p
            .not_in_backup
            .iter()
            .map(|k| (k.category, k.description.as_str()))
            .collect();
        // A new input or mix is one line; its levels in the mixes are in it.
        assert_eq!(
            kept,
            [
                (RestoreCategory::Input, "KEYS"),
                (RestoreCategory::Level, "MEMBER2 mic → Member1"),
                (RestoreCategory::Level, "Member2 → Member1"),
                (RestoreCategory::Group, "extra → Member1"),
                (RestoreCategory::Group, "STEMS → Member1"),
                (RestoreCategory::Output, "Member3"),
            ]
        );
        assert!(
            p.not_in_backup
                .iter()
                .all(|k| k.reason == "not in the backup; stays as it is")
        );
        assert!(p.skipped.is_empty(), "{:?}", p.skipped);
        assert!(p.changes.is_empty(), "{:?}", p.changes);
        // A backup of the running state lacks nothing.
        let same = MixerBackup::new("t".into(), 1, current.clone());
        assert!(
            preview(&v, &current, &|_| None, &same)
                .not_in_backup
                .is_empty()
        );
    }

    #[test]
    fn capture_takes_the_mirror_and_every_members_pins() {
        let v = test_view();
        let dir = tempfile::tempdir().unwrap();
        let store = BandStore::new(dir.path());
        store
            .save_customization("member1", vec![Source::Input(InputId::new("mic1"))], vec![])
            .unwrap();
        let mut m = Mirror::default();
        let state = state_with(|s| {
            s.mixes
                .entry(MixId::new("member1"))
                .or_default()
                .out
                .volume_db = -3.0;
        });
        m.apply(&EngineMsg::State {
            rev: 9,
            state: state.clone(),
            transient: Transient::default(),
        });
        let b = capture(&v, &m, &store, "2026-09-27T13:00:00Z".into()).unwrap();
        assert_eq!((b.rev, &b.state), (9, &state));
        assert_eq!(b.customizations.len(), 10);
        assert_eq!(b.customizations["member1"].pinned.len(), 1);
        let json = serde_json::to_string(&b).unwrap();
        assert!(!json.to_lowercase().contains("pin_"), "no PIN field");
    }

    #[test]
    fn capture_refuses_when_a_members_pins_cannot_be_read() {
        let v = test_view();
        let dir = tempfile::tempdir().unwrap();
        let store = BandStore::new(dir.path());
        store
            .save_customization("member1", vec![Source::Input(InputId::new("mic1"))], vec![])
            .unwrap();
        // A folder where member3's file belongs: reading it fails (it is not
        // "no pins"), and a backup without member3's pins is no backup.
        let broken = dir.path().join("customizations").join("member3.json");
        std::fs::create_dir_all(&broken).unwrap();
        let err = capture(&v, &Mirror::default(), &store, "t".into()).unwrap_err();
        assert!(err.contains("member3"), "{err}");
        // Readable again: every member's pins are in.
        std::fs::remove_dir(&broken).unwrap();
        let b = capture(&v, &Mirror::default(), &store, "t".into()).unwrap();
        assert_eq!(b.customizations.len(), 10);
        assert_eq!(b.customizations["member1"].pinned.len(), 1);
    }

    #[test]
    fn preview_lists_each_difference_in_the_uis_names() {
        let v = test_view();
        let current = state_with(|s| {
            let m = s.mixes.entry(MixId::new("member1")).or_default();
            m.inputs.insert(
                InputId::new("mic3"),
                Level {
                    gain_db: -6.0,
                    pan: 0.0,
                    muted: false,
                },
            );
        });
        let backup_state = state_with(|s| {
            let m = s.mixes.entry(MixId::new("member1")).or_default();
            m.inputs.insert(
                InputId::new("mic3"),
                Level {
                    gain_db: -3.0,
                    pan: 0.0,
                    muted: false,
                },
            );
            m.out.limiter.enabled = false;
            m.groups.insert(
                GroupId::new("stems"),
                MixGroup {
                    gain_db: 2.0,
                    ..MixGroup::default()
                },
            );
            m.mixes.insert(MixId::new("member2"), Level::default());
            let k = s.inputs.entry(InputId::new("keys")).or_default();
            k.trim_db = 4.0;
            k.eq.bands[0].enabled = true;
            s.inputs.insert(InputId::new("gone"), InputState::default());
            s.mixes.insert(MixId::new("oldmix"), Default::default());
        });
        let mut b = MixerBackup::new("t".into(), 1, backup_state);
        b.customizations.insert(
            "member2".into(),
            CustomizationFile::new("member2", vec![Source::Input(InputId::new("keys"))], vec![]),
        );
        b.customizations.insert(
            "ghost".into(),
            CustomizationFile::new("ghost", vec![], vec![]),
        );
        let p = preview(&v, &current, &|_| None, &b);
        let find = |d: &str| p.changes.iter().find(|c| c.description == d).cloned();
        let lvl = find("MEMBER3 mic → Member1").expect("the level");
        assert_eq!(
            (
                lvl.category,
                lvl.current_value.as_str(),
                lvl.backup_value.as_str()
            ),
            (RestoreCategory::Level, "-6.0 dB C", "-3.0 dB C")
        );
        assert_eq!(
            find("Member1: limiter").unwrap().backup_value,
            "off -6.0 dB"
        );
        assert_eq!(find("STEMS → Member1").unwrap().backup_value, "2.0 dB");
        assert_eq!(find("KEYS: trim").unwrap().backup_value, "4.0 dB");
        assert_eq!(find("KEYS EQ").unwrap().category, RestoreCategory::Eq);
        assert_eq!(
            find("Member2: pins and hides").unwrap().backup_value,
            "1 pinned, 0 hidden"
        );
        assert!(find("Member2 → Member1").is_none(), "equal: unchanged");
        let skipped: Vec<&str> = p.skipped.iter().map(|s| s.description.as_str()).collect();
        assert_eq!(skipped, ["gone", "oldmix", "ghost"]);
        assert!(p.unchanged_count > 0);
        // Restoring the running state changes nothing.
        let same = preview(
            &v,
            &current,
            &|_| None,
            &MixerBackup::new("t".into(), 1, current.clone()),
        );
        assert!(same.changes.is_empty(), "{:?}", same.changes);
    }
}
