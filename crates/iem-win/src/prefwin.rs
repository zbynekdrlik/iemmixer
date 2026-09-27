//! The driver's preferred buffer holds 32 only while the driver opens (S6
//! design note §3): REAPER's value stays in the registry at every other moment,
//! so a crash or power loss in dev never leaves REAPER at 32. The driver reads
//! the value when it is opened (S1a). Kind (DWORD or text) is always kept.
//!
//! Portable and mutation-tested: the engine's ASIO backend opens and closes
//! the window, the guard's preference check restores the original before
//! REAPER starts. Both use this module, so the guard never links the ASIO
//! host. On the PC the store is the registry ([`crate::registry`]).

use core::fmt;

/// The kind of the registry value (the one [`crate::registry::Hkcu`] reads
/// and writes).
pub use crate::registry::Kind;

/// A registry value as read: its kind and its decimal text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pref {
    pub kind: Kind,
    pub raw: String,
}

impl fmt::Display for Pref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            Kind::Dword => "DWORD",
            Kind::Text => "text",
        };
        write!(f, "{kind} {:?}", self.raw)
    }
}

/// Where the preferred buffer lives (the registry on the PC, a fake in tests).
pub trait PrefStore {
    fn read(&mut self) -> Result<Pref, String>;
    fn write(&mut self, value: &Pref) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefError {
    Read(String),
    Write(String),
    /// The store does not hold the original: an earlier window did not close.
    NotOriginal {
        found: Pref,
    },
    ReadBack {
        wrote: Pref,
        read: Pref,
    },
}

impl fmt::Display for PrefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(e) => write!(f, "preferred buffer: read failed: {e}"),
            Self::Write(e) => write!(f, "preferred buffer: write failed: {e}"),
            Self::NotOriginal { found } => write!(
                f,
                "preferred buffer holds {found}, not the recorded original"
            ),
            Self::ReadBack { wrote, read } => {
                write!(f, "preferred buffer: wrote {wrote}, read back {read}")
            }
        }
    }
}

impl std::error::Error for PrefError {}

fn write_checked(store: &mut impl PrefStore, want: &Pref) -> Result<(), PrefError> {
    store.write(want).map_err(PrefError::Write)?;
    let got = store.read().map_err(PrefError::Read)?;
    if got == *want {
        Ok(())
    } else {
        Err(PrefError::ReadBack {
            wrote: want.clone(),
            read: got,
        })
    }
}

/// Opens the window: refuses unless the store holds `original`, then writes
/// `frames` with the original's kind and reads it back.
pub fn enter(store: &mut impl PrefStore, original: &Pref, frames: u32) -> Result<(), PrefError> {
    let now = store.read().map_err(PrefError::Read)?;
    if now != *original {
        return Err(PrefError::NotOriginal { found: now });
    }
    write_checked(
        store,
        &Pref {
            kind: original.kind,
            raw: frames.to_string(),
        },
    )
}

/// Closes the window: the original back, read back.
pub fn leave(store: &mut impl PrefStore, original: &Pref) -> Result<(), PrefError> {
    write_checked(store, original)
}

