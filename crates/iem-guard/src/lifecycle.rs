//! The PC's lifecycle (S8 design note
//! `docs/superpowers/specs/2026-10-10-s8-cutover-rollback-design.md` §3.1,
//! §3.4; program spec §4.1): `Trial` before the cutover, `Prod` after it.
//! Persisted in `GuardState.lifecycle`. There is no rollback (the owner's
//! ROZHODNUTÉ on #11, 2026-10-10): REAPER and the predecessor stay
//! installed, and going back to REAPER after the cutover is the event
//! switch; prod stays prod.
//!
//! Pure: the daemon asks it where a starting guard goes, which dev or live
//! entry may run and on which build, what a crash loop means, what `iemmode
//! event` means, and which bundles keep their Defender exclusions. The
//! daemon only applies the answers.
//!
//! Nothing here turns `Trial` into `Prod`: the cutover does (S8 lane 2).
//! The pin changes only by the rules below (a maintenance session that ends
//! on a green `main` build, a prod crash loop's revert); never by an entry
//! or an activation, which change the active bundle only
//! (`GuardState.active`, mirrored into the legacy `GuardState.pins` for an
//! older guard).

use std::fs;
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize};

use crate::bundle::{Record, may_go_live};
use crate::plan::{self, Facts, Mode, Step};

/// Where the PC is in its life (design §3.1).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    /// Before the cutover: every boot is `event` (G1); `live` only as a
    /// trial (`live --build SHA --trial`).
    #[default]
    Trial,
    /// After the cutover: a boot goes `live` on the pin; a crash loop in
    /// live falls back to the previous pin; a dev entry is maintenance.
    /// A state lane 3's guard saved `rolling_back` in (a rollback, dropped
    /// since) reads as `Trial`, alarmed ([`lenient`], [`unreadable_in`]).
    Prod(Prod),
}

/// The cutover's record and the pins (design §3.1, §3.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prod {
    /// The cutover, seconds since the epoch.
    pub since: u64,
    /// The pin: the `main` build prod runs (every boot, every live entry).
    pub pin: String,
    /// The pin before it: where a crash loop in live goes (program spec
    /// §4.1); none at the cutover and after a revert.
    #[serde(default)]
    pub previous: Option<String>,
    /// The build of the maintenance session in progress (`dev --build SHA`
    /// in prod): when live comes back (an entry or a boot), it becomes the
    /// pin if [`may_go_live`], else the pin stays.
    #[serde(default)]
    pub maintenance: Option<String>,
}

impl Prod {
    /// Ends the maintenance session: its build becomes the pin, and the pin
    /// the previous pin, when it is an installed green `main` build other
    /// than the pin; otherwise the pin stays. What it decided, for the
    /// report (none without a session's build).
    pub fn end_maintenance<'r>(
        &self,
        record: &impl Fn(&str) -> Option<&'r Record>,
    ) -> (Self, Option<String>) {
        let mut next = Self {
            maintenance: None,
            ..self.clone()
        };
        let Some(m) = self.maintenance.as_deref() else {
            return (next, None);
        };
        if m == self.pin {
            return (next, None);
        }
        let verdict = record(m).map_or_else(|| Err(format!("{m} is not installed")), may_go_live);
        let note = match verdict {
            Ok(()) => {
                next.previous = Some(self.pin.clone());
                next.pin = m.to_owned();
                format!(
                    "maintenance build {m} becomes the pin; {} is the previous pin",
                    self.pin
                )
            }
            Err(why) => format!("maintenance build {m}: {why}; the pin {} stays", self.pin),
        };
        (next, Some(note))
    }
}

/// Why `pin` may not run live: not installed, or not a green `main` build
/// ([`may_go_live`], G8). The boot and every prod live entry check it: the
/// state file is the user's, and HIL may report the pin red later.
fn pin_refusal<'r>(pin: &str, record: &impl Fn(&str) -> Option<&'r Record>) -> Option<String> {
    match record(pin) {
        None => Some(format!("the pin {pin} is not installed")),
        Some(r) => may_go_live(r).err(),
    }
}

/// Where a starting guard goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// The saved mode stands (a guard restart; an unfinished switch is
    /// still re-planned to event).
    Keep,
    /// The PC is in event: the event plan from the saved mode.
    Event,
    /// Prod after a reboot: `live` on this pin.
    Live(String),
}

