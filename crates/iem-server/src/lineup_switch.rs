//! Switching the active lineup (#58 design §7): a leader of any lineup or
//! the engineer makes another band play, its lineup and every mix at once,
//! in one `ImportState`, without a restart.
//!
//! The order of the steps keeps the engine's state and the recorded active
//! lineup from ever belonging to two bands: under the `switching` lock and
//! with every page's change refused (`frozen`), the live state is stored as
//! the active lineup's, then the intent `{from, to}` is written, then the
//! target's state goes to the engine, then the target is recorded active,
//! then the intent goes. A failure after the intent imports the stored
//! state again and, once it is back in, clears the intent: the active
//! lineup is unchanged. When it does not go back in either, which band's
//! state the engine holds is not known: the intent stays, the pages stay
//! frozen and the switch finishes once the engine answers
//! ([`replay_when_synced`]), as after a restart between the intent and its
//! clearing ([`replay_intent`], once the mirror has synced). After a switch every
//! person's mixer socket closes (`mixer_ws::CLOSE_LINEUP`), and the
//! engineer's devices hear which band plays.
//!
//! Pins and hides live in each lineup's own files
//! (`lineups/<id>/customizations/`): a switch reads and writes nothing of
//! them but the copy inside the stored state (a backup's shape), which is
//! never read back at a switch.

use std::collections::BTreeSet;
use std::time::Duration;

use iem_core::AuthClaims;
use iem_core::backup::MixerBackup;
use iem_core::roster::{Lineup, Roster};
use iem_engine_proto::{Cmd, MixState};

use crate::AppState;
use crate::backup::{capture, keep_running};
use crate::engine::client::EngineError;
use crate::lineup_store::{LineupStore, Running, SwitchIntent};
use crate::roster_watch::{
    end_solos, fit_recorded, note_running, running_for, running_of, soloed_changing,
};
use crate::site_view::SiteView;

/// How long [`replay_when_synced`] waits between two tries.
pub const REPLAY_RETRY: Duration = Duration::from_secs(2);

/// Why a switch did not happen; the active lineup is unchanged.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SwitchError {
    #[error("this site has no roster")]
    NotConfigured,
    #[error("only a band leader or the engineer switches the band")]
    Forbidden,
    #[error("the band is being switched already")]
    Busy,
    /// The id asked for (never echoed: it is the request's).
    #[error("no such lineup")]
    Unknown(String),
    #[error("that band plays already")]
    Same,
    #[error("the engine state is not synced")]
    NotSynced,
    #[error("{0}")]
    Io(String),
    #[error("the engine did not take the band's mixes: {0}")]
    Engine(EngineError),
    /// The engine dropped the switch and the band before could not go back
    /// in either: it finishes once the engine answers again
    /// ([`replay_when_synced`]); why, as text.
    #[error("the band switch finishes once the engine answers again: {0}")]
    Pending(String),
}

fn io(what: &str, e: std::io::Error) -> SwitchError {
    SwitchError::Io(format!("{what}: {}", e.kind()))
}

/// Whether `by` may switch the band: the engineer, or a leader of any
/// lineup (§7).
pub fn may_switch(roster: &Roster, by: &AuthClaims) -> bool {
    by.engineer || roster.leads_any(&by.sub)
}

/// The state the engine takes when `lineup` starts to play: its stored
/// state completed with the running values it lacks (`keep_running`,
/// FG-1), or the first time a copy of the running one; then the lineup's
/// rules (`roster_watch::fit_recorded`: heard mixes only on a leader's set,
/// a set nobody wears silent), keeping `silenced`, the record of the sets
/// the rules silenced in the state it starts from. Pure.
pub fn target_state_recorded(
    target: Option<MixState>,
    current: &MixState,
    roster: &Roster,
    lineup: &Lineup,
    silenced: &mut BTreeSet<String>,
) -> MixState {
    let mut state = match target {
        Some(stored) => keep_running(&stored, current),
        None => current.clone(),
    };
    fit_recorded(&mut state, roster, lineup, silenced);
    state
}

