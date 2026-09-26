//! `site.toml` through the engine's own parser and compiler: the topology
//! the importer compares with, and the graph the state is reconciled on.

use std::path::Path;

use iem_engine::graph::{Graph, compile};
use iem_engine::site::{Site, load};
use iem_engine_proto::{BusId, InputId, SendId, Source};
use iem_rpp::topology::{TopoBus, TopoInput, Topology};

use crate::Failure;

pub struct SiteFile {
    pub site: Site,
    pub graph: Graph,
    pub topology: Topology,
}

pub fn open(path: &Path) -> Result<SiteFile, Failure> {
    let bad = |e: String| Failure::input(format!("{}: {e}", path.display()));
    let site = load(path).map_err(|e| bad(e.to_string()))?;
    let graph = compile(&site).map_err(|e| bad(e.to_string()))?;
    Ok(SiteFile {
        topology: topology(&site),
        site,
        graph,
    })
}

/// The `[engine]` table with its send families expanded.
pub fn topology(site: &Site) -> Topology {
    let is_input = |id: &str| site.inputs.iter().any(|i| i.id == id);
    let mut sends = Vec::new();
    for family in &site.sends {
        for from in &family.from {
            let src = if is_input(from.as_str()) {
                Source::Input(InputId::new(from.clone()))
            } else {
                Source::Bus(BusId::new(from.clone()))
            };
            for to in &family.to {
                sends.push((
                    SendId {
                        src: src.clone(),
                        dst: BusId::new(to.clone()),
                    },
                    family.tap,
                ));
            }
        }
    }
    Topology {
        inputs: site
            .inputs
            .iter()
            .map(|i| TopoInput {
                id: InputId::new(i.id.clone()),
                rx: i.rx.clone(),
                talkback: i.talkback,
            })
            .collect(),
        buses: site
            .buses
            .iter()
            .map(|b| TopoBus {
                id: BusId::new(b.id.clone()),
                kind: b.kind,
                tx: b.tx.clone(),
            })
            .collect(),
        sends,
        engineer: Some(BusId::new(site.engineer.clone())),
    }
}
