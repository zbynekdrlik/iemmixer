//! The page of a person who does not play in the band that plays (#58
//! design §7): which band plays, instead of a mixer that would wait for a
//! socket the server refuses, and the band switch for a leader (a youth
//! leader switches back to main without playing in it). The page reads
//! the band again now and then, so it turns back into the mixer once a
//! switch brings the person's band. The same page, with [`NO_PAGE`], stands
//! for a page the token may not open or the site lacks (#61 second review
//! m3): no socket, no reconnect.

use iem_core::LineupInfo;
use leptos::prelude::*;

use crate::components::lineup_switch::LineupSwitch;

/// The page of a page that is in no band now (a person the roster dropped,
/// a page the token may not open).
pub const NO_PAGE: &str = "Táto stránka teraz nie je v žiadnej kapele.";

/// The notice's sentence.
pub fn notice_text(info: &LineupInfo) -> String {
    format!("Teraz hrá {}. V tejto kapele nehráš.", info.active.name)
}

/// The band switch the notice offers: the band's, when the bearer may
/// switch.
pub fn notice_switch(info: &LineupInfo) -> Option<LineupInfo> {
    info.can_switch.then(|| info.clone())
}

#[component]
pub fn LineupNotice(
    /// The sentence: [`notice_text`] or [`NO_PAGE`].
    text: String,
    /// The band switch to offer ([`notice_switch`]).
    #[prop(default = None)]
    switch: Option<LineupInfo>,
    /// After a switch from here: the page reads the band again.
    on_switched: Callback<()>,
) -> impl IntoView {
    let switch = switch.map(|info| {
        let info = Signal::stored(Some(info));
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

    #[test]
    fn the_notice_offers_the_switch_to_who_may_switch() {
        let youth = iem_core::LineupRef {
            id: "youth".into(),
            name: "Youth band".into(),
        };
        let mut info = LineupInfo {
            active: youth.clone(),
            lineups: vec![youth],
            playing: false,
            leader: false,
            can_switch: false,
        };
        assert_eq!(notice_switch(&info), None);
        info.can_switch = true;
        assert_eq!(notice_switch(&info), Some(info.clone()));
        assert_eq!(NO_PAGE, "Táto stránka teraz nie je v žiadnej kapele.");
    }
}
