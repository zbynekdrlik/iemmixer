//! The engine topology as the importer sees it (S4 design note §3.2): the
//! same content as the `[engine]` table of `site.toml` with every send family
//! expanded, so a project and a site file compare id by id.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};

use iem_engine_proto::{BusId, BusKind, InputId, SendId, Source, Tap};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopoInput {
    pub id: InputId,
    pub rx: Vec<u16>,
    pub talkback: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopoBus {
    pub id: BusId,
    pub kind: BusKind,
    pub tx: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Topology {
    pub inputs: Vec<TopoInput>,
    pub buses: Vec<TopoBus>,
    pub sends: Vec<(SendId, Tap)>,
    pub engineer: Option<BusId>,
}

/// Program spec §3.5 "Data": what an import must reproduce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    /// Inputs plus every bus but the master (the master is not a REAPER track).
    pub tracks: usize,
    pub sends: usize,
    /// Inputs, output buses and stems buses.
    pub eqs: usize,
    /// Output buses.
    pub limiters: usize,
    /// Inputs.
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

pub const fn kind_name(kind: BusKind) -> &'static str {
    match kind {
        BusKind::Output => "output",
        BusKind::Stems => "stems",
        BusKind::Translator => "translator",
        BusKind::Master => "master",
    }
}

pub const fn tap_name(tap: Tap) -> &'static str {
    match tap {
        Tap::Pre => "pre",
        Tap::Post => "post",
    }
}

fn opt(id: Option<&BusId>) -> String {
    id.map_or_else(|| "none".to_owned(), ToString::to_string)
}

fn list(ids: &[String]) -> String {
    let quoted: Vec<String> = ids.iter().map(|i| format!("\"{i}\"")).collect();
    format!("[{}]", quoted.join(", "))
}

impl Topology {
    pub fn input(&self, id: &InputId) -> Option<&TopoInput> {
        self.inputs.iter().find(|i| i.id == *id)
    }

    pub fn bus(&self, id: &BusId) -> Option<&TopoBus> {
        self.buses.iter().find(|b| b.id == *id)
    }

    pub fn tap(&self, id: &SendId) -> Option<Tap> {
        self.sends.iter().find(|(s, _)| s == id).map(|(_, t)| *t)
    }

    pub fn has_send(&self, id: &SendId) -> bool {
        self.tap(id).is_some()
    }

    /// The highest card channel any input or bus uses (1 when none).
    pub fn max_channel(&self) -> u16 {
        self.inputs
            .iter()
            .flat_map(|i| i.rx.iter())
            .chain(self.buses.iter().flat_map(|b| b.tx.iter()))
            .copied()
            .max()
            .unwrap_or(1)
    }

    pub fn counts(&self) -> Counts {
        let of = |k: BusKind| self.buses.iter().filter(|b| b.kind == k).count();
        let outputs = of(BusKind::Output);
        Counts {
            tracks: self.inputs.len() + self.buses.len() - of(BusKind::Master),
            sends: self.sends.len(),
            eqs: self.inputs.len() + outputs + of(BusKind::Stems),
            limiters: outputs,
            trims: self.inputs.len(),
        }
    }

