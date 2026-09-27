//! The engine's part of `site.toml` (program spec I4, §3.1; #20 design note
//! §4): the `[engine]` table of inputs, groups and mixes, and (S6 design
//! note §4) the `[card]` table of the ASIO backend. Everything else in the
//! file belongs to the server and is ignored here, except the stage inputs
//! the interlock listens to (`[activity] inputs`, the `[[inputs]]`
//! categories); inside `[engine]` and `[card]` unknown keys are errors.

use std::path::Path;

use iem_win::prefwin::{Kind, Pref};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteInput {
    pub id: String,
    /// Card RX channels: one (mono) or two (stereo).
    pub rx: Vec<u16>,
    /// The input that carries talkback (A4); at most one.
    #[serde(default)]
    pub talkback: bool,
}

/// A group of inputs (the stems): every mix hears them through one strip.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteGroup {
    pub id: String,
    pub inputs: Vec<String>,
}

/// A mix: one listener. It hears every input; `mixes` are the other mixes it
/// hears (the Mixes tab), each declared before it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteMix {
    pub id: String,
    /// Card TX channels: two (stereo) or one (the mono downmix, A10).
    pub tx: Vec<u16>,
    #[serde(default)]
    pub mixes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Site {
    /// The card's channel map: RX and TX channels are 1…=channels.
    pub channels: u16,
    /// The mix with the fixed listen tap (X3 slot 0).
    pub engineer: String,
    pub inputs: Vec<SiteInput>,
    #[serde(default)]
    pub groups: Vec<SiteGroup>,
    pub mixes: Vec<SiteMix>,
}

/// The card (S6 design note §3, §4): the ASIO driver, its DLL, the buffer
/// and the driver's preferred-buffer value, which holds 32 only while the
/// driver opens and REAPER's original at every other moment. Site values:
/// only in the private ops site (P6).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Card {
    /// The driver's registry description.
    pub driver: String,
    /// The driver DLL: any process holding it refuses the start (I3).
    pub module: String,
    /// The buffer: 32 (I2); the driver's measured period must match.
    pub frames: u32,
    /// The preferred-buffer value: HKCU key and value name.
    pub pref_key: String,
    pub pref_name: String,
    /// REAPER's value, restored right after the driver opened.
    pub pref_original: PrefValue,
    /// CPU Set ids for the engine process (S1c L5); empty: every CPU.
    #[serde(default)]
    pub cpu_sets: Vec<u32>,
}

/// A registry value: its kind and its text (a DWORD in decimal).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrefValue {
    pub kind: PrefKind,
    pub raw: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrefKind {
    /// `REG_DWORD`.
    Dword,
    /// `REG_SZ`.
    Text,
}

/// The only buffer the engine runs at (I2).
pub const CARD_FRAMES: u32 = 32;

impl Card {
    /// REAPER's original preferred buffer, as the preference window reads it.
    pub fn pref_original(&self) -> Pref {
        Pref {
            kind: match self.pref_original.kind {
                PrefKind::Dword => Kind::Dword,
                PrefKind::Text => Kind::Text,
            },
            raw: self.pref_original.raw.clone(),
        }
    }

    fn check(&self) -> Result<(), SiteError> {
        for (field, value) in [
            ("driver", &self.driver),
            ("module", &self.module),
            ("pref_key", &self.pref_key),
            ("pref_name", &self.pref_name),
            ("pref_original.raw", &self.pref_original.raw),
        ] {
            if value.trim().is_empty() {
                return Err(SiteError::CardEmpty(field));
            }
        }
        if self.frames != CARD_FRAMES {
            return Err(SiteError::CardFrames(self.frames));
        }
        // A DWORD reads back as its plain decimal text: anything else could
        // never equal the value the preference window reads.
        let raw = &self.pref_original.raw;
        let decimal = raw.parse::<u32>().is_ok_and(|v| v.to_string() == *raw);
        if self.pref_original.kind == PrefKind::Dword && !decimal {
            return Err(SiteError::CardPref(raw.clone()));
        }
        Ok(())
    }
}

