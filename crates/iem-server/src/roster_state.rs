//! Roster mode (#58 design §5, §6): the roster the server runs on and its
//! active lineup. `AppState.roster` holds `None` in legacy mode (no
//! `roster.toml`), where the site's `[[members]]` are the members as before.
//! In roster mode the members are the active lineup's people on their sets,
//! plus the site's engineer entry; every person is a login tile, playing or
//! not.

use std::collections::BTreeSet;
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

/// Every person's set (#58, the owner's look at the live system): a mix
/// any lineup places a person on, or one the site's `[[members]]` give a
/// member other than the engineer. A set only the site's legacy members
/// name is a person's who left: nobody wears it in any lineup. The
/// engineer's and the translator's mixes are no person's set.
pub fn person_sets(cfg: &Config, roster: &Roster) -> BTreeSet<String> {
    cfg.members
        .iter()
        .filter(|m| m.id != ENGINEER_ID)
        .map(|m| m.mix.clone())
        .chain(roster.lineups.iter().flat_map(|l| l.sets.keys().cloned()))
        .collect()
}

/// Whom the input `id` belongs to (#58, owner option 1): the owner each
/// lineup gives it; only when no lineup gives it one, the site's
/// `[[inputs]]` owner (a member id of the legacy layout, a departed
/// person's when no roster person has it). A legacy owner never outvotes
/// the roster (#58 review).
pub fn input_owners<'a>(
    cfg: &'a Config,
    roster: &'a Roster,
    id: &'a str,
) -> impl Iterator<Item = &'a str> + 'a {
    let lineups: Vec<&'a str> = roster
        .lineups
        .iter()
        .filter_map(|l| l.inputs.get(id)?.owner.as_deref())
        .collect();
    let owners = if lineups.is_empty() {
        cfg.inputs
            .iter()
            .filter(|m| m.id == id)
            .filter_map(|m| m.owner.as_deref())
            .collect()
    } else {
        lineups
    };
    owners.into_iter()
}

/// Whether a person's page leaves the input `id` out while `lineup` plays
/// (#58, owner option 1): `lineup` does not list it, and it belongs to
/// someone who does not play in `lineup` ([`input_owners`]: another
/// lineup's person, a departed member), never the engineer. The inputs of
/// the playing people and the shared ones (hand mics, stems, tech, the
/// engineer's mic) stay; an input `lineup` lists is its own, owner or not.
pub fn foreign_input(cfg: &Config, roster: &Roster, lineup: &Lineup, id: &str) -> bool {
    !lineup.inputs.contains_key(id)
        && input_owners(cfg, roster, id).any(|o| o != ENGINEER_ID && !lineup.plays(o))
}

/// Whether the input `id` belongs only to departed people (#58, the owner
/// on the live system, 2026-10-24): it has an owner other than the
/// engineer ([`input_owners`]) and none of its owners is a roster person.
/// Such an input is on no page, the engineer's included (`view::shows_input`).
pub fn departed_input(cfg: &Config, roster: &Roster, id: &str) -> bool {
    let mut owners = input_owners(cfg, roster, id)
        .filter(|o| *o != ENGINEER_ID)
        .peekable();
    owners.peek().is_some() && owners.all(|o| !roster.people.iter().any(|p| p.id == o))
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
