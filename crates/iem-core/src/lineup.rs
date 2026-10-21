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

#[cfg(test)]
mod tests {
    use super::*;

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
