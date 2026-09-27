//! The portable halves of the PC effects (S6 plan Task 9): parsers of what
//! the PC answers and the decisions on it. `win::WinPc` reads and acts;
//! these functions decide, and are tested and mutated on Linux.
//!
//! - [`argv`]: `{placeholder}` arguments of configured commands;
//! - [`reaper`]: REAPER's web-control lines and meters;
//! - [`app`]: the predecessor app's exit (one process, its log, temp files);
//! - [`engine`]: the engine's supervisor pipe, readiness and health;
//! - [`web`]: `/api/version`, the tunnel, `iem-server`'s CLI, HTTPS via curl;
//! - [`tasks`]: our scheduled tasks through `schtasks.exe`;
//! - [`tuning`]: the elevated tuning task's files and the drift check.

pub mod app;
pub mod argv;
pub mod engine;
pub mod reaper;
pub mod tasks;
pub mod tuning;
pub mod web;

/// The last `n` characters of `text`, trimmed: the end of a command's
/// output for an alarm.
pub fn tail(text: &str, n: usize) -> &str {
    let t = text.trim();
    let skip = t.chars().count().saturating_sub(n);
    t.char_indices()
        .nth(skip)
        .map_or("", |(i, _)| t.get(i..).unwrap_or(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tail_keeps_the_last_characters() {
        assert_eq!(tail("  abcdef \n", 3), "def");
        assert_eq!(tail("abc", 3), "abc");
        assert_eq!(tail("abc", 4), "abc");
        assert_eq!(tail("abc", 0), "");
        assert_eq!(tail("", 5), "");
        // Characters, not bytes.
        assert_eq!(tail("čšžý", 2), "žý");
    }
}
