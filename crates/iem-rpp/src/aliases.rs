//! The private mapping from the predecessor's names to iemmixer ids (S4
//! design note §3.1). Both files live in the ops repo, never here (P6):
//!
//! - `aliases.toml`: `master = "<bus id>"`, `[tracks]` REAPER track name (of
//!   any era) → engine id, `[members]` predecessor member id →
//!   `{ id, bus, stems, archived }` (`archived`: a renamed member, D8);
//! - `eras.toml`: `[[era]] first_seen`, `last_seen` (Unix seconds of the first
//!   and last saved project with this track layout) and `tracks` (track 1…N).

use std::collections::BTreeMap;

use iem_core::config::validate_member_id;
use iem_engine_proto::valid_id;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberAlias {
    /// The iemmixer member id.
    pub id: String,
    /// The member's output bus.
    pub bus: String,
    /// The member's stems bus.
    pub stems: String,
    /// A renamed member: its history is imported archived and read-only.
    #[serde(default)]
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Aliases {
    /// Bus id of the master (it is not a REAPER track).
    pub master: String,
    #[serde(default)]
    pub tracks: BTreeMap<String, String>,
    #[serde(default)]
    pub members: BTreeMap<String, MemberAlias>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Era {
    pub first_seen: i64,
    pub last_seen: i64,
    /// Track names; REAPER track number `k` is `tracks[k - 1]`.
    pub tracks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Eras {
    #[serde(default)]
    pub era: Vec<Era>,
}

pub fn parse_aliases(text: &str) -> Result<Aliases, String> {
    let a: Aliases = toml::from_str(text).map_err(|e| format!("aliases: {e}"))?;
    let mut bad = Vec::new();
    if !valid_id(&a.master) {
        bad.push(format!("master {:?} is not a valid id", a.master));
    }
    for (name, id) in &a.tracks {
        if !valid_id(id) {
            bad.push(format!("track {name:?}: {id:?} is not a valid id"));
        }
    }
    for (legacy, m) in &a.members {
        if let Err(e) = validate_member_id(legacy) {
            bad.push(format!("member {legacy:?}: {e}"));
        }
        if let Err(e) = validate_member_id(&m.id) {
            bad.push(format!("member {legacy:?}: {e}"));
        }
        for bus in [&m.bus, &m.stems] {
            if !valid_id(bus) {
                bad.push(format!("member {legacy:?}: {bus:?} is not a valid id"));
            }
        }
    }
    if bad.is_empty() {
        Ok(a)
    } else {
        Err(format!("aliases: {}", bad.join("; ")))
    }
}

pub fn parse_eras(text: &str) -> Result<Eras, String> {
    let e: Eras = toml::from_str(text).map_err(|e| format!("eras: {e}"))?;
    if e.era.is_empty() {
        return Err("eras: no [[era]]".into());
    }
    for (i, era) in e.era.iter().enumerate() {
        if era.first_seen > era.last_seen {
            return Err(format!("eras: era {} ends before it starts", i + 1));
        }
        if era.tracks.is_empty() {
            return Err(format!("eras: era {} has no tracks", i + 1));
        }
    }
    for (i, pair) in e.era.windows(2).enumerate() {
        if let [a, b] = pair
            && a.last_seen >= b.first_seen
        {
            return Err(format!(
                "eras: era {} overlaps era {} or is out of order",
                i + 1,
                i + 2
            ));
        }
    }
    Ok(e)
}

impl Eras {
    /// The eras an item saved at `t` may belong to: the one whose window holds
    /// `t`, else the two around the gap (only the first or last one outside).
    pub fn candidates(&self, t: i64) -> Vec<usize> {
        if let Some(i) = self
            .era
            .iter()
            .position(|e| e.first_seen <= t && t <= e.last_seen)
        {
            return vec![i];
        }
        match self.era.iter().position(|e| e.first_seen > t) {
            Some(0) => vec![0],
            Some(i) => vec![i - 1, i],
            None => self.era.len().checked_sub(1).into_iter().collect(),
        }
    }

    pub fn newest(&self) -> Option<usize> {
        self.era.len().checked_sub(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALIASES: &str = r#"
master = "master"
[tracks]
"MIC1 trk" = "mic1"
"OLD MIC1" = "mic1"
[members]
member1 = { id = "member1", bus = "member1", stems = "member1.stems" }
oldname = { id = "member1", bus = "member1", stems = "member1.stems", archived = true }
"#;

    #[test]
    fn aliases_parse_and_validate() {
        let a = parse_aliases(ALIASES).unwrap();
        assert_eq!(a.master, "master");
        assert_eq!(a.tracks["OLD MIC1"], "mic1");
        assert!(!a.members["member1"].archived);
        assert!(a.members["oldname"].archived);
        for (bad, why) in [
            ("master = \"Master\"", "master"),
            ("master = \"m\"\n[tracks]\nX = \"Bad Id\"", "track \"X\""),
            (
                "master = \"m\"\n[members]\n\"a b\" = { id = \"a\", bus = \"a\", stems = \"a.s\" }",
                "member \"a b\"",
            ),
            (
                "master = \"m\"\n[members]\na = { id = \"a/b\", bus = \"a\", stems = \"a.s\" }",
                "member \"a\"",
            ),
            (
                "master = \"m\"\n[members]\na = { id = \"a\", bus = \"A\", stems = \"a.s\" }",
                "\"A\"",
            ),
            ("master = \"m\"\ncolour = 1", "colour"),
            ("[tracks]", "master"),
        ] {
            let err = parse_aliases(bad).unwrap_err();
            assert!(err.contains(why), "{bad:?}: {err}");
        }
    }

    fn eras() -> Eras {
        parse_eras(
            "[[era]]\nfirst_seen = 100\nlast_seen = 200\ntracks = [\"A\"]\n\
             [[era]]\nfirst_seen = 300\nlast_seen = 300\ntracks = [\"A\", \"B\"]\n\
             [[era]]\nfirst_seen = 400\nlast_seen = 500\ntracks = [\"B\"]\n",
        )
        .unwrap()
    }

    #[test]
    fn candidates_inside_between_and_outside() {
        let e = eras();
        assert_eq!(e.candidates(100), vec![0]);
        assert_eq!(e.candidates(200), vec![0]);
        assert_eq!(e.candidates(300), vec![1]);
        assert_eq!(e.candidates(250), vec![0, 1]);
        assert_eq!(e.candidates(399), vec![1, 2]);
        assert_eq!(e.candidates(50), vec![0]);
        assert_eq!(e.candidates(501), vec![2]);
        assert_eq!(e.newest(), Some(2));
        assert_eq!(Eras { era: vec![] }.candidates(1), Vec::<usize>::new());
        assert_eq!(Eras { era: vec![] }.newest(), None);
    }

    #[test]
    fn bad_eras_are_refused() {
        for (bad, why) in [
            ("", "no [[era]]"),
            (
                "[[era]]\nfirst_seen = 2\nlast_seen = 1\ntracks = [\"A\"]",
                "ends before",
            ),
            (
                "[[era]]\nfirst_seen = 1\nlast_seen = 1\ntracks = []",
                "no tracks",
            ),
            (
                "[[era]]\nfirst_seen = 1\nlast_seen = 5\ntracks = [\"A\"]\n[[era]]\nfirst_seen = 5\nlast_seen = 6\ntracks = [\"A\"]",
                "overlaps",
            ),
            (
                "[[era]]\nfirst_seen = 1\nlast_seen = 1\ntracks = [\"A\"]\nx = 1",
                "x",
            ),
        ] {
            let err = parse_eras(bad).unwrap_err();
            assert!(err.contains(why), "{bad:?}: {err}");
        }
    }
}
