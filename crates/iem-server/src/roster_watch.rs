//! Loading, checking and reloading the roster (#58 design §9): the private
//! `roster.toml` beside the site file. Without it (and without a roster
//! applied before) the server runs in legacy mode. At the start a roster
//! must parse, have no problems and fit the site's topology; one that does
//! not refuses the start, unless the roster applied last can stand in
//! ([`load`], [`fallback_alarm`]); the legacy layout is migrated first (or
//! a committed migration finished, at every start). While the server runs,
//! [`watch`] polls the file: a change that parses, has no problems, fits the
//! engine's topology and keeps the active lineup is applied without a
//! restart (a running legacy server adopts its first roster the same way);
//! any other is refused and the roster in use stays. `roster.status.json`
//! says what became of each file, in ids and counts only (P6).
//!
//! Applying a roster: a person who moves to another set of a lineup takes
//! their mix along ([`moves`], [`apply_moves`]), and the lineup's rules hold
//! on every set ([`fit_recorded`]: a set worn by no leader hears no other mix, a set
//! nobody wears is silent), in every inactive lineup's stored state and in
//! the active lineup's running one through one `ImportState`, before the new
//! roster is swapped in; then the people it removed are removed for good,
//! their PIN hash, photo and files deleted ([`follow_people`]; a person added
//! again starts fresh).
//!
//! A roster that comes into use without a synced engine (the start, a
//! legacy server's adoption) is `AppState::unsettled` until [`settle`] has
//! put it into the states, under the switching lock, once the engine has
//! synced: no page changes a mix and no switch or restore runs before. Every
//! roster a state may follow is kept (`LineupStore::keep_roster`,
//! `lineups/rosters.kept/<sha256>.toml`) and each state names its own, so the
//! start moves the mixes of people a roster changed while the server was
//! down from the roster each state follows; one without a marker follows
//! the roster applied last (`LineupStore::applied_roster`).

use std::collections::BTreeSet;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

use iem_core::backup::MixerBackup;
use iem_core::config::Config;
use iem_core::roster::{Lineup, ROSTER_FILE, Roster, TopologyFacts, site_facts, valid_person_id};
use iem_engine_proto::shape;
use iem_engine_proto::{Cmd, MixId, MixState, TopologyInfo, Transient};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::engine::client::EngineError;
use crate::lineup_store::{
    ACTIVE_FILE, LineupStore, Pending, Running, SWITCH_FILE, migrate_legacy, migration_problems,
};
use crate::people_removal::{drop_archive, held_ids, remove_person};
use crate::pin_store::{ENGINEER_ID, PinStore};
use crate::roster_state::RosterState;

/// What became of the latest roster file, beside it.
pub const STATUS_FILE: &str = "roster.status.json";
/// How often [`watch`] looks at the roster file.
pub const POLL: Duration = Duration::from_secs(2);

/// The problem of a roster file that is not UTF-8 text.
const NOT_UTF8: &str = "roster file: not UTF-8 text";

/// Refused tries [`settle_when_synced`] makes before it gives up.
pub const MAX_SETTLES: u32 = 3;

/// A roster the states may follow, and the SHA-256 of its file (what a
/// state's marker names: `MixerBackup::follows`, `Running::follows`).
#[derive(Debug, Clone, PartialEq)]
pub struct Base {
    pub roster: Arc<Roster>,
    pub sha256: String,
}

/// The roster in use has not reached the states yet ([`settle`]): they
/// follow `base` (`None`: not known, a legacy layout or a switch a restart
/// cut short, so nobody's mix moves). While `refusing`, no page changes a
/// mix and no switch or restore runs; a settle that gave up frees the pages
/// but keeps `base` for the next roster change (#58 re-review).
#[derive(Debug, Clone, PartialEq)]
pub struct Unsettled {
    pub base: Option<Base>,
    pub refusing: bool,
}

impl Unsettled {
    /// A roster that has just come into use: the pages wait for it.
    pub fn new(base: Option<Base>) -> Self {
        Self {
            base,
            refusing: true,
        }
    }
}

/// What the states follow at the start: the roster recorded last (`None`
/// when there is none or it cannot be read, each logged).
pub fn applied_base(config_dir: &Path) -> Option<Base> {
    match LineupStore::new(config_dir).applied_roster() {
        Ok(Some(text)) => match Roster::parse(&text) {
            Ok(roster) => Some(Base {
                roster: Arc::new(roster),
                sha256: sha256_hex(text.as_bytes()),
            }),
            Err(e) => {
                tracing::error!(error = %e, "the roster applied last does not parse: nobody's mix moves");
                None
            }
        },
        Ok(None) => {
            tracing::info!("no roster applied before: only its rules reach the states");
            None
        }
        Err(e) => {
            tracing::error!(error = %e, "the roster applied last is unreadable: nobody's mix moves");
            None
        }
    }
}

/// Keeps `text` (SHA-256 `sha256`) among the rosters a state may follow
/// (`LineupStore::keep_roster`; a failure is logged: a state marked with it
/// then moves nobody's mix).
fn keep(config_dir: &Path, sha256: &str, text: &str) {
    if let Err(e) = LineupStore::new(config_dir).keep_roster(sha256, text) {
        tracing::error!(error = %e, "a roster in use was not kept");
    }
}

/// Records `text` as the roster the states follow (a failure is logged:
/// the next start then moves nobody's mix).
fn record(config_dir: &Path, text: &str) {
    if let Err(e) = LineupStore::new(config_dir).set_applied_roster(text) {
        tracing::error!(error = %e, "the roster the states follow was not recorded");
    }
}

/// `roster.status.json`: the result for the file with `sha256`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// SHA-256 (hex) of the roster file it is about.
    pub sha256: String,
    /// It is in use and nothing failed.
    pub ok: bool,
    /// It is in use (a refused roster is not; the one before stays).
    pub applied: bool,
    /// Unix seconds.
    pub at: i64,
    /// Why it was refused, or what failed once it was applied.
    pub problems: Vec<String>,
    /// People it brought into use (new ones: nothing of a person removed
    /// before comes back).
    #[serde(default)]
    pub added: Vec<String>,
    /// People it took out of use (removed: their data deleted).
    #[serde(default)]
    pub removed: Vec<String>,
}

impl Status {
    fn applied(
        sha256: &str,
        added: Vec<String>,
        removed: Vec<String>,
        problems: Vec<String>,
    ) -> Self {
        Self {
            sha256: sha256.to_string(),
            ok: problems.is_empty(),
            applied: true,
            at: chrono::Utc::now().timestamp(),
            problems,
            added,
            removed,
        }
    }

    fn refused(sha256: &str, problems: Vec<String>) -> Self {
        Self {
            sha256: sha256.to_string(),
            ok: false,
            applied: false,
            at: chrono::Utc::now().timestamp(),
            problems,
            added: Vec::new(),
            removed: Vec::new(),
        }
    }
}

/// What a roster file did to the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// The roster in use already (same SHA-256): nothing done.
    Unchanged,
    /// In use now. `added` and `removed` are people (ids); `problems` what
    /// failed after it went into use (a removed person's file not deleted).
    Applied {
        added: Vec<String>,
        removed: Vec<String>,
        problems: Vec<String>,
    },
    /// Not in use; the roster before stays. Why.
    Refused(Vec<String>),
    /// Not in use yet: the engine cannot take it now (not synced, gone, no
    /// answer); the roster before stays, no status is written and
    /// [`watch`] tries the file again at its next poll. Why.
    Deferred(Vec<String>),
}

