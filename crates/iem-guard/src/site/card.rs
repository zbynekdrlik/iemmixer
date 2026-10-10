//! The parts of the engine's `[card]` table the guard reads (S6 plan Task
//! 5); its other keys are ignored here.

use iem_win::prefwin::{Kind, Pref};
use serde::Deserialize;

use super::{FRAMES, ends_with};

/// A registry value's kind as the site names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KindSite {
    Dword,
    #[serde(alias = "string", alias = "sz")]
    Text,
}

/// `[card] pref_original`: the value REAPER keeps (design §3).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PrefSite {
    pub kind: KindSite,
    pub raw: String,
}

impl PrefSite {
    pub fn pref(&self) -> Pref {
        Pref {
            kind: match self.kind {
                KindSite::Dword => Kind::Dword,
                KindSite::Text => Kind::Text,
            },
            raw: self.raw.clone(),
        }
    }
}

/// The parts of the engine's `[card]` the guard reads.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CardSite {
    /// The driver's DLL: its holders are read before REAPER or the engine
    /// may open the card (I3).
    pub module: String,
    pub frames: u32,
    /// `HKEY_CURRENT_USER` key and value name of the preferred buffer.
    pub pref_key: String,
    pub pref_name: String,
    pub pref_original: PrefSite,
}

impl CardSite {
    pub fn problems(&self) -> Vec<String> {
        let mut bad = Vec::new();
        let mut check = |ok: bool, why: &str| {
            if !ok {
                bad.push(format!("[card] {why}"));
            }
        };
        check(ends_with(&self.module, ".dll"), "module must be a .dll");
        check(self.frames == FRAMES, "frames must be 32 (I2)");
        check(!self.pref_key.is_empty(), "pref_key is empty");
        check(!self.pref_name.is_empty(), "pref_name is empty");
        let raw = &self.pref_original.raw;
        check(
            self.pref_original.kind == KindSite::Text
                || (!raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit())),
            "pref_original of kind dword must be decimal digits",
        );
        bad
    }
}
