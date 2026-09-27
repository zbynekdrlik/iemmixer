//! The portable decisions of the Windows wrappers: which flags a child gets,
//! which handles and groups a message may go to, how a working set grows,
//! what a DWORD's text may be. They live here, apart from the wrapper
//! modules, so mutation testing reaches them on Linux: the wrapper modules
//! are excluded from it, because their `cfg(windows)` bodies do not compile
//! there (`.cargo/mutants.toml`).

use std::io;
use std::time::{Duration, SystemTime};

use crate::spawn::{CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};

/// The `HWND_BROADCAST` value: a command is never posted to every window.
const BROADCAST: isize = 0xFFFF;

/// One mebibyte, the unit of [`crate::power::lock_min_working_set`].
pub const MIB: usize = 1 << 20;

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// The creation flags of [`crate::spawn::spawn_detached`]: every child
/// breaks away from the caller's job and runs without a console window, so
/// it has a console of its own that no one can close and that the caller
/// does not share (S6 design note §5.5). `new_group` adds
/// `CREATE_NEW_PROCESS_GROUP`: Ctrl-Break then reaches exactly that child's
/// group ([`crate::console::ctrl_break`]).
pub fn creation_flags(new_group: bool) -> u32 {
    let group = if new_group {
        CREATE_NEW_PROCESS_GROUP
    } else {
        0
    };
    CREATE_BREAKAWAY_FROM_JOB | CREATE_NO_WINDOW | group
}

/// The new `(minimum, maximum)` working set for the current `(min, max)`:
/// both grow by `extra_mb` MiB, so the gap between them stays as Windows
/// accepted it; sums saturate.
pub fn working_set_target(min: usize, max: usize, extra_mb: usize) -> (usize, usize) {
    let extra = extra_mb.saturating_mul(MIB);
    (min.saturating_add(extra), max.saturating_add(extra))
}

/// A DWORD's text: decimal digits only (no sign, no space, no `0x`) that fit
/// a `u32`.
pub(crate) fn dword(text: &str) -> io::Result<u32> {
    let digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    if !digits {
        return Err(invalid(format!("not a decimal DWORD: {text:?}")));
    }
    text.parse()
        .map_err(|_| invalid(format!("not a decimal DWORD: {text:?}")))
}

/// A window command or notification goes to one window: never the null
/// handle (a thread message) and never the broadcast handle.
pub(crate) fn one_window(hwnd: isize) -> io::Result<()> {
    if hwnd == 0 || hwnd == BROADCAST {
        return Err(invalid(
            "a window call needs one window, not the null or broadcast handle".to_string(),
        ));
    }
    Ok(())
}

/// `text` as NUL-terminated UTF-16 in a buffer of `N` units, the shape of
/// the shell's fixed text fields: cut after at most `N - 1` units, never
/// inside a surrogate pair; every unit after the text is 0.
pub(crate) fn fixed_wide<const N: usize>(text: &str) -> [u16; N] {
    let mut out = [0u16; N];
    let room = N.saturating_sub(1);
    let mut used = 0;
    for ch in text.chars() {
        let mut units = [0u16; 2];
        let encoded = ch.encode_utf16(&mut units);
        if used + encoded.len() > room {
            break;
        }
        for (slot, unit) in out.iter_mut().skip(used).zip(encoded.iter()) {
            *slot = *unit;
        }
        used += encoded.len();
    }
    out
}

/// Ctrl-Break goes to one process group: never group 0, which is every
/// process on the console, the caller included once it has attached.
pub(crate) fn one_group(pid: u32) -> io::Result<()> {
    if pid == 0 {
        return Err(invalid(
            "Ctrl-Break goes to one process group, never to group 0".to_string(),
        ));
    }
    Ok(())
}

/// A range to lock has a start and at least one byte.
pub(crate) fn lock_range(ptr: *const u8, len: usize) -> io::Result<()> {
    if ptr.is_null() || len == 0 {
        return Err(invalid(format!("nothing to lock: {len} bytes at {ptr:?}")));
    }
    Ok(())
}

/// The name of a mutex in the `Global\` namespace (one across all sessions):
/// `name` itself is not empty and has no namespace separator.
pub(crate) fn global_name(name: &str) -> io::Result<String> {
    if name.is_empty() || name.contains('\\') {
        return Err(invalid(format!("not a plain mutex name: {name:?}")));
    }
    Ok(format!("Global\\{name}"))
}

/// A wait in milliseconds for `WaitForSingleObject`: whole milliseconds,
/// capped below `INFINITE` (`u32::MAX`), so every wait is bounded.
pub(crate) fn wait_ms(timeout: Duration) -> u32 {
    u32::try_from(timeout.as_millis())
        .unwrap_or(u32::MAX)
        .min(u32::MAX - 1)
}

