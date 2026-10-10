//! The texts of the guard's replies (design §5.1): how a switch ended, a
//! mode's name, the cut that keeps a reply inside one frame, a step's
//! result as a request's answer, a request's reply and `iemmode status`'s
//! line.

use super::{DETAIL_CHARS, Guard};
use crate::handover;
use crate::lifecycle;
use crate::pc::R;
use crate::plan::Mode;
use crate::proto::{self, Reply};
use crate::switch_log::{SwitchOutcome, needs_owner_text};

/// How a switch ended; the mode is in `g.state.mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// "ide event" found a healthy engine that did not release: iemmixer keeps serving.
    KeptServing,
    /// The plan stopped, or went on after steps that asked the owner (#10);
    /// the owner gets the prepared ❓ (alarm flagged `owner_question`).
    NeedsOwner,
}

impl From<Outcome> for SwitchOutcome {
    fn from(o: Outcome) -> Self {
        match o {
            Outcome::Done => Self::Done,
            Outcome::KeptServing => Self::KeptServing,
            Outcome::NeedsOwner => Self::NeedsOwner,
        }
    }
}

pub(super) fn outcome_text(o: Option<Outcome>) -> &'static str {
    match o {
        Some(Outcome::Done) => "done",
        Some(Outcome::KeptServing) => "the engine did not release; iemmixer keeps serving",
        Some(Outcome::NeedsOwner) => "stopped; the owner decides",
        None => "no switch yet",
    }
}

/// How a switch to `to` went, the mode now being `now`; `failed`: the
/// steps that asked the owner while the plan went on (#10).
pub(super) fn switch_text(to: Mode, out: Outcome, now: Mode, failed: &[String]) -> String {
    let target = mode_name(to);
    if out == Outcome::Done && now == to {
        format!("{target}: done")
    } else if out == Outcome::Done {
        format!("{target}: not entered; unwound to {}", mode_name(now))
    } else if out == Outcome::NeedsOwner && !failed.is_empty() {
        needs_owner_text(target, mode_name(now), failed)
    } else {
        format!(
            "{target}: {}; the mode is {}",
            outcome_text(Some(out)),
            mode_name(now)
        )
    }
}

/// A mode as the guard pipe names it.
pub fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Event => "event",
        Mode::Dev => "dev",
        Mode::Live => "live",
    }
}

/// The first `max` characters of `text`, each C0 control character but a
/// line break and a tab as a space: JSON escapes those to six bytes each, so
/// a reply of them could pass the frame (S7 Task 3 review, #10).
pub fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).map(plain).collect()
}

fn plain(c: char) -> char {
    match c {
        '\n' | '\t' => c,
        '\0'..='\u{1f}' => ' ',
        other => other,
    }
}

pub(super) fn outcome(r: R<()>, done: &str) -> (bool, String) {
    match r {
        Ok(()) => (true, done.to_owned()),
        Err(e) => (false, e.to_string()),
    }
}

impl Guard {
    /// The answer to a request: the state, every kept alarm and what the
    /// request did.
    pub fn reply(&self, ok: bool, detail: &str) -> Reply {
        let mut text = detail.to_owned();
        for line in &self.report {
            text.push_str("; ");
            text.push_str(line);
        }
        Reply {
            ok,
            mode: self.state.mode,
            switching: self.state.switching.clone(),
            alarms: self.alarms.all().to_vec(),
            detail: cut(&text, DETAIL_CHARS),
            engine: self.engine_status(),
            guard_build: Some(proto::GUARD_BUILD.to_owned()),
            last_switch: self.state.last_switch.clone(),
        }
    }
}

/// `iemmode status`: one line.
pub fn status_text(g: &Guard) -> String {
    let mut parts = vec![format!("mode {}", mode_name(g.state.mode))];
    parts.push(match g.state.active_bundle() {
        Some(sha) => format!("bundle {sha}"),
        None => "no bundle".to_owned(),
    });
    // Before the cutover nothing: the status reads as before S8.
    if let Some(lc) = lifecycle::status(&g.state.lifecycle) {
        parts.push(lc);
    }
    if let Some(cut) = crate::cutover::status(g.state.cutover.as_ref()) {
        parts.push(cut);
    }
    if let Some(run) = g.state.job {
        parts.push(format!("HIL job {run}"));
    }
    if g.reaper_notice {
        parts.push(handover::NOTICE_REPORT.to_owned());
    }
    if let Some(n) = &g.state.pref_held {
        parts.push(n.clone());
    }
    if let Some(n) = &g.subscriptions_note {
        parts.push(n.clone());
    }
    if let Some(n) = &g.lan_note {
        parts.push(n.clone());
    }
    if let Some(n) = g.job_note {
        parts.push(n.to_owned());
    }
    let open = g.alarms.unacked();
    if open > 0 {
        parts.push(format!("{open} unacknowledged alarms"));
    }
    parts.join("; ")
}
