//! The roster (#58 design §5, §6): the people, and the lineups ("bands")
//! that place them on the site's sets, name the inputs whose owner or label
//! depends on the band, and name their leaders. A private `roster.toml`
//! beside the site file holds it (P6: this repository has only the synthetic
//! `config/test-roster.toml`); without the file the server runs as before
//! (legacy mode). Sets and inputs are the site's `[engine]` hardware, never
//! declared here.
//!
//! Validation lists every problem together, in ids only (never a name): on
//! its own ([`Roster::problems`]) and against the engine's topology
//! ([`Roster::problems_against`]). A value that is no valid id (a name typed
//! into an id field) is named by its field and position, never echoed; parse
//! errors carry a line and the message with its quoted spans dropped. So no
//! status file or log line holds a person's name.

use std::collections::{BTreeMap, HashMap, HashSet};

use iem_engine_proto::{TopologyInfo, valid_id};
use serde::Deserialize;

use crate::config::validate_member_id;

/// The roster's file name, beside the site file.
pub const ROSTER_FILE: &str = "roster.toml";

/// The engineer's login id: never a person (the engineer belongs to no band).
const ENGINEER: &str = "engineer";

/// The longest lineup id: it names a directory (`lineups/<id>/`).
pub const MAX_LINEUP_ID: usize = 32;

/// People and lineups.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Roster {
    pub people: Vec<Person>,
    pub lineups: Vec<Lineup>,
}

/// A person: the id is the login, the URL and the key of the PIN, the photo
/// and every file; it never changes. A rename changes the name only.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Person {
    pub id: String,
    pub name: String,
    /// At the migration from the legacy layout (once): the former member id
    /// whose presets, history and pins/hides this person gets (never its PIN
    /// or photo), for a person who used that login until now (§9, §11).
    #[serde(default)]
    pub takes_over: Option<String>,
}

/// A band: who wears which set, whose input is which, who leads it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lineup {
    pub id: String,
    pub name: String,
    /// The band whose people name the sets in Dante (G6); exactly one.
    #[serde(default)]
    pub main: bool,
    /// People of this lineup who hear the others' mixes and switch the band.
    #[serde(default)]
    pub leaders: Vec<String>,
    /// Set (an engine mix id) → the person who wears it.
    pub sets: BTreeMap<String, String>,
    /// Inputs whose label or owner depends on the band; the others keep the
    /// site's label and have no owner.
    #[serde(default)]
    pub inputs: BTreeMap<String, LineupInput>,
}

/// An input's label and owner in one lineup.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineupInput {
    pub name: String,
    #[serde(default)]
    pub owner: Option<String>,
}

/// What the roster needs of one engine mix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixFact {
    pub id: String,
    pub stereo: bool,
    /// The mixes it hears whole, then its peers (its level slots' order).
    pub hears: Vec<String>,
}

/// What the roster needs of the engine's topology: its mixes, inputs and
/// the engineer's mix.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TopologyFacts {
    pub mixes: Vec<MixFact>,
    pub inputs: Vec<String>,
    pub engineer: String,
}

/// Why a roster or a site file could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RosterError {
    #[error("roster file: {0}")]
    Parse(String),
    #[error("site file: {0}")]
    Site(String),
}

/// A lineup id: 1 to 32 of `a-z 0-9 _ -` (it names a directory).
pub fn valid_lineup_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_LINEUP_ID
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
}

/// `message` with every quoted span, `"…"` or `` `…` ``, cut to its quote
/// marks around an ellipsis; inside a span a backslash escapes the next
/// character, and an unclosed span runs to the end. serde quotes the
/// offending value and toml backticks a key, and either may be a name.
fn redact(message: &str) -> String {
    let mut out = String::new();
    let mut open: Option<char> = None;
    let mut escaped = false;
    for c in message.chars() {
        match open {
            None if c == '"' || c == '`' => {
                out.push(c);
                out.push('\u{2026}');
                out.push(c);
                open = Some(c);
            }
            None => out.push(c),
            Some(_) if escaped => escaped = false,
            Some(_) if c == '\\' => escaped = true,
            Some(q) if c == q => open = None,
            Some(_) => {}
        }
    }
    out
}