/// The guard's restore (design note §5.2 step 4): up to `attempts` writes of
/// the original, each read back; Ok as soon as the store holds the original,
/// with the number of writes it took (0 when it already held it). After the
/// last failed attempt, that attempt's error.
pub fn restore(
    store: &mut impl PrefStore,
    original: &Pref,
    attempts: u32,
) -> Result<u32, PrefError> {
    let mut last = PrefError::Read("no attempt".into());
    for n in 1..=attempts {
        if store.read().is_ok_and(|now| now == *original) {
            return Ok(n - 1);
        }
        match leave(store, original) {
            Ok(()) => return Ok(n),
            Err(e) => last = e,
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pref(kind: Kind, raw: &str) -> Pref {
        Pref {
            kind,
            raw: raw.into(),
        }
    }

    /// Holds one value; the n-th read (from 1) can fail, the writes numbered
    /// in `fail_writes` fail, and the n-th write can store "48" instead of
    /// what it was given.
    struct FakeStore {
        value: Pref,
        reads: u32,
        writes: u32,
        fail_read: Option<u32>,
        fail_writes: Vec<u32>,
        corrupt_write: Option<u32>,
    }

    impl FakeStore {
        fn holding(value: Pref) -> Self {
            Self {
                value,
                reads: 0,
                writes: 0,
                fail_read: None,
                fail_writes: Vec::new(),
                corrupt_write: None,
            }
        }
    }

    impl PrefStore for FakeStore {
        fn read(&mut self) -> Result<Pref, String> {
            self.reads += 1;
            if self.fail_read == Some(self.reads) {
                return Err("key not found".into());
            }
            Ok(self.value.clone())
        }

        fn write(&mut self, value: &Pref) -> Result<(), String> {
            self.writes += 1;
            if self.fail_writes.contains(&self.writes) {
                return Err("access denied".into());
            }
            self.value = if self.corrupt_write == Some(self.writes) {
                pref(value.kind, "48")
            } else {
                value.clone()
            };
            Ok(())
        }
    }

    #[test]
    fn enter_writes_the_frames_with_the_originals_kind() {
        for kind in [Kind::Dword, Kind::Text] {
            let original = pref(kind, "64");
            let mut s = FakeStore::holding(original.clone());
            assert_eq!(enter(&mut s, &original, 32), Ok(()));
            assert_eq!(s.value, pref(kind, "32"));
            // One check before the write, one read-back after it.
            assert_eq!((s.reads, s.writes), (2, 1));
        }
    }

    #[test]
    fn enter_refuses_unless_the_store_holds_the_original() {
        let original = pref(Kind::Dword, "64");
        for held in [pref(Kind::Dword, "32"), pref(Kind::Text, "64")] {
            let mut s = FakeStore::holding(held.clone());
            assert_eq!(
                enter(&mut s, &original, 32),
                Err(PrefError::NotOriginal {
                    found: held.clone()
                })
            );
            assert_eq!((s.value, s.writes), (held, 0));
        }
    }

    #[test]
    fn a_value_that_does_not_read_back_is_an_error() {
        let original = pref(Kind::Dword, "64");
        let mut s = FakeStore::holding(original.clone());
        s.corrupt_write = Some(1);
        assert_eq!(
            enter(&mut s, &original, 32),
            Err(PrefError::ReadBack {
                wrote: pref(Kind::Dword, "32"),
                read: pref(Kind::Dword, "48"),
            })
        );
        let mut s = FakeStore::holding(pref(Kind::Dword, "32"));
        s.corrupt_write = Some(1);
        assert_eq!(
            leave(&mut s, &original),
            Err(PrefError::ReadBack {
                wrote: original.clone(),
                read: pref(Kind::Dword, "48"),
            })
        );
    }

    #[test]
    fn leave_restores_the_original_byte_for_byte() {
        let original = pref(Kind::Text, " 64");
        let mut s = FakeStore::holding(original.clone());
        assert_eq!(enter(&mut s, &original, 32), Ok(()));
        assert_eq!(s.value, pref(Kind::Text, "32"));
        assert_eq!(leave(&mut s, &original), Ok(()));
        assert_eq!(s.value, original);
        assert_eq!((s.reads, s.writes), (3, 2));
        // Leaving again (every exit of an open leaves) is harmless.
        assert_eq!(leave(&mut s, &original), Ok(()));
        assert_eq!(s.value, original);
    }

    #[test]
    fn store_errors_are_reported() {
        let original = pref(Kind::Dword, "64");
        let mut s = FakeStore::holding(original.clone());
        s.fail_writes = vec![1];
        assert_eq!(
            enter(&mut s, &original, 32),
            Err(PrefError::Write("access denied".into()))
        );
        assert_eq!(s.value, original);
        let mut s = FakeStore::holding(pref(Kind::Dword, "32"));
        s.fail_writes = vec![1];
        assert_eq!(
            leave(&mut s, &original),
            Err(PrefError::Write("access denied".into()))
        );
        // The check before the write, then the read-back after it.
        let mut s = FakeStore::holding(original.clone());
        s.fail_read = Some(1);
        assert_eq!(
            enter(&mut s, &original, 32),
            Err(PrefError::Read("key not found".into()))
        );
        assert_eq!(s.writes, 0);
        let mut s = FakeStore::holding(original.clone());
        s.fail_read = Some(2);
        assert_eq!(
            enter(&mut s, &original, 32),
            Err(PrefError::Read("key not found".into()))
        );
        assert_eq!(s.writes, 1);
    }

    #[test]
    fn restore_writes_nothing_when_the_original_is_there() {
        let original = pref(Kind::Dword, "64");
        let mut s = FakeStore::holding(original.clone());
        assert_eq!(restore(&mut s, &original, 3), Ok(0));
        assert_eq!((s.value, s.reads, s.writes), (original, 1, 0));
    }

    #[test]
    fn restore_tries_again_after_a_failed_write() {
        let original = pref(Kind::Dword, "64");
        let mut s = FakeStore::holding(pref(Kind::Dword, "32"));
        s.fail_writes = vec![1];
        assert_eq!(restore(&mut s, &original, 3), Ok(2));
        assert_eq!((s.value, s.writes), (original.clone(), 2));
        // A write whose read-back failed still counts: the next attempt finds
        // the original (the same digits as text were not it) and writes no more.
        let mut s = FakeStore::holding(pref(Kind::Text, "64"));
        s.fail_read = Some(2);
        assert_eq!(restore(&mut s, &original, 3), Ok(1));
        assert_eq!((s.value, s.reads, s.writes), (original, 3, 1));
    }

    #[test]
    fn restore_gives_up_after_exactly_the_attempts() {
        let original = pref(Kind::Dword, "64");
        let mut s = FakeStore::holding(pref(Kind::Dword, "32"));
        s.fail_writes = vec![1, 2, 3, 4];
        assert_eq!(
            restore(&mut s, &original, 3),
            Err(PrefError::Write("access denied".into()))
        );
        assert_eq!((s.value, s.writes), (pref(Kind::Dword, "32"), 3));
        // The last attempt's error comes back: here a value that does not
        // read back, after two failed writes.
        let mut s = FakeStore::holding(pref(Kind::Dword, "32"));
        s.fail_writes = vec![1, 2];
        s.corrupt_write = Some(3);
        assert_eq!(
            restore(&mut s, &original, 3),
            Err(PrefError::ReadBack {
                wrote: original.clone(),
                read: pref(Kind::Dword, "48"),
            })
        );
        assert_eq!(s.writes, 3);
        // No attempt: nothing is read or written.
        let mut s = FakeStore::holding(pref(Kind::Dword, "32"));
        assert_eq!(
            restore(&mut s, &original, 0),
            Err(PrefError::Read("no attempt".into()))
        );
        assert_eq!((s.reads, s.writes), (0, 0));
    }

    #[test]
    fn errors_name_the_values() {
        let d = pref(Kind::Dword, "32");
        let t = pref(Kind::Text, " 64");
        assert_eq!(d.to_string(), "DWORD \"32\"");
        assert_eq!(t.to_string(), "text \" 64\"");
        assert_eq!(
            PrefError::Read("key not found".into()).to_string(),
            "preferred buffer: read failed: key not found"
        );
        assert_eq!(
            PrefError::Write("access denied".into()).to_string(),
            "preferred buffer: write failed: access denied"
        );
        assert_eq!(
            PrefError::NotOriginal { found: d.clone() }.to_string(),
            "preferred buffer holds DWORD \"32\", not the recorded original"
        );
        assert_eq!(
            PrefError::ReadBack { wrote: t, read: d }.to_string(),
            "preferred buffer: wrote text \" 64\", read back DWORD \"32\""
        );
    }
}
