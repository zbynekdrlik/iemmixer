//! The daemon's records in its replies (S7, #10): the engine's pid and texts
//! cut to fit a frame. New daemon tests live here, since `daemon.rs` and
//! `daemon/tests.rs` are over their size budget (#36).

use super::*;
use crate::pc::fake::FakePc;
use crate::plan::Facts;
use crate::state::Child;

/// The generation of a guard that began no switch and routed no "ide event".
const INIT: Generation = Generation { epoch: 0, fence: 0 };

/// The soak's "one pid" (S7 design note §4): `Reply.engine` names the engine
/// process the guard started or adopted (`GuardState.pids`), and none while
/// it knows of none.
#[test]
fn the_engine_reply_names_its_pid() {
    let mut pc = FakePc::new(Facts {
        engine: true,
        ..Facts::default()
    });
    let mut g = Guard::for_test(Mode::Dev);
    g.state.pids.engine = Some(Child {
        pid: 4242,
        start_time: 1,
        image: "iem-engine.exe".into(),
    });
    let r = handle(&mut pc, &mut g, Request::Status, INIT);
    assert_eq!(r.engine.map(|e| e.pid), Some(Some(4242)));
    g.state.pids.engine = None;
    let r = handle(&mut pc, &mut g, Request::Status, INIT);
    assert_eq!(
        r.engine.map(|e| e.pid),
        Some(None),
        "an engine seen, no child"
    );
}

/// JSON escapes a C0 control character to six bytes (`\u001f`), so a reply
/// of such alarm texts could pass the frame (S7 Task 3 review, #10): `cut`
/// turns each one into a space wherever alarm texts and reply details are
/// cut. A line break and a tab (two bytes each; alarm texts use line
/// breaks) stay, and so does everything from the space up.
#[test]
fn texts_are_cut_with_control_characters_as_spaces_but_line_breaks_and_tabs() {
    assert_eq!(
        cut("a\nb\tc\u{1f}d\u{20}e\u{0}f\rg\u{7f}h", 64),
        "a\nb\tc d e f g\u{7f}h"
    );
    // The cap counts characters, the same before and after.
    assert_eq!(cut("\u{1}\u{2}\u{3}", 2), "  ");
    let mut g = Guard::for_test(Mode::Event);
    g.raise(None, "line one\nline\u{1b}[31m two\u{7}", false);
    assert_eq!(g.alarms.last().unwrap().text, "line one\nline [31m two ");
    assert_eq!(g.reply(true, "a\u{8}b\tc").detail, "a b\tc");
    assert_eq!(g.shared.view().reply(true, "a\u{8}b\tc").detail, "a b\tc");
}