/// What [`start`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub start: Start,
    /// The lifecycle once `start` ran to its end: for `Start::Live`, saved
    /// only when the PC is live (a failed entry keeps the one before).
    pub lifecycle: Lifecycle,
    /// What the start decided about the pin, for the log.
    pub note: Option<String>,
    /// An alarm to raise: prod's pin may not run live, so the PC goes to
    /// event instead.
    pub alarm: Option<String>,
}

/// Where a starting guard goes (design §3.1). `reset`:
/// `state::reset_to_event` (a reboot, or the band's system up without our
/// engine); `rebooted`: the boot is later than the saved state. `Trial`:
/// event on `reset` (G1, as before S8). `Prod`: a reboot goes live on the
/// pin and ends a maintenance session ([`Prod::end_maintenance`]), unless
/// that pin may not run live (`pin_refusal`: event and an alarm); the
/// band's system up after a guard restart is event (an "ide event" stands).
pub fn start<'r>(
    lc: &Lifecycle,
    reset: bool,
    rebooted: bool,
    record: impl Fn(&str) -> Option<&'r Record>,
) -> Started {
    let (start, lifecycle, note, alarm) = match lc {
        Lifecycle::Prod(p) if rebooted => {
            let (next, note) = p.end_maintenance(&record);
            match pin_refusal(&next.pin, &record) {
                None => (
                    Start::Live(next.pin.clone()),
                    Lifecycle::Prod(next),
                    note,
                    None,
                ),
                Some(why) => (
                    Start::Event,
                    lc.clone(),
                    note,
                    Some(format!(
                        "after a reboot in prod: {why}; the PC stays in event"
                    )),
                ),
            }
        }
        _ if reset => (Start::Event, lc.clone(), None, None),
        _ => (Start::Keep, lc.clone(), None, None),
    };
    Started {
        start,
        lifecycle,
        note,
        alarm,
    }
}

/// A dev or live entry as asked (`iemmode dev|live`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ask<'a> {
    pub to: Mode,
    pub build: Option<&'a str>,
    pub trial: bool,
}

/// What an allowed entry does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entered {
    /// The bundle the entry runs, which becomes the active bundle; none:
    /// the active bundle as it is.
    pub runs: Option<String>,
    /// The lifecycle once the entry is done (saved only when it entered).
    pub lifecycle: Lifecycle,
    /// What the entry decided about the pin, for its report.
    pub note: Option<String>,
}

/// The entry gates (design §3.1, §3.4). Any build must be installed.
/// `Trial`: dev on any build; live only a trial, on a green `main` build
/// ([`may_go_live`]). `Prod`: no trial; dev is maintenance (with a build,
/// the session's build); live ends maintenance and runs the pin
/// ([`Prod::end_maintenance`]): without a build it runs the pin, a build
/// must be that pin.
pub fn entry<'r>(
    lc: &Lifecycle,
    ask: Ask<'_>,
    record: impl Fn(&str) -> Option<&'r Record>,
) -> Result<Entered, String> {
    let rec = ask
        .build
        .map(|sha| record(sha).ok_or_else(|| format!("bundle {sha} is not installed")))
        .transpose()?;
    if ask.to == Mode::Event {
        return Err("an entry goes to dev or live".to_owned());
    }
    let runs = ask.build.map(str::to_owned);
    match lc {
        Lifecycle::Trial => {
            if ask.to == Mode::Live {
                let (sha, rec) = ask.build.zip(rec).ok_or("live needs --build SHA")?;
                may_go_live(rec)?;
                if !ask.trial {
                    return Err(format!(
                        "before the cutover live is a trial: live --build {sha} --trial"
                    ));
                }
            }
            Ok(Entered {
                runs,
                lifecycle: Lifecycle::Trial,
                note: None,
            })
        }
        Lifecycle::Prod(p) => {
            if ask.trial {
                return Err(format!(
                    "after the cutover there are no trials: live runs the pin {}",
                    p.pin
                ));
            }
            if ask.to == Mode::Dev {
                let mut next = p.clone();
                if let Some(build) = &runs {
                    next.maintenance = Some(build.clone());
                }
                return Ok(Entered {
                    runs,
                    lifecycle: Lifecycle::Prod(next),
                    note: None,
                });
            }
            let (next, note) = p.end_maintenance(&record);
            if let Some(why) = pin_refusal(&next.pin, &record) {
                return Err(format!("in prod live runs the pin, and {why}"));
            }
            if ask.build.is_some_and(|b| b != next.pin) {
                let why = note.map_or_else(String::new, |n| format!(" ({n})"));
                return Err(format!(
                    "in prod live runs the pin {pin}{why}: live --build {pin}",
                    pin = next.pin
                ));
            }
            Ok(Entered {
                runs: Some(next.pin.clone()),
                lifecycle: Lifecycle::Prod(next),
                note,
            })
        }
    }
}

