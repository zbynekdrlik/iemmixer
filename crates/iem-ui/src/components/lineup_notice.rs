//! The page of a person who does not play in the band that plays (#58
//! design §7): which band plays, instead of a mixer that would wait for a
//! socket the server refuses, and the band switch for a leader (a youth
//! leader switches back to main without playing in it). The page reads
//! the band again now and then, so it turns back into the mixer once a
//! switch brings the person's band.

use iem_core::LineupInfo;
use leptos::prelude::*;

use crate::components::lineup_switch::LineupSwitch;

/// The notice's sentence.
pub fn notice_text(info: &LineupInfo) -> String {
    format!("Teraz hrá {}. V tejto kapele nehráš.", info.active.name)
}

#[component]
pub fn LineupNotice(
    /// The band that plays and what the bearer may do.
    info: LineupInfo,
    /// After a switch from here: the page reads the band again.
    on_switched: Callback<()>,
) -> impl IntoView {
    let text = notice_text(&info);
    let switch = info.can_switch.then(|| {
        let info = Signal::stored(Some(info.clone()));
        view! { <LineupSwitch info=info on_switched=on_switched /> }
    });
    view! {
        <div class="lineup-notice" data-testid="lineup-notice">
            <div class="lineup-notice-icon">"\u{1F3A7}"</div>
            <p class="lineup-notice-text">{text}</p>
            {switch}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notice_names_the_band_that_plays() {
        let youth = iem_core::LineupRef {
            id: "youth".into(),
            name: "Youth band".into(),
        };
        let info = LineupInfo {
            active: youth.clone(),
            lineups: vec![youth],
            playing: false,
            leader: false,
            can_switch: false,
        };
        assert_eq!(
            notice_text(&info),
            "Teraz hrá Youth band. V tejto kapele nehráš."
        );
    }
}