/// SHA-256 of `bytes`, lower-case hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Writes the status file.
pub fn write_status(config_dir: &Path, status: &Status) -> io::Result<()> {
    let json = serde_json::to_string_pretty(status).map_err(io::Error::other)?;
    crate::atomic_write(&config_dir.join(STATUS_FILE), &json)
}

/// [`write_status`], a failure only logged (the roster's fate does not
/// depend on it).
fn note(config_dir: &Path, status: &Status) {
    if let Err(e) = write_status(config_dir, status) {
        tracing::error!(error = %e, "the roster status file was not written");
    }
}

/// The roster file's bytes; `None` when there is none.
fn read_roster(config_dir: &Path) -> Result<Option<Vec<u8>>, String> {
    let path = config_dir.join(ROSTER_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// The roster in `text`, or every reason it cannot be used: it does not
/// parse, it has problems of its own, or (with the engine's topology
/// known) problems against that topology.
pub fn validate(text: &str, facts: Option<&TopologyFacts>) -> Result<Roster, Vec<String>> {
    let roster = Roster::parse(text).map_err(|e| vec![e.to_string()])?;
    let mut problems = roster.problems();
    if let Some(facts) = facts {
        problems.extend(roster.problems_against(facts));
    }
    if problems.is_empty() {
        Ok(roster)
    } else {
        Err(problems)
    }
}

/// The refusal of a roster that drops the lineup that plays.
fn missing_active(active: &str) -> String {
    format!("the roster has no lineup '{active}', which plays: switch to a lineup it keeps first")
}

/// The refusal of a roster that keeps none of the people of the roster in
/// use (#58 review: every person a roster lacks is deleted, so a valid but
/// wrong file, another band's or ids typed anew, would delete everyone).
const EVERYONE_REMOVED: &str =
    "the roster keeps none of the people of the roster in use: it would remove them all";

/// Whether `new` keeps none of `old`'s people (`old` has some).
fn removes_everyone(old: &Roster, new: &Roster) -> bool {
    !old.people.is_empty() && old.people.iter().all(|p| new.person(&p.id).is_none())
}

/// The problem of a live PIN whose id is no lower-case person id: it is
/// kept (#58 review: on case-insensitive NTFS its photo and files would be
/// a person's), and the PIN store's file needs a look by hand.
const NOT_A_PERSON_ID: &str = "a PIN hash whose id is no lower-case person id was not removed";

/// What [`follow_people`] did, in ids.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct People {
    /// Whom it removed (something of theirs was deleted).
    pub removed: Vec<String>,
    /// A removal that failed (the person and the error's kind).
    pub problems: Vec<String>,
}

/// The people's data follows `roster` (#58; the owner's ruling of
/// 2026-10-24: a removed person is removed, not archived and not hidden):
/// everyone in `removed`, anyone else with a live PIN, and every id that
/// still holds a photo or a lineup's file ([`held_ids`]: a removal that
/// failed part-way, a person removed while the server was stopped; #58
/// review), who is no person of it (nor the engineer) is removed
/// ([`remove_person`]: their PIN hash, photo and every lineup's files
/// deleted), and the PIN hashes and the `archive/` folder an older server
/// archived are deleted. Nothing is ever brought back: a person added
/// again starts fresh. A live PIN whose id is no lower-case person id is
/// kept and named. A failure is logged and named in `problems`; the rest
/// still runs.
pub fn follow_people(
    config_dir: &Path,
    roster: &Roster,
    pins: &mut PinStore,
    removed: &[String],
) -> People {
    let mut out = People::default();
    if let Err(e) = pins.drop_archived() {
        tracing::error!(error = %e, "the PIN hashes an older server archived were not deleted");
        out.problems.push(format!(
            "the PIN hashes an older server archived were not deleted: {}",
            e.kind()
        ));
    }
    if let Err(e) = drop_archive(config_dir) {
        tracing::error!(error = %e, "the archive an older server wrote was not deleted");
        out.problems.push(format!(
            "the archive an older server wrote was not deleted: {}",
            e.kind()
        ));
    }
    let held = held_ids(config_dir).unwrap_or_else(|e| {
        tracing::error!(error = %e, "the people's photos and files were not looked through");
        out.problems.push(format!(
            "the people's photos and files were not looked through: {}",
            e.kind()
        ));
        BTreeSet::new()
    });
    let mut gone: Vec<String> = removed.to_vec();
    for id in pins.member_ids().into_iter().chain(held) {
        if !gone.contains(&id) {
            gone.push(id);
        }
    }
    for id in &gone {
        if id == ENGINEER_ID || roster.person(id).is_some() {
            continue;
        }
        if !valid_person_id(id) {
            // Only a PIN entry can be one (the roster's people are valid
            // ids); never echoed, it may be a name (P6).
            tracing::error!("{NOT_A_PERSON_ID}");
            out.problems.push(NOT_A_PERSON_ID.to_string());
            continue;
        }
        match remove_person(config_dir, pins, id) {
            Ok(true) => out.removed.push(id.clone()),
            Ok(false) => {}
            Err(e) => {
                tracing::error!(
                    person = %id,
                    error = %e,
                    "a person the roster removed was not wholly deleted"
                );
                out.problems
                    .push(format!("person '{id}' was not removed: {}", e.kind()));
            }
        }
    }
    out
}

/// Puts `roster` into use on the files' side: the legacy layout migrated
/// (or a committed migration finished), the recorded active lineup (main
/// before any) checked to be the roster's, who is no person of it removed
/// ([`follow_people`]) unless `remove` is false (a fall-back start). The
/// roster state and whom the migration and the removal took out.
fn adopt(
    config_dir: &Path,
    cfg: &Config,
    pins: &mut PinStore,
    roster: Roster,
    sha256: &str,
    remove: bool,
) -> Result<(RosterState, People), Vec<String>> {
    let report = migrate_legacy(config_dir, &roster, cfg, pins).map_err(|e| vec![e.to_string()])?;
    let active = LineupStore::new(config_dir)
        .active()
        .map_err(|e| vec![e.to_string()])?
        .unwrap_or_else(|| roster.main().id.clone());
    if roster.lineup(&active).is_none() {
        return Err(vec![missing_active(&active)]);
    }
    let mut people = if remove {
        follow_people(config_dir, &roster, pins, &[])
    } else {
        tracing::warn!("a fall-back start removes nobody: the next roster that applies does");
        People::default()
    };
    let mut removed = report.removed;
    removed.append(&mut people.removed);
    people.removed = removed;
    let state = RosterState {
        roster: Arc::new(roster),
        sha256: sha256.to_string(),
        active,
    };
    Ok((state, people))
}

/// The roster a start runs on ([`load`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub roster: RosterState,
    /// `roster.toml` could not be used (or there was none): this is the
    /// roster applied last (`AppState::fallback`).
    pub fallback: bool,
}

/// The problem of a start without `roster.toml` beside a recorded roster.
const NOT_FOUND: &str = "roster file: not found";
/// The last problem of a start that fell back to the recorded roster.
const FALLBACK: &str = "in use instead: the roster applied last";

/// What [`take`] puts into use at the start.
#[derive(Debug, Clone, Copy)]
enum Start<'a> {
    /// `roster.toml`, beside the roster applied last when there is one:
    /// refused when it keeps none of that one's people (#58 review); who is
    /// no person of it is removed.
    File(Option<&'a Roster>),
    /// The roster applied last, standing in for a `roster.toml` that cannot
    /// be used: nobody is removed (#58 review: the file has problems, so
    /// its removals are not to be trusted; the next roster that applies
    /// does them).
    Fallback,
}