fn import(state: MixState) -> Cmd {
    Cmd::ImportState {
        state,
        baseline: false,
    }
}

/// Records `id` as the active lineup in the running server's roster.
fn set_active_in_use(state: &AppState, id: &str) {
    if let Some(mut r) = state.roster() {
        r.active = id.to_string();
        state.set_roster(Some(r));
    }
}

/// Makes `to` the band that plays (see the module docs).
pub async fn switch_lineup(state: &AppState, to: &str, by: &AuthClaims) -> Result<(), SwitchError> {
    let roster = state.roster().ok_or(SwitchError::NotConfigured)?;
    if !may_switch(&roster.roster, by) {
        return Err(SwitchError::Forbidden);
    }
    let Ok(_running) = state.switching.try_lock() else {
        return Err(SwitchError::Busy);
    };
    // The roster as it is now that no reload runs: one that ran since the
    // first look may have taken the right away.
    let roster = state.roster().ok_or(SwitchError::NotConfigured)?;
    if !may_switch(&roster.roster, by) {
        return Err(SwitchError::Forbidden);
    }
    let from = roster.lineup().id.clone();
    let target = roster
        .roster
        .lineup(to)
        .cloned()
        .ok_or_else(|| SwitchError::Unknown(to.to_string()))?;
    if target.id == from {
        return Err(SwitchError::Same);
    }
    // Set already: a switch a restart cut short is not finished yet; or the
    // roster in use has not reached the engine's state yet.
    if state.frozen_now() || state.settling() {
        return Err(SwitchError::Busy);
    }
    let result = match state.site() {
        Some(view) => run(state, &roster.roster, &view, &from, &target).await,
        None => Err(SwitchError::NotSynced),
    };
    if result.is_ok() {
        // Still frozen: no page changes before the roster says `to`; then
        // every page hears of it.
        set_active_in_use(state, &target.id);
        state.thaw();
    }
    match &result {
        Ok(()) => {
            tracing::warn!(%from, to = %target.id, by = %by.sub, "the band was switched");
            tell_engineers(state, &target);
        }
        Err(e) => {
            tracing::error!(%from, to = %target.id, by = %by.sub, error = %e, "the band was not switched");
        }
    }
    result
}

/// The engineer's devices hear which band plays now (a switch done, by a
/// leader or by the replay that finished it), on a task of its own.
fn tell_engineers(state: &AppState, lineup: &Lineup) {
    let (s, payload) = (state.clone(), crate::notify::lineup_payload(&lineup.name));
    tokio::spawn(async move {
        let devices = crate::notify::push_engineers(&s, &payload).await;
        tracing::info!(devices, "the engineer's devices heard of the switch");
    });
}

/// The switch under the lock: the pages frozen ([`AppState::freeze`]: no
/// change on its way is left out of the live state), then its steps. On
/// success the pages stay frozen for the caller to record `to` first; on a
/// failure they are free again.
async fn run(
    state: &AppState,
    roster: &Roster,
    view: &SiteView,
    from: &str,
    to: &Lineup,
) -> Result<(), SwitchError> {
    if !state.engine.mirror().synced {
        return Err(SwitchError::NotSynced);
    }
    if state.freeze().await.map_err(SwitchError::Engine)? {
        // Frozen meanwhile by a switch a restart cut short: it is that one's.
        return Err(SwitchError::Busy);
    }
    match steps(state, roster, view, from, to).await {
        Ok(()) => Ok(()),
        Err(Failed::Back(e)) => {
            state.thaw();
            Err(e)
        }
        Err(Failed::Pending(e)) => {
            // The intent stays and the pages stay frozen: the switch
            // finishes once the engine answers (after this one's lock).
            tracing::error!(error = %e, "the switch finishes once the engine answers again");
            tokio::spawn(replay_when_synced(state.clone()));
            Err(SwitchError::Pending(e.to_string()))
        }
    }
}

