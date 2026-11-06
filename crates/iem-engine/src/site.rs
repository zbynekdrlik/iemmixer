//! The engine's part of `site.toml` (program spec I4): the `[engine]` table of inputs, groups and mixes, and `[guard]
//! hil_tx`. Everything else in the file belongs to the server and is ignored
//! here; inside `[engine]` unknown keys are errors.

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
/// hears whole (the Mixes tab), each declared before it; `peers` the sets it
/// hears as their own mix (#58), declared anywhere.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteMix {
    pub id: String,
    /// Card TX channels: two (stereo) or one (the mono downmix, A10).
    pub tx: Vec<u16>,
    #[serde(default)]
    pub mixes: Vec<String>,
    /// Other stereo set mixes it hears as their own output (#58 design §8):
    /// their inputs and group strips through their EQ, limiter and volume,
    /// without the mixes they hear, so two sets can hear each other.
    #[serde(default)]
    pub peers: Vec<String>,
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
    #[error(
        "mix {mix:?} can hear as a peer only another stereo set mix, each once and not also in mixes: {peer:?}"
    )]
    Peer { mix: String, peer: String },
    #[error(
        "mix {0:?} cannot hear peers: only a stereo set mix does, not the engineer's or a mono mix"
    )]
    PeersOnNonSet(String),
    #[error(
        "mix {mix:?} cannot hear {heard:?} whole: {heard:?}, or a mix it hears whole, hears {mix:?} as a peer, so {mix:?} would hear itself back"
    )]
    HearsItselfBack { mix: String, heard: String },
    #[error("more than one talkback input")]
    SecondTalkback,
    #[error("engineer {0:?} is not a stereo mix")]
    Engineer(String),
    #[error("[guard] hil_tx: {0}")]
    HilTx(String),
}

#[derive(Deserialize)]
struct File {
    engine: Option<Site>,
}

/// The `[guard]` table as the engine reads it: only `hil_tx`, the outputs
/// HIL's test signal may reach.
#[derive(Default, Deserialize)]
struct GuardTable {
    #[serde(default)]
    hil_tx: Vec<u16>,
}

#[derive(Deserialize)]
struct GuardFile {
    #[serde(default)]
    guard: GuardTable,
}

/// A site file's parse error: its line and the parser's message, never the
/// excerpt of the file `toml`'s own text holds, and never a value the message
/// quotes (`invalid type: string "…"`). Logs and a deploy's output, which may
/// be pasted, stay free of the site's values (P6).
fn parse_error(content: &str, e: &toml::de::Error) -> String {
    let message = e.message();
    let mut parts = message.split('"');
    let mut text = parts.next().unwrap_or_default().to_owned();
    for (i, part) in parts.enumerate() {
        text.push_str(if i % 2 == 0 { "\"…\"" } else { part });
    }
    match e.span() {
        Some(span) => {
            let before = content.get(..span.start).unwrap_or_default();
            format!("line {}: {text}", before.matches('\n').count() + 1)
        }
        None => text,
    }
}

/// Parses the `[engine]` table of a site file.
pub fn parse(text: &str) -> Result<Site, SiteError> {
    let file: File = toml::from_str(text).map_err(|e| SiteError::Toml(parse_error(text, &e)))?;
    file.engine.ok_or(SiteError::NoEngineTable)
}

/// `[guard] hil_tx` of a site file: HIL's card outputs (empty without it).
pub fn parse_hil_tx(text: &str) -> Result<Vec<u16>, SiteError> {
    let file: GuardFile =
        toml::from_str(text).map_err(|e| SiteError::Toml(parse_error(text, &e)))?;
    Ok(file.guard.hil_tx)
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
                mixes: vec!["m1".into()],
                peers: Vec::new()
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
