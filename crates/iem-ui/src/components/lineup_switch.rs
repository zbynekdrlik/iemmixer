//! The band switch (#58 design §7): a leader of any band, or the engineer,
//! picks another band and confirms it; the server loads its lineup and
//! every mix at once (`POST /api/lineup/switch`). In the settings of a
//! leader's own page and of `/engineer`, and on the notice of a leader who
//! does not play in the band that plays.

use iem_core::{LineupInfo, LineupRef};
use leptos::prelude::*;

use crate::api::{SwitchFailure, Switched};
use crate::components::confirm_dialog::ConfirmDialog;

/// What a pending switch says (202): the mixer dropped in the middle, and
/// the server finishes the switch once it is back. Not a failure.
pub const PENDING: &str = "Prepnutie sa dokončí, keď sa mixér znova pripojí.";

/// The bands the control offers: every band but the one that plays.
pub fn other_lineups(info: &LineupInfo) -> Vec<LineupRef> {
    info.lineups
        .iter()
        .filter(|l| l.id != info.active.id)
        .cloned()
        .collect()
}

/// The band a switch goes to: `picked` (an id) when it is offered, else the
/// first band offered; none when only one band exists.
pub fn target_of(info: &LineupInfo, picked: &str) -> Option<LineupRef> {
    let others = other_lineups(info);
    others
        .iter()
        .find(|l| l.id == picked)
        .or_else(|| others.first())
        .cloned()
}

/// The confirmation's text.
pub fn confirm_body(active: &str, target: &str) -> String {
    format!(
        "Teraz hrá {active}. Po prepnutí bude hrať {target} a každý dostane svoj mix pre túto kapelu."
    )
}

/// Why a switch did not happen, in a sentence.
pub fn switch_error_message(failure: &SwitchFailure) -> String {
    match (failure.status, failure.code.as_str()) {
        (0, _) => "Server neodpovedal, skús to o chvíľu.".to_string(),
        (403, _) => "Prepnúť môže len vedúci kapely alebo zvukár.".to_string(),
        (409, "ALREADY_PLAYING") => "Táto kapela už hrá.".to_string(),
        (409, _) => "Práve sa prepína, skús to o chvíľu.".to_string(),
        (status, _) => format!("Prepnutie zlyhalo ({status})."),
    }
}

/// Where a switch stands.
#[derive(Debug, Clone, PartialEq)]
pub enum SwitchState {
    Idle,
    Sending,
    /// The switch to this band is done.
    Done(LineupRef),
    /// The switch to this band finishes once the mixer is back ([`PENDING`]).
    Pending(LineupRef),
    /// Why the switch did not happen, while the band `active` (its id)
    /// played.
    Failed {
        text: String,
        active: String,
    },
}

impl SwitchState {
    /// The state after the server's answer to a switch to `target`, asked
    /// while the band `active` (its id) played.
    pub fn after(
        outcome: &Result<Switched, SwitchFailure>,
        target: &LineupRef,
        active: &str,
    ) -> Self {
        match outcome {
            Ok(Switched::Now) => Self::Done(target.clone()),
            Ok(Switched::Pending) => Self::Pending(target.clone()),
            Err(failure) => Self::Failed {
                text: switch_error_message(failure),
                active: active.to_string(),
            },
        }
    }

    /// The line under the control and its class: a failure reads as an
    /// error, a done or pending switch does not.
    pub fn message(&self) -> Option<(String, &'static str)> {
        match self {
            Self::Idle | Self::Sending => None,
            Self::Done(target) => Some((format!("Hrá {}.", target.name), "lineup-switch-message")),
            Self::Pending(_) => Some((PENDING.to_string(), "lineup-switch-message")),
            Self::Failed { text, .. } => Some((text.clone(), "lineup-switch-message pin-error")),
        }
    }

    /// What the line becomes once the band `active` (its id) plays: `None`
    /// while it stays true.
    pub fn follow(&self, active: &str) -> Option<Self> {
        let _ = active;
        None
    }
}

