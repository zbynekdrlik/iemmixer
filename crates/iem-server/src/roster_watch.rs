//! Loading, checking and reloading the roster (#58 design §9): the private
//! `roster.toml` beside the site file. Without it the server runs in legacy
//! mode. At the start a roster must parse and have no problems, or the
//! server does not start; the legacy layout is migrated first (or a
//! committed migration finished, at every start). While the server runs,
//! [`watch`] polls the file: a change that parses, has no problems, fits the
//! engine's topology and keeps the active lineup is applied without a
//! restart (a running legacy server adopts its first roster the same way);
//! any other is refused and the roster in use stays. `roster.status.json`
//! says what became of each file, in ids and counts only (P6).
//!
//! Applying a roster: a person who moves to another set of a lineup takes
//! their mix along ([`moves`], [`apply_moves`]), and the lineup's rules hold
//! on every set ([`fit`]: a set worn by no leader hears no other mix, a set
//! nobody wears is silent), in every inactive lineup's stored state and in
//! the active lineup's running one through one `ImportState`, before the new
//! roster is swapped in; then people it removed are archived and people it
//! brings back restored ([`follow_people`]).
//!
//! A roster that comes into use without a synced engine (the start, a
//! legacy server's adoption) is `AppState::unsettled` until [`settle`] has
//! put it into the states, under the switching lock, once the engine has
//! synced: no page changes a mix and no switch or restore runs before. The
//! roster the states follow is recorded (`LineupStore::applied_roster`), so
//! the start moves the mixes of people a roster changed while the server
//! was down.

use std::collections::BTreeSet;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

use iem_core::backup::MixerBackup;
use iem_core::config::Config;
use iem_core::roster::{Lineup, ROSTER_FILE, Roster, TopologyFacts, site_facts};
use iem_engine_proto::{Cmd, MixId, MixState};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::engine::client::EngineError;
use crate::lineup_store::{
    ACTIVE_FILE, LineupStore, SWITCH_FILE, migrate_legacy, migration_problems,
};
use crate::lineup_switch::REPLAY_RETRY;
use crate::people_archive::{ARCHIVE_DIR, archive_person, restore_person};
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

/// The roster in use has not reached the states yet ([`settle`]): they
/// follow `base` (`None`: not known, a legacy layout or a switch a restart
/// cut short, so nobody's mix moves).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Unsettled {
    pub base: Option<Arc<Roster>>,
}

