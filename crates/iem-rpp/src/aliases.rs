//! The private mapping from the predecessor's names to iemmixer ids (S4
//! design note §3.1). Both files live in the ops repo, never here (P6):
//!
//! - `aliases.toml`: `[tracks]` REAPER track name (of any era) → engine id
//!   (an input, a mix, or for a stems bus the group it is an instance of),
//!   `[members]` predecessor member id → `{ id, mix, archived }` (`archived`:
//!   a renamed member, D8);
//! - `eras.toml`: `[[era]] first_seen`, `last_seen` (Unix seconds of the first
//!   and last saved project with this track layout) and `tracks` (track 1…N);
//!   optional `[[skip]]` entries (#9, #7): the reviewed list of snapshots the
//!   band import leaves out, each `{ file = "snapshots/<legacy member>.json",
//!   name, timestamp, reason }` naming exactly one snapshot of the predecessor.

use std::collections::{BTreeMap, BTreeSet};

use iem_core::config::validate_member_id;
use iem_engine_proto::valid_id;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberAlias {
    /// The iemmixer member id.
    pub id: String,
    /// The member's mix.
    pub mix: String,
    /// A renamed member: its history is imported archived and read-only.
    #[serde(default)]
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Aliases {
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

/// A snapshot the band import leaves out (`[[skip]]` in `eras.toml`): one the
/// eras cannot map and the owner reviewed. Snapshots only; it must match
/// exactly one snapshot (file, name and timestamp), never guessed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Skip {
    /// `snapshots/<legacy member id>.json` in the predecessor's data directory.
    pub file: String,
    /// The snapshot's name (its `label`, e.g. `auto`).
    pub name: String,
    /// The snapshot's `timestamp` (Unix seconds).
    pub timestamp: i64,
    /// Why it is left out (printed in the report).
    pub reason: String,
}

impl Skip {
    /// Whether this entry names the snapshot `name` saved at `timestamp` in
    /// `file` (exact on all three).
    pub fn matches(&self, file: &str, name: &str, timestamp: i64) -> bool {
        self.file == file && self.name == name && self.timestamp == timestamp
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Eras {
    #[serde(default)]
    pub era: Vec<Era>,
    #[serde(default)]
    pub skip: Vec<Skip>,
}

/// `snapshots/<member>.json` with a valid member id: skips apply to
/// snapshots only.
fn snapshot_file(file: &str) -> bool {
    file.strip_prefix("snapshots/")
        .and_then(|f| f.strip_suffix(".json"))
        .is_some_and(|id| validate_member_id(id).is_ok())
}

pub fn parse_aliases(text: &str) -> Result<Aliases, String> {
    let a: Aliases = toml::from_str(text).map_err(|e| format!("aliases: {e}"))?;
    let mut bad = Vec::new();
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
        if !valid_id(&m.mix) {
            bad.push(format!("member {legacy:?}: {:?} is not a valid id", m.mix));
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
    let mut seen = BTreeSet::new();
    for (i, s) in e.skip.iter().enumerate() {
        if !snapshot_file(&s.file) {
            return Err(format!(
                "eras: skip {}: file {:?} is not snapshots/<member>.json (a skip names a snapshot only)",
                i + 1,
                s.file
            ));
        }
        if s.reason.trim().is_empty() {
            return Err(format!("eras: skip {}: no reason", i + 1));
        }
        if !seen.insert((&s.file, &s.name, s.timestamp)) {
            return Err(format!("eras: skip {} repeats an earlier entry", i + 1));
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
[tracks]
"MIC1 trk" = "mic1"
"OLD MIC1" = "mic1"
"M1 STEMS" = "stems"
[members]
member1 = { id = "member1", mix = "member1" }
oldname = { id = "member1", mix = "member1", archived = true }
"#;

    #[test]
    fn aliases_parse_and_validate() {
        let a = parse_aliases(ALIASES).unwrap();
        assert_eq!(a.tracks["OLD MIC1"], "mic1");
        assert_eq!(a.tracks["M1 STEMS"], "stems");
        assert_eq!(a.members["member1"].mix, "member1");
        assert!(!a.members["member1"].archived);
        assert!(a.members["oldname"].archived);
        assert_eq!(parse_aliases("").unwrap(), Aliases::default());
        for (bad, why) in [
            ("[tracks]\nX = \"Bad Id\"", "track \"X\""),
            (
                "[members]\n\"a b\" = { id = \"a\", mix = \"a\" }",
                "member \"a b\"",
            ),
            (
                "[members]\na = { id = \"a/b\", mix = \"a\" }",
                "member \"a\"",
            ),
            ("[members]\na = { id = \"a\", mix = \"A\" }", "\"A\""),
            // The REAPER-shaped aliases of S4 are refused, not half-read.
            ("master = \"m\"", "master"),
            (
                "[members]\na = { id = \"a\", bus = \"a\", stems = \"a.s\" }",
                "bus",
            ),
            ("colour = 1", "colour"),
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
        let none = Eras {
            era: vec![],
            skip: vec![],
        };
        assert_eq!(none.candidates(1), Vec::<usize>::new());
        assert_eq!(none.newest(), None);
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

    const ERA: &str = "[[era]]\nfirst_seen = 1\nlast_seen = 9\ntracks = [\"A\"]\n";

    fn skip(file: &str, name: &str, timestamp: i64, reason: &str) -> String {
        format!(
            "[[skip]]\nfile = {file:?}\nname = {name:?}\ntimestamp = {timestamp}\nreason = {reason:?}\n"
        )
    }

    /// `[[skip]]` is additive: an eras.toml without it has no skips, and the
    /// entries parse in file order.
    #[test]
    fn skips_parse_in_order_and_are_optional() {
        assert!(parse_eras(ERA).unwrap().skip.is_empty());
        let e = parse_eras(&format!(
            "{ERA}{}{}",
            skip("snapshots/m1.json", "auto", 5, "layout never saved"),
            skip("snapshots/m-2_x.json", "before gig", 5, "r"),
        ))
        .unwrap();
        assert_eq!(
            e.skip,
            vec![
                Skip {
                    file: "snapshots/m1.json".into(),
                    name: "auto".into(),
                    timestamp: 5,
                    reason: "layout never saved".into(),
                },
                Skip {
                    file: "snapshots/m-2_x.json".into(),
                    name: "before gig".into(),
                    timestamp: 5,
                    reason: "r".into(),
                },
            ]
        );
        assert_eq!(e.era.len(), 1);
    }

    #[test]
    fn bad_skips_are_refused() {
        let ok = skip("snapshots/m1.json", "auto", 5, "r");
        for (bad, why) in [
            // Snapshots only: never presets, customizations or anything else.
            (
                skip("presets/m1.json", "auto", 5, "r"),
                "eras: skip 1: file \"presets/m1.json\" is not snapshots/<member>.json",
            ),
            (
                skip("customizations/m1.json", "auto", 5, "r"),
                "is not snapshots/<member>.json",
            ),
            (skip("pins.json", "auto", 5, "r"), "is not snapshots"),
            (
                skip("snapshots/m1.jsonx", "auto", 5, "r"),
                "is not snapshots",
            ),
            (skip("snapshots/.json", "auto", 5, "r"), "is not snapshots"),
            (
                skip("snapshots/a/b.json", "auto", 5, "r"),
                "is not snapshots",
            ),
            (
                skip("snapshots/../m1.json", "auto", 5, "r"),
                "is not snapshots",
            ),
            (skip("m1.json", "auto", 5, "r"), "is not snapshots"),
            // Every entry is reviewed: it says why.
            (
                skip("snapshots/m1.json", "auto", 5, " \t"),
                "eras: skip 1: no reason",
            ),
            // The same snapshot twice.
            (
                format!("{ok}{}{ok}", skip("snapshots/m1.json", "auto", 6, "r")),
                "eras: skip 3 repeats an earlier entry",
            ),
            // Unknown keys are refused, not ignored.
            (format!("{ok}kind = \"preset\"\n"), "kind"),
            (
                "[[skip]]\nfile = \"snapshots/m1.json\"\nname = \"auto\"\nreason = \"r\"\n".into(),
                "timestamp",
            ),
        ] {
            let err = parse_eras(&format!("{ERA}{bad}")).unwrap_err();
            assert!(err.contains(why), "{bad:?}: {err}");
        }
        // Near misses are distinct entries.
        let e = parse_eras(&format!(
            "{ERA}{ok}{}{}{}",
            skip("snapshots/m1.json", "auto", 6, "r"),
            skip("snapshots/m1.json", "Auto", 5, "r"),
            skip("snapshots/m2.json", "auto", 5, "r"),
        ))
        .unwrap();
        assert_eq!(e.skip.len(), 4);
    }

    /// A skip names one snapshot exactly: file, name and timestamp.
    #[test]
    fn a_skip_matches_on_all_three_fields() {
        let s = Skip {
            file: "snapshots/m1.json".into(),
            name: "auto".into(),
            timestamp: 1200,
            reason: "r".into(),
        };
        assert!(s.matches("snapshots/m1.json", "auto", 1200));
        assert!(!s.matches("snapshots/m1.json", "auto", 1201), "timestamp");
        assert!(!s.matches("snapshots/m1.json", "auto", 1199), "timestamp");
        assert!(!s.matches("snapshots/m1.json", "Auto", 1200), "name");
        assert!(!s.matches("snapshots/m1.json", "auto ", 1200), "name");
        assert!(!s.matches("snapshots/m2.json", "auto", 1200), "file");
        assert!(!s.matches("presets/m1.json", "auto", 1200), "file");
    }
}
