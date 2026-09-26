//! `site.toml` through the engine's own parser and compiler: the topology
//! the importer compares with, and the compiled topology the state is
//! reconciled on.

use std::path::Path;

use iem_engine::site::{Site, SiteError, load};
use iem_engine::topology::{Topology as Compiled, compile};
use iem_engine_proto::{GroupId, InputId, MixId};
use iem_rpp::topology::{TopoGroup, TopoInput, TopoMix, Topology};

use crate::Failure;

pub struct SiteFile {
    pub site: Site,
    pub compiled: Compiled,
    pub topology: Topology,
}

pub fn open(path: &Path) -> Result<SiteFile, Failure> {
    let bad = |e: String| Failure::input(format!("{}: {e}", path.display()));
    let site = load(path).map_err(|e| bad(e.to_string()))?;
    let compiled = compile(&site).map_err(|e| bad(e.to_string()))?;
    Ok(SiteFile {
        topology: topology(&site),
        site,
        compiled,
    })
}

/// Like [`open`], but `Ok(None)` while the file has no `[engine]` table yet
/// (the first import proposes one).
pub fn open_optional(path: &Path) -> Result<Option<SiteFile>, Failure> {
    match load(path) {
        Err(SiteError::NoEngineTable) => Ok(None),
        _ => open(path).map(Some),
    }
}

/// The `[engine]` table as the importer's topology.
pub fn topology(site: &Site) -> Topology {
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
        groups: site
            .groups
            .iter()
            .map(|g| TopoGroup {
                id: GroupId::new(g.id.clone()),
                inputs: g.inputs.iter().map(|i| InputId::new(i.clone())).collect(),
            })
            .collect(),
        mixes: site
            .mixes
            .iter()
            .map(|m| TopoMix {
                id: MixId::new(m.id.clone()),
                tx: m.tx.clone(),
                mixes: m.mixes.iter().map(|h| MixId::new(h.clone())).collect(),
            })
            .collect(),
        engineer: Some(MixId::new(site.engineer.clone())),
    }
}
