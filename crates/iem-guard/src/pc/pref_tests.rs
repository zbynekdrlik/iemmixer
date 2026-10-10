//! `PrefCheck`'s holders and what it reports (#9 2026-09-28), the fake's
//! decision included.

use super::fake::FakePc;
use super::tests::{NO_HOLDER, up};
use super::*;

fn holders(list: &[(u32, &str)]) -> Vec<(u32, String)> {
    list.iter()
        .map(|(pid, n)| (*pid, (*n).to_owned()))
        .collect()
}

fn by(reaper: bool, names: &str) -> CardHolders {
    CardHolders {
        reaper,
        names: names.to_owned(),
    }
}

/// `PrefCheck` never writes while a process holds the driver module
/// (#9 2026-09-28): who holds it, as read; anything counts, REAPER is
/// named. An unreadable list assumes that a running REAPER holds it (as
/// `facts_from` does), so a failed read never writes under a REAPER that
/// may hold the card.
#[test]
fn pref_check_names_whatever_holds_the_driver() {
    let reaper = [11];
    assert_eq!(card_holders(Some(NO_HOLDER), &reaper), None);
    assert_eq!(card_holders(Some(NO_HOLDER), &[]), None);
    let one = holders(&[(11, "reaper.exe")]);
    assert_eq!(
        card_holders(Some(one.as_slice()), &reaper),
        Some(by(true, "reaper.exe (11)"))
    );
    let spike = holders(&[(99, "spike.exe")]);
    assert_eq!(
        card_holders(Some(spike.as_slice()), &reaper),
        Some(by(false, "spike.exe (99)"))
    );
    let both = holders(&[(99, "spike.exe"), (11, "reaper.exe")]);
    assert_eq!(
        card_holders(Some(both.as_slice()), &reaper),
        Some(by(true, "spike.exe (99), reaper.exe (11)"))
    );
    // An engine holds it too.
    let engine = holders(&[(13, "iem-engine.exe")]);
    assert_eq!(
        card_holders(Some(engine.as_slice()), &[]),
        Some(by(false, "iem-engine.exe (13)"))
    );
    // Unreadable: a running REAPER is assumed to hold it, nobody else.
    assert_eq!(card_holders(None, &reaper), Some(by(true, "REAPER (11)")));
    assert_eq!(card_holders(None, &[]), None);
}

#[test]
fn a_held_preference_reads_as_a_sentence() {
    let held = |value: Option<&str>, by: CardHolders| PrefHeld {
        value: value.map(str::to_owned),
        by,
    };
    assert_eq!(
        held(Some("32"), by(true, "reaper.exe (11)")).text(),
        "REAPER runs with the preferred buffer at 32; it is restored at REAPER's next start"
    );
    assert_eq!(
        held(None, by(true, "REAPER (11)")).text(),
        "REAPER runs with the preferred buffer unreadable; it is restored at REAPER's next \
         start"
    );
    assert_eq!(
        held(Some("128"), by(false, "spike.exe (99)")).text(),
        "the driver module is held by spike.exe (99) with the preferred buffer at 128; \
         nothing was written"
    );
    assert_eq!(
        held(None, by(false, "spike.exe (99), iem-engine.exe (13)")).text(),
        "the driver module is held by spike.exe (99), iem-engine.exe (13) with the preferred \
         buffer unreadable; nothing was written"
    );
}

#[test]
fn a_check_becomes_what_pref_check_reports() {
    use iem_win::prefwin::{Checked, Kind, Pref};
    assert_eq!(PrefSeen::from(Checked::Original(0)), PrefSeen::Original(0));
    assert_eq!(PrefSeen::from(Checked::Original(2)), PrefSeen::Original(2));
    let found = Pref {
        kind: Kind::Dword,
        raw: "32".into(),
    };
    assert_eq!(
        PrefSeen::from(Checked::Open {
            found: Some(found),
            by: by(true, "reaper.exe (11)")
        }),
        PrefSeen::Held(PrefHeld {
            value: Some("32".into()),
            by: by(true, "reaper.exe (11)")
        })
    );
    assert_eq!(
        PrefSeen::from(Checked::Open {
            found: None,
            by: by(false, "spike.exe (99)")
        }),
        PrefSeen::Held(PrefHeld {
            value: None,
            by: by(false, "spike.exe (99)")
        })
    );
    assert_eq!(PREF_ATTEMPTS, 3);
}

/// The fake decides with `card_holders` over its facts: REAPER, a
/// foreign holder or an engine holds the driver.
#[test]
fn the_fake_never_writes_the_preference_under_a_holder() {
    let mut pc = FakePc::new(Facts::default());
    assert_eq!(pc.pref_value, "32");
    pc.pref_attempts = 2;
    assert_eq!(pc.pref_check().unwrap(), PrefSeen::Original(2));
    assert_eq!(pc.pref_writes, 2);
    for (f, holder) in [
        (up(), by(true, "reaper.exe (1)")),
        (
            Facts {
                other_module_holder: true,
                ..Facts::default()
            },
            by(false, "spike.exe (99)"),
        ),
        (
            Facts {
                engine: true,
                ..Facts::default()
            },
            by(false, "iem-engine.exe (2)"),
        ),
    ] {
        let mut pc = FakePc::new(f);
        pc.pref_attempts = 1;
        assert_eq!(
            pc.pref_check().unwrap(),
            PrefSeen::Held(PrefHeld {
                value: Some("32".into()),
                by: holder
            }),
            "{f:?}"
        );
        assert_eq!(pc.pref_writes, 0, "{f:?}");
        // The original is there: nothing to hold back.
        pc.pref_attempts = 0;
        assert_eq!(pc.pref_check().unwrap(), PrefSeen::Original(0));
    }
}
