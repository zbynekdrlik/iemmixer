//! The driver's preferred buffer holds the engine's frames (32) for as long
//! as the engine holds the card, and REAPER's original at every other moment
//! (S6 design note §3, #9 2026-09-28). The driver reads the value when it is
//! opened (S1a). The engine's first run on the PC faulted on its reopen budget
//! ~110 ms after the open while the original was written back right after
//! `createBuffers`: most likely the driver asks for a reset when the value
//! changes while it is open. So a [`Window`] writes the frames once, before
//! the first open, keeps them through every reopen without writing, and
//! writes the original back when the engine releases the card. Kind (DWORD or
//! text) is always kept.
//!
//! Portable and mutation-tested: the engine's backend holds a [`Window`];
//! the guard's `PrefCheck` [`restore`]s the original before REAPER starts
//! (design §5.2 step 4), also after an engine that ended while it held the
//! card (a crash, a power loss); both through a [`PrefStore`] (on the PC
//! [`crate::registry::HkcuPref`]).

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
    /// A first open found no original in the store: an engine ended while
    /// it held the card (a crash, a power loss), or another program wrote it.
    NotOriginal {
        found: Pref,
    },
    /// A reopen found the store without the frames this window wrote:
    /// another program wrote it while the card was held.
    NotHeld {
        found: Pref,
        held: Pref,
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
                "the preferred buffer holds {found:?}, not the original \
                 (an engine ended while it held the card, or another program wrote it)"
            ),
            Self::NotHeld { found, held } => write!(
                f,
                "the preferred buffer holds {found:?}, not {held:?}, which this engine wrote \
                 for the card it holds (another program wrote it)"
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

/// Where a [`Window`] stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// The store holds REAPER's original as far as the window knows: nothing
    /// was written yet, or its restore read back.
    Original,
    /// The window wrote its frames: the card is held (or opening) at them,
    /// and the release owes the original. Also after a write that failed or
    /// read back wrong, so the release restores whatever that write left.
    Held,
}

/// What [`Window::enter`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entered {
    /// Original → Held: the frames written and read back (the first open).
    Wrote,
    /// Held → Held: the store still holds the frames, read and not written
    /// again (a reopen).
    Kept,
}

/// What [`Window::leave`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Left {
    /// Held → Original: the original written and read back.
    Restored,
    /// The window held nothing: no read, no write.
    Untouched,
}

/// The preference window of one engine: [`Window::enter`] before every open
/// of the card, [`Window::leave`] at every release that no open follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    original: Pref,
    held: Pref,
    state: State,
}

