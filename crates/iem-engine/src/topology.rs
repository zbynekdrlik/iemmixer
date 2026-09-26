//! The topology compiled once per run from the site (program spec I4; #20
//! design note §3, §4, §6): validated, inputs with their group, groups with
//! their inputs, the ungrouped inputs every mix sums directly, and mixes in
//! declaration order with the earlier mixes each one hears. There is no graph
//! to sort: a mix may hear only mixes declared before it.
//!
//! Every mix has one level per input and one per heard mix; a mix's level
//! slots are the inputs in site order, then its heard mixes in its order.

use std::collections::{BTreeSet, HashMap};

use iem_engine_proto::{
    GroupId, GroupInfo, InputId, InputInfo, MixId, MixInfo, Source, TopologyInfo, valid_id,
};
use sha2::{Digest, Sha256};

use crate::SAMPLE_RATE;
use crate::site::{Site, SiteError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputNode {
    pub id: InputId,
    /// Indices into [`Topology::rx`]; a mono input has the same index twice.
    pub rx: [usize; 2],
    pub stereo: bool,
    pub talkback: bool,
    /// Index into [`Topology::groups`].
    pub group: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupNode {
    pub id: GroupId,
    /// Indices into [`Topology::inputs`], in site order.
    pub inputs: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixNode {
    pub id: MixId,
    /// Indices into [`Topology::tx`]; a mono mix has only the first.
    pub tx: [Option<usize>; 2],
    pub mono: bool,
    /// The mixes it hears (indices below its own), in its order.
    pub mixes: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    pub inputs: Vec<InputNode>,
    pub groups: Vec<GroupNode>,
    /// In declaration order, which is the processing order.
    pub mixes: Vec<MixNode>,
    /// The inputs in no group, in site order: every mix sums them directly.
    pub direct: Vec<usize>,
    /// Card RX channels in the order the backend delivers them.
    pub rx: Vec<u16>,
    /// Card TX channels in the order the backend expects them.
    pub tx: Vec<u16>,
    pub engineer: usize,
    pub hash: String,
    input_index: HashMap<InputId, usize>,
    group_index: HashMap<GroupId, usize>,
    mix_index: HashMap<MixId, usize>,
}

fn check_channels(
    id: &str,
    chans: &[u16],
    map: u16,
    used: &mut BTreeSet<u16>,
) -> Result<(), SiteError> {
    for &ch in chans {
        if ch == 0 || ch > map {
            return Err(SiteError::ChannelRange { id: id.into(), ch });
        }
        if !used.insert(ch) {
            return Err(SiteError::ChannelReused { ch });
        }
    }
    Ok(())
}

fn check_id<'a>(id: &'a str, seen: &mut BTreeSet<&'a str>) -> Result<(), SiteError> {
    if !valid_id(id) {
        return Err(SiteError::BadId(id.into()));
    }
    if !seen.insert(id) {
        return Err(SiteError::DuplicateId(id.into()));
    }
    Ok(())
}

/// Validates the site and compiles its topology.
pub fn compile(site: &Site) -> Result<Topology, SiteError> {
    let mut seen = BTreeSet::new();
    for id in site
        .inputs
        .iter()
        .map(|i| i.id.as_str())
        .chain(site.groups.iter().map(|g| g.id.as_str()))
        .chain(site.mixes.iter().map(|m| m.id.as_str()))
    {
        check_id(id, &mut seen)?;
    }

    let input_pos: HashMap<&str, usize> = site
        .inputs
        .iter()
        .enumerate()
        .map(|(i, x)| (x.id.as_str(), i))
        .collect();
    let mut rx = Vec::new();
    let mut rx_used = BTreeSet::new();
    let mut talkback = false;
    let mut inputs = Vec::with_capacity(site.inputs.len());
    for input in &site.inputs {
        if !(1..=2).contains(&input.rx.len()) {
            return Err(SiteError::ChannelCount {
                id: input.id.clone(),
                expected: "1 or 2",
                got: input.rx.len(),
            });
        }
        check_channels(&input.id, &input.rx, site.channels, &mut rx_used)?;
        if input.talkback {
            if talkback {
                return Err(SiteError::SecondTalkback);
            }
            talkback = true;
        }
        let first = rx.len();
        rx.extend_from_slice(&input.rx);
        let stereo = input.rx.len() == 2;
        inputs.push(InputNode {
            id: InputId::new(input.id.clone()),
            rx: [first, if stereo { first + 1 } else { first }],
            stereo,
            talkback: input.talkback,
            group: None,
        });
    }

    let mut groups = Vec::with_capacity(site.groups.len());
    for (g, group) in site.groups.iter().enumerate() {
        if group.inputs.is_empty() {
            return Err(SiteError::EmptyGroup(group.id.clone()));
        }
        let mut members = Vec::with_capacity(group.inputs.len());
        for name in &group.inputs {
            let Some(&i) = input_pos.get(name.as_str()) else {
                return Err(SiteError::UnknownInput {
                    group: group.id.clone(),
                    input: name.clone(),
                });
            };
            let Some(node) = inputs.get_mut(i) else {
                continue;
            };
            if node.group.is_some() {
                return Err(SiteError::SecondGroup(name.clone()));
            }
            node.group = Some(g);
            members.push(i);
        }
        members.sort_unstable();
        groups.push(GroupNode {
            id: GroupId::new(group.id.clone()),
            inputs: members,
        });
    }
    let direct = inputs
        .iter()
        .enumerate()
        .filter(|(_, n)| n.group.is_none())
        .map(|(i, _)| i)
        .collect();

    let mut tx = Vec::new();
    let mut tx_used = BTreeSet::new();
    let mut declared: HashMap<&str, usize> = HashMap::new();
    let mut mixes = Vec::with_capacity(site.mixes.len());
    for (m, mix) in site.mixes.iter().enumerate() {
        if !(1..=2).contains(&mix.tx.len()) {
            return Err(SiteError::ChannelCount {
                id: mix.id.clone(),
                expected: "1 or 2",
                got: mix.tx.len(),
            });
        }
        check_channels(&mix.id, &mix.tx, site.channels, &mut tx_used)?;
        let mut heard = Vec::with_capacity(mix.mixes.len());
        for name in &mix.mixes {
            match declared.get(name.as_str()) {
                Some(&s) if !heard.contains(&s) => heard.push(s),
                _ => {
                    return Err(SiteError::HeardMix {
                        mix: mix.id.clone(),
                        heard: name.clone(),
                    });
                }
            }
        }
        declared.insert(mix.id.as_str(), m);
        let first = tx.len();
        tx.extend_from_slice(&mix.tx);
        let mono = mix.tx.len() == 1;
        mixes.push(MixNode {
            id: MixId::new(mix.id.clone()),
            tx: [Some(first), (!mono).then_some(first + 1)],
            mono,
            mixes: heard,
        });
    }
    let engineer = match declared.get(site.engineer.as_str()) {
        Some(&m) if mixes.get(m).is_some_and(|n| !n.mono) => m,
        _ => return Err(SiteError::Engineer(site.engineer.clone())),
    };

    let mut topo = Topology {
        input_index: inputs
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.clone(), i))
            .collect(),
        group_index: groups
            .iter()
            .enumerate()
            .map(|(g, n)| (n.id.clone(), g))
            .collect(),
        mix_index: mixes
            .iter()
            .enumerate()
            .map(|(m, n)| (n.id.clone(), m))
            .collect(),
        inputs,
        groups,
        mixes,
        direct,
        rx,
        tx,
        engineer,
        hash: String::new(),
    };
    let info = serde_json::to_vec(&topo.info()).unwrap_or_default();
    topo.hash = Sha256::digest(&info)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(topo)
}

