//! Stable ids (program spec I4, I6): inputs and buses share one namespace in
//! the site file; a send is identified by its (source, destination) pair,
//! of which a site has at most one.

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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InputId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BusId(pub String);

impl InputId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

impl BusId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

impl fmt::Display for InputId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for BusId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The source of a send: an input (pre-fader tap) or a bus (post-fader tap).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Input(InputId),
    Bus(BusId),
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(id) => write!(f, "{id}"),
            Self::Bus(id) => write!(f, "{id}"),
        }
    }
}

/// A send: at most one per (source, destination) pair (I4).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SendId {
    pub src: Source,
    pub dst: BusId,
}

impl fmt::Display for SendId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}>{}", self.src, self.dst)
    }
}

/// Whose EQ a command addresses.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EqOwner {
    Input(InputId),
    Bus(BusId),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_validated() {
        let longest = "a".repeat(64);
        let too_long = "a".repeat(65);
        for ok in [
            "member1.stems",
            "mic1",
            "0",
            "eng_mic",
            "a-b",
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
        let send = SendId {
            src: Source::Input(InputId::new("mic1")),
            dst: BusId::new("member1"),
        };
        assert_eq!(
            serde_json::to_string(&send).unwrap(),
            r#"{"src":{"input":"mic1"},"dst":"member1"}"#
        );
        assert_eq!(send.to_string(), "mic1>member1");
        let bus = Source::Bus(BusId::new("member2"));
        assert_eq!(serde_json::to_string(&bus).unwrap(), r#"{"bus":"member2"}"#);
        assert_eq!(bus.to_string(), "member2");
        assert_eq!(
            serde_json::to_string(&EqOwner::Bus(BusId::new("engineer"))).unwrap(),
            r#"{"bus":"engineer"}"#
        );
        assert!(Source::Input(InputId::new("z")) < Source::Bus(BusId::new("a")));
    }
}