/// The moment the system started: `now` minus the time since the start.
pub(crate) fn boot_time(now: SystemTime, since_boot: Duration) -> io::Result<SystemTime> {
    now.checked_sub(since_boot)
        .ok_or_else(|| io::Error::other("the system started before the clock's range"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind;
    use std::io::ErrorKind::InvalidInput;
    use std::time::UNIX_EPOCH;

    #[test]
    fn every_child_breaks_away_without_a_window_and_a_new_group_is_its_own() {
        assert_eq!(creation_flags(true), 0x0900_0200);
        assert_eq!(creation_flags(false), 0x0900_0000);
    }

    #[test]
    fn the_working_set_grows_by_whole_mebibytes_and_saturates() {
        assert_eq!(
            working_set_target(200 * 1024, 1380 * 1024, 64),
            (200 * 1024 + 64 * MIB, 1380 * 1024 + 64 * MIB)
        );
        assert_eq!(working_set_target(10, 20, 0), (10, 20));
        assert_eq!(
            working_set_target(usize::MAX - 1, usize::MAX, 1),
            (usize::MAX, usize::MAX)
        );
        assert_eq!(
            working_set_target(0, 0, usize::MAX),
            (usize::MAX, usize::MAX)
        );
    }

    #[test]
    fn a_dword_is_plain_decimal_digits_that_fit() {
        assert_eq!(dword("0").unwrap(), 0);
        assert_eq!(dword("32").unwrap(), 32);
        assert_eq!(dword("0064").unwrap(), 64);
        assert_eq!(dword("4294967295").unwrap(), u32::MAX);
        for bad in [
            "",
            "+32",
            "-1",
            " 32",
            "32 ",
            "x32",
            "0x20",
            "3.2",
            "4294967296",
        ] {
            assert_eq!(kind(dword(bad)), Some(InvalidInput), "{bad:?}");
        }
    }

    #[test]
    fn a_command_goes_to_one_window() {
        assert_eq!(kind(one_window(0)), Some(InvalidInput));
        assert_eq!(kind(one_window(0xFFFF)), Some(InvalidInput));
        assert!(one_window(1).is_ok());
        assert!(one_window(0x1234).is_ok());
        assert!(one_window(0xFFFE).is_ok());
        assert!(one_window(0x1_0000).is_ok());
    }

    #[test]
    fn fixed_wide_text_is_cut_whole_and_ends_in_nul() {
        let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        // Fits with room to spare: the rest is zero.
        let mut expect = [0u16; 6];
        expect[..3].copy_from_slice(&w("abc"));
        assert_eq!(fixed_wide::<6>("abc"), expect);
        // Exactly N - 1 units: kept whole, the last unit is the NUL.
        let mut expect = [0u16; 6];
        expect[..5].copy_from_slice(&w("abcde"));
        assert_eq!(fixed_wide::<6>("abcde"), expect);
        // One unit too many: cut to N - 1.
        assert_eq!(fixed_wide::<6>("abcdef"), expect);
        assert_eq!(fixed_wide::<6>("abcdefghij"), expect);
        // A surrogate pair that would end on the NUL is left out whole ...
        let mut expect = [0u16; 4];
        expect[..2].copy_from_slice(&w("ab"));
        assert_eq!(fixed_wide::<4>("ab\u{1F3A7}"), expect);
        // ... and one that fits is kept whole, after other text.
        let mut expect = [0u16; 4];
        expect[..3].copy_from_slice(&w("a\u{1F3A7}"));
        assert_eq!(fixed_wide::<4>("a\u{1F3A7}"), expect);
        assert_eq!(fixed_wide::<4>("a\u{1F3A7}b"), expect);
        // Text outside ASCII but below the surrogates: one unit a character.
        let mut expect = [0u16; 8];
        expect[..7].copy_from_slice(&w("strážca"));
        assert_eq!(fixed_wide::<8>("strážca — x"), expect);
        let mut expect = [0u16; 7];
        expect[..6].copy_from_slice(&w("strážc"));
        assert_eq!(fixed_wide::<7>("strážca"), expect);
        // Nothing fits in a buffer of one unit, and nothing in none.
        assert_eq!(fixed_wide::<1>("abc"), [0u16; 1]);
        assert_eq!(fixed_wide::<0>("abc"), [0u16; 0]);
        assert_eq!(fixed_wide::<3>(""), [0u16; 3]);
    }

    #[test]
    fn ctrl_break_goes_to_one_group() {
        assert_eq!(kind(one_group(0)), Some(InvalidInput));
        assert!(one_group(1).is_ok());
        assert!(one_group(4242).is_ok());
    }

    #[test]
    fn a_range_to_lock_has_a_start_and_a_length() {
        let buf = [0u8; 8];
        assert!(lock_range(buf.as_ptr(), 8).is_ok());
        assert!(lock_range(buf.as_ptr(), 1).is_ok());
        assert_eq!(kind(lock_range(buf.as_ptr(), 0)), Some(InvalidInput));
        assert_eq!(kind(lock_range(std::ptr::null(), 8)), Some(InvalidInput));
        assert_eq!(kind(lock_range(std::ptr::null(), 0)), Some(InvalidInput));
    }

    #[test]
    fn a_mutex_name_is_plain_and_global() {
        assert_eq!(
            global_name("iemmixer-guard").unwrap(),
            "Global\\iemmixer-guard"
        );
        assert_eq!(kind(global_name("")), Some(InvalidInput));
        assert_eq!(kind(global_name("Local\\x")), Some(InvalidInput));
        assert_eq!(kind(global_name("x\\")), Some(InvalidInput));
    }

    #[test]
    fn a_wait_is_whole_milliseconds_and_never_infinite() {
        assert_eq!(wait_ms(Duration::ZERO), 0);
        assert_eq!(wait_ms(Duration::from_micros(999)), 0);
        assert_eq!(wait_ms(Duration::from_millis(1500)), 1500);
        assert_eq!(
            wait_ms(Duration::from_millis(u64::from(u32::MAX) - 1)),
            u32::MAX - 1
        );
        assert_eq!(
            wait_ms(Duration::from_millis(u64::from(u32::MAX))),
            u32::MAX - 1
        );
        assert_eq!(wait_ms(Duration::MAX), u32::MAX - 1);
    }

    #[test]
    fn the_boot_time_is_now_minus_the_time_since_boot() {
        let now = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        assert_eq!(
            boot_time(now, Duration::from_millis(90_500)).unwrap(),
            UNIX_EPOCH + Duration::from_millis(1_799_999_909_500)
        );
        assert_eq!(boot_time(now, Duration::ZERO).unwrap(), now);
        assert_eq!(
            kind(boot_time(now, Duration::MAX)),
            Some(std::io::ErrorKind::Other)
        );
    }
}
