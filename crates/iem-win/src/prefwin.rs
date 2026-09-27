//! The driver's preferred buffer holds 32 only while the driver opens (S6
//! design note §3): REAPER's value stays in the registry at every other moment,
//! so a crash or power loss in dev never leaves REAPER at 32. The driver reads
//! the value when it is opened (S1a). Kind (DWORD or text) is always kept.
//!
//! Portable and mutation-tested: the engine's backend opens and closes the
//! window, the guard's `PrefCheck` restores it (design §5.2 step 4), both
//! through a [`PrefStore`] (on the PC [`crate::registry::HkcuPref`]).

/// The kind of a registry value; a write names it, so a value read as a
/// DWORD is written back as a DWORD and a string as a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `REG_DWORD`; its text is decimal.
    Dword,
    /// `REG_SZ`.
    Text,
}

/// A registry value as read: its kind and its decimal text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pref {
    pub kind: Kind,
    pub raw: String,
}

/// Where the preference lives: one value, read and written whole.
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

impl std::fmt::Display for PrefError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(e) => write!(f, "reading the preferred buffer failed: {e}"),
            Self::Write(e) => write!(f, "writing the preferred buffer failed: {e}"),
            Self::NotOriginal { found } => write!(
                f,
                "the preferred buffer holds {found:?}, not the original (an earlier window did not close)"
            ),
            Self::ReadBack { wrote, read } => write!(
                f,
                "the preferred buffer read back {read:?} after {wrote:?} was written"
            ),
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

