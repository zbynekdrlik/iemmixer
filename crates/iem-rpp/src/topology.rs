//! The engine topology as the importer sees it (#20 design note §4, §7): the
//! same content as the `[engine]` table of `site.toml` — inputs, groups,
//! mixes and the mixes each one hears — so a project and a site file compare
//! id by id. The project's own routing (which levels and group strips it can
//! hold) is [`Routing`]; its REAPER counts are [`Counts`].

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

use iem_engine_proto::{GroupId, InputId, MixId, Source};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopoInput {
    pub id: InputId,
    pub rx: Vec<u16>,
    pub talkback: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopoGroup {
    pub id: GroupId,
    pub inputs: Vec<InputId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopoMix {
    pub id: MixId,
    /// Two TX channels (stereo) or one (mono).
    pub tx: Vec<u16>,
    /// The mixes it hears.
    pub mixes: Vec<MixId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Topology {
    pub inputs: Vec<TopoInput>,
    pub groups: Vec<TopoGroup>,
    pub mixes: Vec<TopoMix>,
    pub engineer: Option<MixId>,
}

/// What a predecessor project can hold beyond the topology: the levels that
/// have a receive (an ungrouped input or a heard mix into a mix, a grouped
/// input into the mix's group instance) and the group strips that have an
/// instance (a stems bus). The engine holds a level for every input and a
/// strip for every group; the rest cannot be written back.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Routing {
    pub levels: BTreeSet<(MixId, Source)>,
    pub strips: BTreeSet<(MixId, GroupId)>,
}

impl Routing {
    pub fn has_level(&self, mix: &MixId, source: &Source) -> bool {
        self.levels.contains(&(mix.clone(), source.clone()))
    }

    pub fn has_strip(&self, mix: &MixId, group: &GroupId) -> bool {
        self.strips.contains(&(mix.clone(), group.clone()))
    }
}

/// Program spec §3.5 "Data": the predecessor project's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    /// Tracks: inputs, mixes and group instances (the master is no track).
    pub tracks: usize,
    /// Receives.
    pub sends: usize,
    /// ReaEQ instances.
    pub eqs: usize,
    pub limiters: usize,
    pub trims: usize,
}

const COUNT_NAMES: [&str; 5] = ["tracks", "sends", "eqs", "limiters", "trims"];

impl Counts {
    fn values(&self) -> [usize; 5] {
        [self.tracks, self.sends, self.eqs, self.limiters, self.trims]
    }

    /// Checks `--expect tracks=45,sends=268,…` (any subset, any order).
    pub fn check(&self, expect: &str) -> Result<(), String> {
        let mut wrong = Vec::new();
        let mut seen = false;
        for part in expect.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (name, value) = part
                .split_once('=')
                .ok_or_else(|| format!("--expect: {part:?} is not name=count"))?;
            let (name, value) = (name.trim(), value.trim());
            let want: usize = value
                .parse()
                .map_err(|_| format!("--expect: {part:?} has no count"))?;
            let pos = COUNT_NAMES.iter().position(|n| *n == name).ok_or_else(|| {
                format!("--expect: unknown count {name:?} (tracks, sends, eqs, limiters, trims)")
            })?;
            let got = self.values().get(pos).copied().unwrap_or_default();
            if got != want {
                wrong.push(format!("{name} {got}, expected {want}"));
            }
            seen = true;
        }
        if !seen {
            return Err("--expect: no counts given".into());
        }
        if wrong.is_empty() {
            Ok(())
        } else {
            Err(format!("counts differ: {}", wrong.join(", ")))
        }
    }
}

impl fmt::Display for Counts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = COUNT_NAMES
            .iter()
            .zip(self.values())
            .map(|(n, v)| format!("{n} {v}"))
            .collect();
        f.write_str(&parts.join(", "))
    }
}

fn opt(id: Option<&MixId>) -> String {
    id.map_or_else(|| "none".to_owned(), ToString::to_string)
}