/// The roster in `bytes` (SHA-256 `sha256`) validated and adopted
/// ([`adopt`]) as `start` says; a refusal writes nothing.
fn take(
    config_dir: &Path,
    cfg: &Config,
    pins: &mut PinStore,
    bytes: &[u8],
    sha256: &str,
    facts: Option<&TopologyFacts>,
    start: Start,
) -> Result<(RosterState, People), Vec<String>> {
    let text = std::str::from_utf8(bytes).map_err(|_| vec![NOT_UTF8.to_string()])?;
    let roster = validate(text, facts)?;
    if let Start::File(Some(base)) = start
        && removes_everyone(base, &roster)
    {
        return Err(vec![EVERYONE_REMOVED.to_string()]);
    }
    let remove = matches!(start, Start::File(_));
    adopt(config_dir, cfg, pins, roster, sha256, remove)
}

/// The roster at the server's start: `None` without `roster.toml` (legacy
/// mode). Otherwise it must parse and have no problems (against `facts`
/// too, when given); the legacy layout is migrated first, or a committed
/// migration finished; its active lineup is the recorded one and must be
/// the roster's; who is no person of it is removed. A `roster.toml` that cannot
/// be used (one that keeps none of the people of the recorded roster too,
/// #58 review), or none, beside a recorded roster
/// (`LineupStore::applied_roster`, the one the states follow) starts with
/// that one (#58 review: a server that does not start is the worst outcome;
/// [`Loaded::fallback`]), removing nobody ([`Start::Fallback`]). The status
/// file says what happened; an error refuses the start.
pub fn load(
    config_dir: &Path,
    cfg: &Config,
    pins: &mut PinStore,
    facts: Option<&TopologyFacts>,
) -> Result<Option<Loaded>, String> {
    let file = read_roster(config_dir)?;
    let recorded = match LineupStore::new(config_dir).applied_roster() {
        Ok(recorded) => recorded,
        Err(e) => {
            tracing::error!(error = %e, "the roster applied last is unreadable");
            None
        }
    };
    let base = recorded.as_deref().and_then(|t| Roster::parse(t).ok());
    let (sha256, problems) = match &file {
        Some(bytes) => {
            let sha256 = sha256_hex(bytes);
            match take(
                config_dir,
                cfg,
                pins,
                bytes,
                &sha256,
                facts,
                Start::File(base.as_ref()),
            ) {
                Ok((state, people)) => {
                    if let Ok(text) = std::str::from_utf8(bytes) {
                        keep(config_dir, &sha256, text);
                    }
                    tracing::info!(
                        lineup = %state.active,
                        people = state.roster.people.len(),
                        removed = people.removed.len(),
                        problems = people.problems.len(),
                        "roster mode"
                    );
                    note(
                        config_dir,
                        &Status::applied(&sha256, Vec::new(), people.removed, people.problems),
                    );
                    return Ok(Some(Loaded {
                        roster: state,
                        fallback: false,
                    }));
                }
                Err(problems) => (sha256, problems),
            }
        }
        None => (String::new(), vec![NOT_FOUND.to_string()]),
    };
    if let Some(text) = recorded {
        let copy = sha256_hex(text.as_bytes());
        match take(
            config_dir,
            cfg,
            pins,
            text.as_bytes(),
            &copy,
            facts,
            Start::Fallback,
        ) {
            Ok((state, _)) => {
                keep(config_dir, &copy, &text);
                tracing::error!(
                    lineup = %state.active,
                    problems = problems.len(),
                    "roster.toml cannot be used: the roster applied last is in use"
                );
                let mut why = problems;
                why.push(FALLBACK.to_string());
                note(config_dir, &Status::refused(&sha256, why));
                return Ok(Some(Loaded {
                    roster: state,
                    fallback: true,
                }));
            }
            Err(more) => tracing::error!(
                problems = more.len(),
                "the roster applied last cannot be used either"
            ),
        }
    }
    if file.is_none() {
        return Ok(None);
    }
    note(config_dir, &Status::refused(&sha256, problems.clone()));
    Err(format!("{ROSTER_FILE}: {}", problems.join("; ")))
}

/// The people of `new` that `old` lacks, and those of `old` that `new`
/// lacks (ids, roster order).
pub fn people_diff(old: &Roster, new: &Roster) -> (Vec<String>, Vec<String>) {
    let missing = |from: &Roster, other: &Roster| -> Vec<String> {
        from.people
            .iter()
            .filter(|p| other.person(&p.id).is_none())
            .map(|p| p.id.clone())
            .collect()
    };
    (missing(new, old), missing(old, new))
}

/// The people who wear another set of `lineup` in `new` than in `old`:
/// `(from set, to set)` each, so their mix goes along (#58 design §9). A
/// lineup either roster lacks moves nobody.
pub fn moves(old: &Roster, new: &Roster, lineup: &str) -> Vec<(String, String)> {
    let (Some(before), Some(after)) = (old.lineup(lineup), new.lineup(lineup)) else {
        return Vec::new();
    };
    after
        .sets
        .iter()
        .filter_map(|(set, person)| {
            let from = before.set_of(person)?;
            (from != set.as_str()).then(|| (from.to_string(), set.clone()))
        })
        .collect()
}

/// Each `(from, to)`: mix `to` becomes a copy of mix `from` as it was
/// before any move (two people may swap sets); a vacated set keeps its
/// mix. In the copy the level `from` had of `to` becomes its level of
/// `from` (a mix never hears itself; whom it heard there sits there now
/// when two swap).
pub fn apply_moves(state: &mut MixState, moves: &[(String, String)]) {
    let before = state.mixes.clone();
    for (from, to) in moves {
        let (from, to) = (MixId::new(from.as_str()), MixId::new(to.as_str()));
        let Some(mut mix) = before.get(&from).cloned() else {
            continue;
        };
        if let Some(heard) = mix.mixes.remove(&to) {
            mix.mixes.insert(from, heard);
        }
        state.mixes.insert(to, mix);
    }
}

/// Every set of the roster: the mixes any lineup places people on.
fn roster_sets(roster: &Roster) -> BTreeSet<String> {
    roster
        .lineups
        .iter()
        .flat_map(|l| l.sets.keys().cloned())
        .collect()
}

/// `lineup`'s rules on a state (#58 design §7): on a set whose wearer is
/// not one of `lineup`'s leaders every mix it hears, whole or as a peer, is
/// muted, so only a leader hears another person's mix; a set of the roster
/// nobody wears in `lineup` is muted. Only mixes the state holds are
/// touched. `silenced` is the lineup's record of the sets its rules muted
/// because nobody wore them (#58 review): such a set is recorded when the
/// rules mute it, and unmuted and forgotten once someone wears it. A set
/// that was muted already (its wearer's choice) is never recorded, so the
/// rules never unmute it.
pub fn fit_recorded(
    state: &mut MixState,
    roster: &Roster,
    lineup: &Lineup,
    silenced: &mut BTreeSet<String>,
) {
    fit_sets(state, &roster_sets(roster), lineup, silenced);
}

/// [`fit_recorded`] over `sets`: a reload passes the old roster's sets too,
/// so a set the new roster no longer lists at all is silent as well.
fn fit_sets(
    state: &mut MixState,
    sets: &BTreeSet<String>,
    lineup: &Lineup,
    silenced: &mut BTreeSet<String>,
) {
    for set in sets {
        let Some(mix) = state.mixes.get_mut(&MixId::new(set.as_str())) else {
            continue;
        };
        match lineup.person_on(set) {
            None => {
                if !mix.out.muted {
                    mix.out.muted = true;
                    silenced.insert(set.clone());
                }
            }
            Some(person) => {
                if silenced.remove(set) {
                    mix.out.muted = false;
                }
                if !lineup.leads(person) {
                    for level in mix.mixes.values_mut() {
                        level.muted = true;
                    }
                }
            }
        }
    }
}