    /// Differences between this topology (the project's) and `site`'s, by id.
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
        let a: BTreeMap<&BusId, &TopoBus> = self.buses.iter().map(|x| (&x.id, x)).collect();
        let b: BTreeMap<&BusId, &TopoBus> = site.buses.iter().map(|x| (&x.id, x)).collect();
        for (id, x) in &a {
            match b.get(id) {
                None => out.push(format!("bus {id}: in the project, not in site.toml")),
                Some(y) => {
                    if x.kind != y.kind {
                        out.push(format!(
                            "bus {id}: {} in the project, {} in site.toml",
                            kind_name(x.kind),
                            kind_name(y.kind)
                        ));
                    }
                    if x.tx != y.tx {
                        out.push(format!(
                            "bus {id}: tx {:?} in the project, {:?} in site.toml",
                            x.tx, y.tx
                        ));
                    }
                }
            }
        }
        for id in b.keys().filter(|id| !a.contains_key(*id)) {
            out.push(format!("bus {id}: in site.toml, not in the project"));
        }
        let a: BTreeMap<&SendId, Tap> = self.sends.iter().map(|(s, t)| (s, *t)).collect();
        let b: BTreeMap<&SendId, Tap> = site.sends.iter().map(|(s, t)| (s, *t)).collect();
        for (id, x) in &a {
            match b.get(id) {
                None => out.push(format!("send {id}: in the project, not in site.toml")),
                Some(y) if y != x => out.push(format!(
                    "send {id}: tap {} in the project, {} in site.toml",
                    tap_name(*x),
                    tap_name(*y)
                )),
                Some(_) => {}
            }
        }
        for id in b.keys().filter(|id| !a.contains_key(*id)) {
            out.push(format!("send {id}: in site.toml, not in the project"));
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

    /// This topology as a `site.toml` `[engine]` table: sources with the same
    /// tap and destinations share one send family.
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
        for b in &self.buses {
            let _ = write!(
                s,
                "\n[[engine.buses]]\nid = \"{}\"\nkind = \"{}\"\n",
                b.id,
                kind_name(b.kind)
            );
            if !b.tx.is_empty() {
                let _ = writeln!(s, "tx = {:?}", b.tx);
            }
        }
        let sources = self
            .inputs
            .iter()
            .map(|i| Source::Input(i.id.clone()))
            .chain(self.buses.iter().map(|b| Source::Bus(b.id.clone())));
        let mut families: Vec<(Tap, Vec<String>, Vec<String>)> = Vec::new();
        for src in sources {
            let mut tap = None;
            let mut to = Vec::new();
            for b in &self.buses {
                let id = SendId {
                    src: src.clone(),
                    dst: b.id.clone(),
                };
                if let Some(t) = self.tap(&id) {
                    tap = Some(t);
                    to.push(b.id.to_string());
                }
            }
            let Some(tap) = tap else { continue };
            match families.iter_mut().find(|(t, _, d)| *t == tap && *d == to) {
                Some((_, from, _)) => from.push(src.to_string()),
                None => families.push((tap, vec![src.to_string()], to)),
            }
        }
        for (tap, from, to) in &families {
            let _ = write!(
                s,
                "\n[[engine.sends]]\nfrom = {}\nto = {}\ntap = \"{}\"\n",
                list(from),
                list(to),
                tap_name(*tap)
            );
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sitegen::synthetic_site;

    fn send(src: Source, dst: &str) -> SendId {
        SendId {
            src,
            dst: BusId::new(dst),
        }
    }

    #[test]
    fn the_synthetic_site_has_the_program_counts() {
        let c = synthetic_site().counts();
        assert_eq!(
            c,
            Counts {
                tracks: 45,
                sends: 268,
                eqs: 44,
                limiters: 10,
                trims: 24
            }
        );
        assert_eq!(
            c.to_string(),
            "tracks 45, sends 268, eqs 44, limiters 10, trims 24"
        );
    }

    #[test]
    fn expect_checks_any_subset() {
        let c = synthetic_site().counts();
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
        b.buses.reverse();
        b.sends.reverse();
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
        b.buses[0].tx = vec![1, 2];
        b.buses[1].kind = BusKind::Stems;
        b.buses.remove(2);
        b.buses.push(TopoBus {
            id: BusId::new("spare"),
            kind: BusKind::Stems,
            tx: vec![],
        });
        let first = b.sends[0].0.clone();
        b.sends[0].1 = Tap::Post;
        b.sends
            .retain(|(s, _)| *s != send(Source::Input(InputId::new("mic1")), "member2"));
        b.sends.push((
            send(Source::Input(InputId::new("mic3")), "translator"),
            Tap::Pre,
        ));
        b.engineer = None;
        let d = a.diff(&b);
        for want in [
            "input mic1: rx [101] in the project, [1] in site.toml".to_owned(),
            "input eng_mic: talkback true in the project, false in site.toml".to_owned(),
            "input mic2: in the project, not in site.toml".to_owned(),
            "input extra: in site.toml, not in the project".to_owned(),
            "bus member1: tx [71, 72] in the project, [1, 2] in site.toml".to_owned(),
            "bus member2: output in the project, stems in site.toml".to_owned(),
            "bus member3: in the project, not in site.toml".to_owned(),
            "bus spare: in site.toml, not in the project".to_owned(),
            format!("send {first}: tap pre in the project, post in site.toml"),
            "send mic1>member2: in the project, not in site.toml".to_owned(),
            "send mic3>translator: in site.toml, not in the project".to_owned(),
            "engineer: engineer in the project, none in site.toml".to_owned(),
        ] {
            assert!(d.contains(&want), "missing {want:?} in {d:#?}");
        }
        assert_eq!(d.len(), 12, "{d:#?}");
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
        buses: Vec<toml::Value>,
        sends: Vec<Family>,
    }

    #[derive(serde::Deserialize)]
    struct Family {
        from: Vec<String>,
        to: Vec<String>,
        tap: String,
    }

    #[test]
    fn the_engine_table_round_trips_through_toml() {
        let t = synthetic_site();
        let text = t.engine_toml(160);
        let file: File = toml::from_str(&text).unwrap();
        assert_eq!(file.engine.channels, 160);
        assert_eq!(file.engine.engineer, "engineer");
        assert_eq!(file.engine.inputs.len(), 24);
        assert_eq!(file.engine.buses.len(), 22);
        let talkback: Vec<&toml::Value> = file
            .engine
            .inputs
            .iter()
            .filter(|i| i.get("talkback").is_some())
            .collect();
        assert_eq!(talkback.len(), 1);
        assert!(file.engine.buses.iter().all(|b| {
            let kind = b.get("kind").and_then(toml::Value::as_str).unwrap();
            b.get("tx").is_some() == (kind != "stems")
        }));
        let mut expanded = Vec::new();
        for f in &file.engine.sends {
            for from in &f.from {
                for to in &f.to {
                    expanded.push((from.clone(), to.clone(), f.tap.clone()));
                }
            }
        }
        let mut want: Vec<(String, String, String)> = t
            .sends
            .iter()
            .map(|(s, tap)| {
                (
                    s.src.to_string(),
                    s.dst.to_string(),
                    tap_name(*tap).to_owned(),
                )
            })
            .collect();
        expanded.sort();
        want.sort();
        assert_eq!(expanded, want);
        let direct = file
            .engine
            .sends
            .iter()
            .find(|f| f.from.contains(&"mic2".to_owned()))
            .unwrap();
        assert_eq!(direct.from.len(), 16, "hand1 also feeds the translator");
        assert_eq!(direct.to.len(), 10);
        assert_eq!(direct.tap, "pre");
        let no_engineer = Topology {
            engineer: None,
            ..t
        };
        assert!(!no_engineer.engine_toml(8).contains("engineer ="));
    }

    #[test]
    fn the_highest_channel_counts_rx_and_tx() {
        let mut t = synthetic_site();
        assert_eq!(t.max_channel(), 132);
        t.buses[0].tx = vec![150, 151];
        assert_eq!(t.max_channel(), 151);
        assert_eq!(Topology::default().max_channel(), 1);
    }

    #[test]
    fn lookups_find_ids() {
        let t = synthetic_site();
        assert!(t.input(&InputId::new("mic1")).is_some());
        assert!(t.input(&InputId::new("nope")).is_none());
        assert_eq!(
            t.bus(&BusId::new("translator")).unwrap().kind,
            BusKind::Translator
        );
        let s = send(Source::Bus(BusId::new("member2")), "member1");
        assert_eq!(t.tap(&s), Some(Tap::Post));
        assert!(t.has_send(&s));
        assert!(!t.has_send(&send(Source::Bus(BusId::new("member1")), "member2")));
        assert_eq!(kind_name(BusKind::Master), "master");
    }
}