fn list<T: fmt::Display>(ids: &[T]) -> String {
    let quoted: Vec<String> = ids.iter().map(|i| format!("\"{i}\"")).collect();
    format!("[{}]", quoted.join(", "))
}

fn sorted<T: Ord + Clone>(v: &[T]) -> Vec<T> {
    let mut v = v.to_vec();
    v.sort();
    v
}

impl Topology {
    pub fn input(&self, id: &InputId) -> Option<&TopoInput> {
        self.inputs.iter().find(|i| i.id == *id)
    }

    pub fn group(&self, id: &GroupId) -> Option<&TopoGroup> {
        self.groups.iter().find(|g| g.id == *id)
    }

    pub fn mix(&self, id: &MixId) -> Option<&TopoMix> {
        self.mixes.iter().find(|m| m.id == *id)
    }

    /// The group `input` belongs to.
    pub fn group_of(&self, input: &InputId) -> Option<&TopoGroup> {
        self.groups.iter().find(|g| g.inputs.contains(input))
    }

    /// Whether `mix` hears `source`: every input, and the mixes it lists.
    pub fn hears(&self, mix: &MixId, source: &Source) -> bool {
        match source {
            Source::Input(id) => self.mix(mix).is_some() && self.input(id).is_some(),
            Source::Mix(id) => self.mix(mix).is_some_and(|m| m.mixes.contains(id)),
        }
    }

    /// The highest card channel any input or mix uses (1 when none).
    pub fn max_channel(&self) -> u16 {
        self.inputs
            .iter()
            .flat_map(|i| i.rx.iter())
            .chain(self.mixes.iter().flat_map(|m| m.tx.iter()))
            .copied()
            .max()
            .unwrap_or(1)
    }

    /// Differences between this topology (the project's) and `site`'s, by id;
    /// the order of groups' inputs and of heard mixes does not matter.
    pub fn diff(&self, site: &Topology) -> Vec<String> {
        let mut out = Vec::new();
        let a: BTreeMap<&InputId, &TopoInput> = self.inputs.iter().map(|i| (&i.id, i)).collect();
        let b: BTreeMap<&InputId, &TopoInput> = site.inputs.iter().map(|i| (&i.id, i)).collect();
        for (id, x) in &a {
            match b.get(id) {
                None => out.push(format!("input {id}: in the project, not in site.toml")),
                Some(y) => {
                    if x.rx != y.rx {
                        out.push(format!(
                            "input {id}: rx {:?} in the project, {:?} in site.toml",
                            x.rx, y.rx
                        ));
                    }
                    if x.talkback != y.talkback {
                        out.push(format!(
                            "input {id}: talkback {} in the project, {} in site.toml",
                            x.talkback, y.talkback
                        ));
                    }
                }
            }
        }
        for id in b.keys().filter(|id| !a.contains_key(*id)) {
            out.push(format!("input {id}: in site.toml, not in the project"));
        }
        let a: BTreeMap<&GroupId, &TopoGroup> = self.groups.iter().map(|g| (&g.id, g)).collect();
        let b: BTreeMap<&GroupId, &TopoGroup> = site.groups.iter().map(|g| (&g.id, g)).collect();
        for (id, x) in &a {
            match b.get(id) {
                None => out.push(format!("group {id}: in the project, not in site.toml")),
                Some(y) if sorted(&x.inputs) != sorted(&y.inputs) => out.push(format!(
                    "group {id}: inputs {} in the project, {} in site.toml",
                    list(&sorted(&x.inputs)),
                    list(&sorted(&y.inputs))
                )),
                Some(_) => {}
            }
        }
        for id in b.keys().filter(|id| !a.contains_key(*id)) {
            out.push(format!("group {id}: in site.toml, not in the project"));
        }
        let a: BTreeMap<&MixId, &TopoMix> = self.mixes.iter().map(|m| (&m.id, m)).collect();
        let b: BTreeMap<&MixId, &TopoMix> = site.mixes.iter().map(|m| (&m.id, m)).collect();
        for (id, x) in &a {
            match b.get(id) {
                None => out.push(format!("mix {id}: in the project, not in site.toml")),
                Some(y) => {
                    if x.tx != y.tx {
                        out.push(format!(
                            "mix {id}: tx {:?} in the project, {:?} in site.toml",
                            x.tx, y.tx
                        ));
                    }
                    if sorted(&x.mixes) != sorted(&y.mixes) {
                        out.push(format!(
                            "mix {id}: hears {} in the project, {} in site.toml",
                            list(&sorted(&x.mixes)),
                            list(&sorted(&y.mixes))
                        ));
                    }
                }
            }
        }
        for id in b.keys().filter(|id| !a.contains_key(*id)) {
            out.push(format!("mix {id}: in site.toml, not in the project"));
        }
        if self.engineer != site.engineer {
            out.push(format!(
                "engineer: {} in the project, {} in site.toml",
                opt(self.engineer.as_ref()),
                opt(site.engineer.as_ref())
            ));
        }
        out
    }