/// `silenced` after `moves` ([`apply_moves`]): a set that takes a copy of
/// another set's mix takes its record too.
pub fn move_silenced(silenced: &mut BTreeSet<String>, moves: &[(String, String)]) {
    let before = silenced.clone();
    for (from, to) in moves {
        if before.contains(from) {
            silenced.insert(to.clone());
        } else {
            silenced.remove(to);
        }
    }
}

/// The mixes someone solos in `transient` whose set has another wearer in
/// `after` than in `before` (#58 review): the engine keeps a solo across
/// an import, the new wearer must not hear the old one's.
pub(crate) fn soloed_changing(
    transient: &Transient,
    before: &Lineup,
    after: &Lineup,
) -> Vec<MixId> {
    transient
        .solo
        .iter()
        .filter(|s| {
            !s.sources.is_empty() && before.person_on(&s.mix.0) != after.person_on(&s.mix.0)
        })
        .map(|s| s.mix.clone())
        .collect()
}

/// Every mix someone solos in `transient`.
pub(crate) fn every_solo(transient: &Transient) -> Vec<MixId> {
    transient
        .solo
        .iter()
        .filter(|s| !s.sources.is_empty())
        .map(|s| s.mix.clone())
        .collect()
}

/// Ends the solo of each of `mixes` (a failure is logged: the solo then
/// ends when its mix's last page has gone).
pub(crate) async fn end_solos(state: &AppState, mixes: &[MixId]) {
    for mix in mixes {
        let end = Cmd::SetSolo {
            mix: mix.clone(),
            sources: Vec::new(),
        };
        if let Err(e) = state.engine.request_applied(end, None).await {
            tracing::error!(mix = %mix.0, error = %e, "a solo did not end with its set's wearer");
        }
    }
}

/// What the running state follows (one that cannot be read is logged: not
/// known); `held` is the [`state_digest`] of the engine's state, which
/// resolves an import whose answer was never known ([`resolved`]). A
/// resolved record is saved at once (#58 fourth review), so it is never
/// resolved again against a state pages have changed since.
pub(crate) fn running_of(store: &LineupStore, held: &str) -> Option<Running> {
    let running = store.running().unwrap_or_else(|e| {
        tracing::error!(error = %e, "what the running state follows is unknown");
        None
    })?;
    if running.pending.is_none() {
        return Some(running);
    }
    let settled = resolved(running, held);
    note_running(store, &settled);
    Some(settled)
}

/// What the running state follows when it is `lineup`'s: its marker and
/// the record of the sets the rules silenced (none and none otherwise);
/// `held` as for [`running_of`].
pub(crate) fn running_for(
    store: &LineupStore,
    lineup: &str,
    held: &str,
) -> (Option<String>, BTreeSet<String>) {
    match running_of(store, held) {
        Some(r) if r.lineup == lineup => (r.follows, r.silenced),
        _ => (None, BTreeSet::new()),
    }
}

/// `running` with its pending import (`Running::pending`, #58 third review)
/// settled: it went in when the engine holds the state sent (`held` is its
/// digest, the comparison [`landed`] makes), else the record before stays.
fn resolved(mut running: Running, held: &str) -> Running {
    match running.pending.take() {
        Some(p) if p.digest == held => {
            tracing::info!(lineup = %running.lineup, "an import whose answer was lost had gone in");
            Running {
                lineup: running.lineup,
                follows: Some(p.follows),
                silenced: p.silenced,
                pending: None,
            }
        }
        Some(_) => {
            tracing::info!(lineup = %running.lineup, "an import whose answer was lost had not gone in");
            running
        }
        None => running,
    }
}

/// The SHA-256 of `state`'s JSON, how [`Running::pending`] names the state
/// an import sent.
pub(crate) fn state_digest(state: &MixState) -> String {
    serde_json::to_vec(state)
        .map(|json| sha256_hex(&json))
        .unwrap_or_default()
}

/// Records what the running state follows (a failure is logged: a later
/// roster change may move a person's mix there again, and a set the rules
/// silenced may stay muted once someone wears it).
pub(crate) fn note_running(store: &LineupStore, running: &Running) {
    if let Err(e) = store.set_running(running) {
        tracing::error!(error = %e, "what the running state follows was not recorded");
    }
}

/// The engine's announced topology as the roster checks it.
fn engine_facts(state: &AppState) -> Option<TopologyFacts> {
    let topo = state.engine.mirror().topology.clone()?;
    Some(TopologyFacts::from(&*topo))
}

/// Applies the roster `text` (SHA-256 `sha256`) to the running server: see
/// the module docs. Refused (the roster in use stays) when it does not
/// validate, against the engine's topology once announced; when it drops
/// the active lineup; when a stored state cannot be read or written; when
/// the engine refuses the active lineup's new mixes. Deferred (tried again
/// at the next poll) while the engine is not synced, gone or silent. A
/// legacy server adopts it (the migration runs). Never beside a band
/// switch: it waits for one that runs (`AppState::switching`), and pages'
/// changes are refused from its import until the new roster is in use.
pub async fn apply_text(state: &AppState, text: &str, sha256: &str) -> Applied {
    let _running = state.switching.lock().await;
    let current = state.roster();
    if current.as_ref().is_some_and(|r| r.sha256 == sha256) {
        return Applied::Unchanged;
    }
    let roster = match validate(text, engine_facts(state).as_ref()) {
        Ok(r) => r,
        Err(problems) => return Applied::Refused(problems),
    };
    match current {
        None => adopt_live(state, roster, text, sha256).await,
        Some(old) => reload(state, &old, roster, text, sha256).await,
    }
}

/// A running legacy server takes its first roster; its rules reach the
/// states once the engine has synced ([`settle_when_synced`], spawned).
async fn adopt_live(state: &AppState, roster: Roster, text: &str, sha256: &str) -> Applied {
    let adopted = {
        let mut pins = state.pin_store.write().await;
        adopt(
            &state.config_dir,
            &state.site_config,
            &mut pins,
            roster,
            sha256,
            true,
        )
    };
    match adopted {
        Ok((r, people)) => {
            tracing::warn!(lineup = %r.active, "roster mode: a roster is in use from now on");
            keep(&state.config_dir, sha256, text);
            // Before the roster: no page changes a mix until it is settled.
            state.set_unsettled(Some(Unsettled::new(None)));
            state.set_roster(Some(r));
            state.announce_lineup();
            tokio::spawn(settle_when_synced(state.clone()));
            Applied::Applied {
                added: Vec::new(),
                removed: people.removed,
                problems: people.problems,
            }
        }
        Err(problems) => Applied::Refused(problems),
    }
}

/// A stored state a transition writes: the lineup, the state as it was,
/// the state it writes.
type Planned = (String, MixerBackup, MixerBackup);

/// Writes back the stored states a transition had written (best effort).
fn undo(store: &LineupStore, written: &[Planned]) {
    for (id, original, _) in written {
        if let Err(e) = store.save_state(id, original) {
            tracing::error!(lineup = %id, error = %e, "a lineup's stored state was not put back");
        }
    }
}

/// Writes the planned stored states; on a failure puts back those it
/// wrote and says why.
fn write_planned(store: &LineupStore, planned: &[Planned]) -> Result<(), Applied> {
    for (n, (id, _, next)) in planned.iter().enumerate() {
        if let Err(e) = store.save_state(id, next) {
            undo(store, &planned[..n]);
            return Err(Applied::Refused(vec![format!(
                "the stored state of lineup {id} was not written: {}",
                e.kind()
            )]));
        }
    }
    Ok(())
}