/// A parse error as a line number and the message with every quoted span
/// dropped ([`redact`]): toml's own rendering shows the line, and neither
/// it nor a quoted value may reach a status file or a log.
fn parse_error(text: &str, e: &toml::de::Error) -> String {
    let message = redact(e.message());
    match e.span() {
        Some(span) => {
            let end = span.start.min(text.len());
            let line = text.as_bytes()[..end]
                .iter()
                .filter(|b| **b == b'\n')
                .count()
                + 1;
            format!("line {line}: {message}")
        }
        None => message,
    }
}

/// A member-style id that is lower case: what a person id and a
/// `takes_over` id must be.
fn lower_member_id(id: &str) -> bool {
    validate_member_id(id).is_ok() && !id.chars().any(|c| c.is_ascii_uppercase())
}

/// How a problem names a value: `'<value>'` when it is a valid id, else
/// `#<n>`, its 1-based position. A name typed into an id field never
/// reaches a status file or a log.
fn quoted(value: &str, valid: bool, n: usize) -> String {
    if valid {
        format!("'{value}'")
    } else {
        format!("#{n}")
    }
}

/// [`quoted`] without the quotes (the leader check's sentence).
fn unquoted(value: &str, valid: bool, n: usize) -> String {
    if valid {
        value.to_string()
    } else {
        format!("#{n}")
    }
}

/// A set's wearer or an input's owner as a problem names it: `'<id>'`, or
/// `fallback` when it is no person id.
fn someone(id: &str, fallback: &str) -> String {
    if lower_member_id(id) {
        format!("'{id}'")
    } else {
        fallback.to_string()
    }
}

impl Lineup {
    /// The set `person` wears in this lineup.
    pub fn set_of(&self, person: &str) -> Option<&str> {
        self.sets
            .iter()
            .find(|(_, p)| p.as_str() == person)
            .map(|(set, _)| set.as_str())
    }

    /// The person who wears `set` in this lineup.
    pub fn person_on(&self, set: &str) -> Option<&str> {
        self.sets.get(set).map(String::as_str)
    }

    /// Whether `person` plays (wears a set) in this lineup.
    pub fn plays(&self, person: &str) -> bool {
        self.set_of(person).is_some()
    }

    /// Whether `person` leads this lineup.
    pub fn leads(&self, person: &str) -> bool {
        self.leaders.iter().any(|l| l == person)
    }
}

impl Roster {
    /// Reads a roster file (unknown keys are errors). Validation is
    /// [`Roster::problems`] and [`Roster::problems_against`].
    pub fn parse(text: &str) -> Result<Roster, RosterError> {
        toml::from_str(text).map_err(|e| RosterError::Parse(parse_error(text, &e)))
    }