impl Topology {
    pub fn input_index(&self, id: &InputId) -> Option<usize> {
        self.input_index.get(id).copied()
    }

    pub fn group_index(&self, id: &GroupId) -> Option<usize> {
        self.group_index.get(id).copied()
    }

    pub fn mix_index(&self, id: &MixId) -> Option<usize> {
        self.mix_index.get(id).copied()
    }

    /// Level slots of mix `m`: every input, then the mixes it hears.
    pub fn levels(&self, m: usize) -> usize {
        self.inputs.len() + self.mixes.get(m).map_or(0, |n| n.mixes.len())
    }

    /// The level slot of `source` in mix `m`; `None` when the mix does not
    /// hear it.
    pub fn slot(&self, m: usize, source: &Source) -> Option<usize> {
        let node = self.mixes.get(m)?;
        match source {
            Source::Input(id) => self.input_index(id),
            Source::Mix(id) => {
                let s = self.mix_index(id)?;
                let k = node.mixes.iter().position(|&h| h == s)?;
                Some(self.inputs.len() + k)
            }
        }
    }

    /// The source of level slot `k` in mix `m`.
    pub fn source(&self, m: usize, k: usize) -> Option<Source> {
        let node = self.mixes.get(m)?;
        if let Some(n) = self.inputs.get(k) {
            return Some(Source::Input(n.id.clone()));
        }
        let s = *node.mixes.get(k.checked_sub(self.inputs.len())?)?;
        self.mixes.get(s).map(|n| Source::Mix(n.id.clone()))
    }