/// The rosters the states may follow, for a [`transition`] (#58
/// re-review): a state's marker names one by its SHA-256; a state without
/// one (written before markers, or a backup's) follows `unmarked`.
struct Follows {
    known: Vec<Base>,
    unmarked: Option<Base>,
}

impl Follows {
    /// Learns the roster `marker` names from the kept ones
    /// (`LineupStore::kept_roster`, #58 third review) when it is not known
    /// yet; one that is not kept, or not readable, stays unknown (logged).
    fn learn(&mut self, store: &LineupStore, marker: Option<&str>) {
        let Some(sha) = marker else {
            return;
        };
        if self.known.iter().any(|b| b.sha256 == sha) {
            return;
        }
        match store.kept_roster(sha) {
            Ok(Some(text)) => match Roster::parse(&text) {
                Ok(roster) => self.known.push(Base {
                    roster: Arc::new(roster),
                    sha256: sha.to_string(),
                }),
                Err(e) => {
                    tracing::error!(error = %e, "a kept roster does not parse: nobody's mix moves from it");
                }
            },
            Ok(None) => {
                tracing::warn!(
                    "a state follows a roster not kept here: nobody's mix moves from it"
                );
            }
            Err(e) => {
                tracing::error!(error = %e, "a kept roster is unreadable: nobody's mix moves from it");
            }
        }
    }

    /// The roster a state marked `marker` moves people's mixes from on its
    /// way to the roster `new_sha`: none when it follows that one already,
    /// or one not known (nobody's mix moves then).
    fn base_of(&self, marker: Option<&str>, new_sha: &str) -> Option<&Roster> {
        match marker {
            Some(sha) if sha == new_sha => None,
            Some(sha) => self
                .known
                .iter()
                .find(|b| b.sha256 == sha)
                .map(|b| &*b.roster),
            None => self.unmarked.as_ref().map(|b| &*b.roster),
        }
    }

    /// Every set the rosters known so far list; [`transition`] reads it
    /// before any marker's roster is learned, so it is the roster before's
    /// and the unsettled base's (#58 fourth review).
    fn sets(&self) -> BTreeSet<String> {
        self.known
            .iter()
            .chain(&self.unmarked)
            .flat_map(|b| roster_sets(&b.roster))
            .collect()
    }
}

/// `state` and its record `silenced` made to follow `new`: a person who
/// wears another set of `lineup` in `new` than in `from` takes their mix
/// along (nobody without `from`), then `lineup`'s rules hold on `sets`.
fn follow(
    state: &mut MixState,
    silenced: &mut BTreeSet<String>,
    from: Option<&Roster>,
    new: &Roster,
    sets: &BTreeSet<String>,
    lineup: &Lineup,
    topology: Option<&TopologyInfo>,
) {
    let shift = from.map(|f| moves(f, new, &lineup.id)).unwrap_or_default();
    apply_moves(state, &shift);
    move_silenced(silenced, &shift);
    // A moved mix keeps the slots of the set it came from: shaped as the
    // engine will hold it (the running state, #58 third review).
    if let Some(topology) = topology {
        *state = shape::reconcile(topology, state).0;
    }
    fit_sets(state, sets, lineup, silenced);
}

/// Whether the engine took `sent`, an import whose answer was lost (#58
/// re-review): waits, the pages frozen and nothing put back, until the
/// engine answers a `Ping` again (every change it made before is in the
/// mirror then), and compares the mirror's state with `sent`. An engine
/// still away `AppState::absent_alarm` after the wait began is told to the
/// engineer's devices once (#58 third review); the wait goes on.
async fn landed(state: &AppState, sent: &MixState) -> bool {
    let began = tokio::time::Instant::now();
    let mut alarmed = false;
    loop {
        if state.engine.wait_synced(state.retry).await
            && state.engine.request_applied(Cmd::Ping, None).await.is_ok()
        {
            return state.engine.mirror().state == *sent;
        }
        if absent_alarm_due(alarmed, began.elapsed(), state.absent_alarm) {
            alarmed = true;
            let devices = crate::notify::push_alarm(state, ABSENT_TITLE, ABSENT_BODY).await;
            tracing::warn!(
                devices,
                waited_s = began.elapsed().as_secs(),
                "the engine is away while a roster's import waits: the engineer's devices heard of it"
            );
        }
        tokio::time::sleep(state.retry).await;
    }
}

/// How long [`landed`] waits for an absent engine before the alarm
/// (`AppState::absent_alarm`).
pub const ABSENT_ALARM: Duration = Duration::from_secs(60);
const ABSENT_TITLE: &str = "Engine neodpovedá";
const ABSENT_BODY: &str = "Server poslal enginu zmenu zostavy a engine odvtedy neodpovedá: \
                           mixy sa nedajú meniť, kým sa engine neozve. Server naň čaká ďalej \
                           a zmenu potom dokončí alebo vráti.";

/// Whether [`landed`] alarms now: not yet `alarmed`, and `waited` reached
/// `after`.
fn absent_alarm_due(alarmed: bool, waited: Duration, after: Duration) -> bool {
    !alarmed && waited >= after
}

/// Why a roster waits: its rules reach the running state only through a
/// synced engine.
const NOT_SYNCED: &str = "the engine state is not synced: the roster waits for it";
/// Why a roster waits: what the running state follows once its import is in
/// could not be written before it (`Running::pending`), so nothing went.
const NOT_RECORDED: &str = "what the running state follows could not be written: the roster waits";
/// Why a roster waits: a switch a restart cut short is not finished, so
/// which band's state the engine holds is not known.
const UNFINISHED_SWITCH: &str = "a band switch is not finished: the roster waits for it";

/// What [`transition`] did.
struct Transition {
    /// People who moved to another set of the active lineup.
    moved: usize,
    /// `Some(was)`: the pages are frozen; the caller puts `frozen` back to
    /// `was` once the roster in use is the one the states follow.
    frozen: Option<bool>,
}