impl Window {
    /// A window for REAPER's `original`, not yet held; it holds `frames` in
    /// the original's kind.
    pub fn new(original: Pref, frames: u32) -> Self {
        let held = Pref {
            kind: original.kind,
            raw: frames.to_string(),
        };
        Self {
            original,
            held,
            state: State::Original,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// REAPER's value.
    pub fn original(&self) -> &Pref {
        &self.original
    }

    /// The engine's frames in the original's kind.
    pub fn held(&self) -> &Pref {
        &self.held
    }

    /// Before an open. Original → Held: the store must hold the original;
    /// the frames are written and read back. Held → Held (a reopen): the
    /// store must still hold the frames, and nothing is written, since the
    /// driver may ask for a reset when the value changes while it is open.
    /// Anything else refuses without a write.
    pub fn enter(&mut self, store: &mut impl PrefStore) -> Result<Entered, PrefError> {
        let now = store.read().map_err(PrefError::Read)?;
        match self.state {
            State::Held if now == self.held => Ok(Entered::Kept),
            State::Held => Err(PrefError::NotHeld {
                found: now,
                held: self.held.clone(),
            }),
            State::Original if now == self.original => {
                // From the write on the release owes the original, also
                // when the write or its read-back fails.
                self.state = State::Held;
                write_checked(store, &self.held).map(|()| Entered::Wrote)
            }
            State::Original => Err(PrefError::NotOriginal { found: now }),
        }
    }

    /// At a release that no open follows. Held → Original: the original is
    /// written and read back; after a failure the window stays Held, so the
    /// next release writes again. Original: nothing to do, nothing touched.
    pub fn leave(&mut self, store: &mut impl PrefStore) -> Result<Left, PrefError> {
        if self.state == State::Original {
            return Ok(Left::Untouched);
        }
        write_checked(store, &self.original)?;
        self.state = State::Original;
        Ok(Left::Restored)
    }
}

/// The guard's restore (design §5.2 step 4): up to `attempts` writes of the
/// original, each read back; Ok as soon as the store holds the original, with
/// the number of writes it took (0: it already did). Whatever the store holds
/// otherwise is overwritten: an engine's frames left by a crash or a power
/// loss while it held the card are a restore, never a refusal.
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
        match write_checked(store, original) {
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

    /// REAPER's DWORD 64 at 32 frames, and a store holding it.
    fn at_64() -> (Window, FakeStore) {
        (
            Window::new(dword("64"), 32),
            FakeStore::holding(dword("64")),
        )
    }

    #[test]
    fn a_new_window_is_original_and_holds_the_frames_in_the_originals_kind() {
        let w = Window::new(dword("64"), 32);
        assert_eq!(w.state(), State::Original);
        assert_eq!(w.original(), &dword("64"));
        assert_eq!(w.held(), &dword("32"));

        let w = Window::new(text(" 64"), 48);
        assert_eq!(w.state(), State::Original);
        assert_eq!(w.original(), &text(" 64"));
        assert_eq!(w.held(), &text("48"));
    }

    /// Original → Held (#9 2026-09-28): the first open writes the frames with
    /// the original's kind and reads them back.
    #[test]
    fn the_first_enter_writes_the_frames_and_holds_them() {
        let (mut w, mut store) = at_64();
        assert_eq!(w.enter(&mut store), Ok(Entered::Wrote));
        assert_eq!(store.value, dword("32"));
        assert_eq!((store.reads, store.writes), (2, 1));
        assert_eq!(w.state(), State::Held);

        let mut w = Window::new(text("64"), 32);
        let mut store = FakeStore::holding(text("64"));
        assert_eq!(w.enter(&mut store), Ok(Entered::Wrote));
        assert_eq!(store.value, text("32"));
        assert_eq!(w.state(), State::Held);
    }

    /// Held → Held: a reopen reads the frames and writes nothing (the driver
    /// may ask for a reset when the value changes while it is open).
    #[test]
    fn a_reopen_keeps_the_frames_without_writing() {
        let (mut w, mut store) = at_64();
        assert_eq!(w.enter(&mut store), Ok(Entered::Wrote));
        for reads in [3, 4] {
            assert_eq!(w.enter(&mut store), Ok(Entered::Kept));
            assert_eq!((store.reads, store.writes), (reads, 1));
            assert_eq!(store.value, dword("32"));
            assert_eq!(w.state(), State::Held);
        }
    }

    /// The first open refuses anything but the original, without a write: an
    /// engine that ended while it held the card (a crash, a power loss) left
    /// its frames, or another program wrote the value. The window stays
    /// Original, so its release touches nothing.
    #[test]
    fn the_first_enter_refuses_anything_but_the_original_without_writing() {
        // The same digits with the other kind are not the original either.
        for found in [dword("32"), text("64"), dword("128")] {
            let mut w = Window::new(dword("64"), 32);
            let mut store = FakeStore::holding(found.clone());
            assert_eq!(
                w.enter(&mut store),
                Err(PrefError::NotOriginal {
                    found: found.clone()
                })
            );
            assert_eq!((store.value.clone(), store.writes), (found, 0));
            assert_eq!(w.state(), State::Original);
            assert_eq!(w.leave(&mut store), Ok(Left::Untouched));
            assert_eq!((store.reads, store.writes), (1, 0));
        }
    }

    /// A reopen refuses anything but its own frames, without a write: another
    /// program wrote the value while the card was held (REAPER's original
    /// included). The window stays Held, so the release still writes the
    /// original.
    #[test]
    fn a_reopen_refuses_anything_but_the_frames_without_writing() {
        for found in [dword("64"), text("32"), dword("48")] {
            let (mut w, mut store) = at_64();
            assert_eq!(w.enter(&mut store), Ok(Entered::Wrote));
            store.value = found.clone();
            assert_eq!(
                w.enter(&mut store),
                Err(PrefError::NotHeld {
                    found: found.clone(),
                    held: dword("32")
                })
            );
            assert_eq!((store.value.clone(), store.writes), (found, 1));
            assert_eq!(w.state(), State::Held);
            assert_eq!(w.leave(&mut store), Ok(Left::Restored));
            assert_eq!(store.value, dword("64"));
        }
    }

    /// Held → Original at the release: the original back byte for byte and
    /// read back; a second release has nothing to do and touches nothing.
    #[test]
    fn the_release_restores_the_original_once() {
        let mut w = Window::new(text(" 64"), 32);
        let mut store = FakeStore::holding(text(" 64"));
        assert_eq!(w.enter(&mut store), Ok(Entered::Wrote));
        assert_eq!(w.leave(&mut store), Ok(Left::Restored));
        assert_eq!(store.value, text(" 64"));
        assert_eq!((store.reads, store.writes), (3, 2));
        assert_eq!(w.state(), State::Original);
        assert_eq!(w.leave(&mut store), Ok(Left::Untouched));
        assert_eq!((store.reads, store.writes), (3, 2));
    }

    #[test]
    fn a_window_that_never_held_the_card_touches_nothing_at_its_release() {
        let (mut w, mut store) = at_64();
        store.bad_reads = vec![1];
        store.bad_writes = vec![1];
        assert_eq!(w.leave(&mut store), Ok(Left::Untouched));
        assert_eq!((store.reads, store.writes), (0, 0));
        assert_eq!(w.state(), State::Original);
    }

    /// The state machine has no dead end: after its release a window opens
    /// again like a new one.
    #[test]
    fn a_released_window_opens_again() {
        let (mut w, mut store) = at_64();
        assert_eq!(w.enter(&mut store), Ok(Entered::Wrote));
        assert_eq!(w.leave(&mut store), Ok(Left::Restored));
        assert_eq!(w.enter(&mut store), Ok(Entered::Wrote));
        assert_eq!((store.value.clone(), store.writes), (dword("32"), 3));
        assert_eq!(w.state(), State::Held);
    }

    /// A write that failed or read back wrong may have left anything: the
    /// window is Held from the write on, so the release restores the
    /// original.
    #[test]
    fn a_failed_first_write_still_owes_the_original() {
        let (mut w, mut store) = at_64();
        store.bad_writes = vec![1];
        assert_eq!(
            w.enter(&mut store),
            Err(PrefError::Write("write 1 failed".into()))
        );
        assert_eq!(store.value, dword("64"));
        assert_eq!(w.state(), State::Held);
        assert_eq!(w.leave(&mut store), Ok(Left::Restored));
        assert_eq!((store.value.clone(), store.writes), (dword("64"), 2));
        assert_eq!(w.state(), State::Original);

        let (mut w, mut store) = at_64();
        store.corrupt_writes = vec![1];
        assert_eq!(
            w.enter(&mut store),
            Err(PrefError::ReadBack {
                wrote: dword("32"),
                read: dword("320")
            })
        );
        assert_eq!(w.state(), State::Held);
        assert_eq!(w.leave(&mut store), Ok(Left::Restored));
        assert_eq!(store.value, dword("64"));

        // The read-back after the write failed.
        let (mut w, mut store) = at_64();
        store.bad_reads = vec![2];
        assert_eq!(
            w.enter(&mut store),
            Err(PrefError::Read("read 2 failed".into()))
        );
        assert_eq!(w.state(), State::Held);
    }

    /// A read that fails before anything is written changes nothing.
    #[test]
    fn a_failed_first_read_changes_nothing() {
        let (mut w, mut store) = at_64();
        store.bad_reads = vec![1];
        assert_eq!(
            w.enter(&mut store),
            Err(PrefError::Read("read 1 failed".into()))
        );
        assert_eq!(store.writes, 0);
        assert_eq!(w.state(), State::Original);

        // At a reopen: still Held, nothing written.
        let (mut w, mut store) = at_64();
        assert_eq!(w.enter(&mut store), Ok(Entered::Wrote));
        store.bad_reads = vec![3];
        assert_eq!(
            w.enter(&mut store),
            Err(PrefError::Read("read 3 failed".into()))
        );
        assert_eq!((store.value.clone(), store.writes), (dword("32"), 1));
        assert_eq!(w.state(), State::Held);
    }

    /// A release that failed keeps the window Held: the next one writes
    /// again.
    #[test]
    fn a_failed_release_keeps_the_window_held() {
        let (mut w, mut store) = at_64();
        assert_eq!(w.enter(&mut store), Ok(Entered::Wrote));
        store.bad_writes = vec![2];
        assert_eq!(
            w.leave(&mut store),
            Err(PrefError::Write("write 2 failed".into()))
        );
        assert_eq!((store.value.clone(), w.state()), (dword("32"), State::Held));
        store.corrupt_writes = vec![3];
        assert_eq!(
            w.leave(&mut store),
            Err(PrefError::ReadBack {
                wrote: dword("64"),
                read: dword("640")
            })
        );
        assert_eq!(w.state(), State::Held);
        assert_eq!(w.leave(&mut store), Ok(Left::Restored));
        assert_eq!((store.value.clone(), store.writes), (dword("64"), 4));
        assert_eq!(w.state(), State::Original);
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

    /// An engine that ended while it held the card (a crash, a power loss)
    /// left its frames (#9 2026-09-28): the next engine's first open refuses
    /// them, and the guard's `PrefCheck` before REAPER starts restores the
    /// original — a restore, never a refusal. After it the next open works.
    #[test]
    fn restore_takes_back_the_frames_of_an_engine_that_ended_holding_the_card() {
        let (mut ended, mut store) = at_64();
        assert_eq!(ended.enter(&mut store), Ok(Entered::Wrote));
        // The engine ends here, without its release.
        let mut next = Window::new(dword("64"), 32);
        assert_eq!(
            next.enter(&mut store),
            Err(PrefError::NotOriginal { found: dword("32") })
        );
        assert_eq!(restore(&mut store, ended.original(), 3), Ok(1));
        assert_eq!(store.value, dword("64"));
        assert_eq!(next.enter(&mut store), Ok(Entered::Wrote));
    }

    /// The guard's `PrefCheck` (#9 2026-09-28): the original is there → no
    /// write, and nobody is asked who holds the driver.
    #[test]
    fn check_writes_nothing_and_asks_nobody_when_the_original_is_there() {
        for original in [dword("64"), text(" 64")] {
            let mut store = FakeStore::holding(original.clone());
            let mut asked = false;
            let got = check(&mut store, &original, 3, || {
                asked = true;
                Some("reaper.exe (11)")
            });
            assert_eq!(got, Ok(Checked::Original(0)));
            assert!(!asked, "{original:?}");
            assert_eq!((store.reads, store.writes), (1, 0));
        }
    }

    /// Not the original and no process holds the driver's module: restored
    /// with read-back, as `restore` does.
    #[test]
    fn check_restores_while_nothing_holds_the_driver() {
        let mut store = FakeStore::holding(dword("32"));
        let mut asked = 0;
        let got = check(&mut store, &dword("64"), 3, || {
            asked += 1;
            None::<&str>
        });
        assert_eq!(got, Ok(Checked::Original(1)));
        assert_eq!(asked, 1);
        assert_eq!((store.value.clone(), store.writes), (dword("64"), 1));

        // The same digits in the other kind are not the original either.
        let mut store = FakeStore::holding(text("64"));
        let got = check(&mut store, &dword("64"), 3, || None::<&str>);
        assert_eq!(got, Ok(Checked::Original(1)));
        assert_eq!(store.value, dword("64"));

        // A failed write is tried again, up to the attempts given.
        let mut store = FakeStore::holding(dword("32"));
        store.bad_writes = vec![1];
        let got = check(&mut store, &dword("64"), 3, || None::<&str>);
        assert_eq!(got, Ok(Checked::Original(2)));
        let mut store = FakeStore::holding(dword("32"));
        store.bad_writes = vec![1, 2, 3];
        assert_eq!(
            check(&mut store, &dword("64"), 3, || None::<&str>),
            Err(PrefError::Write("write 3 failed".into()))
        );
        assert_eq!((store.value.clone(), store.writes), (dword("32"), 3));

        // An unreadable value with nothing holding the driver is restored.
        let mut store = FakeStore::holding(dword("32"));
        store.bad_reads = vec![1];
        let got = check(&mut store, &dword("64"), 3, || None::<&str>);
        assert_eq!(got, Ok(Checked::Original(1)));
        assert_eq!(store.value, dword("64"));
    }

    /// Not the original while a process holds the driver's module (REAPER
    /// autostarted at 32 after a power loss in dev time): nothing is
    /// written, since the driver most likely asks its host for a reset when
    /// the value changes while it is open (a dropout mid-event). The check
    /// names what it found and who holds it.
    #[test]
    fn check_never_writes_while_a_process_holds_the_driver() {
        let mut store = FakeStore::holding(dword("32"));
        let got = check(&mut store, &dword("64"), 3, || Some("reaper.exe (11)"));
        assert_eq!(
            got,
            Ok(Checked::Open {
                found: Some(dword("32")),
                by: "reaper.exe (11)"
            })
        );
        assert_eq!(
            (store.value.clone(), store.reads, store.writes),
            (dword("32"), 1, 0)
        );

        // An unreadable value is not written under a holder either.
        let mut store = FakeStore::holding(dword("32"));
        store.bad_reads = vec![1];
        let got = check(&mut store, &dword("64"), 3, || Some(7_u32));
        assert_eq!(got, Ok(Checked::Open { found: None, by: 7 }));
        assert_eq!(store.writes, 0);
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
             (an engine ended while it held the card, or another program wrote it)"
        );
        assert_eq!(
            PrefError::NotHeld {
                found: dword("64"),
                held: dword("32")
            }
            .to_string(),
            "the preferred buffer holds Pref { kind: Dword, raw: \"64\" }, not \
             Pref { kind: Dword, raw: \"32\" }, which this engine wrote for the card it holds \
             (another program wrote it)"
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