/// What the interlock listens to (S6 design note §4): the server's
/// `[activity] inputs`, and the `[[inputs]]` categories for the fallback
/// (every `mics` input when the list is empty).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stage {
    pub inputs: Vec<String>,
    /// `[[inputs]]` entries: id and category (`None`: `mics`).
    pub categories: Vec<(String, Option<String>)>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SiteError {
    #[error("site file: {0}")]
    Toml(String),
    #[error("site file: cannot read {0}")]
    Io(String),
    #[error("site file has no [engine] table")]
    NoEngineTable,
    #[error("invalid id {0:?} (1–64 of a-z 0-9 _ . -, starting with a letter or digit)")]
    BadId(String),
    #[error("id {0:?} is used twice")]
    DuplicateId(String),
    #[error("{id}: expected {expected} channel(s), got {got}")]
    ChannelCount {
        id: String,
        expected: &'static str,
        got: usize,
    },
    #[error("{id}: channel {ch} is outside the card's map")]
    ChannelRange { id: String, ch: u16 },
    #[error("card channel {ch} is used twice")]
    ChannelReused { ch: u16 },
    #[error("group {group:?}: {input:?} is not an input")]
    UnknownInput { group: String, input: String },
    #[error("group {0:?} has no inputs")]
    EmptyGroup(String),
    #[error("input {0:?} is in two groups")]
    SecondGroup(String),
    #[error("mix {mix:?} can hear only mixes declared before it, each once: {heard:?}")]
    HeardMix { mix: String, heard: String },
    #[error("more than one talkback input")]
    SecondTalkback,
    #[error("engineer {0:?} is not a stereo mix")]
    Engineer(String),
    #[error("site file has no [card] table (the asio backend needs one)")]
    NoCardTable,
    #[error("[card] {0} is empty")]
    CardEmpty(&'static str),
    #[error("[card] frames must be 32 (I2), not {0}")]
    CardFrames(u32),
    #[error("[card] pref_original {0:?} is not a DWORD's decimal text")]
    CardPref(String),
    #[error("[activity] input {0:?} is not an engine input")]
    StageInput(String),
    #[error("no stage input to listen to ([activity] inputs, or inputs of category mics)")]
    NoStage,
}

#[derive(Deserialize)]
struct File {
    engine: Option<Site>,
}

#[derive(Deserialize)]
struct CardFile {
    card: Option<Card>,
}

#[derive(Default, Deserialize)]
struct ActivityTable {
    #[serde(default)]
    inputs: Vec<String>,
}

#[derive(Deserialize)]
struct InputMeta {
    id: String,
    #[serde(default)]
    category: Option<String>,
}

#[derive(Deserialize)]
struct StageFile {
    #[serde(default)]
    activity: ActivityTable,
    #[serde(default)]
    inputs: Vec<InputMeta>,
}

fn toml_error(e: &toml::de::Error) -> SiteError {
    SiteError::Toml(e.to_string())
}

/// Parses the `[engine]` table of a site file.
pub fn parse(text: &str) -> Result<Site, SiteError> {
    let file: File = toml::from_str(text).map_err(|e| toml_error(&e))?;
    file.engine.ok_or(SiteError::NoEngineTable)
}

/// Parses and checks the `[card]` table of a site file, if it has one.
pub fn parse_card(text: &str) -> Result<Option<Card>, SiteError> {
    let file: CardFile = toml::from_str(text).map_err(|e| toml_error(&e))?;
    if let Some(card) = &file.card {
        card.check()?;
    }
    Ok(file.card)
}

/// The stage inputs' part of a site file (the server's tables, read only
/// for the interlock).
pub fn parse_stage(text: &str) -> Result<Stage, SiteError> {
    let file: StageFile = toml::from_str(text).map_err(|e| toml_error(&e))?;
    Ok(Stage {
        inputs: file.activity.inputs,
        categories: file
            .inputs
            .into_iter()
            .map(|i| (i.id, i.category))
            .collect(),
    })
}

/// The text of a site file.
pub fn read(path: &Path) -> Result<String, SiteError> {
    std::fs::read_to_string(path).map_err(|e| SiteError::Io(format!("{}: {e}", path.display())))
}

pub fn load(path: &Path) -> Result<Site, SiteError> {
    parse(&read(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_engine_table_is_read_and_the_rest_ignored() {
        let site = parse(
            r#"
            port = 8080
            [dante_outputs]
            MEMBER1 = [71, 72]
            [engine]
            channels = 8
            engineer = "eng"
            [[engine.inputs]]
            id = "mic"
            rx = [1]
            talkback = true
            [[engine.inputs]]
            id = "drums"
            rx = [2, 3]
            [[engine.groups]]
            id = "stems"
            inputs = ["drums"]
            [[engine.mixes]]
            id = "m1"
            tx = [3, 4]
            [[engine.mixes]]
            id = "eng"
            tx = [1, 2]
            mixes = ["m1"]
            "#,
        )
        .unwrap();
        assert_eq!(site.channels, 8);
        assert_eq!(site.engineer, "eng");
        assert_eq!(
            site.inputs[0],
            SiteInput {
                id: "mic".into(),
                rx: vec![1],
                talkback: true
            }
        );
        assert!(!site.inputs[1].talkback);
        assert_eq!(
            site.groups,
            vec![SiteGroup {
                id: "stems".into(),
                inputs: vec!["drums".into()]
            }]
        );
        assert!(site.mixes[0].mixes.is_empty());
        assert_eq!(
            site.mixes[1],
            SiteMix {
                id: "eng".into(),
                tx: vec![1, 2],
                mixes: vec!["m1".into()]
            }
        );
    }

    /// The synthetic card of `config/test-site.toml` (P6: never a real one).
    const CARD: &str = r#"
[card]
driver = "Test Card"
module = "testcard.dll"
frames = 32
pref_key = 'Software\ASIO\Test Card'
pref_name = "PrefBuffSize"
pref_original = { kind = "dword", raw = "64" }
"#;

    #[test]
    fn the_card_table_is_read_and_checked() {
        let card = parse_card(CARD).unwrap().unwrap();
        assert_eq!(
            card,
            Card {
                driver: "Test Card".into(),
                module: "testcard.dll".into(),
                frames: 32,
                pref_key: r"Software\ASIO\Test Card".into(),
                pref_name: "PrefBuffSize".into(),
                pref_original: PrefValue {
                    kind: PrefKind::Dword,
                    raw: "64".into()
                },
                cpu_sets: Vec::new(),
            }
        );
        assert_eq!(
            card.pref_original(),
            Pref {
                kind: Kind::Dword,
                raw: "64".into()
            }
        );
        let text = parse_card(&CARD.replace(
            r#"kind = "dword", raw = "64""#,
            r#"kind = "text", raw = "64 samples""#,
        ))
        .unwrap()
        .unwrap();
        assert_eq!(
            text.pref_original(),
            Pref {
                kind: Kind::Text,
                raw: "64 samples".into()
            }
        );
        let sets = parse_card(&format!("{CARD}cpu_sets = [256, 257]\n"))
            .unwrap()
            .unwrap();
        assert_eq!(sets.cpu_sets, [256, 257]);
        // Optional; the other tables are not the card's business.
        assert_eq!(parse_card("port = 1\n[engine]\nx = 1\n"), Ok(None));
        for (from, to, want) in [
            ("frames = 32", "frames = 64", SiteError::CardFrames(64)),
            ("frames = 32", "frames = 31", SiteError::CardFrames(31)),
            (
                r#"driver = "Test Card""#,
                r#"driver = " ""#,
                SiteError::CardEmpty("driver"),
            ),
            (
                r#"module = "testcard.dll""#,
                r#"module = """#,
                SiteError::CardEmpty("module"),
            ),
            (
                r"pref_key = 'Software\ASIO\Test Card'",
                "pref_key = ''",
                SiteError::CardEmpty("pref_key"),
            ),
            (
                r#"pref_name = "PrefBuffSize""#,
                r#"pref_name = """#,
                SiteError::CardEmpty("pref_name"),
            ),
            (
                r#"raw = "64""#,
                r#"raw = """#,
                SiteError::CardEmpty("pref_original.raw"),
            ),
            (
                r#"raw = "64""#,
                r#"raw = "+64""#,
                SiteError::CardPref("+64".into()),
            ),
            (
                r#"raw = "64""#,
                r#"raw = "064""#,
                SiteError::CardPref("064".into()),
            ),
            (
                r#"raw = "64""#,
                r#"raw = "x""#,
                SiteError::CardPref("x".into()),
            ),
        ] {
            assert_eq!(parse_card(&CARD.replace(from, to)), Err(want), "{to}");
        }
        assert!(matches!(
            parse_card(&format!("{CARD}colour = 1\n")),
            Err(SiteError::Toml(m)) if m.contains("colour")
        ));
        assert!(matches!(
            parse_card(&CARD.replace(r#""dword""#, r#""qword""#)),
            Err(SiteError::Toml(_))
        ));
    }

    #[test]
    fn the_stage_tables_are_read_and_the_rest_ignored() {
        let stage = parse_stage(
            r#"
            port = 1
            [activity]
            threshold_dbfs = -50.0
            inputs = ["mic1", "keys"]
            [[inputs]]
            id = "mic1"
            name = "M"
            owner = "member1"
            [[inputs]]
            id = "content"
            name = "C"
            category = "tech"
            [engine]
            channels = 2
            "#,
        )
        .unwrap();
        assert_eq!(
            stage,
            Stage {
                inputs: vec!["mic1".into(), "keys".into()],
                categories: vec![
                    ("mic1".into(), None),
                    ("content".into(), Some("tech".into()))
                ],
            }
        );
        assert_eq!(parse_stage("port = 1\n").unwrap(), Stage::default());
        assert!(matches!(
            parse_stage("[activity]\ninputs = 3\n"),
            Err(SiteError::Toml(_))
        ));
    }

    #[test]
    fn a_missing_table_and_unknown_keys_are_errors() {
        assert_eq!(parse("port = 1"), Err(SiteError::NoEngineTable));
        let unknown = parse(
            "[engine]\nchannels = 2\nengineer = \"e\"\ninputs = []\nmixes = []\ncolour = 1\n",
        );
        assert!(matches!(unknown, Err(SiteError::Toml(m)) if m.contains("colour")));
        let old_shape = parse(
            "[engine]\nchannels = 2\nengineer = \"e\"\ninputs = []\nmixes = []\n[[engine.sends]]\nfrom = []\nto = []\ntap = \"pre\"\n",
        );
        assert!(matches!(old_shape, Err(SiteError::Toml(m)) if m.contains("sends")));
        let groupless =
            parse("[engine]\nchannels = 2\nengineer = \"e\"\ninputs = []\nmixes = []\n").unwrap();
        assert!(groupless.groups.is_empty());
        assert!(matches!(parse("[engine"), Err(SiteError::Toml(_))));
        assert!(matches!(
            load(Path::new("/nonexistent/site.toml")),
            Err(SiteError::Io(m)) if m.contains("/nonexistent/site.toml")
        ));
    }
}