#[component]
pub fn LineupSwitch(
    /// The band that plays and the bands there are (`GET /api/lineup`).
    #[prop(into)]
    info: Signal<Option<LineupInfo>>,
    /// After a switch the server took (done or pending), while this control
    /// is shown: the page reads the band again.
    on_switched: Callback<()>,
) -> impl IntoView {
    let (state, set_state) = signal(SwitchState::Idle);
    let (asking, set_asking) = signal(false);
    let (picked, set_picked) = signal(String::new());

    let target = move || info.with(|i| i.as_ref().and_then(|i| target_of(i, &picked.get())));
    let options = move || {
        info.with(|i| i.as_ref().map(other_lineups))
            .unwrap_or_default()
    };
    let body = Signal::derive(move || {
        let active = info.with(|i| i.as_ref().map(|i| i.active.name.clone()));
        match (active, target()) {
            (Some(active), Some(target)) => confirm_body(&active, &target.name),
            _ => String::new(),
        }
    });
    let sending = move || state.get() == SwitchState::Sending;
    let unavailable = move || sending() || target().is_none();

    let confirm = Callback::new(move |_: ()| {
        let _ = set_asking.try_set(false);
        let Some((to, active)) = info.with_untracked(|i| {
            i.as_ref().and_then(|i| {
                target_of(i, &picked.get_untracked()).map(|to| (to, i.active.id.clone()))
            })
        }) else {
            return;
        };
        let _ = set_state.try_set(SwitchState::Sending);
        wasm_bindgen_futures::spawn_local(async move {
            let outcome = crate::api::switch_lineup(&to.id).await;
            let next = SwitchState::after(&outcome, &to, &active);
            // Only while this control is shown: its page is then alive.
            if set_state.try_set(next).is_none() && outcome.is_ok() {
                on_switched.run(());
            }
        });
    });
    let cancel = Callback::new(move |_: ()| {
        let _ = set_asking.try_set(false);
    });

    view! {
        <div class="lineup-switch" data-testid="lineup-switch">
            <div class="lineup-switch-row">
                <select
                    class="lineup-target"
                    data-testid="lineup-target"
                    prop:value=move || target().map(|t| t.id).unwrap_or_default()
                    on:change=move |ev| {
                        let _ = set_picked.try_set(event_target_value(&ev));
                    }
                >
                    {move || options().into_iter().map(|l| view! {
                        <option value=l.id>{l.name}</option>
                    }).collect_view()}
                </select>
                <button
                    class="settings-action-btn lineup-switch-btn"
                    data-testid="lineup-switch-button"
                    disabled=unavailable
                    on:click=move |_| { let _ = set_asking.try_set(true); }
                >
                    "Prepnúť kapelu"
                </button>
            </div>
            {move || state.get().message().map(|(text, class)| view! {
                <div class=class data-testid="lineup-switch-message">{text}</div>
            })}
            <ConfirmDialog
                visible=asking
                title="Prepnúť kapelu?".to_string()
                body=body
                confirm_label="Prepnúť".to_string()
                cancel_label="Zrušiť".to_string()
                on_confirm=confirm
                on_cancel=cancel
            />
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band(id: &str) -> LineupRef {
        LineupRef {
            id: id.into(),
            name: format!("{id} band"),
        }
    }

    fn info(active: &str, ids: &[&str]) -> LineupInfo {
        LineupInfo {
            active: band(active),
            lineups: ids.iter().map(|id| band(id)).collect(),
            playing: true,
            leader: true,
            can_switch: true,
        }
    }

    fn failure(status: u16, code: &str) -> SwitchFailure {
        SwitchFailure {
            status,
            code: code.into(),
        }
    }

    #[test]
    fn switch_errors_read_as_sentences() {
        assert_eq!(
            switch_error_message(&failure(403, "FORBIDDEN")),
            "Prepnúť môže len vedúci kapely alebo zvukár."
        );
        for code in ["BUSY", "SWITCHING"] {
            assert_eq!(
                switch_error_message(&failure(409, code)),
                "Práve sa prepína, skús to o chvíľu.",
                "{code}"
            );
        }
        assert_eq!(
            switch_error_message(&failure(409, "ALREADY_PLAYING")),
            "Táto kapela už hrá."
        );
        assert_eq!(
            switch_error_message(&failure(503, "ENGINE_UNAVAILABLE")),
            "Prepnutie zlyhalo (503)."
        );
        assert_eq!(
            switch_error_message(&failure(502, "SWITCH_FAILED")),
            "Prepnutie zlyhalo (502)."
        );
        assert_eq!(
            switch_error_message(&failure(0, "NETWORK")),
            "Server neodpovedal, skús to o chvíľu."
        );
    }

    fn youth() -> LineupRef {
        LineupRef {
            id: "youth".into(),
            name: "Youth band".into(),
        }
    }

    #[test]
    fn a_switch_answer_reads_as_a_sentence_and_pending_is_no_failure() {
        let done = SwitchState::after(&Ok(Switched::Now), &youth(), "main");
        assert_eq!(done, SwitchState::Done(youth()));
        assert_eq!(
            done.message(),
            Some(("Hrá Youth band.".to_string(), "lineup-switch-message"))
        );
        let pending = SwitchState::after(&Ok(Switched::Pending), &youth(), "main");
        assert_eq!(pending, SwitchState::Pending(youth()));
        assert_eq!(PENDING, "Prepnutie sa dokončí, keď sa mixér znova pripojí.");
        assert_eq!(
            pending.message(),
            Some((PENDING.to_string(), "lineup-switch-message")),
            "pending reads as no error"
        );
        let failed = SwitchState::after(&Err(failure(409, "BUSY")), &youth(), "main");
        assert_eq!(
            failed,
            SwitchState::Failed {
                text: "Práve sa prepína, skús to o chvíľu.".into(),
                active: "main".into()
            }
        );
        assert_eq!(
            failed.message(),
            Some((
                "Práve sa prepína, skús to o chvíľu.".to_string(),
                "lineup-switch-message pin-error"
            ))
        );
        assert_eq!(SwitchState::Idle.message(), None);
        assert_eq!(SwitchState::Sending.message(), None);
    }

    /// #61 review M2: the line under the control follows the band that
    /// plays: "Hrá …" goes once another band plays, a pending switch reads
    /// done once its band plays, a refusal goes once the band it was about
    /// no longer plays. Here `follow` still keeps every line.
    #[test]
    fn the_switch_message_follows_the_band_that_plays() {
        let done = SwitchState::Done(youth());
        assert_eq!(done.follow("youth"), None, "still true");
        assert_eq!(
            done.follow("main"),
            Some(SwitchState::Idle),
            "another switch since"
        );
        let pending = SwitchState::Pending(youth());
        assert_eq!(pending.follow("main"), None, "not finished yet");
        assert_eq!(
            pending.follow("youth"),
            Some(SwitchState::Done(youth())),
            "finished"
        );
        let failed = SwitchState::Failed {
            text: "Táto kapela už hrá.".into(),
            active: "main".into(),
        };
        assert_eq!(failed.follow("main"), None);
        assert_eq!(failed.follow("youth"), Some(SwitchState::Idle));
        assert_eq!(SwitchState::Idle.follow("youth"), None);
        assert_eq!(
            SwitchState::Sending.follow("youth"),
            None,
            "its own answer decides"
        );
    }

    #[test]
    fn the_other_lineups_are_offered() {
        let main = info("main", &["main", "youth", "choir"]);
        let ids = |list: Vec<LineupRef>| list.into_iter().map(|l| l.id).collect::<Vec<_>>();
        assert_eq!(ids(other_lineups(&main)), ["youth", "choir"]);
        assert_eq!(
            ids(other_lineups(&info("youth", &["main", "youth"]))),
            ["main"]
        );
        // The picked band when it is offered, else the first.
        assert_eq!(target_of(&main, "choir"), Some(band("choir")));
        assert_eq!(target_of(&main, ""), Some(band("youth")));
        assert_eq!(
            target_of(&main, "main"),
            Some(band("youth")),
            "not the one that plays"
        );
        assert_eq!(target_of(&info("main", &["main"]), ""), None);
    }

    #[test]
    fn the_confirmation_names_both_bands() {
        assert_eq!(
            confirm_body("Main band", "Youth band"),
            "Teraz hrá Main band. Po prepnutí bude hrať Youth band a každý dostane svoj mix pre túto kapelu."
        );
    }
}
