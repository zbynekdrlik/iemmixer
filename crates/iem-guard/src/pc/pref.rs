//! What `PrefCheck` found (design §5.2; #9 2026-09-28): REAPER's original,
//! or a value it did not write under the driver module's holders.

use iem_win::prefwin::Checked;

use crate::effects::app::holders_text;

/// `PrefCheck`'s restore: up to this many writes, each read back.
pub const PREF_ATTEMPTS: u32 = 3;

/// Who holds the driver module when `PrefCheck` finds something other than
/// REAPER's original (#9 2026-09-28).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardHolders {
    /// REAPER is one of them.
    pub reaper: bool,
    /// Every holder: image and pid.
    pub names: String,
}

/// The holders `PrefCheck` must not write under; `None` while nothing holds
/// the driver module (the restore may write). Anything counts, our engine
/// too. An unreadable list assumes that a running REAPER holds it (as
/// [`facts_from`](super::facts_from) does), so a failed read never writes under a REAPER that
/// may hold the card.
pub fn card_holders(holders: Option<&[(u32, String)]>, reaper: &[u32]) -> Option<CardHolders> {
    let assumed: Vec<(u32, String)> = match holders {
        Some(_) => Vec::new(),
        None => reaper
            .iter()
            .map(|pid| (*pid, "REAPER".to_owned()))
            .collect(),
    };
    let list = holders.unwrap_or(&assumed);
    (!list.is_empty()).then(|| CardHolders {
        reaper: list.iter().any(|(pid, _)| reaper.contains(pid)),
        names: holders_text(list),
    })
}

/// A preference that is not REAPER's original while the driver module is
/// held: `PrefCheck` wrote nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefHeld {
    /// What the preference reads (`None`: unreadable).
    pub value: Option<String>,
    pub by: CardHolders,
}

impl PrefHeld {
    /// The alarm, the report line and the status line.
    pub fn text(&self) -> String {
        let at = self
            .value
            .as_ref()
            .map_or_else(|| "unreadable".to_owned(), |v| format!("at {v}"));
        if self.by.reaper {
            format!(
                "REAPER runs with the preferred buffer {at}; it is restored at REAPER's next start"
            )
        } else {
            format!(
                "the driver module is held by {} with the preferred buffer {at}; nothing was \
                 written",
                self.by.names
            )
        }
    }
}

/// What `PrefCheck` found (design §5.2; #9 2026-09-28).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefSeen {
    /// REAPER's original is there, after this many writes (each read back;
    /// 0: it already was).
    Original(u32),
    /// Not the original while the driver module is held: nothing written.
    Held(PrefHeld),
}

impl From<Checked<CardHolders>> for PrefSeen {
    fn from(c: Checked<CardHolders>) -> Self {
        match c {
            Checked::Original(writes) => Self::Original(writes),
            Checked::Open { found, by } => Self::Held(PrefHeld {
                value: found.map(|p| p.raw),
                by,
            }),
        }
    }
}
