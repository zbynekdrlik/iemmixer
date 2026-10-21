//! Roster mode (#58 design §5, §6): the roster the server runs on and its
//! active lineup. `AppState.roster` holds `None` in legacy mode (no
//! `roster.toml`), where the site's `[[members]]` are the members as before.
//! In roster mode the members are the active lineup's people on their sets,
//! plus the site's engineer entry; every person is a login tile, playing or
//! not.

use std::sync::Arc;

use iem_core::config::{Config, SiteMember};
use iem_core::roster::{Lineup, Roster};

use crate::pin_store::ENGINEER_ID;

/// The roster in use and its active lineup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterState {
    pub roster: Arc<Roster>,
    /// SHA-256 (hex) of the roster file it was read from.
    pub sha256: String,
    /// The active lineup's id.
    pub active: String,
}

impl RosterState {
    /// The active lineup. A roster is applied only while it holds its active
    /// lineup; should it not, the main lineup stands in.
    pub fn lineup(&self) -> &Lineup {
        self.roster
            .lineup(&self.active)
            .unwrap_or_else(|| self.roster.main())
    }

    /// Whether `person` leads the active lineup.
    pub fn is_leader(&self, person: &str) -> bool {
        self.lineup().leads(person)
    }
}

/// One login tile (`GET /api/members`): a person, or in legacy mode a site
/// member, and whether they play in the active lineup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonView {
    pub id: String,
    pub name: String,
    pub playing: bool,
}

/// The members of the site view before the topology filters them: legacy,
/// the site's `[[members]]`; roster mode, the active lineup's people on
/// their sets in set order, then the site's engineer entry.
pub fn site_members(cfg: &Config, roster: Option<&RosterState>) -> Vec<SiteMember> {
    let Some(r) = roster else {
        return cfg.members.clone();
    };
    r.lineup()
        .sets
        .iter()
        .filter_map(|(set, person)| {
            r.roster.person(person).map(|p| SiteMember {
                id: p.id.clone(),
                name: p.name.clone(),
                mix: set.clone(),
            })
        })
        .chain(cfg.member(ENGINEER_ID).cloned())
        .collect()
}

/// Every login tile: legacy, the site's members in their order, all
/// playing; roster mode, the active lineup's people in set order, then the
/// other people in roster order (not playing), then the site's engineer.
pub fn people(cfg: &Config, roster: Option<&RosterState>) -> Vec<PersonView> {
    let tile = |m: &SiteMember| PersonView {
        id: m.id.clone(),
        name: m.name.clone(),
        playing: true,
    };
    let Some(r) = roster else {
        return cfg.members.iter().map(tile).collect();
    };
    let lineup = r.lineup();
    let mut out: Vec<PersonView> = site_members(cfg, roster)
        .iter()
        .filter(|m| m.id != ENGINEER_ID)
        .map(tile)
        .collect();
    out.extend(
        r.roster
            .people
            .iter()
            .filter(|p| !lineup.plays(&p.id))
            .map(|p| PersonView {
                id: p.id.clone(),
                name: p.name.clone(),
                playing: false,
            }),
    );
    out.extend(cfg.member(ENGINEER_ID).map(tile));
    out
}

#[cfg(test)]
pub(crate) mod tests;