/// How a switch's steps failed.
enum Failed {
    /// The band that played plays on, recorded as before.
    Back(SwitchError),
    /// The old band's state did not go back in either: which band's state
    /// the engine holds is not known, so the intent stays for
    /// [`replay_when_synced`] to finish the switch.
    Pending(SwitchError),
}

/// The switch's steps, the pages frozen.
async fn steps(
    state: &AppState,
    roster: &Roster,
    view: &SiteView,
    from: &str,
    to: &Lineup,
) -> Result<(), Failed> {
    let store = LineupStore::new(&state.config_dir);
    // The live state and the active lineup's pins and hides, as its stored
    // state.
    let mut live = {
        let mirror = state.engine.mirror();
        if !mirror.synced {
            return Err(Failed::Back(SwitchError::NotSynced));
        }
        let at = chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string();
        capture(view, &mirror, &state.band_of(Some(from)), at)
            .map_err(|e| Failed::Back(SwitchError::Io(e)))?
    };
    live.lineup = Some(from.to_string());
    // With what the running state follows, in the same file.
    (live.follows, live.silenced) = running_for(&store, from);
    store
        .save_state(from, &live)
        .map_err(|e| Failed::Back(io("the band's state was not stored", e)))?;
    let intent = SwitchIntent {
        from: from.to_string(),
        to: to.id.clone(),
    };
    store
        .set_intent(&intent)
        .map_err(|e| Failed::Back(io("the switch's intent was not written", e)))?;
    if let Err(e) = forward(state, &store, roster, &live, from, to).await {
        // The band that played plays on, once its state is back in for
        // sure; else the intent stays.
        if let Err(back) = state
            .engine
            .request_applied(import(live.state.clone()), None)
            .await
        {
            tracing::error!(error = %back, %from, "the band's state was not imported again");
            return Err(Failed::Pending(e));
        }
        if let Err(clear) = store.clear_intent() {
            tracing::error!(error = %clear, "the switch's intent was not cleared");
        }
        return Err(Failed::Back(e));
    }
    if let Err(e) = store.clear_intent() {
        // The switch happened; the next start clears it (active is `to`).
        tracing::warn!(error = %e, "the switch's intent was not cleared");
    }
    Ok(())
}

/// The state `lineup` plays from and what it follows then: its stored
/// state, or (it never played) the running state `current`, which follows
/// `running` (its marker and record), completed with the running values it
/// lacks and under its rules ([`target_state_recorded`]). A switch moves
/// nobody's mix: the marker goes along as it was.
fn target_of(
    stored: Option<MixerBackup>,
    current: &MixState,
    running: (Option<String>, BTreeSet<String>),
    roster: &Roster,
    lineup: &Lineup,
) -> (MixState, Running) {
    let (from, follows, mut silenced) = match stored {
        Some(b) => (Some(b.state), b.follows, b.silenced),
        None => (None, running.0, running.1),
    };
    let target = target_state_recorded(from, current, roster, lineup, &mut silenced);
    let running = Running {
        lineup: lineup.id.clone(),
        follows,
        silenced,
    };
    (target, running)
}

/// The target's state to the engine, then the target recorded active and
/// what the running state follows now.
async fn forward(
    state: &AppState,
    store: &LineupStore,
    roster: &Roster,
    live: &MixerBackup,
    from: &str,
    to: &Lineup,
) -> Result<(), SwitchError> {
    let stored = store
        .state(&to.id)
        .map_err(|e| io("the band's stored state is unreadable", e))?;
    let running = (live.follows.clone(), live.silenced.clone());
    let (target, running) = target_of(stored, &live.state, running, roster, to);
    // The engine keeps solos across the import: each set whose wearer
    // changes loses its solo (the pages are frozen still).
    let soloed = match roster.lineup(from) {
        Some(before) => soloed_changing(&state.engine.mirror().transient, before, to),
        None => Vec::new(),
    };
    state
        .engine
        .request_applied(import(target), None)
        .await
        .map_err(SwitchError::Engine)?;
    store
        .set_active(&to.id, chrono::Utc::now().timestamp())
        .map_err(|e| io("the band that plays was not recorded", e))?;
    note_running(store, &running);
    end_solos(state, &soloed).await;
    Ok(())
}