/// Whether an entry refreshes the band's data from the predecessor (the
/// data step: `pc.toml`'s `data_dev`/`data_live`). Only before the cutover:
/// the REAPER project is the data authority only until then (program spec
/// §3; ROZHODNUTÉ on #11). In prod iemmixer's own state (PINs, mixes,
/// snapshots the band changes) is the only authority, and the cutover's
/// final import (its live trial entry, still in `Trial`) is the last
/// import; while rolling back no entry runs.
pub fn refreshes_data(lc: &Lifecycle) -> bool {
    matches!(lc, Lifecycle::Trial)
}

/// A switch's steps as the lifecycle runs them: `plan::plan`, without the
/// data step where [`refreshes_data`] says no. The runner and the dry run
/// both plan through it.
pub fn plan(lc: &Lifecycle, to: Mode, facts: &Facts) -> Vec<Step> {
    let mut steps = plan::plan(to, facts);
    if !refreshes_data(lc) {
        steps.retain(|s| *s != Step::Data);
    }
    steps
}

/// What a crash loop (3 abnormal engine exits in 10 min) means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fallback {
    /// Back to REAPER: before the cutover (dev or a trial), or an engine in
    /// event.
    Event,
    /// Maintenance: iemmixer stops and live runs on this pin; the session's
    /// build never becomes the pin.
    Pin(String),
    /// Maintenance whose pin may not go live (`pin_refusal`, why): back to
    /// REAPER; the session's build is dropped.
    Reaper(String),
    /// Prod live: the engine of the previous pin, which is the pin from now
    /// on (no previous pin is left).
    Previous(String),
    /// Prod live with no previous pin it may go to (none, it looped too, or
    /// it may not go live): no respawn, an alarm that names the way back
    /// to REAPER (`iemmode event`); the pin stays. The text says on which
    /// pin and why.
    Down(String),
}

/// The crash rules (design §3.4; program spec §4.1) and the lifecycle after
/// them. A pin the fallback would go live on is checked as at the boot
/// (`pin_refusal`, G8).
pub fn crash_loop<'r>(
    lc: &Lifecycle,
    mode: Mode,
    record: impl Fn(&str) -> Option<&'r Record>,
) -> (Fallback, Lifecycle) {
    match (lc, mode) {
        (Lifecycle::Prod(p), Mode::Dev) => {
            let next = Lifecycle::Prod(Prod {
                maintenance: None,
                ..p.clone()
            });
            match pin_refusal(&p.pin, &record) {
                None => (Fallback::Pin(p.pin.clone()), next),
                Some(why) => (Fallback::Reaper(why), next),
            }
        }
        (Lifecycle::Prod(p), Mode::Live) => match p.previous.as_deref() {
            None => (
                Fallback::Down(format!("on the pin {}, and no previous pin is left", p.pin)),
                lc.clone(),
            ),
            Some(previous) => match pin_refusal(previous, &record) {
                None => (
                    Fallback::Previous(previous.to_owned()),
                    Lifecycle::Prod(Prod {
                        pin: previous.to_owned(),
                        previous: None,
                        ..p.clone()
                    }),
                ),
                Some(why) => (
                    Fallback::Down(format!(
                        "on the pin {}, and the previous pin may not go live ({why})",
                        p.pin
                    )),
                    lc.clone(),
                ),
            },
        },
        _ => (Fallback::Event, lc.clone()),
    }
}

/// The bundles that keep their Defender exclusions when `sha` is activated:
/// the way back (`GuardState::way_back_bundle` after the activation, as
/// `pins.previous` before S8) and, in prod, the pin and the previous pin (a
/// boot and a crash loop go back to them); never `sha`, each once.
pub fn kept(lc: &Lifecycle, way_back: Option<&str>, sha: &str) -> Vec<String> {
    let mut keep: Vec<String> = Vec::new();
    let pins = match lc {
        Lifecycle::Prod(p) => [Some(p.pin.as_str()), p.previous.as_deref()],
        Lifecycle::Trial => [None, None],
    };
    for b in [way_back].into_iter().chain(pins).flatten() {
        if b != sha && !keep.iter().any(|k| k == b) {
            keep.push(b.to_owned());
        }
    }
    keep
}

