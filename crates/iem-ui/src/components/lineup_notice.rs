//! The page of a person who does not play in the band that plays (#58
//! design §7): which band plays, instead of a mixer that would wait for a
//! socket the server refuses. The page reads the band again now and then,
//! so it turns back into the mixer once a switch brings the person's band.

use iem_core::LineupInfo;
use leptos::prelude::*;

/// The notice's sentence.
pub fn notice_text(info: &LineupInfo) -> String {
    format!("Teraz hrá {}. V tejto kapele nehráš.", info.active.name)
}

#[component]
pub fn LineupNotice(
    /// The band that plays and what the bearer may do.
    info: LineupInfo,
) -> impl IntoView {
    let text = notice_text(&info);
    view! {
        <div class="lineup-notice" data-testid="lineup-notice">
            <div class="lineup-notice-icon">"\u{1F3A7}"</div>
            <p class="lineup-notice-text">{text}</p>
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