/// Imports `lineup`'s state (`stored`, or the running state, as
/// [`target_of`]) and records it as the band that plays, on disk and in
/// use, with what the running state follows.
async fn play(
    state: &AppState,
    store: &LineupStore,
    roster: &Roster,
    lineup: &Lineup,
    stored: Option<MixerBackup>,
    other: Option<&Lineup>,
) -> Result<(), String> {
    let (current, soloed) = {
        let mirror = state.engine.mirror();
        if !mirror.synced {
            return Err("the engine state is not synced".into());
        }
        // The engine keeps solos across the import: a set whose wearer is
        // another in `other`, the band the switch was between, loses its
        // solo (the pages are frozen still).
        let soloed = other.map_or_else(Vec::new, |o| soloed_changing(&mirror.transient, o, lineup));
        (mirror.state.clone(), soloed)
    };
    let running = running_of(store).map_or_else(Default::default, |r| (r.follows, r.silenced));
    let (target, running) = target_of(stored, &current, running, roster, lineup);
    state
        .engine
        .request_applied(import(target), None)
        .await
        .map_err(|e| e.to_string())?;
    store
        .set_active(&lineup.id, chrono::Utc::now().timestamp())
        .map_err(|e| e.to_string())?;
    note_running(store, &running);
    end_solos(state, &soloed).await;
    set_active_in_use(state, &lineup.id);
    Ok(())
}

/// Finishes a switch the engine dropped or a restart cut short (§7 step
/// 5), once the mirror has
/// synced: with the intent's target recorded active it only clears the
/// intent; otherwise it imports the target's state and records it (a
/// target the roster no longer holds: the active lineup's stored state
/// again), then clears it. Mix changes stay refused until it succeeds.
pub async fn replay_intent(state: &AppState) -> Result<(), String> {
    let _running = state.switching.lock().await;
    let store = LineupStore::new(&state.config_dir);
    let (Some(intent), Some(roster)) = (store.intent().map_err(|e| e.to_string())?, state.roster())
    else {
        state.thaw();
        return Ok(());
    };
    let active = store.active().map_err(|e| e.to_string())?;
    let lineup = roster
        .roster
        .lineup(&intent.to)
        .unwrap_or_else(|| roster.lineup())
        .clone();
    if active.as_deref() != Some(intent.to.as_str()) {
        let stored = store.state(&lineup.id).map_err(|e| e.to_string())?;
        let other = roster.roster.lineup(&intent.from);
        play(state, &store, &roster.roster, &lineup, stored, other).await?;
    }
    store.clear_intent().map_err(|e| e.to_string())?;
    tracing::warn!(lineup = %lineup.id, "a switch the engine dropped or a restart cut short is finished");
    // As after any switch: the pages and the engineer's devices hear of it.
    state.thaw();
    tell_engineers(state, &lineup);
    Ok(())
}

