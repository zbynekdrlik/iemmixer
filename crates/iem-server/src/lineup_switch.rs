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

use std::sync::atomic::Ordering;
use std::time::Duration;

use iem_core::AuthClaims;
use iem_core::roster::{Lineup, Roster};
use iem_engine_proto::{Cmd, MixState};

use crate::AppState;
use crate::backup::{capture, keep_running};
use crate::engine::client::EngineError;
use crate::lineup_store::{LineupStore, SwitchIntent};
use crate::roster_watch::fit;
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
/// rules (`roster_watch::fit`: heard mixes only on a leader's set, a set
/// nobody wears silent). Pure.
pub fn target_state(
    target: Option<MixState>,
    current: &MixState,
    roster: &Roster,
    lineup: &Lineup,
) -> MixState {
    let mut state = match target {
        Some(stored) => keep_running(&stored, current),
        None => current.clone(),
    };
    fit(&mut state, roster, lineup);
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
    // The roster as it is now that no reload runs.
    let roster = state.roster().ok_or(SwitchError::NotConfigured)?;
    let from = roster.lineup().id.clone();
    let target = roster
        .roster
        .lineup(to)
        .cloned()
        .ok_or_else(|| SwitchError::Unknown(to.to_string()))?;
    if target.id == from {
        return Err(SwitchError::Same);
    }
    // Set already: a switch a restart cut short is not finished yet.
    if state.frozen_now() {
        return Err(SwitchError::Busy);
    }
    let result = match state.site() {
        Some(view) => run(state, &roster.roster, &view, &from, &target).await,
        None => Err(SwitchError::NotSynced),
    };
    if result.is_ok() {
        // Still frozen: no page changes before the roster says `to`.
        set_active_in_use(state, &target.id);
        state.frozen.store(false, Ordering::SeqCst);
    }
    match &result {
        Ok(()) => {
            tracing::warn!(%from, to = %target.id, by = %by.sub, "the band was switched");
            state.announce_lineup();
            let (s, payload) = (state.clone(), crate::notify::lineup_payload(&target.name));
            tokio::spawn(async move {
                let devices = crate::notify::push_engineers(&s, &payload).await;
                tracing::info!(devices, "the engineer's devices heard of the switch");
            });
        }
        Err(e) => {
            tracing::error!(%from, to = %target.id, by = %by.sub, error = %e, "the band was not switched");
        }
    }
    result
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
            state.frozen.store(false, Ordering::SeqCst);
            Err(e)
        }
        Err(Failed::Pending(e)) => {
            // The intent stays and the pages stay frozen: the switch
            // finishes once the engine answers (after this one's lock).
            tracing::error!(error = %e, "the switch finishes once the engine answers again");
            tokio::spawn(replay_when_synced(state.clone()));
            Err(e)
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
    if let Err(e) = forward(state, &store, roster, &live.state, to).await {
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

/// The target's state to the engine, then the target recorded active.
async fn forward(
    state: &AppState,
    store: &LineupStore,
    roster: &Roster,
    live: &MixState,
    to: &Lineup,
) -> Result<(), SwitchError> {
    let stored = store
        .state(&to.id)
        .map_err(|e| io("the band's stored state is unreadable", e))?
        .map(|b| b.state);
    let target = target_state(stored, live, roster, to);
    state
        .engine
        .request_applied(import(target), None)
        .await
        .map_err(SwitchError::Engine)?;
    store
        .set_active(&to.id, chrono::Utc::now().timestamp())
        .map_err(|e| io("the band that plays was not recorded", e))
}

/// Imports `lineup`'s state (`stored` completed with the running values
/// and under the lineup's rules, as [`target_state`]) and records it as the
/// band that plays, on disk and in use.
async fn play(
    state: &AppState,
    store: &LineupStore,
    roster: &Roster,
    lineup: &Lineup,
    stored: Option<MixState>,
) -> Result<(), String> {
    let current = {
        let mirror = state.engine.mirror();
        if !mirror.synced {
            return Err("the engine state is not synced".into());
        }
        mirror.state.clone()
    };
    let target = target_state(stored, &current, roster, lineup);
    state
        .engine
        .request_applied(import(target), None)
        .await
        .map_err(|e| e.to_string())?;
    store
        .set_active(&lineup.id, chrono::Utc::now().timestamp())
        .map_err(|e| e.to_string())?;
    set_active_in_use(state, &lineup.id);
    Ok(())
}

/// Finishes a switch a restart cut short (§7 step 5), once the mirror has
/// synced: with the intent's target recorded active it only clears the
/// intent; otherwise it imports the target's state and records it (a
/// target the roster no longer holds: the active lineup's stored state
/// again), then clears it. Mix changes stay refused until it succeeds.
pub async fn replay_intent(state: &AppState) -> Result<(), String> {
    let _running = state.switching.lock().await;
    let store = LineupStore::new(&state.config_dir);
    let (Some(intent), Some(roster)) = (store.intent().map_err(|e| e.to_string())?, state.roster())
    else {
        state.frozen.store(false, Ordering::SeqCst);
        return Ok(());
    };
    let active = store.active().map_err(|e| e.to_string())?;
    let played = active.as_deref() != Some(intent.to.as_str());
    if played {
        let lineup = roster
            .roster
            .lineup(&intent.to)
            .unwrap_or_else(|| roster.lineup())
            .clone();
        let stored = store
            .state(&lineup.id)
            .map_err(|e| e.to_string())?
            .map(|b| b.state);
        play(state, &store, &roster.roster, &lineup, stored).await?;
        tracing::warn!(lineup = %lineup.id, "a switch a restart cut short is finished");
    }
    store.clear_intent().map_err(|e| e.to_string())?;
    state.frozen.store(false, Ordering::SeqCst);
    if played {
        state.announce_lineup();
    }
    Ok(())
}

/// Gives up a switch a restart cut short: the band it came from plays again
/// from its stored state (the running one where that cannot be read),
/// recorded, the intent cleared, the pages free.
pub async fn fall_back(state: &AppState) -> Result<(), String> {
    let _running = state.switching.lock().await;
    let store = LineupStore::new(&state.config_dir);
    let (Some(intent), Some(roster)) = (store.intent().map_err(|e| e.to_string())?, state.roster())
    else {
        state.frozen.store(false, Ordering::SeqCst);
        return Ok(());
    };
    let lineup = roster
        .roster
        .lineup(&intent.from)
        .unwrap_or_else(|| roster.lineup())
        .clone();
    let stored = match store.state(&lineup.id) {
        Ok(stored) => stored.map(|b| b.state),
        Err(e) => {
            tracing::error!(lineup = %lineup.id, error = %e, "the band's stored state is unreadable: the running state goes on");
            None
        }
    };
    play(state, &store, &roster.roster, &lineup, stored).await?;
    store.clear_intent().map_err(|e| e.to_string())?;
    state.frozen.store(false, Ordering::SeqCst);
    tracing::warn!(lineup = %lineup.id, "a switch a restart cut short was given up: the band before it plays");
    state.announce_lineup();
    Ok(())
}

/// Tries [`replay_when_synced`] makes before it gives a switch up.
pub const MAX_REPLAYS: u32 = 3;

/// At the start, and after a switch the engine dropped in: when a switch
/// was cut short (`frozen` set), waits for the engine's state and finishes
/// it ([`replay_intent`]), trying again every [`REPLAY_RETRY`]. After
/// [`MAX_REPLAYS`] failed tries the engineer's devices hear of it (once)
/// and the band before the switch plays again ([`fall_back`], tried until
/// it succeeds).
pub async fn replay_when_synced(state: AppState) {
    if !state.frozen_now() {
        return;
    }
    let mut failures = 0;
    loop {
        let synced = state.engine.mirror().synced;
        if synced {
            let done = if failures < MAX_REPLAYS {
                replay_intent(&state).await
            } else {
                fall_back(&state).await
            };
            match done {
                Ok(()) => return,
                Err(e) => {
                    failures += 1;
                    tracing::error!(
                        error = %e,
                        failures,
                        "a switch a restart cut short is not finished: trying again"
                    );
                    if failures == MAX_REPLAYS {
                        let devices = crate::notify::push_alarm(
                            &state,
                            "Kapela neprepnutá",
                            "Prepnutie kapely, ktoré prerušil reštart servera, sa nedá dokončiť: \
                             hrá znova kapela pred ním.",
                        )
                        .await;
                        tracing::warn!(
                            devices,
                            "the engineer's devices heard the switch is given up"
                        );
                        continue;
                    }
                }
            }
        }
        tokio::time::sleep(REPLAY_RETRY).await;
    }
}

#[cfg(test)]
mod tests;