    /// The mixes in an order where every mix follows the mixes it hears
    /// (the engine's define-before-use rule), otherwise as listed; mixes in a
    /// hearing cycle keep their place at the end (the engine refuses them).
    pub fn declaration_order(&self) -> Vec<&TopoMix> {
        let mut done: Vec<&TopoMix> = Vec::with_capacity(self.mixes.len());
        let mut left: Vec<&TopoMix> = self.mixes.iter().collect();
        loop {
            let ready = left
                .iter()
                .position(|m| m.mixes.iter().all(|h| done.iter().any(|d| d.id == *h)));
            match ready {
                Some(k) => done.push(left.remove(k)),
                None => break,
            }
        }
        done.extend(left);
        done
    }

    /// This topology as a `site.toml` `[engine]` table.
    pub fn engine_toml(&self, channels: u16) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "[engine]\nchannels = {channels}");
        if let Some(e) = &self.engineer {
            let _ = writeln!(s, "engineer = \"{e}\"");
        }
        for i in &self.inputs {
            let _ = write!(
                s,
                "\n[[engine.inputs]]\nid = \"{}\"\nrx = {:?}\n",
                i.id, i.rx
            );
            if i.talkback {
                s.push_str("talkback = true\n");
            }
        }
        for g in &self.groups {
            let _ = write!(
                s,
                "\n[[engine.groups]]\nid = \"{}\"\ninputs = {}\n",
                g.id,
                list(&g.inputs)
            );
        }
        for m in self.declaration_order() {
            let _ = write!(
                s,
                "\n[[engine.mixes]]\nid = \"{}\"\ntx = {:?}\n",
                m.id, m.tx
            );
            if !m.mixes.is_empty() {
                let _ = writeln!(s, "mixes = {}", list(&m.mixes));
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sitegen::synthetic_site;

    fn mix(id: &str) -> MixId {
        MixId::new(id)
    }

    #[test]
    fn expect_checks_any_subset() {
        let c = Counts {
            tracks: 45,
            sends: 268,
            eqs: 44,
            limiters: 10,
            trims: 24,
        };
        assert_eq!(
            c.to_string(),
            "tracks 45, sends 268, eqs 44, limiters 10, trims 24"
        );
        assert_eq!(
            c.check("tracks=45,sends=268,eqs=44,limiters=10,trims=24"),
            Ok(())
        );
        assert_eq!(c.check(" sends = 268 "), Ok(()));
        assert_eq!(
            c.check("tracks=44,trims=24,sends=1"),
            Err("counts differ: tracks 45, expected 44, sends 268, expected 1".into())
        );
        assert!(c.check("tracks").unwrap_err().contains("name=count"));
        assert!(c.check("tracks=x").unwrap_err().contains("no count"));
        assert!(c.check("colours=1").unwrap_err().contains("unknown count"));
        assert!(c.check(" , ").unwrap_err().contains("no counts"));
    }

    #[test]
    fn equal_topologies_in_another_order_have_no_diff() {
        let a = synthetic_site();
        let mut b = a.clone();
        b.inputs.reverse();
        b.mixes.reverse();
        for m in &mut b.mixes {
            m.mixes.reverse();
        }
        for g in &mut b.groups {
            g.inputs.reverse();
        }
        assert!(a.diff(&b).is_empty());
    }

    #[test]
    fn diff_names_every_difference() {
        let a = synthetic_site();
        let mut b = a.clone();
        b.inputs[0].rx = vec![1];
        b.inputs[13].talkback = false;
        b.inputs.remove(1);
        b.inputs.push(TopoInput {
            id: InputId::new("extra"),
            rx: vec![150],
            talkback: false,
        });
        b.groups[0].inputs.pop();
        b.groups.push(TopoGroup {
            id: GroupId::new("more"),
            inputs: vec![InputId::new("extra")],
        });
        let m1 = b.mixes.iter().position(|m| m.id == mix("member1")).unwrap();
        b.mixes[m1].tx = vec![1, 2];
        b.mixes[m1].mixes.pop();
        let m2 = b.mixes.iter().position(|m| m.id == mix("member3")).unwrap();
        b.mixes.remove(m2);
        b.mixes.push(TopoMix {
            id: mix("spare"),
            tx: vec![150],
            mixes: vec![],
        });
        b.engineer = None;
        let d = a.diff(&b);
        for want in [
            "input mic1: rx [101] in the project, [1] in site.toml",
            "input eng_mic: talkback true in the project, false in site.toml",
            "input mic2: in the project, not in site.toml",
            "input extra: in site.toml, not in the project",
            "group stems: inputs [\"bass\", \"bgvs\", \"click\", \"drums\", \"guide\", \"inst\", \"other\"] in the project, [\"bass\", \"click\", \"drums\", \"guide\", \"inst\", \"other\"] in site.toml",
            "group more: in site.toml, not in the project",
            "mix member1: tx [71, 72] in the project, [1, 2] in site.toml",
            "mix member1: hears [\"member2\", \"member3\", \"member4\", \"member5\", \"member6\", \"member7\", \"member8\", \"member9\"] in the project, [\"member2\", \"member3\", \"member4\", \"member5\", \"member6\", \"member7\", \"member8\"] in site.toml",
            "mix member3: in the project, not in site.toml",
            "mix spare: in site.toml, not in the project",
            "engineer: engineer in the project, none in site.toml",
        ] {
            assert!(d.contains(&want.to_owned()), "missing {want:?} in {d:#?}");
        }
        assert_eq!(d.len(), 11, "{d:#?}");
        // The reverse direction names the other side.
        let back = b.diff(&a);
        assert!(back.contains(&"group more: in the project, not in site.toml".to_owned()));
        assert!(back.contains(&"mix spare: in the project, not in site.toml".to_owned()));
        assert!(back.contains(&"engineer: none in the project, engineer in site.toml".to_owned()));
    }

    #[derive(serde::Deserialize)]
    struct File {
        engine: Engine,
    }

    #[derive(serde::Deserialize)]
    struct Engine {
        channels: u16,
        engineer: String,
        inputs: Vec<toml::Value>,
        groups: Vec<Group>,
        mixes: Vec<Mix>,
    }

    #[derive(serde::Deserialize)]
    struct Group {
        id: String,
        inputs: Vec<String>,
    }

    #[derive(serde::Deserialize)]
    struct Mix {
        id: String,
        tx: Vec<u16>,
        #[serde(default)]
        mixes: Vec<String>,
    }

    #[test]
    fn the_engine_table_round_trips_through_toml_in_declaration_order() {
        let t = synthetic_site();
        let text = t.engine_toml(160);
        let file: File = toml::from_str(&text).unwrap();
        assert_eq!(file.engine.channels, 160);
        assert_eq!(file.engine.engineer, "engineer");
        assert_eq!(file.engine.inputs.len(), 24);
        let talkback = file
            .engine
            .inputs
            .iter()
            .filter(|i| i.get("talkback").is_some())
            .count();
        assert_eq!(talkback, 1);
        assert_eq!(file.engine.groups.len(), 1);
        assert_eq!(file.engine.groups[0].id, "stems");
        assert_eq!(file.engine.groups[0].inputs.len(), 7);
        assert_eq!(file.engine.mixes.len(), 11);
        // Every mix follows the mixes it hears.
        for (k, m) in file.engine.mixes.iter().enumerate() {
            for h in &m.mixes {
                let at = file.engine.mixes.iter().position(|x| x.id == *h).unwrap();
                assert!(at < k, "{} before {}", h, m.id);
            }
        }
        let tr = file
            .engine
            .mixes
            .iter()
            .find(|m| m.id == "translator")
            .unwrap();
        assert_eq!(tr.tx, vec![93]);
        assert!(tr.mixes.is_empty() && !text.contains("mixes = []"));
        let no_engineer = Topology {
            engineer: None,
            ..t
        };
        assert!(!no_engineer.engine_toml(8).contains("engineer ="));
    }

    #[test]
    fn declaration_order_puts_heard_mixes_first_and_keeps_cycles_last() {
        let m = |id: &str, hears: &[&str]| TopoMix {
            id: mix(id),
            tx: vec![1, 2],
            mixes: hears.iter().map(|h| mix(h)).collect(),
        };
        let t = Topology {
            mixes: vec![
                m("eng", &["a", "b"]),
                m("a", &["b"]),
                m("b", &[]),
                m("x", &["y"]),
                m("y", &["x"]),
            ],
            ..Topology::default()
        };
        let order: Vec<&str> = t
            .declaration_order()
            .iter()
            .map(|m| m.id.0.as_str())
            .collect();
        assert_eq!(order, vec!["b", "a", "eng", "x", "y"]);
    }

    #[test]
    fn the_highest_channel_counts_rx_and_tx() {
        let mut t = synthetic_site();
        assert_eq!(t.max_channel(), 132);
        t.mixes[0].tx = vec![150, 151];
        assert_eq!(t.max_channel(), 151);
        assert_eq!(Topology::default().max_channel(), 1);
    }

    #[test]
    fn lookups_find_ids_and_what_a_mix_hears() {
        let t = synthetic_site();
        assert!(t.input(&InputId::new("mic1")).is_some());
        assert!(t.input(&InputId::new("nope")).is_none());
        assert_eq!(t.mix(&mix("translator")).unwrap().tx, vec![93]);
        assert!(t.group(&GroupId::new("stems")).is_some());
        assert!(t.group(&GroupId::new("nope")).is_none());
        assert_eq!(
            t.group_of(&InputId::new("drums")).map(|g| g.id.0.as_str()),
            Some("stems")
        );
        assert!(t.group_of(&InputId::new("mic1")).is_none());
        let heard = Source::Mix(mix("member2"));
        assert!(t.hears(&mix("member1"), &heard));
        assert!(t.hears(&mix("engineer"), &heard));
        assert!(!t.hears(&mix("member3"), &heard));
        assert!(t.hears(&mix("translator"), &Source::Input(InputId::new("drums"))));
        assert!(!t.hears(&mix("ghost"), &Source::Input(InputId::new("mic1"))));
        assert!(!t.hears(&mix("member1"), &Source::Input(InputId::new("ghost"))));
        let mut r = Routing::default();
        r.levels.insert((mix("member1"), heard.clone()));
        r.strips.insert((mix("member1"), GroupId::new("stems")));
        assert!(r.has_level(&mix("member1"), &heard));
        assert!(!r.has_level(&mix("member2"), &heard));
        assert!(r.has_strip(&mix("member1"), &GroupId::new("stems")));
        assert!(!r.has_strip(&mix("translator"), &GroupId::new("stems")));
    }
}