/// Gives up an unfinished switch: the band it came from plays again
/// from its stored state (the running one where that cannot be read),
/// recorded, the intent cleared, the pages free.
pub async fn fall_back(state: &AppState) -> Result<(), String> {
    let _running = state.switching.lock().await;
    let store = LineupStore::new(&state.config_dir);
    let (Some(intent), Some(roster)) = (store.intent().map_err(|e| e.to_string())?, state.roster())
    else {
        state.thaw();
        return Ok(());
    };
    let lineup = roster
        .roster
        .lineup(&intent.from)
        .unwrap_or_else(|| roster.lineup())
        .clone();
    let stored = match store.state(&lineup.id) {
        Ok(stored) => stored,
        Err(e) => {
            tracing::error!(lineup = %lineup.id, error = %e, "the band's stored state is unreadable: the running state goes on");
            None
        }
    };
    let other = roster.roster.lineup(&intent.to);
    play(state, &store, &roster.roster, &lineup, stored, other).await?;
    store.clear_intent().map_err(|e| e.to_string())?;
    tracing::warn!(lineup = %lineup.id, "an unfinished switch was given up: the band before it plays");
    state.thaw();
    Ok(())
}

/// Tries [`replay_when_synced`] makes to finish a switch, and then to go
/// back to the band before it.
pub const MAX_REPLAYS: u32 = 3;

/// The alarms' title, and what they say once the outcome is known.
const NOT_SWITCHED: &str = "Kapela neprepnutá";
const FELL_BACK: &str = "Prepnutie kapely sa nedalo dokončiť: hrá znova kapela pred ním.";
const GAVE_UP: &str = "Prepnutie kapely sa nedalo dokončiť a kapela pred ním sa nevrátila: \
                       server nevie, čie mixy engine drží. Mixy sa dajú znova meniť; \
                       skontroluj ich a prepni kapelu znova.";

/// How [`replay_when_synced`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Replayed {
    /// No switch was left to finish.
    Nothing,
    /// The switch was finished at try `tries`.
    Finished { tries: u32 },
    /// It could not be finished in `tries` tries: the band before plays
    /// again.
    FellBack { tries: u32 },
    /// It could neither be finished nor go back: the pages are free again
    /// with the state the engine holds, the intent kept for the next start.
    GaveUp,
}

/// At the start, and after a switch the engine dropped in: when a switch
/// was not finished (`frozen` set), waits for the engine's state (woken by
/// it, [`EngineClient::wait_synced`](crate::engine::EngineClient::wait_synced))
/// and finishes it ([`replay_intent`]), up to [`MAX_REPLAYS`] tries
/// `AppState::retry` apart; then the band before plays again
/// ([`fall_back`], as many tries), and the engineer's devices hear of it
/// once that is done. When that fails too the pages are freed with the
/// state the engine holds, and the devices hear that.
pub async fn replay_when_synced(state: AppState) -> Replayed {
    if !state.frozen_now() {
        return Replayed::Nothing;
    }
    let mut tries = 0;
    while tries < MAX_REPLAYS {
        if !state.engine.wait_synced(state.retry).await {
            continue;
        }
        tries += 1;
        match replay_intent(&state).await {
            Ok(()) => return Replayed::Finished { tries },
            Err(e) => {
                tracing::error!(error = %e, tries, "an unfinished switch is not finished: trying again");
                tokio::time::sleep(state.retry).await;
            }
        }
    }
    let mut back = 0;
    while back < MAX_REPLAYS {
        if !state.engine.wait_synced(state.retry).await {
            continue;
        }
        back += 1;
        match fall_back(&state).await {
            Ok(()) => {
                alarm(&state, FELL_BACK).await;
                return Replayed::FellBack { tries };
            }
            Err(e) => {
                tracing::error!(error = %e, tries = back, "the band before an unfinished switch does not play again: trying again");
                tokio::time::sleep(state.retry).await;
            }
        }
    }
    tracing::error!("an unfinished switch can neither be finished nor go back: the pages are free");
    state.thaw();
    alarm(&state, GAVE_UP).await;
    Replayed::GaveUp
}

/// The engineer's devices hear how an unfinished switch ended.
async fn alarm(state: &AppState, body: &str) {
    let devices = crate::notify::push_alarm(state, NOT_SWITCHED, body).await;
    tracing::warn!(devices, "the engineer's devices heard how the switch ended");
}

#[cfg(test)]
mod tests;