/// The states go over to following `new` (SHA-256 `new_sha`), whose lineup
/// `active` plays: every other lineup's stored state, then the running
/// state through one import. Each state moves from the roster its marker
/// names ([`Follows`]): a person who wears another set of a lineup there
/// than in `new` takes their mix along (nobody when that roster is not
/// known, or the state follows `new` already), and `new`'s rules hold on
/// every set either lists ([`fit_sets`]: a set only the old one lists is
/// worn by nobody now). A state is written with its marker and record in
/// one file (the running one's in `running.json`, right after the import),
/// so a transition cut short (a stop, a crash) never moves anyone twice.
///
/// Waits (`Deferred`) for a synced engine before it touches anything.
/// When the running state changes the pages are frozen
/// ([`AppState::freeze`]) before any file is written and the running state
/// is read again; they stay frozen for the caller. An import whose answer
/// is lost (no engine, no answer) is waited out, still frozen
/// ([`landed`]): taken, the transition goes on; not taken, every stored
/// state it wrote is put back, as on any other failure, and the pages are
/// as before.
async fn transition(
    state: &AppState,
    mut follows: Follows,
    new: &Roster,
    new_sha: &str,
    active: &str,
) -> Result<Transition, Applied> {
    let Some(lineup) = new.lineup(active).cloned() else {
        return Err(Applied::Refused(vec![missing_active(active)]));
    };
    if state.frozen_now() {
        // A switch a restart cut short, between its replay's tries.
        return Err(Applied::Deferred(vec![UNFINISHED_SWITCH.to_string()]));
    }
    let topology = {
        let m = state.engine.mirror();
        m.topology.clone().filter(|_| m.synced)
    };
    let Some(topology) = topology else {
        return Err(Applied::Deferred(vec![NOT_SYNCED.to_string()]));
    };
    let topology: &TopologyInfo = &topology;
    let store = LineupStore::new(&state.config_dir);
    // The rules hold on the sets the roster before (or the unsettled base)
    // and the new one list; a roster learned from a marker below only moves
    // people (#58 fourth review).
    let sets: BTreeSet<String> = follows.sets().into_iter().chain(roster_sets(new)).collect();
    // What the running state follows, first: reading it resolves and saves
    // a pending import against the synced engine's state, whatever fails
    // after (#58 fifth review).
    let held = state_digest(&state.engine.mirror().state);
    let (marker, recorded) = running_for(&store, active, &held);
    follows.learn(&store, marker.as_deref());
    // Every other lineup's stored state, each marker's roster learned from
    // the kept ones.
    let mut stored_states = Vec::new();
    for l in new.lineups.iter().filter(|l| l.id != active) {
        match store.state(&l.id) {
            Ok(Some(b)) => {
                follows.learn(&store, b.follows.as_deref());
                stored_states.push((l, b));
            }
            Ok(None) => {}
            Err(e) => {
                return Err(Applied::Refused(vec![format!(
                    "the stored state of lineup {} is unreadable: {}",
                    l.id,
                    e.kind()
                )]));
            }
        }
    }
    // The stored states, worked out first.
    let mut planned: Vec<Planned> = Vec::new();
    for (l, stored) in stored_states {
        let mut next = stored.clone();
        let from = follows.base_of(stored.follows.as_deref(), new_sha);
        follow(
            &mut next.state,
            &mut next.silenced,
            from,
            new,
            &sets,
            l,
            None,
        );
        next.follows = Some(new_sha.to_string());
        if next != stored {
            planned.push((l.id.clone(), stored, next));
        }
    }
    // The running state, through one import.
    let from = follows.base_of(marker.as_deref(), new_sha);
    let moved = from.map_or(0, |f| moves(f, new, active).len());
    // The state the engine holds is shaped by its topology: the import is
    // sent so shaped, and compared so (#58 third review).
    let shaped = |s: &MixState| shape::reconcile(topology, s).0;
    let next_of = |current: &MixState| {
        let mut next = current.clone();
        let mut silenced = recorded.clone();
        follow(
            &mut next,
            &mut silenced,
            from,
            new,
            &sets,
            &lineup,
            Some(topology),
        );
        let running = Running {
            lineup: active.to_string(),
            follows: Some(new_sha.to_string()),
            silenced,
            pending: None,
        };
        (next, running)
    };
    // A solo ends on every set of the active lineup whose wearer changes;
    // when who wore the sets is not known, every solo ends (#58 third
    // review); a state on the new roster already changes nobody's set.
    let already = marker.as_deref() == Some(new_sha);
    let before = from.and_then(|f| f.lineup(active));
    let soloed_now = |t: &Transient| match before {
        _ if already => Vec::new(),
        Some(b) => soloed_changing(t, b, &lineup),
        None => every_solo(t),
    };
    let (current, soloed) = {
        let m = state.engine.mirror();
        (m.state.clone(), soloed_now(&m.transient))
    };
    let (next, running) = next_of(&current);
    if next == shaped(&current) && soloed.is_empty() {
        write_planned(&store, &planned)?;
        note_running(&store, &running);
        return Ok(Transition {
            moved,
            frozen: None,
        });
    }
    let failed = |e: &EngineError| {
        let why = vec![format!(
            "the engine did not take lineup {active}'s new mixes: {e}"
        )];
        // A refusal is the roster's; no engine or no answer is the
        // moment's, and the watcher tries again.
        match e {
            EngineError::Refused(_) => Applied::Refused(why),
            EngineError::Disconnected | EngineError::Timeout => Applied::Deferred(why),
        }
    };
    // No page's change lands between the state read here and the import.
    let was = match state.freeze().await {
        Ok(was) => was,
        Err(e) => return Err(failed(&e)),
    };
    if let Err(not) = write_planned(&store, &planned) {
        // Nothing went in: the pages hear of it only when one missed an
        // engine event (#58 fifth review).
        if !was {
            state.thaw_unchanged();
        }
        return Err(not);
    }
    let (synced, current, soloed) = {
        let m = state.engine.mirror();
        (m.synced, m.state.clone(), soloed_now(&m.transient))
    };
    let taken = if synced {
        let (next, running) = next_of(&current);
        if next == shaped(&current) {
            Ok(running)
        } else {
            // What the running state follows once the import is in, written
            // before it: its answer may be lost with the server (#58 third
            // review). Unwritten, nothing is imported (#58 fourth review);
            // a failed import puts the record before back.
            let prior = Running {
                lineup: active.to_string(),
                follows: marker.clone(),
                silenced: recorded.clone(),
                pending: None,
            };
            let pending = Pending {
                follows: new_sha.to_string(),
                silenced: running.silenced.clone(),
                digest: state_digest(&next),
            };
            let noted = store.set_running(&Running {
                pending: Some(pending),
                ..prior.clone()
            });
            if let Err(e) = noted {
                tracing::error!(lineup = %active, error = %e, "what the running state follows was not recorded before the import: nothing imported");
                Err(Applied::Deferred(vec![NOT_RECORDED.to_string()]))
            } else {
                let sent = match state
                    .engine
                    .request_applied(import(next.clone()), None)
                    .await
                {
                    Ok(()) => Ok(running),
                    Err(e @ EngineError::Refused(_)) => Err(failed(&e)),
                    Err(e) => {
                        tracing::warn!(lineup = %active, error = %e, "a roster's import got no answer: waiting for the engine to answer again");
                        if landed(state, &next).await {
                            tracing::info!(lineup = %active, "the engine had taken the roster's import");
                            Ok(running)
                        } else {
                            Err(failed(&e))
                        }
                    }
                };
                if sent.is_err() {
                    note_running(&store, &prior);
                }
                sent
            }
        }
    } else {
        Err(Applied::Deferred(vec![NOT_SYNCED.to_string()]))
    };
    match taken {
        Ok(running) => {
            note_running(&store, &running);
            end_solos(state, &soloed).await;
        }
        Err(not) => {
            undo(&store, &planned);
            if !was {
                state.thaw_unchanged();
            }
            return Err(not);
        }
    }
    Ok(Transition {
        moved,
        frozen: Some(was),
    })
}

/// The import of `state` as the engine's whole running state.
fn import(state: MixState) -> Cmd {
    Cmd::ImportState {
        state,
        baseline: false,
    }
}