/// What the states follow at the start: the roster recorded last (`None`
/// when there is none or it cannot be read, each logged).
pub fn applied_base(config_dir: &Path) -> Option<Roster> {
    match LineupStore::new(config_dir).applied_roster() {
        Ok(Some(text)) => match Roster::parse(&text) {
            Ok(roster) => Some(roster),
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
    /// People it brought (back) into use.
    #[serde(default)]
    pub added: Vec<String>,
    /// People it took out of use (archived).
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
    /// failed after it went into use (an archive move).
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

/// What [`follow_people`] did, in ids.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct People {
    pub restored: Vec<String>,
    pub archived: Vec<String>,
    /// A move that failed (the person and the error's kind).
    pub problems: Vec<String>,
}

/// The people archive follows `roster` (#58 decision 6): everyone in
/// `removed`, and anyone else with a live PIN, who is no person of it (nor
/// the engineer) is archived; every person of it with something in the
/// archive gets it back. A failure is named in `problems`; the rest still
/// runs.
pub fn follow_people(
    config_dir: &Path,
    roster: &Roster,
    pins: &mut PinStore,
    removed: &[String],
) -> People {
    let mut out = People::default();
    let mut gone: Vec<String> = removed.to_vec();
    for id in pins.member_ids() {
        if !gone.contains(&id) {
            gone.push(id);
        }
    }
    for id in &gone {
        if id == ENGINEER_ID || roster.person(id).is_some() {
            continue;
        }
        match archive_person(config_dir, pins, id) {
            Ok(true) => out.archived.push(id.clone()),
            Ok(false) => {}
            Err(e) => out
                .problems
                .push(format!("person '{id}' was not archived: {}", e.kind())),
        }
    }
    for p in &roster.people {
        let archived = pins.archived_hash(&p.id).is_some()
            || config_dir.join(ARCHIVE_DIR).join(&p.id).exists();
        if !archived {
            continue;
        }
        match restore_person(config_dir, pins, &p.id) {
            Ok(true) => out.restored.push(p.id.clone()),
            Ok(false) => {}
            Err(e) => {
                out.problems
                    .push(format!("person '{}' was not restored: {}", p.id, e.kind()))
            }
        }
    }
    out
}

/// Puts `roster` into use on the files' side: the legacy layout migrated
/// (or a committed migration finished), the recorded active lineup (main
/// before any) checked to be the roster's, the people archive following
/// it. The roster state and what the migration and the archive did.
fn adopt(
    config_dir: &Path,
    cfg: &Config,
    pins: &mut PinStore,
    roster: Roster,
    sha256: &str,
) -> Result<(RosterState, People), Vec<String>> {
    let report = migrate_legacy(config_dir, &roster, cfg, pins).map_err(|e| vec![e.to_string()])?;
    let active = LineupStore::new(config_dir)
        .active()
        .map_err(|e| vec![e.to_string()])?
        .unwrap_or_else(|| roster.main().id.clone());
    if roster.lineup(&active).is_none() {
        return Err(vec![missing_active(&active)]);
    }
    let mut people = follow_people(config_dir, &roster, pins, &[]);
    let mut archived = report.archived;
    archived.append(&mut people.archived);
    people.archived = archived;
    let state = RosterState {
        roster: Arc::new(roster),
        sha256: sha256.to_string(),
        active,
    };
    Ok((state, people))
}

/// The problem of a start without `roster.toml` beside a recorded roster.
const NOT_FOUND: &str = "roster file: not found";
/// The last problem of a start that fell back to the recorded roster.
const FALLBACK: &str = "in use instead: the roster applied last";

/// The roster in `bytes` (SHA-256 `sha256`) validated and adopted
/// ([`adopt`]).
fn take(
    config_dir: &Path,
    cfg: &Config,
    pins: &mut PinStore,
    bytes: &[u8],
    sha256: &str,
    facts: Option<&TopologyFacts>,
) -> Result<(RosterState, People), Vec<String>> {
    std::str::from_utf8(bytes)
        .map_err(|_| vec![NOT_UTF8.to_string()])
        .and_then(|text| validate(text, facts))
        .and_then(|roster| adopt(config_dir, cfg, pins, roster, sha256))
}

/// The roster at the server's start: `None` without `roster.toml` (legacy
/// mode). Otherwise it must parse and have no problems (against `facts`
/// too, when given); the legacy layout is migrated first, or a committed
/// migration finished; its active lineup is the recorded one and must be
/// the roster's; the people archive follows it. A `roster.toml` that cannot
/// be used, or none, beside a recorded roster (`LineupStore::applied_roster`,
/// the one the states follow) starts with that one (#58 review: a server
/// that does not start is the worst outcome). The status file says what
/// happened; an error refuses the start.
pub fn load(
    config_dir: &Path,
    cfg: &Config,
    pins: &mut PinStore,
    facts: Option<&TopologyFacts>,
) -> Result<Option<RosterState>, String> {
    let file = read_roster(config_dir)?;
    let (sha256, problems) = match &file {
        Some(bytes) => {
            let sha256 = sha256_hex(bytes);
            match take(config_dir, cfg, pins, bytes, &sha256, facts) {
                Ok((state, people)) => {
                    tracing::info!(
                        lineup = %state.active,
                        people = state.roster.people.len(),
                        restored = people.restored.len(),
                        archived = people.archived.len(),
                        "roster mode"
                    );
                    note(
                        config_dir,
                        &Status::applied(
                            &sha256,
                            people.restored,
                            people.archived,
                            people.problems,
                        ),
                    );
                    return Ok(Some(state));
                }
                Err(problems) => (sha256, problems),
            }
        }
        None => (String::new(), vec![NOT_FOUND.to_string()]),
    };
    let recorded = match LineupStore::new(config_dir).applied_roster() {
        Ok(recorded) => recorded,
        Err(e) => {
            tracing::error!(error = %e, "the roster applied last is unreadable");
            None
        }
    };
    if let Some(text) = recorded {
        let copy = sha256_hex(text.as_bytes());
        match take(config_dir, cfg, pins, text.as_bytes(), &copy, facts) {
            Ok((state, _)) => {
                tracing::error!(
                    lineup = %state.active,
                    problems = problems.len(),
                    "roster.toml cannot be used: the roster applied last is in use"
                );
                let mut why = problems;
                why.push(FALLBACK.to_string());
                note(config_dir, &Status::refused(&sha256, why));
                return Ok(Some(state));
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
/// touched.
pub fn fit(state: &mut MixState, roster: &Roster, lineup: &Lineup) {
    fit_sets(state, &roster_sets(roster), lineup);
}

/// [`fit`] over `sets`: a reload passes the old roster's sets too, so a set
/// the new roster no longer lists at all is silent as well.
fn fit_sets(state: &mut MixState, sets: &BTreeSet<String>, lineup: &Lineup) {
    for set in sets {
        let Some(mix) = state.mixes.get_mut(&MixId::new(set.as_str())) else {
            continue;
        };
        match lineup.person_on(set) {
            None => mix.out.muted = true,
            Some(person) if !lineup.leads(person) => {
                for level in mix.mixes.values_mut() {
                    level.muted = true;
                }
            }
            Some(_) => {}
        }
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
        None => adopt_live(state, roster, sha256).await,
        Some(old) => reload(state, &old, roster, text, sha256).await,
    }
}

/// A running legacy server takes its first roster; its rules reach the
/// states once the engine has synced ([`settle_when_synced`], spawned).
async fn adopt_live(state: &AppState, roster: Roster, sha256: &str) -> Applied {
    let adopted = {
        let mut pins = state.pin_store.write().await;
        adopt(
            &state.config_dir,
            &state.site_config,
            &mut pins,
            roster,
            sha256,
        )
    };
    match adopted {
        Ok((r, people)) => {
            tracing::warn!(lineup = %r.active, "roster mode: a roster is in use from now on");
            // Before the roster: no page changes a mix until it is settled.
            state.set_unsettled(Some(Unsettled::default()));
            state.set_roster(Some(r));
            state.announce_lineup();
            tokio::spawn(settle_when_synced(state.clone()));
            Applied::Applied {
                added: people.restored,
                removed: people.archived,
                problems: people.problems,
            }
        }
        Err(problems) => Applied::Refused(problems),
    }
}

/// Writes back stored states a refused reload had changed (best effort).
fn undo(store: &LineupStore, written: &[(String, MixerBackup)]) {
    for (id, original) in written {
        if let Err(e) = store.save_state(id, original) {
            tracing::error!(lineup = %id, error = %e, "a lineup's stored state was not put back");
        }
    }
}

/// Why a roster waits: its rules reach the running state only through a
/// synced engine.
const NOT_SYNCED: &str = "the engine state is not synced: the roster waits for it";

/// What [`transition`] did.
struct Transition {
    /// People who moved to another set of the active lineup.
    moved: usize,
    /// `Some(was)`: the pages are frozen; the caller puts `frozen` back to
    /// `was` once the roster in use is the one the states follow.
    frozen: Option<bool>,
}

/// The states go from following `base` to following `new`, whose lineup
/// `active` plays: every other lineup's stored state, then the running
/// state through one import. A person who wears another set of a lineup
/// in `new` than in `base` takes their mix along ([`moves`]; no `base`:
/// nobody moves), and `new`'s rules hold on every set `base` or `new` lists
/// ([`fit_sets`]: a set only `base` lists is worn by nobody now). Waits
/// (`Deferred`) for a synced engine before it touches anything; the running
/// state is read again once the pages are frozen ([`AppState::freeze`],
/// only when something changes), and they stay frozen for the caller. On a
/// failure every stored state it wrote is put back and the pages are free
/// as before.
async fn transition(
    state: &AppState,
    base: Option<&Roster>,
    new: &Roster,
    active: &str,
) -> Result<Transition, Applied> {
    let Some(lineup) = new.lineup(active).cloned() else {
        return Err(Applied::Refused(vec![missing_active(active)]));
    };
    if !state.engine.mirror().synced {
        return Err(Applied::Deferred(vec![NOT_SYNCED.to_string()]));
    }
    let store = LineupStore::new(&state.config_dir);
    let sets: BTreeSet<String> = base
        .map(roster_sets)
        .unwrap_or_default()
        .into_iter()
        .chain(roster_sets(new))
        .collect();
    let shift_of = |id: &str| base.map(|b| moves(b, new, id)).unwrap_or_default();
    // The other lineups' stored states first, each original kept so a
    // failure below can put it back.
    let mut written: Vec<(String, MixerBackup)> = Vec::new();
    for l in new.lineups.iter().filter(|l| l.id != active) {
        let stored = match store.state(&l.id) {
            Ok(Some(b)) => b,
            Ok(None) => continue,
            Err(e) => {
                undo(&store, &written);
                return Err(Applied::Refused(vec![format!(
                    "the stored state of lineup {} is unreadable: {}",
                    l.id,
                    e.kind()
                )]));
            }
        };
        let mut next = stored.clone();
        apply_moves(&mut next.state, &shift_of(&l.id));
        fit_sets(&mut next.state, &sets, l);
        if next.state == stored.state {
            continue;
        }
        if let Err(e) = store.save_state(&l.id, &next) {
            undo(&store, &written);
            return Err(Applied::Refused(vec![format!(
                "the stored state of lineup {} was not written: {}",
                l.id,
                e.kind()
            )]));
        }
        written.push((l.id.clone(), stored));
    }
    // The active lineup's running state, through one import.
    let shift = shift_of(active);
    let next_of = |current: &MixState| {
        let mut next = current.clone();
        apply_moves(&mut next, &shift);
        fit_sets(&mut next, &sets, &lineup);
        next
    };
    let current = state.engine.mirror().state.clone();
    if next_of(&current) == current {
        return Ok(Transition {
            moved: shift.len(),
            frozen: None,
        });
    }
    let failed = |e: EngineError| {
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
        Err(e) => {
            undo(&store, &written);
            return Err(failed(e));
        }
    };
    let (synced, current) = {
        let m = state.engine.mirror();
        (m.synced, m.state.clone())
    };
    let imported = if synced {
        let next = next_of(&current);
        if next == current {
            Ok(())
        } else {
            state
                .engine
                .request_applied(
                    Cmd::ImportState {
                        state: next,
                        baseline: false,
                    },
                    None,
                )
                .await
                .map_err(failed)
        }
    } else {
        Err(Applied::Deferred(vec![NOT_SYNCED.to_string()]))
    };
    if let Err(not) = imported {
        state.frozen.store(was, Ordering::SeqCst);
        undo(&store, &written);
        return Err(not);
    }
    Ok(Transition {
        moved: shift.len(),
        frozen: Some(was),
    })
}

/// A new roster (`text`) in place of `old` (see [`apply_text`]): the states
/// go over from what they follow (`old`, or an unsettled base), and `text`
/// is recorded as what they follow now.
async fn reload(
    state: &AppState,
    old: &RosterState,
    new: Roster,
    text: &str,
    sha256: &str,
) -> Applied {
    let active = old.lineup().id.clone();
    let unsettled = state.unsettled();
    let base = match &unsettled {
        Some(u) => u.base.as_deref(),
        None => Some(&*old.roster),
    };
    let done = match transition(state, base, &new, &active).await {
        Ok(done) => done,
        Err(not) => return not,
    };
    let (added, removed) = people_diff(&old.roster, &new);
    let roster = Arc::new(new);
    state.set_roster(Some(RosterState {
        roster: Arc::clone(&roster),
        sha256: sha256.to_string(),
        active,
    }));
    state.set_unsettled(None);
    // Free again only now: a page of the roster before must not change a
    // mix that moved.
    if let Some(was) = done.frozen {
        state.frozen.store(was, Ordering::SeqCst);
    }
    record(&state.config_dir, text);
    state.announce_lineup();
    let people = {
        let mut pins = state.pin_store.write().await;
        follow_people(&state.config_dir, &roster, &mut pins, &removed)
    };
    tracing::info!(
        added = added.len(),
        removed = removed.len(),
        moved = done.moved,
        restored = people.restored.len(),
        archived = people.archived.len(),
        "roster reloaded"
    );
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
    let done = transition(state, unsettled.base.as_deref(), &r.roster, &active).await?;
    state.set_unsettled(None);
    if let Some(was) = done.frozen {
        state.frozen.store(was, Ordering::SeqCst);
    }
    record_in_use(&state.config_dir, &r.sha256);
    tracing::info!(lineup = %active, moved = done.moved, "the roster reached the states");
    if done.frozen.is_some() {
        state.announce_lineup();
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
/// synced, tried again every [`REPLAY_RETRY`] while the engine is not there.
/// After [`MAX_SETTLES`] refused tries (the engine refuses the import, a
/// stored state cannot be read or written) the pages are freed without it
/// and the engineer's devices hear of it (once): the next roster change
/// brings the rules in again.
pub async fn settle_when_synced(state: AppState) {
    let mut refusals = 0;
    loop {
        if state.unsettled().is_none() {
            return;
        }
        let synced = state.engine.mirror().synced;
        if synced {
            match settle(&state).await {
                Ok(()) => return,
                Err(Applied::Deferred(why)) => {
                    tracing::info!(reasons = why.len(), "the roster waits for the engine");
                }
                Err(not) => {
                    refusals += 1;
                    tracing::error!(?not, refusals, "the roster did not reach the states");
                    if refusals == MAX_SETTLES {
                        state.set_unsettled(None);
                        let devices = crate::notify::push_alarm(
                            &state,
                            "Zostava neplatí v mixoch",
                            "Server po zmene zostavy nevedel upraviť mixy (napr. stíšiť prázdne \
                             sety): mixy zostávajú, ako boli.",
                        )
                        .await;
                        tracing::warn!(
                            devices,
                            "the engineer's devices heard the roster did not reach the states"
                        );
                        return;
                    }
                }
            }
        }
        tokio::time::sleep(REPLAY_RETRY).await;
    }
}

/// Reads `roster.toml` once and applies it ([`apply_text`]), writing the
/// status file unless nothing changed. `None` when the file cannot be read
/// (gone: the roster in use stays until a restart).
pub async fn check_file(state: &AppState) -> Option<Applied> {
    let bytes = match read_roster(&state.config_dir) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            tracing::warn!(
                "roster.toml is gone: the roster in use stays until the server restarts"
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
    match &applied {
        Applied::Unchanged => {}
        Applied::Applied {
            added,
            removed,
            problems,
        } => note(
            &state.config_dir,
            &Status::applied(&sha256, added.clone(), removed.clone(), problems.clone()),
        ),
        Applied::Refused(problems) => {
            tracing::warn!(
                problems = problems.len(),
                "a roster was refused: the one in use stays"
            );
            note(
                &state.config_dir,
                &Status::refused(&sha256, problems.clone()),
            );
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
        let now = stamp(&path);
        if now != seen {
            seen = now;
            if let Some(Applied::Deferred(_)) = check_file(&state).await {
                seen = None;
            }
        }
    }
}

/// `iem-server roster check <file>`: the roster file's problems on its own
/// and against the site file's topology (`[engine]`), with the migration's
/// own while the site's directory is not migrated yet, and against the
/// site's lineup files the start reads (the lineup that plays, the target
/// of a switch a restart cut short).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckReport {
    pub ok: bool,
    pub problems: Vec<String>,
    pub sha256: String,
}

/// [`CheckReport`] of the roster at `roster_path` for the site at
/// `site_path`; it only reads.
pub fn check_cli(roster_path: &Path, site_path: &Path) -> CheckReport {
    let (problems, sha256) = match std::fs::read(roster_path) {
        Ok(bytes) => (check_problems(&bytes, site_path), sha256_hex(&bytes)),
        Err(e) => (vec![format!("roster file: {}", e.kind())], String::new()),
    };
    CheckReport {
        ok: problems.is_empty(),
        problems,
        sha256,
    }
}

fn check_problems(bytes: &[u8], site_path: &Path) -> Vec<String> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return vec![NOT_UTF8.to_string()];
    };
    let site = match std::fs::read_to_string(site_path) {
        Ok(site) => site,
        Err(e) => return vec![format!("site file: {}", e.kind())],
    };
    let facts = match site_facts(&site) {
        Ok(facts) => facts,
        Err(e) => return vec![e.to_string()],
    };
    let roster = match Roster::parse(text) {
        Ok(roster) => roster,
        Err(e) => return vec![e.to_string()],
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
    problems
}

#[cfg(test)]
mod tests;