/// `activate` (online and offline) in prod takes only a bundle whose
/// guard keeps the lifecycle (S8 lane 5,
/// `bundle::keeps_lifecycle`): an older guard that took over would drop it
/// on its next save, and prod would read back as trial with pin_changes
/// open and the predecessor's autostarts disabled. Before the cutover any
/// bundle activates. `keeps` reads the bundle's manifest, only when it
/// matters.
pub fn activation_refusal(
    lc: &Lifecycle,
    sha: &str,
    keeps: impl FnOnce() -> Result<bool, String>,
) -> Option<String> {
    if *lc == Lifecycle::Trial {
        return None;
    }
    let state = status(lc).unwrap_or_default();
    match keeps() {
        Ok(true) => None,
        Ok(false) => Some(format!(
            "{sha}'s guard predates the lifecycle (its manifest names no guard_lifecycle): in \
             {state} no activation of a guard that would drop it"
        )),
        Err(why) => Some(format!(
            "{sha}'s manifest cannot be read ({why}): in {state} no activation of a guard that \
             may not keep the lifecycle"
        )),
    }
}

/// The lifecycle in `iemmode status`; none in `Trial` (the status reads as
/// before S8).
pub fn status(lc: &Lifecycle) -> Option<String> {
    match lc {
        Lifecycle::Trial => None,
        Lifecycle::Prod(p) => {
            let mut text = format!(
                "prod since {}: pin {}, previous {}",
                p.since,
                p.pin,
                p.previous.as_deref().unwrap_or("none")
            );
            if let Some(m) = &p.maintenance {
                text.push_str(&format!(", maintenance {m}"));
            }
            Some(text)
        }
    }
}

/// Whether the lifecycle is prod: the pipe's routing reads it from the
/// view (`View.prod`, S8 lane 5): in prod an "ide event" waits for a
/// switch to live and never pre-empts it.
pub fn is_prod(lc: &Lifecycle) -> bool {
    matches!(lc, Lifecycle::Prod(_))
}

/// What `iemmode event` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnEvent {
    /// The event plan.
    Plan,
    /// Prod's band system: live on the pin (a maintenance session ends).
    Live,
    /// Prod live with a healthy engine: iemmixer already serves the band.
    Stay,
}

/// What `iemmode event` means (S8 lane 3; the rollback dropped, the
/// owner's ROZHODNUTÉ on #11). `signal`: the owner's "ide event" (`iempc
/// event` sends `--signal`); without it the engineer's "Back to REAPER"
/// (`POST /api/mode/event`, the site's `back_to_reaper`) or a person's
/// `iemmode event`: the event plan in every lifecycle (in prod the PC stays
/// prod; the next boot goes live on the pin). Before the cutover "ide event"
/// is the event plan too. After it iemmixer serves the band at an event, so
/// "ide event" ends a maintenance session (dev) with live on the pin, leaves
/// a healthy live as it is, and runs the event plan, REAPER for this event,
/// when the engine does not play or the PC is already in event. `healthy`
/// reads the engine, only for prod live.
pub fn on_event(
    lc: &Lifecycle,
    mode: Mode,
    signal: bool,
    healthy: impl FnOnce() -> bool,
) -> OnEvent {
    match (lc, mode) {
        (Lifecycle::Prod(_), Mode::Dev) if signal => OnEvent::Live,
        (Lifecycle::Prod(_), Mode::Live) if signal && healthy() => OnEvent::Stay,
        _ => OnEvent::Plan,
    }
}

/// `GuardState.lifecycle` as a guard reads it: missing is `Trial` (a state
/// an older guard saved); one this guard cannot read (a newer guard's
/// shape) is `Trial` too, alarmed ([`unreadable_in`]): every boot is then
/// `event`, never `live` on a pin it could not read.
pub fn lenient<'de, D: Deserializer<'de>>(d: D) -> Result<Lifecycle, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(v.and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default())
}

/// The alarm for a saved state at `path` whose lifecycle [`lenient`] read as
/// `Trial` because it could not read it; none for a missing or null one, or
/// a file that is no JSON object (the state's own load names that).
pub fn unreadable_in(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let lc = value.get("lifecycle").filter(|lc| !lc.is_null())?;
    let e = serde_json::from_value::<Lifecycle>(lc.clone()).err()?;
    Some(format!(
        "the saved lifecycle is unreadable ({e}): trial, every boot in event"
    ))
}

#[cfg(test)]
mod tests;