/// A new roster (`text`) in place of `old` (see [`apply_text`]): the states
/// go over from what they follow (`old`, or an unsettled base), and `text`
/// is recorded as what they follow now. One that keeps none of `old`'s
/// people is refused first (#58 review).
async fn reload(
    state: &AppState,
    old: &RosterState,
    new: Roster,
    text: &str,
    sha256: &str,
) -> Applied {
    // Before anything is read or written (#58 review).
    if removes_everyone(&old.roster, &new) {
        return Applied::Refused(vec![EVERYONE_REMOVED.to_string()]);
    }
    let active = old.lineup().id.clone();
    // Kept before any state is marked with it.
    keep(&state.config_dir, sha256, text);
    let unsettled = state.unsettled();
    let old_base = Base {
        roster: Arc::clone(&old.roster),
        sha256: old.sha256.clone(),
    };
    let base = unsettled.as_ref().and_then(|u| u.base.clone());
    let follows = Follows {
        known: std::iter::once(old_base.clone())
            .chain(base.clone())
            .collect(),
        unmarked: if unsettled.is_some() {
            base
        } else {
            Some(old_base)
        },
    };
    let done = match transition(state, follows, &new, sha256, &active).await {
        Ok(done) => done,
        Err(not) => {
            count_unrecorded(state, &not).await;
            return not;
        }
    };
    state.unrecorded.store(0, Ordering::SeqCst);
    let (added, removed) = people_diff(&old.roster, &new);
    let roster = Arc::new(new);
    state.set_roster(Some(RosterState {
        roster: Arc::clone(&roster),
        sha256: sha256.to_string(),
        active,
    }));
    state.set_unsettled(None);
    record(&state.config_dir, text);
    // Free again only now (a page of the roster before must not change a
    // mix that moved), and every page hears of the new roster: the thaw
    // announces it when this froze them.
    if done.frozen == Some(false) {
        state.thaw();
    } else {
        state.announce_lineup();
    }
    let people = {
        let mut pins = state.pin_store.write().await;
        follow_people(&state.config_dir, &roster, &mut pins, &removed)
    };
    tracing::info!(
        added = added.len(),
        removed = removed.len(),
        moved = done.moved,
        deleted = people.removed.len(),
        problems = people.problems.len(),
        "roster reloaded"
    );
    // Whom the roster took out, then whom the look through the people's
    // data removed besides (#58 review).
    let mut removed = removed;
    for id in people.removed {
        if !removed.contains(&id) {
            removed.push(id);
        }
    }
    Applied::Applied {
        added,
        removed,
        problems: people.problems,
    }
}

/// Puts the roster in use into the states when it is unsettled (see the
/// module docs): [`transition`] from what they follow, under the switching
/// lock; then the pages are free and the roster is recorded. `Deferred`
/// while the engine is not synced or does not answer.
pub async fn settle(state: &AppState) -> Result<(), Applied> {
    let _running = state.switching.lock().await;
    let Some(unsettled) = state.unsettled() else {
        return Ok(());
    };
    let Some(r) = state.roster() else {
        state.set_unsettled(None);
        return Ok(());
    };
    let active = r.lineup().id.clone();
    let follows = Follows {
        known: unsettled.base.iter().cloned().collect(),
        unmarked: unsettled.base,
    };
    let done = transition(state, follows, &r.roster, &r.sha256, &active).await?;
    state.set_unsettled(None);
    record_in_use(&state.config_dir, &r.sha256);
    tracing::info!(lineup = %active, moved = done.moved, "the roster reached the states");
    if done.frozen == Some(false) {
        state.thaw();
    }
    Ok(())
}

/// Records the roster file in use as the one the states follow, read again
/// (the start keeps no text): one changed since is the watcher's to apply
/// and record.
fn record_in_use(config_dir: &Path, sha256: &str) {
    match read_roster(config_dir) {
        Ok(Some(bytes)) if sha256_hex(&bytes) == sha256 => match std::str::from_utf8(&bytes) {
            Ok(text) => record(config_dir, text),
            Err(_) => tracing::error!("the roster file in use is no UTF-8 text"),
        },
        Ok(_) => {
            tracing::info!("the roster in use is no longer roster.toml: the record stays as it is")
        }
        Err(e) => tracing::error!(error = %e, "the roster file is unreadable"),
    }
}

/// At the start, after the replay of a switch a restart cut short, and
/// after a legacy server adopted its roster: [`settle`] once the engine has
/// synced (woken by its state arriving), tried again `AppState::retry` after a try that failed.
/// After [`MAX_SETTLES`] refused tries (the engine refuses the import, a
/// stored state cannot be read or written) the pages are freed without it
/// and the engineer's devices hear of it (once); what the states follow is
/// kept, so the next roster change moves people and brings the rules in.
pub async fn settle_when_synced(state: AppState) {
    let mut refusals = 0;
    loop {
        if !state.settling() {
            return;
        }
        // Woken by the engine's state arriving, not by the next retry.
        if !state.engine.wait_synced(state.retry).await {
            continue;
        }
        let Err(not) = settle(&state).await else {
            return;
        };
        if waits(&not) {
            tracing::info!(?not, "the roster waits for the engine");
        } else {
            refusals += 1;
            tracing::error!(?not, refusals, "the roster did not reach the states");
            if refusals == MAX_SETTLES {
                give_up(&state).await;
                let devices = crate::notify::push_alarm(
                    &state,
                    "Zostava neplatí v mixoch",
                    "Server nevedel preniesť zmenu zostavy do mixov: presunutí ľudia ešte \
                     nemajú svoje mixy na nových setoch a pravidlá zostavy (napr. stíšené \
                     prázdne sety) neplatia. Mixy sa dajú meniť; doplní sa to pri ďalšej \
                     zmene zostavy.",
                )
                .await;
                tracing::warn!(
                    devices,
                    "the engineer's devices heard the roster did not reach the states"
                );
                return;
            }
        }
        tokio::time::sleep(state.retry).await;
    }
}

/// A settle that gave up: the pages are free, what the states follow stays
/// for the next roster change (under the switching lock: a reload that
/// settled them meanwhile is not undone).
async fn give_up(state: &AppState) {
    let _running = state.switching.lock().await;
    if let Some(mut unsettled) = state.unsettled() {
        unsettled.refusing = false;
        state.set_unsettled(Some(unsettled));
    }
}

/// Whether a roster that did not reach the states waits for the engine
/// (gone, silent, not synced: tried again) rather than being refused. A
/// pending record that could not be written ([`unrecorded`]) counts as
/// refused (#58 fifth review): waiting does not make the file writable.
fn waits(not: &Applied) -> bool {
    matches!(not, Applied::Deferred(_)) && !unrecorded(not)
}

/// Whether `not` is a transition that imported nothing because its pending
/// record could not be written ([`NOT_RECORDED`]).
fn unrecorded(not: &Applied) -> bool {
    matches!(not, Applied::Deferred(why) if why.iter().any(|w| w == NOT_RECORDED))
}

/// Counts a reload that imported nothing because its pending record could
/// not be written; at [`MAX_SETTLES`] in a row the engineer's devices hear
/// of it, once (#58 fifth review): the watcher tries it again every poll.
async fn count_unrecorded(state: &AppState, not: &Applied) {
    if !unrecorded(not) {
        return;
    }
    let tries = state.unrecorded.fetch_add(1, Ordering::SeqCst) + 1;
    if tries == MAX_SETTLES {
        let devices = crate::notify::push_alarm(
            state,
            "Zmena zostavy čaká",
            "Server nevie zapísať súbor lineups/running.json, preto zmenu zostavy \
             nepreniesol do mixov a skúša to znova. Skontroluj miesto na disku a \
             práva k priečinku lineups.",
        )
        .await;
        tracing::warn!(
            devices,
            tries,
            "the engineer's devices heard a reload cannot record its import"
        );
    }
}

/// After a start on the roster applied last (`AppState::fallback`), the
/// engineer's devices hear of it once (#58 re-review): the reason is in
/// `roster.status.json`. How many devices took it (none without a
/// fallback).
pub async fn fallback_alarm(state: &AppState) -> usize {
    if !state.fallback.load(Ordering::SeqCst) {
        return 0;
    }
    let devices = crate::notify::push_alarm(
        state,
        "Zostava zo zálohy",
        "Server sa spustil so zostavou použitou naposledy: roster.toml sa nedá použiť \
         (dôvod je v roster.status.json).",
    )
    .await;
    tracing::warn!(
        devices,
        "the engineer's devices heard of the start on the roster applied last"
    );
    devices
}

