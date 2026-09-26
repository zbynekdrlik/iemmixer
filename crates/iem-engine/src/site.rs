//! The engine's part of `site.toml` (program spec I4, §3.1; #20 design note
//! §4): the `[engine]` table of inputs, groups and mixes. Everything else in
//! the file belongs to the server and is ignored here; inside `[engine]`
//! unknown keys are errors.

use std::path::Path;

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
}

#[derive(Deserialize)]
struct File {
    engine: Option<Site>,
}

/// Parses the `[engine]` table of a site file.
pub fn parse(text: &str) -> Result<Site, SiteError> {
    let file: File = toml::from_str(text).map_err(|e| SiteError::Toml(e.to_string()))?;
    file.engine.ok_or(SiteError::NoEngineTable)
}

pub fn load(path: &Path) -> Result<Site, SiteError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| SiteError::Io(format!("{}: {e}", path.display())))?;
    parse(&text)
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
