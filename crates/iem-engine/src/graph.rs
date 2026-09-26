//! The graph compiled once per run from the site (program spec I4, §3.1;
//! design note §3.2): validated, buses in a processing order where every
//! source bus precedes its destinations, incoming sends grouped per bus, and
//! for X13 which TX buses each input reaches.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;

use iem_engine_proto::{
    BusId, BusInfo, BusKind, InputId, InputInfo, SendId, SendInfo, Source, Tap, TopologyInfo,
    valid_id,
};
use sha2::{Digest, Sha256};

use crate::SAMPLE_RATE;
use crate::site::{Site, SiteError};

/// Where a send reads: an input's pre-fader tap or a bus's post-fader output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Src {
    Pre(usize),
    Post(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputNode {
    pub id: InputId,
    /// Indices into [`Graph::rx`]; a mono input has the same index twice.
    pub rx: [usize; 2],
    pub stereo: bool,
    pub talkback: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusNode {
    pub id: BusId,
    pub kind: BusKind,
    /// Indices into [`Graph::tx`].
    pub tx: [Option<usize>; 2],
    /// This bus's incoming sends in [`Graph::sends`].
    pub sends: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendEdge {
    pub id: SendId,
    pub src: Src,
    /// Index into [`Graph::buses`].
    pub dst: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Graph {
    pub inputs: Vec<InputNode>,
    /// In processing order.
    pub buses: Vec<BusNode>,
    /// Grouped by destination, in processing order.
    pub sends: Vec<SendEdge>,
    /// Card RX channels in the order the backend delivers them.
    pub rx: Vec<u16>,
    /// Card TX channels in the order the backend expects them.
    pub tx: Vec<u16>,
    pub engineer: usize,
    pub master: Option<usize>,
    /// `reach[input][bus]`: a TX bus the input's signal can get to (X13).
    pub reach: Vec<Vec<bool>>,
    pub hash: String,
    input_index: HashMap<InputId, usize>,
    bus_index: HashMap<BusId, usize>,
    send_index: HashMap<SendId, usize>,
}

enum Node {
    Input(usize),
    Bus(usize),
}

fn channels_for(kind: BusKind) -> (usize, &'static str) {
    match kind {
        BusKind::Output | BusKind::Master => (2, "2"),
        BusKind::Translator => (1, "1"),
        BusKind::Stems => (0, "0"),
    }
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

fn has_tx(kind: BusKind) -> bool {
    kind != BusKind::Stems
}

/// Validates the site and compiles its graph.
pub fn compile(site: &Site) -> Result<Graph, SiteError> {
    let mut nodes: HashMap<&str, Node> = HashMap::new();
    for (i, input) in site.inputs.iter().enumerate() {
        if !valid_id(&input.id) {
            return Err(SiteError::BadId(input.id.clone()));
        }
        if nodes.insert(input.id.as_str(), Node::Input(i)).is_some() {
            return Err(SiteError::DuplicateId(input.id.clone()));
        }
    }
    for (b, bus) in site.buses.iter().enumerate() {
        if !valid_id(&bus.id) {
            return Err(SiteError::BadId(bus.id.clone()));
        }
        if nodes.insert(bus.id.as_str(), Node::Bus(b)).is_some() {
            return Err(SiteError::DuplicateId(bus.id.clone()));
        }
    }

    let mut rx_used = BTreeSet::new();
    let mut talkback = false;
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
    }
    let mut tx_used = BTreeSet::new();
    let mut master = None;
    for (b, bus) in site.buses.iter().enumerate() {
        let (count, expected) = channels_for(bus.kind);
        if bus.tx.len() != count {
            return Err(SiteError::ChannelCount {
                id: bus.id.clone(),
                expected,
                got: bus.tx.len(),
            });
        }
        check_channels(&bus.id, &bus.tx, site.channels, &mut tx_used)?;
        if bus.kind == BusKind::Master {
            if master.is_some() {
                return Err(SiteError::SecondMaster);
            }
            master = Some(b);
        }
    }
    let engineer = match nodes.get(site.engineer.as_str()) {
        Some(Node::Bus(b)) if site.buses.get(*b).map(|bus| bus.kind) == Some(BusKind::Output) => *b,
        _ => return Err(SiteError::Engineer(site.engineer.clone())),
    };

    // Sends, in declaration order, as (site bus of destination, source, id, tap).
    let mut edges: Vec<(usize, SrcSite, SendId, Tap)> = Vec::new();
    let mut pairs = BTreeSet::new();
    for family in &site.sends {
        for from in &family.from {
            for to in &family.to {
                let src = match nodes.get(from.as_str()) {
                    None => return Err(SiteError::UnknownId(from.clone())),
                    Some(Node::Input(i)) => {
                        if family.tap != Tap::Pre {
                            return Err(SiteError::TapMismatch { from: from.clone() });
                        }
                        SrcSite::Input(*i)
                    }
                    Some(Node::Bus(b)) => {
                        let kind = site.buses.get(*b).map(|bus| bus.kind);
                        if !matches!(kind, Some(BusKind::Output | BusKind::Stems)) {
                            return Err(SiteError::BadSource { from: from.clone() });
                        }
                        if family.tap != Tap::Post {
                            return Err(SiteError::TapMismatch { from: from.clone() });
                        }
                        SrcSite::Bus(*b)
                    }
                };
                let dst = match nodes.get(to.as_str()) {
                    None => return Err(SiteError::UnknownId(to.clone())),
                    Some(Node::Input(_)) => {
                        return Err(SiteError::BadDestination { to: to.clone() });
                    }
                    Some(Node::Bus(b)) => {
                        if site.buses.get(*b).map(|bus| bus.kind) == Some(BusKind::Master) {
                            return Err(SiteError::BadDestination { to: to.clone() });
                        }
                        *b
                    }
                };
                if src == SrcSite::Bus(dst) {
                    return Err(SiteError::Cycle(to.clone()));
                }
                if !pairs.insert((from.as_str(), to.as_str())) {
                    return Err(SiteError::DuplicateSend {
                        from: from.clone(),
                        to: to.clone(),
                    });
                }
                let src_id = match src {
                    SrcSite::Input(_) => Source::Input(InputId::new(from.clone())),
                    SrcSite::Bus(_) => Source::Bus(BusId::new(from.clone())),
                };
                let id = SendId {
                    src: src_id,
                    dst: BusId::new(to.clone()),
                };
                edges.push((dst, src, id, family.tap));
            }
        }
    }

    let order = processing_order(site, &edges, master)?;
    let mut position = vec![0usize; site.buses.len()];
    for (p, &b) in order.iter().enumerate() {
        if let Some(slot) = position.get_mut(b) {
            *slot = p;
        }
    }
    let pos = |b: usize| position.get(b).copied().unwrap_or(0);

    let mut rx = Vec::new();
    let mut inputs = Vec::with_capacity(site.inputs.len());
    for input in &site.inputs {
        let first = rx.len();
        rx.extend_from_slice(&input.rx);
        let stereo = input.rx.len() == 2;
        inputs.push(InputNode {
            id: InputId::new(input.id.clone()),
            rx: [first, if stereo { first + 1 } else { first }],
            stereo,
            talkback: input.talkback,
        });
    }
    let mut tx = Vec::new();
    let mut tx_of = vec![[None, None]; site.buses.len()];
    for (b, bus) in site.buses.iter().enumerate() {
        let first = tx.len();
        tx.extend_from_slice(&bus.tx);
        if let Some(slot) = tx_of.get_mut(b) {
            *slot = match bus.tx.len() {
                2 => [Some(first), Some(first + 1)],
                1 => [Some(first), None],
                _ => [None, None],
            };
        }
    }

    let mut sends = Vec::with_capacity(edges.len());
    let mut buses = Vec::with_capacity(order.len());
    for &b in &order {
        let start = sends.len();
        for (dst, src, id, _) in &edges {
            if *dst == b {
                sends.push(SendEdge {
                    id: id.clone(),
                    src: match *src {
                        SrcSite::Input(i) => Src::Pre(i),
                        SrcSite::Bus(s) => Src::Post(pos(s)),
                    },
                    dst: pos(b),
                });
            }
        }
        let Some(bus) = site.buses.get(b) else {
            continue;
        };
        buses.push(BusNode {
            id: BusId::new(bus.id.clone()),
            kind: bus.kind,
            tx: tx_of.get(b).copied().unwrap_or([None, None]),
            sends: start..sends.len(),
        });
    }

    let master = master.map(pos);
    let reach = reach(&inputs, &buses, &sends, master);
    let mut graph = Graph {
        input_index: inputs
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.clone(), i))
            .collect(),
        bus_index: buses
            .iter()
            .enumerate()
            .map(|(b, n)| (n.id.clone(), b))
            .collect(),
        send_index: sends
            .iter()
            .enumerate()
            .map(|(s, e)| (e.id.clone(), s))
            .collect(),
        inputs,
        buses,
        sends,
        rx,
        tx,
        engineer: pos(engineer),
        master,
        reach,
        hash: String::new(),
    };
    let info = serde_json::to_vec(&graph.info()).unwrap_or_default();
    graph.hash = Sha256::digest(&info)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(graph)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SrcSite {
    Input(usize),
    Bus(usize),
}

/// Kahn's algorithm over bus → bus sends and master's implicit stems inputs;
/// among ready buses the one declared first goes first (a stable order).
fn processing_order(
    site: &Site,
    edges: &[(usize, SrcSite, SendId, Tap)],
    master: Option<usize>,
) -> Result<Vec<usize>, SiteError> {
    let n = site.buses.len();
    let mut deps: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
    for (dst, src, _, _) in edges {
        if let (SrcSite::Bus(s), Some(d)) = (src, deps.get_mut(*dst)) {
            d.insert(*s);
        }
    }
    if let Some(m) = master {
        for (b, bus) in site.buses.iter().enumerate() {
            if bus.kind == BusKind::Stems
                && let Some(d) = deps.get_mut(m)
            {
                d.insert(b);
            }
        }
    }
    let mut done = vec![false; n];
    let mut order = Vec::with_capacity(n);
    while order.len() < n {
        let next = (0..n).find(|&b| {
            !done.get(b).copied().unwrap_or(true)
                && deps
                    .get(b)
                    .is_some_and(|d| d.iter().all(|s| done.get(*s).copied().unwrap_or(false)))
        });
        let Some(b) = next else {
            let stuck: Vec<&str> = site
                .buses
                .iter()
                .zip(&done)
                .filter(|(_, d)| !**d)
                .map(|(bus, _)| bus.id.as_str())
                .collect();
            return Err(SiteError::Cycle(stuck.join(", ")));
        };
        if let Some(d) = done.get_mut(b) {
            *d = true;
        }
        order.push(b);
    }
    Ok(order)
}

/// For each input, the TX buses its signal can reach through sends, the
/// master's implicit inputs and stems buses included.
fn reach(
    inputs: &[InputNode],
    buses: &[BusNode],
    sends: &[SendEdge],
    master: Option<usize>,
) -> Vec<Vec<bool>> {
    (0..inputs.len())
        .map(|i| {
            let mut hit = vec![false; buses.len()];
            if let Some(m) = master.and_then(|m| hit.get_mut(m)) {
                *m = true;
            }
            // Buses are in processing order: one pass sees every source first.
            for (b, bus) in buses.iter().enumerate() {
                let fed = sends
                    .get(bus.sends.clone())
                    .unwrap_or_default()
                    .iter()
                    .any(|e| match e.src {
                        Src::Pre(s) => s == i,
                        Src::Post(s) => hit.get(s).copied().unwrap_or(false),
                    });
                if fed && let Some(h) = hit.get_mut(b) {
                    *h = true;
                }
            }
            hit.iter()
                .zip(buses)
                .map(|(h, bus)| *h && has_tx(bus.kind))
                .collect()
        })
        .collect()
}

impl Graph {
    pub fn input_index(&self, id: &InputId) -> Option<usize> {
        self.input_index.get(id).copied()
    }

    pub fn bus_index(&self, id: &BusId) -> Option<usize> {
        self.bus_index.get(id).copied()
    }

    pub fn send_index(&self, id: &SendId) -> Option<usize> {
        self.send_index.get(id).copied()
    }

    pub fn kind(&self, bus: usize) -> Option<BusKind> {
        self.buses.get(bus).map(|b| b.kind)
    }

    /// Output and stems buses carry an EQ (§3.1: 44 EQs with the inputs).
    pub fn has_eq(&self, bus: usize) -> bool {
        matches!(self.kind(bus), Some(BusKind::Output | BusKind::Stems))
    }

    /// Output buses carry a limiter (§3.1: 10 limiters).
    pub fn has_limiter(&self, bus: usize) -> bool {
        self.kind(bus) == Some(BusKind::Output)
    }

    /// The topology as the protocol describes it (without its hash inside
    /// the hash computation).
    pub fn info(&self) -> TopologyInfo {
        TopologyInfo {
            hash: self.hash.clone(),
            sample_rate: SAMPLE_RATE,
            engineer: self
                .buses
                .get(self.engineer)
                .map(|b| b.id.clone())
                .unwrap_or_else(|| BusId::new("")),
            inputs: self
                .inputs
                .iter()
                .map(|n| InputInfo {
                    id: n.id.clone(),
                    channels: if n.stereo { 2 } else { 1 },
                    talkback: n.talkback,
                })
                .collect(),
            buses: self
                .buses
                .iter()
                .enumerate()
                .map(|(b, n)| BusInfo {
                    id: n.id.clone(),
                    kind: n.kind,
                    tx_channels: n.tx.iter().flatten().count() as u8,
                    eq: self.has_eq(b),
                    limiter: self.has_limiter(b),
                })
                .collect(),
            sends: self
                .sends
                .iter()
                .map(|e| SendInfo {
                    id: e.id.clone(),
                    tap: match e.src {
                        Src::Pre(_) => Tap::Pre,
                        Src::Post(_) => Tap::Post,
                    },
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

    fn pos(g: &Graph, id: &str) -> usize {
        g.bus_index(&BusId::new(id)).unwrap()
    }

    #[test]
    fn test_site_has_the_program_shape() {
        let g = test_site();
        assert_eq!(g.inputs.len(), 24);
        assert_eq!(g.rx.len(), 32);
        assert_eq!(g.tx.len(), 23);
        assert_eq!(g.sends.len(), 268);
        let pre = g
            .sends
            .iter()
            .filter(|e| matches!(e.src, Src::Pre(_)))
            .count();
        assert_eq!((pre, g.sends.len() - pre), (241, 27));
        let eqs = g.inputs.len() + (0..g.buses.len()).filter(|&b| g.has_eq(b)).count();
        assert_eq!(eqs, 44);
        assert_eq!((0..g.buses.len()).filter(|&b| g.has_limiter(b)).count(), 10);
        assert_eq!(g.master, Some(g.buses.len() - 1));
        let m1 = pos(&g, "member1");
        for m in 2..=9 {
            assert!(pos(&g, &format!("member{m}")) < m1);
            assert!(pos(&g, &format!("member{m}.stems")) < pos(&g, &format!("member{m}")));
        }
        assert!(m1 < pos(&g, "engineer"));
        assert_eq!(g.engineer, pos(&g, "engineer"));
        assert_eq!(g.inputs.iter().filter(|n| n.talkback).count(), 1);
        assert_eq!(g.inputs.iter().filter(|n| n.stereo).count(), 8);
        // Every post source precedes its destination; sends are grouped per bus.
        for (b, bus) in g.buses.iter().enumerate() {
            for e in &g.sends[bus.sends.clone()] {
                assert_eq!(e.dst, b);
                if let Src::Post(s) = e.src {
                    assert!(s < b);
                }
                assert_eq!(
                    g.send_index(&e.id),
                    Some(g.sends.iter().position(|x| x == e).unwrap())
                );
            }
        }
        let info = g.info();
        assert_eq!(info.hash, g.hash);
        assert_eq!(info.engineer, BusId::new("engineer"));
        assert_eq!(
            info.buses
                .iter()
                .map(|b| usize::from(b.tx_channels))
                .sum::<usize>(),
            23
        );
        assert_eq!(g.input_index(&InputId::new("keys")), Some(14));
        assert_eq!(g.inputs[14].rx, [14, 15]);
        assert_eq!(g.inputs[0].rx, [0, 0]);
        assert_eq!(g.bus_index(&BusId::new("nope")), None);
    }

    #[test]
    fn reach_follows_sends_and_master() {
        let g = test_site();
        let reach = |input: &str, bus: &str| {
            g.reach[g.input_index(&InputId::new(input)).unwrap()][pos(&g, bus)]
        };
        for m in 1..=9 {
            assert!(reach("mic1", &format!("member{m}")));
            assert!(
                !reach("mic1", &format!("member{m}.stems")),
                "stems carry no TX"
            );
        }
        assert!(reach("mic1", "engineer"));
        assert!(reach("mic1", "master"));
        assert!(!reach("mic1", "translator"));
        assert!(reach("hand1", "translator"));
        assert!(reach("drums", "member3"));
        // drums feeds member3.stems, but a stems bus has no TX to reach.
        assert!(!reach("drums", "member3.stems"));
        assert!(reach("drums", "master"));
        assert!(!reach("drums", "translator"));
    }

    #[test]
    fn hash_is_stable_and_topology_sensitive() {
        let a = test_site();
        assert_eq!(a.hash, test_site().hash);
        assert_eq!(a.hash.len(), 64);
        let text = test_site_text();
        let more = format!(
            "{text}\n[[engine.sends]]\nfrom = [\"hand2\"]\nto = [\"translator\"]\ntap = \"pre\"\n"
        );
        let b = compile(&parse(&more).unwrap()).unwrap();
        assert_eq!(b.sends.len(), 269);
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
[[engine.buses]]
id = "eng"
kind = "output"
tx = [1, 2]
[[engine.buses]]
id = "m1"
kind = "output"
tx = [3, 4]
[[engine.buses]]
id = "m1.stems"
kind = "stems"
[[engine.buses]]
id = "tr"
kind = "translator"
tx = [5]
[[engine.buses]]
id = "master"
kind = "master"
tx = [6, 7]
[[engine.sends]]
from = ["mic", "keys"]
to = ["eng", "m1"]
tap = "pre"
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
        let g = compile(&parse(BASE).unwrap()).unwrap();
        assert_eq!(g.rx, vec![1, 2, 3]);
        assert_eq!(g.tx, vec![1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(g.sends.len(), 4);
        let tr = pos(&g, "tr");
        assert_eq!(g.buses[tr].tx, [Some(4), None]);
        assert_eq!(g.buses[pos(&g, "m1.stems")].tx, [None, None]);
    }

    #[test]
    fn every_site_error_is_detected() {
        let dup = "[[engine.inputs]]\nid = \"eng\"\nrx = [9]\n";
        assert_eq!(err(dup), SiteError::DuplicateId("eng".into()));
        assert_eq!(
            err("[[engine.inputs]]\nid = \"mic\"\nrx = [9]\n"),
            SiteError::DuplicateId("mic".into())
        );
        assert_eq!(
            err("[[engine.buses]]\nid = \"m1\"\nkind = \"stems\"\n"),
            SiteError::DuplicateId("m1".into())
        );
        assert_eq!(
            err("[[engine.sends]]\nfrom = [\"ghost\"]\nto = [\"eng\"]\ntap = \"pre\"\n"),
            SiteError::UnknownId("ghost".into())
        );
        assert_eq!(
            err("[[engine.sends]]\nfrom = [\"mic\"]\nto = [\"ghost\"]\ntap = \"pre\"\n"),
            SiteError::UnknownId("ghost".into())
        );
        assert_eq!(
            replaced("tx = [1, 2]", "tx = [1]"),
            SiteError::ChannelCount {
                id: "eng".into(),
                expected: "2",
                got: 1
            }
        );
        assert_eq!(
            replaced("tx = [5]", "tx = [5, 8]"),
            SiteError::ChannelCount {
                id: "tr".into(),
                expected: "1",
                got: 2
            }
        );
        assert_eq!(
            err("[[engine.buses]]\nid = \"s2\"\nkind = \"stems\"\ntx = [9]\n"),
            SiteError::ChannelCount {
                id: "s2".into(),
                expected: "0",
                got: 1
            }
        );
        assert_eq!(
            replaced("rx = [2, 3]", "rx = [2, 3, 4]"),
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
            replaced("tx = [6, 7]", "tx = [6, 17]"),
            SiteError::ChannelRange {
                id: "master".into(),
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
        // The last channel of the map is in range.
        let last = compile(&parse(&BASE.replacen("rx = [1]", "rx = [16]", 1)).unwrap()).unwrap();
        assert_eq!(last.rx, vec![16, 2, 3]);
        // The same channel number may be an RX and a TX channel.
        compile(&parse(&BASE.replacen("rx = [1]", "rx = [9]", 1)).unwrap()).unwrap();
        assert_eq!(
            err("[[engine.sends]]\nfrom = [\"keys\"]\nto = [\"m1\"]\ntap = \"pre\"\n"),
            SiteError::DuplicateSend {
                from: "keys".into(),
                to: "m1".into()
            }
        );
        assert_eq!(
            err("[[engine.sends]]\nfrom = [\"mic\"]\nto = [\"tr\"]\ntap = \"post\"\n"),
            SiteError::TapMismatch { from: "mic".into() }
        );
        assert_eq!(
            err("[[engine.sends]]\nfrom = [\"m1\"]\nto = [\"eng\"]\ntap = \"pre\"\n"),
            SiteError::TapMismatch { from: "m1".into() }
        );
        assert_eq!(
            err("[[engine.sends]]\nfrom = [\"mic\"]\nto = [\"master\"]\ntap = \"pre\"\n"),
            SiteError::BadDestination {
                to: "master".into()
            }
        );
        assert_eq!(
            err("[[engine.sends]]\nfrom = [\"mic\"]\nto = [\"keys\"]\ntap = \"pre\"\n"),
            SiteError::BadDestination { to: "keys".into() }
        );
        assert_eq!(
            err("[[engine.sends]]\nfrom = [\"master\"]\nto = [\"eng\"]\ntap = \"post\"\n"),
            SiteError::BadSource {
                from: "master".into()
            }
        );
        assert_eq!(
            err("[[engine.sends]]\nfrom = [\"tr\"]\nto = [\"eng\"]\ntap = \"post\"\n"),
            SiteError::BadSource { from: "tr".into() }
        );
        assert_eq!(
            err("[[engine.sends]]\nfrom = [\"m1\"]\nto = [\"m1\"]\ntap = \"post\"\n"),
            SiteError::Cycle("m1".into())
        );
        assert_eq!(
            err(
                "[[engine.sends]]\nfrom = [\"m1\"]\nto = [\"eng\"]\ntap = \"post\"\n[[engine.sends]]\nfrom = [\"eng\"]\nto = [\"m1\"]\ntap = \"post\"\n"
            ),
            SiteError::Cycle("eng, m1".into())
        );
        assert_eq!(
            err("[[engine.buses]]\nid = \"master2\"\nkind = \"master\"\ntx = [8, 9]\n"),
            SiteError::SecondMaster
        );
        assert_eq!(
            replaced("engineer = \"eng\"", "engineer = \"m1.stems\""),
            SiteError::Engineer("m1.stems".into())
        );
        assert_eq!(
            replaced("engineer = \"eng\"", "engineer = \"mic\""),
            SiteError::Engineer("mic".into())
        );
        assert_eq!(
            replaced("id = \"mic\"", "id = \"Mic 1\""),
            SiteError::BadId("Mic 1".into())
        );
        assert_eq!(
            replaced("id = \"m1.stems\"", "id = \"-s\""),
            SiteError::BadId("-s".into())
        );
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
                expected: "2",
                got: 1
            }
            .to_string(),
            "a: expected 2 channel(s), got 1"
        );
        assert_eq!(
            SiteError::Cycle("a, b".into()).to_string(),
            "the sends form a cycle through a, b"
        );
    }
}
