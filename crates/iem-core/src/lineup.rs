//! The lineups ("bands") as the server and the UI share them (#58 design
//! §7): a lineup's id and name. The roster itself (people, sets, leaders)
//! stays on the server (`roster`); a page only learns which lineup plays.

use serde::{Deserialize, Serialize};

/// A lineup by its id and the name the band knows it by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineupRef {
    pub id: String,
    pub name: String,
}

/// `GET /api/lineup`: the lineup that plays, every lineup (roster order),
/// and what the bearer may do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineupInfo {
    pub active: LineupRef,
    pub lineups: Vec<LineupRef>,
    /// The bearer wears a set of the active lineup (the engineer: always).
    /// A person who does not sees which band plays instead of a mixer.
    pub playing: bool,
    /// The bearer leads the active lineup (the Mixes tab is theirs).
    pub leader: bool,
    /// The bearer may switch the band: the engineer, or a leader of any
    /// lineup (a youth leader may switch back to main without playing in
    /// it, design §7).
    pub can_switch: bool,
}

/// `POST /api/lineup/switch`: the lineup to play from now on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchRequest {
    pub to: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The page builds and reads these by name (Part C).
    #[test]
    fn lineup_info_and_the_switch_request_have_stable_shapes() {
        let youth = LineupRef {
            id: "youth".into(),
            name: "Youth band".into(),
        };
        let info = LineupInfo {
            active: youth.clone(),
            lineups: vec![
                LineupRef {
                    id: "main".into(),
                    name: "Main band".into(),
                },
                youth,
            ],
            playing: false,
            leader: false,
            can_switch: true,
        };
        let json = concat!(
            r#"{"active":{"id":"youth","name":"Youth band"},"#,
            r#""lineups":[{"id":"main","name":"Main band"},{"id":"youth","name":"Youth band"}],"#,
            r#""playing":false,"leader":false,"can_switch":true}"#
        );
        assert_eq!(serde_json::to_string(&info).unwrap(), json);
        assert_eq!(serde_json::from_str::<LineupInfo>(json).unwrap(), info);
        let to = SwitchRequest { to: "main".into() };
        assert_eq!(serde_json::to_string(&to).unwrap(), r#"{"to":"main"}"#);
        assert_eq!(
            serde_json::from_str::<SwitchRequest>(r#"{"to":"main"}"#).unwrap(),
            to
        );
    }

    /// The page reads these fields by name: a serde rename must fail here.
    #[test]
    fn a_lineup_ref_is_its_id_and_name() {
        let r = LineupRef {
            id: "youth".into(),
            name: "Youth band".into(),
        };
        let json = r#"{"id":"youth","name":"Youth band"}"#;
        assert_eq!(serde_json::to_string(&r).unwrap(), json);
        assert_eq!(serde_json::from_str::<LineupRef>(json).unwrap(), r);
    }
}
