//! The data refresh of a `dev`/`live` entry (P9, S6 design note §5.2 step
//! 6): `pc.toml`'s commands in order, each with exit 0. A command that has
//! started changes the guard's own data, so it finishes (a mutation); "ide
//! event" is honoured before each command and once the last one has
//! finished, so an event plan never waits on the commands after it.

use crate::cancel::Cancel;
use crate::pc::{R, StepError};
use crate::plan::Mode;

/// Runs `run` on each of `commands` in order and joins what they report
/// (`; `). None configured fails; the first failure stops the rest; a
/// pre-emption stops the refresh between two commands and after the last.
pub fn refresh<T>(
    mode: Mode,
    commands: &[T],
    c: &Cancel,
    mut run: impl FnMut(&T) -> R<String>,
) -> R<String> {
    if commands.is_empty() {
        return Err(StepError::failed(format!(
            "pc.toml has no data commands for {mode:?}"
        )));
    }
    let mut done = Vec::with_capacity(commands.len());
    for command in commands {
        if c.preempted() {
            return Err(StepError::Preempted);
        }
        done.push(run(command)?);
    }
    if c.preempted() {
        return Err(StepError::Preempted);
    }
    Ok(done.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_runs_in_order_and_reports() {
        let mut ran = Vec::new();
        let out = refresh(Mode::Dev, &["band", "shadow"], &Cancel::default(), |cmd| {
            ran.push(*cmd);
            Ok(format!("{cmd}: ok"))
        });
        assert_eq!(out, Ok("band: ok; shadow: ok".to_owned()));
        assert_eq!(ran, ["band", "shadow"]);
    }

    #[test]
    fn no_command_configured_fails() {
        let none: [&str; 0] = [];
        assert_eq!(
            refresh(Mode::Live, &none, &Cancel::default(), |_| Ok(String::new())),
            Err(StepError::Failed(
                "pc.toml has no data commands for Live".into()
            ))
        );
    }

    #[test]
    fn the_first_failure_stops_the_rest() {
        let mut ran = 0;
        let out = refresh(Mode::Dev, &["band", "shadow"], &Cancel::default(), |_| {
            ran += 1;
            Err(StepError::failed("iem-migrate.exe ended with Some(2)"))
        });
        assert_eq!(
            out,
            Err(StepError::Failed(
                "iem-migrate.exe ended with Some(2)".into()
            ))
        );
        assert_eq!(ran, 1);
    }

    /// "ide event" before the refresh: nothing runs.
    #[test]
    fn an_event_before_the_refresh_runs_nothing() {
        let c = Cancel::default();
        c.preempt();
        let mut ran = 0;
        let out = refresh(Mode::Dev, &["band", "shadow"], &c, |_| {
            ran += 1;
            Ok(String::new())
        });
        assert_eq!(out, Err(StepError::Preempted));
        assert_eq!(ran, 0);
    }

    /// "ide event" during a command: that command finishes (a mutation),
    /// the next ones never start.
    #[test]
    fn an_event_during_a_command_lets_it_finish_and_skips_the_rest() {
        let c = Cancel::default();
        let mut ran = Vec::new();
        let out = refresh(Mode::Dev, &["band", "shadow", "import"], &c, |cmd| {
            ran.push(*cmd);
            if *cmd == "band" {
                c.preempt();
            }
            Ok(format!("{cmd}: ok"))
        });
        assert_eq!(out, Err(StepError::Preempted));
        assert_eq!(ran, ["band"]);
    }

    /// "ide event" during the last command is honoured once it finished:
    /// the event plan never waits on the steps after the refresh.
    #[test]
    fn an_event_during_the_last_command_is_honoured_after_it() {
        let c = Cancel::default();
        let mut ran = Vec::new();
        let out = refresh(Mode::Dev, &["band", "shadow"], &c, |cmd| {
            ran.push(*cmd);
            if *cmd == "shadow" {
                c.preempt();
            }
            Ok(format!("{cmd}: ok"))
        });
        assert_eq!(out, Err(StepError::Preempted));
        assert_eq!(ran, ["band", "shadow"]);
    }
}
