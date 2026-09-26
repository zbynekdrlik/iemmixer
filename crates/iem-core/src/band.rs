//! Band data files, schema 2 (S4 design note §3.4): presets, snapshots and
//! customizations keyed by the engine's stable ids instead of REAPER track
//! indices. The S4 migration writes them from the predecessor's data; the S5
//! server reads and writes them.

use std::collections::BTreeMap;

use iem_engine_proto::{Eq as EqSettings, InputId, Source};
use serde::{Deserialize, Serialize};

pub const SCHEMA: u32 = 2;
pub const PRESETS_FORMAT: &str = "iemmixer-presets";
pub const SNAPSHOTS_FORMAT: &str = "iemmixer-snapshots";
pub const CUSTOMIZATION_FORMAT: &str = "iemmixer-customization";

/// One source in a member's mix: the send from `src` into the member's bus
/// (or stems bus, for the stems group), in dB (≤ −150 = off) and pan −1…1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MixSend {
    pub src: Source,
    #[serde(default)]
    pub gain_db: f64,
    #[serde(default)]
    pub pan: f64,
    #[serde(default)]
    pub muted: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preset {
    pub name: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub sends: Vec<MixSend>,
    /// The member's stems bus fader.
    pub stems_fader_db: Option<f64>,
    /// Input EQ saved with the preset: metadata, never applied (Q2).
    pub input_eq: BTreeMap<InputId, EqSettings>,
    /// Imported from a renamed member (D8): shown read-only.
    pub archived: bool,
    /// The predecessor member id an archived entry came from.
    pub legacy_member: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub timestamp: i64,
    pub label: String,
    pub pinned: bool,
    pub sends: Vec<MixSend>,
    pub stems_fader_db: Option<f64>,
    /// Input EQ saved with the snapshot: metadata, never applied (Q2).
    pub input_eq: BTreeMap<InputId, EqSettings>,
    pub archived: bool,
    pub legacy_member: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PresetFile {
    pub format: String,
    pub schema: u32,
    pub member: String,
    pub presets: Vec<Preset>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SnapshotFile {
    pub format: String,
    pub schema: u32,
    pub member: String,
    pub snapshots: Vec<Snapshot>,
}

/// Per-member channel pins and hides (the predecessor's customizations).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomizationFile {
    pub format: String,
    pub schema: u32,
    pub member: String,
    pub pinned: Vec<Source>,
    pub hidden: Vec<Source>,
}

impl PresetFile {
    pub fn new(member: impl Into<String>, presets: Vec<Preset>) -> Self {
        Self {
            format: PRESETS_FORMAT.into(),
            schema: SCHEMA,
            member: member.into(),
            presets,
        }
    }
}

impl SnapshotFile {
    pub fn new(member: impl Into<String>, snapshots: Vec<Snapshot>) -> Self {
        Self {
            format: SNAPSHOTS_FORMAT.into(),
            schema: SCHEMA,
            member: member.into(),
            snapshots,
        }
    }
}

impl CustomizationFile {
    pub fn new(member: impl Into<String>, pinned: Vec<Source>, hidden: Vec<Source>) -> Self {
        Self {
            format: CUSTOMIZATION_FORMAT.into(),
            schema: SCHEMA,
            member: member.into(),
            pinned,
            hidden,
        }
    }
}

#[cfg(test)]
mod tests {
    use iem_engine_proto::BusId;

    use super::*;

    #[test]
    fn files_round_trip_through_json_with_their_headers() {
        let send = MixSend {
            src: Source::Input(InputId::new("mic1")),
            gain_db: -6.5,
            pan: 0.5,
            muted: true,
        };
        let mut eq = BTreeMap::new();
        eq.insert(InputId::new("mic1"), EqSettings::default());
        let preset = Preset {
            name: "p".into(),
            created_at: 1,
            updated_at: 2,
            sends: vec![send.clone()],
            stems_fader_db: Some(-3.0),
            input_eq: eq,
            archived: true,
            legacy_member: Some("old".into()),
        };
        let file = PresetFile::new("member1", vec![preset]);
        let json = serde_json::to_string(&file).unwrap();
        assert!(json.starts_with(r#"{"format":"iemmixer-presets","schema":2,"member":"member1""#));
        assert!(json.contains(r#""input_eq":{"mic1":{"#), "{json}");
        assert_eq!(serde_json::from_str::<PresetFile>(&json).unwrap(), file);
        let snap = SnapshotFile::new(
            "member1",
            vec![Snapshot {
                timestamp: 5,
                label: "auto".into(),
                sends: vec![send],
                ..Snapshot::default()
            }],
        );
        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains(r#""format":"iemmixer-snapshots""#));
        assert_eq!(serde_json::from_str::<SnapshotFile>(&json).unwrap(), snap);
        let c = CustomizationFile::new(
            "member1",
            vec![Source::Bus(BusId::new("member2"))],
            vec![Source::Input(InputId::new("keys"))],
        );
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains(r#""pinned":[{"bus":"member2"}]"#), "{json}");
        assert_eq!(serde_json::from_str::<CustomizationFile>(&json).unwrap(), c);
    }

    #[test]
    fn readers_default_missing_fields_and_ignore_unknown_ones() {
        let p: Preset = serde_json::from_str(r#"{"name":"x","future":1}"#).unwrap();
        assert_eq!(p.name, "x");
        assert!(p.sends.is_empty() && !p.archived && p.stems_fader_db.is_none());
        let s: MixSend = serde_json::from_str(r#"{"src":{"input":"mic1"}}"#).unwrap();
        assert_eq!((s.gain_db, s.pan, s.muted), (0.0, 0.0, false));
    }
}