    /// The topology as the protocol describes it (without its hash inside
    /// the hash computation).
    pub fn info(&self) -> TopologyInfo {
        let mix_id = |m: &usize| self.mixes.get(*m).map(|n| n.id.clone());
        TopologyInfo {
            hash: self.hash.clone(),
            sample_rate: SAMPLE_RATE,
            engineer: mix_id(&self.engineer).unwrap_or_else(|| MixId::new("")),
            inputs: self
                .inputs
                .iter()
                .map(|n| InputInfo {
                    id: n.id.clone(),
                    channels: if n.stereo { 2 } else { 1 },
                    talkback: n.talkback,
                    group: n
                        .group
                        .and_then(|g| self.groups.get(g))
                        .map(|g| g.id.clone()),
                })
                .collect(),
            groups: self
                .groups
                .iter()
                .map(|g| GroupInfo {
                    id: g.id.clone(),
                    inputs: g
                        .inputs
                        .iter()
                        .filter_map(|i| self.inputs.get(*i).map(|n| n.id.clone()))
                        .collect(),
                })
                .collect(),
            mixes: self
                .mixes
                .iter()
                .map(|n| MixInfo {
                    id: n.id.clone(),
                    channels: if n.mono { 1 } else { 2 },
                    mixes: n.mixes.iter().filter_map(mix_id).collect(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::parse;
    use crate::test_support::{test_site, test_site_text};

    fn mix(t: &Topology, id: &str) -> usize {
        t.mix_index(&MixId::new(id)).unwrap()
    }

    #[test]
    fn test_site_has_the_program_shape() {
        let t = test_site();
        assert_eq!(t.inputs.len(), 24);
        assert_eq!(t.rx.len(), 32);
        assert_eq!(t.mixes.len(), 11);
        assert_eq!(t.tx.len(), 21);
        assert_eq!(t.groups.len(), 1);
        assert_eq!(t.groups[0].id, GroupId::new("stems"));
        assert_eq!(t.groups[0].inputs.len(), 7);
        assert_eq!(t.direct.len(), 17);
        for &i in &t.groups[0].inputs {
            assert_eq!(t.inputs[i].group, Some(0));
            assert!(!t.direct.contains(&i));
        }
        assert_eq!(t.inputs.iter().filter(|n| n.talkback).count(), 1);
        assert_eq!(t.inputs.iter().filter(|n| n.stereo).count(), 8);
        let m1 = mix(&t, "member1");
        assert_eq!(t.mixes[m1].mixes.len(), 8);
        for m in 2..=9 {
            let s = mix(&t, &format!("member{m}"));
            assert!(s < m1, "member{m} is declared before member1");
            assert!(t.mixes[s].mixes.is_empty());
        }
        let eng = mix(&t, "engineer");
        assert_eq!(t.engineer, eng);
        assert_eq!(t.mixes[eng].mixes.len(), 9);
        assert!(t.mixes[eng].mixes.iter().all(|&s| s < eng));
        let tr = mix(&t, "translator");
        assert!(t.mixes[tr].mono);
        assert_eq!(t.mixes[tr].tx[1], None);
        assert_eq!(t.mixes.iter().filter(|n| n.mono).count(), 1);
        // Level slots: 24 inputs, then the heard mixes.
        assert_eq!(t.levels(m1), 32);
        assert_eq!(t.levels(eng), 33);
        assert_eq!(t.levels(tr), 24);
        assert_eq!(t.input_index(&InputId::new("keys")), Some(14));
        assert_eq!(t.inputs[14].rx, [14, 15]);
        assert_eq!(t.inputs[0].rx, [0, 0]);
        assert_eq!(t.mix_index(&MixId::new("nope")), None);
        assert_eq!(t.group_index(&GroupId::new("stems")), Some(0));
        assert_eq!(t.group_index(&GroupId::new("nope")), None);
        let info = t.info();
        assert_eq!(info.hash, t.hash);
        assert_eq!(info.engineer, MixId::new("engineer"));
        assert_eq!(
            info.mixes
                .iter()
                .map(|m| usize::from(m.channels))
                .sum::<usize>(),
            21
        );
        assert_eq!(info.groups[0].inputs.len(), 7);
        assert_eq!(info.inputs.iter().filter(|i| i.group.is_some()).count(), 7);
        assert_eq!(info.mixes[eng].mixes[0], MixId::new("member1"));
    }

    #[test]
    fn slots_map_sources_both_ways() {
        let t = test_site();
        let m1 = mix(&t, "member1");
        let eng = mix(&t, "engineer");
        let keys = Source::Input(InputId::new("keys"));
        assert_eq!(t.slot(m1, &keys), Some(14));
        assert_eq!(t.source(m1, 14), Some(keys));
        let m2 = Source::Mix(MixId::new("member2"));
        let k = t.slot(m1, &m2).unwrap();
        assert!(k >= 24);
        assert_eq!(t.source(m1, k), Some(m2));
        // The engineer hears member1 first.
        let heard = Source::Mix(MixId::new("member1"));
        assert_eq!(t.slot(eng, &heard), Some(24));
        assert_eq!(t.source(eng, 24), Some(heard.clone()));
        // member1 does not hear itself; the translator hears no mix.
        assert_eq!(t.slot(m1, &heard), None);
        assert_eq!(t.slot(mix(&t, "translator"), &m2_of()), None);
        assert_eq!(t.slot(m1, &Source::Input(InputId::new("ghost"))), None);
        assert_eq!(t.slot(m1, &Source::Mix(MixId::new("ghost"))), None);
        assert_eq!(t.slot(99, &m2_of()), None);
        assert_eq!(t.slot(99, &Source::Input(InputId::new("keys"))), None);
        assert_eq!(t.source(99, 0), None);
        assert_eq!(t.source(m1, 32), None);
        assert_eq!(t.source(99, 30), None);
    }

    fn m2_of() -> Source {
        Source::Mix(MixId::new("member2"))
    }

    #[test]
    fn hash_is_stable_and_topology_sensitive() {
        let a = test_site();
        assert_eq!(a.hash, test_site().hash);
        assert_eq!(a.hash.len(), 64);
        let text = test_site_text();
        let more = format!("{text}\n[[engine.mixes]]\nid = \"spare\"\ntx = [150]\n");
        let b = compile(&parse(&more).unwrap()).unwrap();
        assert_eq!(b.mixes.len(), 12);
        assert_ne!(a.hash, b.hash);
    }

    const BASE: &str = r#"
[engine]
channels = 16
engineer = "eng"
[[engine.inputs]]
id = "mic"
rx = [1]
[[engine.inputs]]
id = "keys"
rx = [2, 3]
[[engine.inputs]]
id = "drums"
rx = [4, 5]
[[engine.groups]]
id = "stems"
inputs = ["drums"]
[[engine.mixes]]
id = "m1"
tx = [3, 4]
[[engine.mixes]]
id = "tr"
tx = [5]
[[engine.mixes]]
id = "eng"
tx = [1, 2]
mixes = ["m1"]
"#;

    fn err(extra: &str) -> SiteError {
        compile(&parse(&format!("{BASE}{extra}")).unwrap()).unwrap_err()
    }

    fn replaced(from: &str, to: &str) -> SiteError {
        assert!(BASE.contains(from), "{from}");
        compile(&parse(&BASE.replacen(from, to, 1)).unwrap()).unwrap_err()
    }

    #[test]
    fn the_minimal_site_compiles() {
        let t = compile(&parse(BASE).unwrap()).unwrap();
        assert_eq!(t.rx, vec![1, 2, 3, 4, 5]);
        assert_eq!(t.tx, vec![3, 4, 5, 1, 2]);
        assert_eq!(t.direct, vec![0, 1]);
        assert_eq!(t.groups[0].inputs, vec![2]);
        let tr = mix(&t, "tr");
        assert_eq!(t.mixes[tr].tx, [Some(2), None]);
        assert!(t.mixes[tr].mono);
        assert_eq!(t.mixes[mix(&t, "eng")].tx, [Some(3), Some(4)]);
        assert_eq!(t.mixes[mix(&t, "eng")].mixes, vec![0]);
        assert_eq!(t.engineer, 2);
        assert_eq!(t.inputs[2].rx, [3, 4]);
    }

    #[test]
    fn group_members_are_in_site_order_whatever_the_group_lists() {
        let text = BASE.replacen("inputs = [\"drums\"]", "inputs = [\"drums\", \"mic\"]", 1);
        let t = compile(&parse(&text).unwrap()).unwrap();
        assert_eq!(t.groups[0].inputs, vec![0, 2]);
        assert_eq!(t.direct, vec![1]);
        assert_eq!(t.inputs[0].group, Some(0));
    }

    #[test]
    fn every_site_error_is_detected() {
        assert_eq!(
            err("[[engine.inputs]]\nid = \"eng\"\nrx = [9]\n"),
            SiteError::DuplicateId("eng".into())
        );
        assert_eq!(
            err("[[engine.inputs]]\nid = \"mic\"\nrx = [9]\n"),
            SiteError::DuplicateId("mic".into())
        );
        assert_eq!(
            err("[[engine.groups]]\nid = \"m1\"\ninputs = [\"mic\"]\n"),
            SiteError::DuplicateId("m1".into())
        );
        assert_eq!(
            err("[[engine.mixes]]\nid = \"stems\"\ntx = [9]\n"),
            SiteError::DuplicateId("stems".into())
        );
        assert_eq!(
            replaced("tx = [1, 2]", "tx = [1, 2, 6]"),
            SiteError::ChannelCount {
                id: "eng".into(),
                expected: "1 or 2",
                got: 3
            }
        );
        assert_eq!(
            replaced("tx = [5]", "tx = []"),
            SiteError::ChannelCount {
                id: "tr".into(),
                expected: "1 or 2",
                got: 0
            }
        );
        assert_eq!(
            replaced("rx = [2, 3]", "rx = [2, 3, 6]"),
            SiteError::ChannelCount {
                id: "keys".into(),
                expected: "1 or 2",
                got: 3
            }
        );
        assert_eq!(
            replaced("rx = [1]", "rx = []"),
            SiteError::ChannelCount {
                id: "mic".into(),
                expected: "1 or 2",
                got: 0
            }
        );
        assert_eq!(
            replaced("rx = [1]", "rx = [0]"),
            SiteError::ChannelRange {
                id: "mic".into(),
                ch: 0
            }
        );
        assert_eq!(
            replaced("rx = [1]", "rx = [17]"),
            SiteError::ChannelRange {
                id: "mic".into(),
                ch: 17
            }
        );
        assert_eq!(
            replaced("tx = [5]", "tx = [17]"),
            SiteError::ChannelRange {
                id: "tr".into(),
                ch: 17
            }
        );
        assert_eq!(
            replaced("rx = [1]", "rx = [3]"),
            SiteError::ChannelReused { ch: 3 }
        );
        assert_eq!(
            replaced("tx = [5]", "tx = [4]"),
            SiteError::ChannelReused { ch: 4 }
        );
        // The last channel of the map is in range; an RX number may also be a TX one.
        let last = compile(&parse(&BASE.replacen("rx = [1]", "rx = [16]", 1)).unwrap()).unwrap();
        assert_eq!(last.rx, vec![16, 2, 3, 4, 5]);
        compile(&parse(&BASE.replacen("rx = [1]", "rx = [9]", 1)).unwrap()).unwrap();
        assert_eq!(
            replaced("inputs = [\"drums\"]", "inputs = [\"m1\"]"),
            SiteError::UnknownInput {
                group: "stems".into(),
                input: "m1".into()
            }
        );
        assert_eq!(
            replaced("inputs = [\"drums\"]", "inputs = []"),
            SiteError::EmptyGroup("stems".into())
        );
        assert_eq!(
            replaced("inputs = [\"drums\"]", "inputs = [\"drums\", \"drums\"]"),
            SiteError::SecondGroup("drums".into())
        );
        assert_eq!(
            err("[[engine.groups]]\nid = \"more\"\ninputs = [\"drums\"]\n"),
            SiteError::SecondGroup("drums".into())
        );
        assert_eq!(
            replaced("engineer = \"eng\"", "engineer = \"tr\""),
            SiteError::Engineer("tr".into())
        );
        assert_eq!(
            replaced("engineer = \"eng\"", "engineer = \"mic\""),
            SiteError::Engineer("mic".into())
        );
        assert_eq!(
            replaced("engineer = \"eng\"", "engineer = \"stems\""),
            SiteError::Engineer("stems".into())
        );
        assert_eq!(
            replaced("id = \"mic\"", "id = \"Mic 1\""),
            SiteError::BadId("Mic 1".into())
        );
        assert_eq!(
            replaced("id = \"stems\"", "id = \"-s\""),
            SiteError::BadId("-s".into())
        );
        assert_eq!(
            replaced("id = \"tr\"", "id = \"_t\""),
            SiteError::BadId("_t".into())
        );
    }

    #[test]
    fn a_heard_mix_must_be_declared_before() {
        let heard = |list: &str| replaced("mixes = [\"m1\"]", &format!("mixes = [{list}]"));
        let e = |name: &str| SiteError::HeardMix {
            mix: "eng".into(),
            heard: name.into(),
        };
        // Declared later, unknown, itself, not a mix, twice.
        let later = BASE.replacen(
            "id = \"m1\"\ntx = [3, 4]\n",
            "id = \"m1\"\ntx = [3, 4]\nmixes = [\"tr\"]\n",
            1,
        );
        assert_eq!(
            compile(&parse(&later).unwrap()).unwrap_err(),
            SiteError::HeardMix {
                mix: "m1".into(),
                heard: "tr".into()
            }
        );
        assert_eq!(heard("\"ghost\""), e("ghost"));
        assert_eq!(heard("\"eng\""), e("eng"));
        assert_eq!(heard("\"mic\""), e("mic"));
        assert_eq!(heard("\"m1\", \"m1\""), e("m1"));
        // Two earlier mixes, in the listed order.
        let two = BASE.replacen("mixes = [\"m1\"]", "mixes = [\"tr\", \"m1\"]", 1);
        let t = compile(&parse(&two).unwrap()).unwrap();
        assert_eq!(t.mixes[2].mixes, vec![1, 0]);
    }

    #[test]
    fn a_second_talkback_input_is_refused() {
        let two = BASE
            .replacen("rx = [1]", "rx = [1]\ntalkback = true", 1)
            .replacen("rx = [2, 3]", "rx = [2, 3]\ntalkback = true", 1);
        assert_eq!(
            compile(&parse(&two).unwrap()).unwrap_err(),
            SiteError::SecondTalkback
        );
        let one = BASE.replacen("rx = [1]", "rx = [1]\ntalkback = true", 1);
        assert!(compile(&parse(&one).unwrap()).unwrap().inputs[0].talkback);
    }

    #[test]
    fn errors_read_well() {
        assert_eq!(
            SiteError::ChannelCount {
                id: "a".into(),
                expected: "1 or 2",
                got: 3
            }
            .to_string(),
            "a: expected 1 or 2 channel(s), got 3"
        );
        assert_eq!(
            SiteError::HeardMix {
                mix: "a".into(),
                heard: "b".into()
            }
            .to_string(),
            "mix \"a\" can hear only mixes declared before it, each once: \"b\""
        );
    }
}
