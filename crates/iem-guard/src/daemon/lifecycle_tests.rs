//! S8 (#11): the lifecycle's call sites against `FakePc` (design note
//! `docs/superpowers/specs/2026-10-10-s8-cutover-rollback-design.md` §3.1,
//! §3.4): an entry or an activation never promotes the pin.

use super::tests::{INIT, OTHER, SHA, T0, band_up, fixed, iemmixer_up, record};
use super::*;
use crate::bundle::{Hil, Pins};
use crate::install;
use crate::pc::fake::FakePc;
use crate::plan::Facts;
use crate::proto::Request;

/// The pins as an earlier guard left them: `OTHER`, nothing before it.
fn other_pinned() -> Pins {
    Pins {
        current: Some(OTHER.into()),
        previous: None,
    }
}

/// The pin bug (design §3.4): every dev or live entry promoted its build to
/// the pin before any HIL result. An entry runs its build and leaves the
/// pins as they were: a dev entry with a build, and a live trial.
#[test]
fn an_entry_never_promotes_the_pin() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    g.state.pins = other_pinned();
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "dev", Hil::Pending));
    let dev = Request::Dev {
        build: Some(SHA.into()),
        dry_run: false,
    };
    let r = handle(&mut pc, &mut g, dev, INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Dev);
    assert_eq!(
        g.state.pins,
        other_pinned(),
        "a dev entry promoted its build"
    );
    assert_eq!(pc.bundle.as_deref(), Some(SHA), "the entry runs its build");
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins = other_pinned();
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    let trial = Request::Live {
        build: SHA.into(),
        trial: true,
        dry_run: false,
    };
    let r = handle(&mut pc, &mut g, trial, INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(g.state.pins, other_pinned(), "a trial promoted its build");
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
}

/// `activate` makes its bundle the active one for dev (the engine and the
/// server run it, the bundle before it keeps its Defender exclusions) and
/// leaves the pins as they were.
#[test]
fn an_activation_never_promotes_the_pin() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    g.state.mode = Mode::Dev;
    let zip = install::tests::good_zip(dir.path(), SHA);
    assert!(install_bundle(&mut g, &zip).0);
    g.state.pins = other_pinned();
    let mut pc = FakePc::new(iemmixer_up());
    let r = handle(&mut pc, &mut g, Request::Activate { sha: SHA.into() }, INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.pins, other_pinned(), "activate promoted its bundle");
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
    assert_eq!(pc.excluded, [(SHA.to_owned(), vec![OTHER.to_owned()])]);
}