    /// Every problem of the roster on its own, in ids and positions only.
    /// A `takes_over` that names a person is the migration's to refuse
    /// (`takes_over` is read only then), never a problem of the roster.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut ids = HashSet::new();
        let person = |i: usize, p: &Person| {
            format!("person {}", quoted(&p.id, lower_member_id(&p.id), i + 1))
        };
        for (i, p) in self.people.iter().enumerate() {
            let who = person(i, p);
            if !lower_member_id(&p.id) {
                out.push(format!(
                    "{who}: the id must be lower-case a-z 0-9 _ -, at least one"
                ));
            }
            if p.id == ENGINEER {
                out.push(format!("{who} is the engineer's id, never a person's"));
            }
            if !ids.insert(p.id.as_str()) {
                out.push(format!("{who} is listed twice"));
            }
            if p.name.trim().is_empty() {
                out.push(format!("{who} has no name"));
            }
        }
        let mut taken: HashMap<&str, String> = HashMap::new();
        for (i, p) in self.people.iter().enumerate() {
            let Some(t) = p.takes_over.as_deref() else {
                continue;
            };
            let who = person(i, p);
            if !lower_member_id(t) || t == p.id || t == ENGINEER {
                out.push(format!(
                    "{who}: takes_over must be another member's lower-case id"
                ));
            } else if let Some(first) = taken.insert(t, who.clone()) {
                out.push(format!(
                    "takes_over '{t}' is claimed by both {first} and {who}"
                ));
            }
        }
        let mut lineup_ids = HashSet::new();
        for (i, l) in self.lineups.iter().enumerate() {
            let lineup = format!("lineup {}", quoted(&l.id, valid_lineup_id(&l.id), i + 1));
            if !valid_lineup_id(&l.id) {
                out.push(format!(
                    "{lineup}: the id must be 1 to {MAX_LINEUP_ID} of a-z 0-9 _ -"
                ));
            }
            if !lineup_ids.insert(l.id.as_str()) {
                out.push(format!("{lineup} is listed twice"));
            }
            if l.name.trim().is_empty() {
                out.push(format!("{lineup} has no name"));
            }
            let mut placed = HashSet::new();
            for (j, (set, p)) in l.sets.iter().enumerate() {
                let set = format!("set {}", quoted(set, valid_id(set), j + 1));
                let wearer = someone(p, "its wearer");
                if !ids.contains(p.as_str()) {
                    out.push(format!("{lineup}: {set}: {wearer} is not a person"));
                }
                if !placed.insert(p.as_str()) {
                    out.push(format!("{lineup}: {set}: {wearer} wears another set too"));
                }
            }
            for (k, leader) in l.leaders.iter().enumerate() {
                if !placed.contains(leader.as_str()) {
                    let who = quoted(leader, lower_member_id(leader), k + 1);
                    out.push(format!("{lineup}: leader {who} wears no set of it"));
                }
            }
            for (k, (input, i)) in l.inputs.iter().enumerate() {
                let input = format!("input {}", quoted(input, valid_id(input), k + 1));
                if i.name.trim().is_empty() {
                    out.push(format!("{lineup}: {input} has no name"));
                }
                if let Some(o) = &i.owner
                    && !placed.contains(o.as_str())
                {
                    let owner = someone(o, "its owner");
                    out.push(format!(
                        "{lineup}: {input}: owner {owner} wears no set of it"
                    ));
                }
            }
        }
        let mains = self.lineups.iter().filter(|l| l.main).count();
        if mains != 1 {
            out.push(format!("exactly one lineup must be main, not {mains}"));
        }
        out
    }

    /// Every problem of the roster against the site's topology: a set is a
    /// stereo mix other than the engineer's, an input is the site's, and
    /// each leader's set hears every other set of their lineup (whole or as
    /// a peer). Values that are no valid id are named by position.
    pub fn problems_against(&self, facts: &TopologyFacts) -> Vec<String> {
        let mut out = Vec::new();
        let mix = |id: &str| facts.mixes.iter().find(|m| m.id == id);
        for (i, l) in self.lineups.iter().enumerate() {
            let valid = valid_lineup_id(&l.id);
            let lineup = format!("lineup {}", quoted(&l.id, valid, i + 1));
            for (j, set) in l.sets.keys().enumerate() {
                if !mix(set.as_str()).is_some_and(|m| m.stereo && m.id != facts.engineer) {
                    out.push(format!(
                        "{lineup}: set {} is not a stereo mix of the site other than the engineer's",
                        quoted(set, valid_id(set), j + 1)
                    ));
                }
            }
            for (k, input) in l.inputs.keys().enumerate() {
                if !facts.inputs.contains(input) {
                    out.push(format!(
                        "{lineup}: input {} is not an input of the site",
                        quoted(input, valid_id(input), k + 1)
                    ));
                }
            }
            let line = if valid {
                l.id.clone()
            } else {
                format!("lineup #{}", i + 1)
            };
            let sets: Vec<&String> = l.sets.keys().collect();
            for (k, leader) in l.leaders.iter().enumerate() {
                let Some(set) = l.set_of(leader) else {
                    continue; // problems() names a leader without a set
                };
                let who = unquoted(leader, lower_member_id(leader), k + 1);
                let n = sets.iter().position(|s| s.as_str() == set).unwrap_or(0) + 1;
                let own = unquoted(set, valid_id(set), n);
                let hears = mix(set).map(|m| m.hears.as_slice()).unwrap_or_default();
                for (j, other) in sets.iter().enumerate() {
                    if other.as_str() != set && !hears.contains(other) {
                        out.push(format!(
                            "leader {who} in {line}: set {own} does not hear {}; the site needs peers (F30)",
                            unquoted(other, valid_id(other), j + 1)
                        ));
                    }
                }
            }
        }
        out
    }

    pub fn person(&self, id: &str) -> Option<&Person> {
        self.people.iter().find(|p| p.id == id)
    }

    pub fn lineup(&self, id: &str) -> Option<&Lineup> {
        self.lineups.iter().find(|l| l.id == id)
    }

    /// The main lineup. Only for a roster without problems, which has
    /// exactly one ([`Roster::problems`]); panics otherwise.
    pub fn main(&self) -> &Lineup {
        self.lineups
            .iter()
            .find(|l| l.main)
            .expect("a roster without problems has a main lineup")
    }

    /// The set `person` wears in `lineup`.
    pub fn set_of(&self, lineup: &str, person: &str) -> Option<&str> {
        self.lineup(lineup)?.set_of(person)
    }

    /// Whether `person` leads any lineup (a leader may switch the band from
    /// any of them, §7).
    pub fn leads_any(&self, person: &str) -> bool {
        self.lineups.iter().any(|l| l.leads(person))
    }
}