/// The removals a fall-back start held back ([`Start::Fallback`]), run
/// once `roster.toml` is the roster in use again ([`follow_people`] on it,
/// under the switching lock as a reload's).
async fn held_back(state: &AppState) -> People {
    let _running = state.switching.lock().await;
    let Some(r) = state.roster() else {
        return People::default();
    };
    let mut pins = state.pin_store.write().await;
    let people = follow_people(&state.config_dir, &r.roster, &mut pins, &[]);
    tracing::info!(
        deleted = people.removed.len(),
        problems = people.problems.len(),
        "roster.toml is in use again: the removals the fall-back held back ran"
    );
    people
}

/// Reads `roster.toml` once and applies it ([`apply_text`]), writing the
/// status file unless nothing changed. `None` when the file cannot be read
/// (gone: the roster in use stays until a restart).
pub async fn check_file(state: &AppState) -> Option<Applied> {
    let bytes = match read_roster(&state.config_dir) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            tracing::warn!(
                "roster.toml is gone: the roster in use stays (a restart starts with the roster applied last)"
            );
            return None;
        }
        Err(e) => {
            tracing::error!(error = %e, "the roster file is unreadable");
            return None;
        }
    };
    let sha256 = sha256_hex(&bytes);
    let applied = match std::str::from_utf8(&bytes) {
        Ok(text) => apply_text(state, text, &sha256).await,
        Err(_) => Applied::Refused(vec![NOT_UTF8.to_string()]),
    };
    // After a start on the roster applied last, the status says so until a
    // roster file is in use again.
    let fallback = state.fallback.load(Ordering::SeqCst);
    match &applied {
        Applied::Unchanged if fallback => {
            // roster.toml is the roster in use again: the removals the
            // fall-back start held back run now (#58 review).
            state.fallback.store(false, Ordering::SeqCst);
            let people = held_back(state).await;
            note(
                &state.config_dir,
                &Status::applied(&sha256, Vec::new(), people.removed, people.problems),
            );
        }
        Applied::Unchanged => {}
        Applied::Applied {
            added,
            removed,
            problems,
        } => {
            state.fallback.store(false, Ordering::SeqCst);
            note(
                &state.config_dir,
                &Status::applied(&sha256, added.clone(), removed.clone(), problems.clone()),
            );
        }
        Applied::Refused(problems) => {
            tracing::warn!(
                problems = problems.len(),
                "a roster was refused: the one in use stays"
            );
            let mut why = problems.clone();
            if fallback {
                why.push(FALLBACK.to_string());
            }
            note(&state.config_dir, &Status::refused(&sha256, why));
        }
        Applied::Deferred(why) => {
            tracing::info!(
                reasons = why.len(),
                "a roster waits for the engine: tried again at the next poll"
            );
        }
    }
    Some(applied)
}

/// What [`watch`] compares: the file's length and modification time
/// (`None`: no file).
type Stamp = Option<(u64, Option<SystemTime>)>;

fn stamp(path: &Path) -> Stamp {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()))
}

/// Polls `roster.toml` every [`POLL`] for the server's life; a changed
/// length or modification time reads it ([`check_file`]), and so does the
/// first poll (a file changed since the start's read; the one in use reads
/// as unchanged) and every poll after a roster [`Applied::Deferred`].
pub async fn watch(state: AppState) {
    let path = state.config_dir.join(ROSTER_FILE);
    let mut seen: Stamp = None;
    loop {
        tokio::time::sleep(POLL).await;
        seen = poll(&state, &path, seen).await;
    }
}

/// One poll of [`watch`]: the file at `path` is read ([`check_file`]) when
/// its stamp is not `seen`. Returns what the next poll compares with: the
/// file's stamp, or `None` after a roster [`Applied::Deferred`] so the next
/// poll reads it again.
async fn poll(state: &AppState, path: &Path, seen: Stamp) -> Stamp {
    let now = stamp(path);
    if now == seen {
        return seen;
    }
    match check_file(state).await {
        Some(Applied::Deferred(_)) => None,
        _ => now,
    }
}

/// `iem-server roster check <file>`: the roster file's problems on its own
/// and against the site file's topology (`[engine]`), with the migration's
/// own while the site's directory is not migrated yet, and against the
/// site's lineup files the start reads (the lineup that plays, the target
/// of a switch a restart cut short, the roster applied last: whom it
/// removes, and a refusal when that is everyone).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckReport {
    pub ok: bool,
    pub problems: Vec<String>,
    pub sha256: String,
    /// The people of the roster applied last (`lineups/applied-roster.toml`)
    /// that this roster removes, ids in that roster's order; `None` when no
    /// roster was applied before or it cannot be read (#58 review).
    pub removes: Option<Vec<String>>,
}

/// [`CheckReport`] of the roster at `roster_path` for the site at
/// `site_path`; it only reads.
pub fn check_cli(roster_path: &Path, site_path: &Path) -> CheckReport {
    let ((problems, removes), sha256) = match std::fs::read(roster_path) {
        Ok(bytes) => (check_problems(&bytes, site_path), sha256_hex(&bytes)),
        Err(e) => (
            (vec![format!("roster file: {}", e.kind())], None),
            String::new(),
        ),
    };
    CheckReport {
        ok: problems.is_empty(),
        problems,
        sha256,
        removes,
    }
}

/// The check's problems, and whom the roster removes against the roster
/// applied last ([`CheckReport::removes`]).
fn check_problems(bytes: &[u8], site_path: &Path) -> (Vec<String>, Option<Vec<String>>) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return (vec![NOT_UTF8.to_string()], None);
    };
    let site = match std::fs::read_to_string(site_path) {
        Ok(site) => site,
        Err(e) => return (vec![format!("site file: {}", e.kind())], None),
    };
    let facts = match site_facts(&site) {
        Ok(facts) => facts,
        Err(e) => return (vec![e.to_string()], None),
    };
    let roster = match Roster::parse(text) {
        Ok(roster) => roster,
        Err(e) => return (vec![e.to_string()], None),
    };
    let config_dir = crate::provision::config_dir_of(site_path);
    let migrated = config_dir.join(ACTIVE_FILE).exists();
    let mut problems = if migrated {
        roster.problems()
    } else {
        migration_problems(&roster)
    };
    problems.extend(roster.problems_against(&facts));
    // What the start depends on (#58 review): the lineup that plays, and the
    // one a switch a restart cut short goes to.
    let store = LineupStore::new(&config_dir);
    match store.active() {
        Ok(Some(active)) if roster.lineup(&active).is_none() => {
            problems.push(missing_active(&active));
        }
        Ok(_) => {}
        Err(e) => problems.push(format!("{ACTIVE_FILE}: {}", e.kind())),
    }
    match store.intent() {
        Ok(Some(intent)) if roster.lineup(&intent.to).is_none() => problems.push(format!(
            "the roster has no lineup '{}', which a switch a restart cut short goes to",
            intent.to
        )),
        Ok(_) => {}
        Err(e) => problems.push(format!("{SWITCH_FILE}: {}", e.kind())),
    }
    // Whom it removes against the roster the states follow, as the start
    // and a reload refuse one that removes everyone (#58 review); none
    // known before a roster was applied or when that one cannot be read.
    let base = store
        .applied_roster()
        .ok()
        .flatten()
        .and_then(|t| Roster::parse(&t).ok());
    let removes = base.map(|base| {
        if removes_everyone(&base, &roster) {
            problems.push(EVERYONE_REMOVED.to_string());
        }
        people_diff(&base, &roster).1
    });
    (problems, removes)
}

#[cfg(test)]
mod tests;
