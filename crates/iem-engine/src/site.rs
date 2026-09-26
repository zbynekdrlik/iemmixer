//! The engine's part of `site.toml` (program spec I4, §3.1; design note §3.1):
//! the `[engine]` table. Everything else in the file belongs to the server and
//! is ignored here; inside `[engine]` unknown keys are errors.

use std::path::Path;

use iem_engine_proto::{BusKind, Tap};
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteBus {
    pub id: String,
    pub kind: BusKind,
    /// Card TX channels: output and master 2, translator 1, stems none.
    #[serde(default)]
    pub tx: Vec<u16>,
}

/// A family of sends: every `from` to every `to`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteSends {
    pub from: Vec<String>,
    pub to: Vec<String>,
    pub tap: Tap,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Site {
    /// The card's channel map: RX and TX channels are 1…=channels.
    pub channels: u16,
    /// The output bus with the fixed listen tap (X3 slot 0).
    pub engineer: String,
    pub inputs: Vec<SiteInput>,
    pub buses: Vec<SiteBus>,
    #[serde(default)]
    pub sends: Vec<SiteSends>,
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
    #[error("unknown id {0:?}")]
    UnknownId(String),
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
    #[error("second send from {from:?} to {to:?}")]
    DuplicateSend { from: String, to: String },
    #[error("{from:?}: inputs send pre, buses send post")]
    TapMismatch { from: String },
    #[error("{to:?} cannot receive sends")]
    BadDestination { to: String },
    #[error("{from:?} cannot send")]
    BadSource { from: String },
    #[error("the sends form a cycle through {0}")]
    Cycle(String),
    #[error("more than one master bus")]
    SecondMaster,
    #[error("more than one talkback input")]
    SecondTalkback,
    #[error("engineer {0:?} is not an output bus")]
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
            [[engine.buses]]
            id = "eng"
            kind = "output"
            tx = [1, 2]
            [[engine.sends]]
            from = ["mic"]
            to = ["eng"]
            tap = "pre"
            "#,
        )
        .unwrap();
        assert_eq!(site.channels, 8);
        assert_eq!(site.engineer, "eng");
        assert_eq!(
            site.inputs,
            vec![SiteInput {
                id: "mic".into(),
                rx: vec![1],
                talkback: true
            }]
        );
        assert_eq!(site.buses[0].kind, BusKind::Output);
        assert_eq!(site.sends[0].tap, Tap::Pre);
    }

    #[test]
    fn a_missing_table_and_unknown_keys_are_errors() {
        assert_eq!(parse("port = 1"), Err(SiteError::NoEngineTable));
        let unknown = parse(
            "[engine]\nchannels = 2\nengineer = \"e\"\ninputs = []\nbuses = []\ncolour = 1\n",
        );
        assert!(matches!(unknown, Err(SiteError::Toml(m)) if m.contains("colour")));
        assert!(matches!(parse("[engine"), Err(SiteError::Toml(_))));
        assert!(matches!(
            load(Path::new("/nonexistent/site.toml")),
            Err(SiteError::Io(m)) if m.contains("/nonexistent/site.toml")
        ));
    }
}