impl From<&TopologyInfo> for TopologyFacts {
    /// The facts of the engine's announced topology (a roster reload checks
    /// against the running engine).
    fn from(topo: &TopologyInfo) -> Self {
        Self {
            mixes: topo
                .mixes
                .iter()
                .map(|m| MixFact {
                    id: m.id.0.clone(),
                    stereo: m.channels == 2,
                    hears: m
                        .mixes
                        .iter()
                        .chain(&m.peers)
                        .map(|h| h.0.clone())
                        .collect(),
                })
                .collect(),
            inputs: topo.inputs.iter().map(|i| i.id.0.clone()).collect(),
            engineer: topo.engineer.0.clone(),
        }
    }
}

/// The site file's `[engine]` table as the roster needs it; every other
/// table and key is ignored (the engine checks them).
#[derive(Deserialize)]
struct SiteFile {
    engine: Option<EngineTable>,
}

#[derive(Deserialize)]
struct EngineTable {
    engineer: String,
    #[serde(default)]
    inputs: Vec<EngineInput>,
    #[serde(default)]
    mixes: Vec<EngineMix>,
}

#[derive(Deserialize)]
struct EngineInput {
    id: String,
}

#[derive(Deserialize)]
struct EngineMix {
    id: String,
    tx: Vec<u16>,
    #[serde(default)]
    mixes: Vec<String>,
    #[serde(default)]
    peers: Vec<String>,
}

/// The topology facts of a site file's `[engine]` table (`iem-server roster
/// check`, with no engine running).
pub fn site_facts(site_text: &str) -> Result<TopologyFacts, RosterError> {
    let site: SiteFile =
        toml::from_str(site_text).map_err(|e| RosterError::Site(parse_error(site_text, &e)))?;
    let engine = site
        .engine
        .ok_or_else(|| RosterError::Site("no [engine] table".into()))?;
    Ok(TopologyFacts {
        mixes: engine
            .mixes
            .into_iter()
            .map(|m| MixFact {
                stereo: m.tx.len() == 2,
                hears: m.mixes.into_iter().chain(m.peers).collect(),
                id: m.id,
            })
            .collect(),
        inputs: engine.inputs.into_iter().map(|i| i.id).collect(),
        engineer: engine.engineer,
    })
}

#[cfg(test)]
mod tests;