/// The guard's restore (design §5.2 step 4): up to `attempts` writes of the
/// original, each read back; Ok as soon as the store holds the original, with
/// the number of writes it took (0: it already did).
pub fn restore(
    store: &mut impl PrefStore,
    original: &Pref,
    attempts: u32,
) -> Result<u32, PrefError> {
    let mut last = PrefError::Read("no attempt".into());
    for n in 1..=attempts {
        // A failed read counts as "not the original": the write follows.
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

    fn dword(raw: &str) -> Pref {
        Pref {
            kind: Kind::Dword,
            raw: raw.to_string(),
        }
    }

    fn text(raw: &str) -> Pref {
        Pref {
            kind: Kind::Text,
            raw: raw.to_string(),
        }
    }

    /// A store holding one value. Reads and writes are counted from 1; the
    /// listed ones fail (a failed write changes nothing) or, for
    /// `corrupt_writes`, store the text with a `0` appended.
    #[derive(Debug)]
    struct FakeStore {
        value: Pref,
        reads: u32,
        writes: u32,
        bad_reads: Vec<u32>,
        bad_writes: Vec<u32>,
        corrupt_writes: Vec<u32>,
    }

    impl FakeStore {
        fn holding(value: Pref) -> Self {
            Self {
                value,
                reads: 0,
                writes: 0,
                bad_reads: Vec::new(),
                bad_writes: Vec::new(),
                corrupt_writes: Vec::new(),
            }
        }
    }

    impl PrefStore for FakeStore {
        fn read(&mut self) -> Result<Pref, String> {
            self.reads += 1;
            if self.bad_reads.contains(&self.reads) {
                return Err(format!("read {} failed", self.reads));
            }
            Ok(self.value.clone())
        }

        fn write(&mut self, value: &Pref) -> Result<(), String> {
            self.writes += 1;
            if self.bad_writes.contains(&self.writes) {
                return Err(format!("write {} failed", self.writes));
            }
            self.value = if self.corrupt_writes.contains(&self.writes) {
                Pref {
                    kind: value.kind,
                    raw: format!("{}0", value.raw),
                }
            } else {
                value.clone()
            };
            Ok(())
        }
    }

    #[test]
    fn enter_writes_the_frames_with_the_originals_kind() {
        let mut store = FakeStore::holding(dword("64"));
        assert_eq!(enter(&mut store, &dword("64"), 32), Ok(()));
        assert_eq!(store.value, dword("32"));
        assert_eq!((store.reads, store.writes), (2, 1));

        let mut store = FakeStore::holding(text("64"));
        assert_eq!(enter(&mut store, &text("64"), 32), Ok(()));
        assert_eq!(store.value, text("32"));
        assert_eq!((store.reads, store.writes), (2, 1));

        let mut store = FakeStore::holding(text("64"));
        assert_eq!(enter(&mut store, &text("64"), 48), Ok(()));
        assert_eq!(store.value, text("48"));
    }

    #[test]
    fn enter_refuses_without_writing_unless_the_store_holds_the_original() {
        let mut store = FakeStore::holding(dword("32"));
        assert_eq!(
            enter(&mut store, &dword("64"), 32),
            Err(PrefError::NotOriginal { found: dword("32") })
        );
        assert_eq!((store.value.clone(), store.writes), (dword("32"), 0));

        // The same digits with the other kind are not the original either.
        let mut store = FakeStore::holding(text("64"));
        assert_eq!(
            enter(&mut store, &dword("64"), 32),
            Err(PrefError::NotOriginal { found: text("64") })
        );
        assert_eq!((store.value.clone(), store.writes), (text("64"), 0));
    }

    #[test]
    fn a_write_that_reads_back_different_is_a_readback_error() {
        let mut store = FakeStore::holding(dword("64"));
        store.corrupt_writes = vec![1];
        assert_eq!(
            enter(&mut store, &dword("64"), 32),
            Err(PrefError::ReadBack {
                wrote: dword("32"),
                read: dword("320")
            })
        );

        let mut store = FakeStore::holding(dword("32"));
        store.corrupt_writes = vec![1];
        assert_eq!(
            leave(&mut store, &dword("64")),
            Err(PrefError::ReadBack {
                wrote: dword("64"),
                read: dword("640")
            })
        );
    }

    #[test]
    fn leave_restores_the_original_byte_for_byte() {
        let mut store = FakeStore::holding(text("32"));
        assert_eq!(leave(&mut store, &text(" 64")), Ok(()));
        assert_eq!(store.value, text(" 64"));
        assert_eq!((store.reads, store.writes), (1, 1));
    }

    #[test]
    fn store_errors_say_which_step_failed() {
        let mut store = FakeStore::holding(dword("64"));
        store.bad_writes = vec![1];
        assert_eq!(
            enter(&mut store, &dword("64"), 32),
            Err(PrefError::Write("write 1 failed".into()))
        );
        assert_eq!(store.value, dword("64"));

        let mut store = FakeStore::holding(dword("64"));
        store.bad_reads = vec![1];
        assert_eq!(
            enter(&mut store, &dword("64"), 32),
            Err(PrefError::Read("read 1 failed".into()))
        );
        assert_eq!(store.writes, 0);

        // The read-back after the write.
        let mut store = FakeStore::holding(dword("64"));
        store.bad_reads = vec![2];
        assert_eq!(
            enter(&mut store, &dword("64"), 32),
            Err(PrefError::Read("read 2 failed".into()))
        );
    }

    #[test]
    fn restore_writes_nothing_when_the_original_is_there() {
        let mut store = FakeStore::holding(dword("64"));
        assert_eq!(restore(&mut store, &dword("64"), 3), Ok(0));
        assert_eq!((store.reads, store.writes), (1, 0));
    }

    #[test]
    fn restore_counts_the_writes_it_took() {
        let mut store = FakeStore::holding(dword("32"));
        assert_eq!(restore(&mut store, &dword("64"), 3), Ok(1));
        assert_eq!((store.value.clone(), store.writes), (dword("64"), 1));

        let mut store = FakeStore::holding(dword("32"));
        store.bad_writes = vec![1];
        assert_eq!(restore(&mut store, &dword("64"), 3), Ok(2));
        assert_eq!((store.value.clone(), store.writes), (dword("64"), 2));

        // A read that fails before an attempt counts as "not the original".
        let mut store = FakeStore::holding(dword("64"));
        store.bad_reads = vec![1];
        assert_eq!(restore(&mut store, &dword("64"), 3), Ok(1));
        assert_eq!(store.writes, 1);
    }

    #[test]
    fn restore_gives_up_after_exactly_the_given_attempts() {
        let mut store = FakeStore::holding(dword("32"));
        store.bad_writes = vec![1, 2, 3, 4];
        assert_eq!(
            restore(&mut store, &dword("64"), 3),
            Err(PrefError::Write("write 3 failed".into()))
        );
        assert_eq!((store.value.clone(), store.writes), (dword("32"), 3));

        let mut store = FakeStore::holding(dword("32"));
        store.corrupt_writes = vec![1, 2];
        assert_eq!(
            restore(&mut store, &dword("64"), 2),
            Err(PrefError::ReadBack {
                wrote: dword("64"),
                read: dword("640")
            })
        );
        assert_eq!(store.writes, 2);

        let mut store = FakeStore::holding(dword("32"));
        assert_eq!(
            restore(&mut store, &dword("64"), 0),
            Err(PrefError::Read("no attempt".into()))
        );
        assert_eq!((store.reads, store.writes), (0, 0));
    }

    #[test]
    fn errors_read_as_sentences() {
        assert_eq!(
            PrefError::Read("gone".into()).to_string(),
            "reading the preferred buffer failed: gone"
        );
        assert_eq!(
            PrefError::Write("denied".into()).to_string(),
            "writing the preferred buffer failed: denied"
        );
        assert_eq!(
            PrefError::NotOriginal { found: dword("32") }.to_string(),
            "the preferred buffer holds Pref { kind: Dword, raw: \"32\" }, not the original \
             (an earlier window did not close)"
        );
        assert_eq!(
            PrefError::ReadBack {
                wrote: text("64"),
                read: text("640")
            }
            .to_string(),
            "the preferred buffer read back Pref { kind: Text, raw: \"640\" } after \
             Pref { kind: Text, raw: \"64\" } was written"
        );
    }
}
