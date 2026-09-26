//! Stable ids (program spec I4, I6; #20 design note §5): inputs, groups and
//! mixes share one namespace in the site file.

use core::fmt;

use serde::{Deserialize, Serialize};

/// Longest id in bytes; the engine refuses longer ones from any sender (§2.3).
pub const MAX_ID_LEN: usize = 64;

/// An id is 1–64 bytes of `a-z`, `0-9`, `_`, `.`, `-`, starting with a letter or digit.
pub fn valid_id(s: &str) -> bool {
    let bytes = s.as_bytes();
    let first_ok = bytes
        .first()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    first_ok
        && bytes.len() <= MAX_ID_LEN
        && bytes.iter().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'.' | b'-')
        })
}

/// An input: a channel strip on one or two of the card's RX channels.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InputId(pub String);

/// A group of inputs (the stems): every mix has one strip for it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GroupId(pub String);

/// A mix: one listener's in-ear mix or feed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MixId(pub String);

impl InputId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

impl GroupId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

impl MixId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

impl fmt::Display for InputId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for GroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for MixId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a level in a mix reads: an input, or another mix the mix hears (the
/// Mixes tab, F16).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Input(InputId),
    Mix(MixId),
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(id) => write!(f, "{id}"),
            Self::Mix(id) => write!(f, "{id}"),
        }
    }
}

/// Whose EQ a command addresses (F11): an input's, a mix's output, or a
/// group's strip in one mix.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EqTarget {
    Input(InputId),
    Mix(MixId),
    Group { mix: MixId, group: GroupId },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_validated() {
        let longest = "a".repeat(64);
        let too_long = "a".repeat(65);
        for ok in [
            "member1",
            "mic1",
            "0",
            "eng_mic",
            "a-b",
            "a.b",
            longest.as_str(),
        ] {
            assert!(valid_id(ok), "{ok}");
        }
        for bad in [
            "",
            "A",
            "-x",
            "_x",
            ".x",
            "a b",
            "Mic1",
            "é",
            too_long.as_str(),
        ] {
            assert!(!valid_id(bad), "{bad}");
        }
    }

    #[test]
    fn ids_serialise_as_plain_strings_and_display() {
        assert_eq!(
            serde_json::to_string(&Source::Input(InputId::new("mic1"))).unwrap(),
            r#"{"input":"mic1"}"#
        );
        let heard = Source::Mix(MixId::new("member2"));
        assert_eq!(
            serde_json::to_string(&heard).unwrap(),
            r#"{"mix":"member2"}"#
        );
        assert_eq!(heard.to_string(), "member2");
        assert_eq!(Source::Input(InputId::new("keys")).to_string(), "keys");
        assert_eq!(GroupId::new("stems").to_string(), "stems");
        assert_eq!(MixId::new("engineer").to_string(), "engineer");
        assert_eq!(InputId::new("mic2").to_string(), "mic2");
        assert_eq!(
            serde_json::to_string(&EqTarget::Group {
                mix: MixId::new("member1"),
                group: GroupId::new("stems")
            })
            .unwrap(),
            r#"{"group":{"mix":"member1","group":"stems"}}"#
        );
        assert_eq!(
            serde_json::to_string(&EqTarget::Mix(MixId::new("engineer"))).unwrap(),
            r#"{"mix":"engineer"}"#
        );
        assert!(Source::Input(InputId::new("z")) < Source::Mix(MixId::new("a")));
        assert_eq!(
            serde_json::to_string(&GroupId::new("stems")).unwrap(),
            r#""stems""#
        );
    }
}
