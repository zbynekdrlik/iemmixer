//! The portable decisions of the Windows wrappers: where a child starts and
//! which flags it gets, which handles and groups a message may go to, how a
//! working set grows, what a DWORD's text may be. They live here, apart from
//! the wrapper modules, so mutation testing reaches them on Linux: the
//! wrapper modules are excluded from it, because their `cfg(windows)` bodies
//! do not compile there (`.cargo/mutants.toml`).

use std::io;
use std::time::{Duration, SystemTime};

use crate::spawn::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, JOB_BREAKAWAY_OK,
    JOB_ENDS_ON_CLOSE, JOB_SILENT_BREAKAWAY_OK,
};

/// The `HWND_BROADCAST` value: a command is never posted to every window.
const BROADCAST: isize = 0xFFFF;

/// One mebibyte, the unit of [`crate::power::lock_min_working_set`].
pub const MIB: usize = 1 << 20;

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// The job a process runs in, as far as its children care
/// ([`crate::spawn::job_limits`]): whether there is one, and the limits of
/// the immediate one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JobLimits {
    /// The process runs in a job.
    pub in_job: bool,
    /// A child may leave the job with `CREATE_BREAKAWAY_FROM_JOB`
    /// ([`JOB_BREAKAWAY_OK`]).
    pub breakaway_ok: bool,
    /// Every child starts outside the job, without the flag
    /// ([`JOB_SILENT_BREAKAWAY_OK`]).
    pub silent_breakaway_ok: bool,
    /// Closing the job's last handle ends every process in it
    /// ([`JOB_ENDS_ON_CLOSE`]).
    pub kill_on_close: bool,
}

impl JobLimits {
    /// The limits of a job from its `LimitFlags`; every other limit (a
    /// working set, a process count) changes nothing for a child's start.
    pub fn from_flags(flags: u32) -> Self {
        Self {
            in_job: true,
            breakaway_ok: (flags & JOB_BREAKAWAY_OK) != 0,
            silent_breakaway_ok: (flags & JOB_SILENT_BREAKAWAY_OK) != 0,
            kill_on_close: (flags & JOB_ENDS_ON_CLOSE) != 0,
        }
    }
}

/// Why no long-lived child starts in a job that allows no breakaway and ends
/// its processes when it closes.
pub const ENDS_WITH_THE_JOB: &str = "this process's job allows no breakaway and ends its \
    processes when it closes, which may come with this process's end: a child started in \
    it could end with it (S6 design note §5.1, I9)";

/// Where a long-lived child starts ([`crate::spawn::spawn_detached`]), so
/// that the end of its starter never ends it (S6 design note §5.1, I9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Outside any job of the starter's, without the breakaway flag: the
    /// starter runs in no job, or its job lets every child out silently.
    NoJob,
    /// Out of the starter's job with `CREATE_BREAKAWAY_FROM_JOB`: the job
    /// allows it.
    Breakaway,
    /// Inside the starter's job, which allows no breakaway and does not end
    /// its processes when it closes: the job lives on while any of them
    /// runs, so the child outlives its starter.
    InJob,
    /// Not started: the job allows no breakaway and ends its processes when
    /// it closes. The reason.
    Refuse(&'static str),
}

/// Where a long-lived child of a process in `job` starts: read from the
/// job's limits, never found by trying (#9 2026-09-28: the PC's task job
/// allows no breakaway).
pub fn placement(job: JobLimits) -> Placement {
    match (
        job.in_job,
        job.breakaway_ok,
        job.silent_breakaway_ok,
        job.kill_on_close,
    ) {
        (false, ..) => Placement::NoJob,
        (true, true, ..) => Placement::Breakaway,
        (true, false, true, _) => Placement::NoJob,
        (true, false, false, true) => Placement::Refuse(ENDS_WITH_THE_JOB),
        (true, false, false, false) => Placement::InJob,
    }
}

/// The creation flags of [`crate::spawn::spawn_detached`] for a child
/// placed by `placed`: no console window, so it has a console of its own
/// that no one can close and that the starter does not share (S6 design
/// note §5.5); `CREATE_BREAKAWAY_FROM_JOB` only for
/// [`Placement::Breakaway`]. `new_group` adds `CREATE_NEW_PROCESS_GROUP`:
/// Ctrl-Break then reaches exactly that child's group
/// ([`crate::console::ctrl_break`]). [`Placement::Refuse`] starts nothing:
/// its reason, as `PermissionDenied`.
pub fn creation_flags(new_group: bool, placed: Placement) -> io::Result<u32> {
    let breakaway = match placed {
        Placement::Breakaway => CREATE_BREAKAWAY_FROM_JOB,
        Placement::NoJob | Placement::InJob => 0,
        Placement::Refuse(why) => {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, why));
        }
    };
    let group = if new_group {
        CREATE_NEW_PROCESS_GROUP
    } else {
        0
    };
    Ok(breakaway | CREATE_NO_WINDOW | group)
}

/// The creation flags of a bounded helper command the guard runs (`schtasks`,
/// `curl`, `iem-migrate`, `iem-server notify`): no console window, and for a
/// waiting one (Ctrl-Break on "ide event") a process group of its own, so the
/// Ctrl-Break reaches that helper only (S6 design note §5.5). Helpers stay in
/// the caller's job: they are short, and a job that refuses breakaway must not
/// refuse them. Only the long-lived children and the card's interlock are
/// placed by the job ([`placement`], [`creation_flags`], I9).
pub fn helper_flags(waiting: bool) -> u32 {
    let group = if waiting { CREATE_NEW_PROCESS_GROUP } else { 0 };
    CREATE_NO_WINDOW | group
}

/// SDDL's name for the SYSTEM account (`S-1-5-18`).
pub const SYSTEM: &str = "SY";

/// The accounts of a DACL written as SDDL (`D:<flags>(ace)(ace)…`), in
/// order, when every ACE is a plain allow ACE (`A`, six fields); `None` for
/// anything else: a deny, object or conditional ACE, an empty or null DACL,
/// an owner or SACL part in the text.
pub fn dacl_sids(sddl: &str) -> Option<Vec<&str>> {
    let dacl = sddl.strip_prefix("D:")?;
    let aces = dacl.get(dacl.find('(')?..)?;
    let aces = aces.strip_prefix('(')?.strip_suffix(')')?;
    aces.split(")(")
        .map(|ace| match ace.split(';').collect::<Vec<_>>().as_slice() {
            ["A", _, _, _, _, account] => Some(*account),
            _ => None,
        })
        .collect()
}

/// Whether a pipe's DACL (as [`crate::token::pipe_sddl`] reads it back)
/// admits `user` and nobody but `user` and SYSTEM (the engine's pipe tests,
/// the guard's `Reply.engine.pipe_private` for HIL). `user` is written the
/// way SDDL writes it ([`crate::token::sddl_sid`]).
pub fn sddl_is_private(sddl: &str, user: &str) -> bool {
    dacl_sids(sddl).is_some_and(|accounts| {
        accounts.contains(&user)
            && accounts
                .iter()
                .all(|&account| account == user || account == SYSTEM)
    })
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

/// Windows' `ERROR_ACCESS_DENIED`.
const ACCESS_DENIED: i32 = 5;

/// Whether a durable rename that failed is made again with POSIX
/// semantics ([`crate::file::rename_durable`]): only after
/// `ERROR_ACCESS_DENIED`, which `MoveFileExW` gives for a target another
/// process holds with delete sharing (#32 F3-r4 5). Any other error is the
/// rename's own.
pub(crate) fn rename_again(e: &io::Error) -> bool {
    e.raw_os_error() == Some(ACCESS_DENIED)
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

/// A GUID's parts as text, lower case with hyphens and no braces
/// (`01234567-89ab-cdef-0001-020304050607`).
pub fn guid_text(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> String {
    let [a, b, c, d, e, f, g, h] = data4;
    format!(
        "{data1:08x}-{data2:04x}-{data3:04x}-{a:02x}{b:02x}-{c:02x}{d:02x}{e:02x}{f:02x}{g:02x}{h:02x}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind;
    use std::io::ErrorKind::InvalidInput;
    use std::time::UNIX_EPOCH;

    /// #32 F3-r4 5: only `ERROR_ACCESS_DENIED` (5) makes a durable rename
    /// try again with POSIX semantics; a missing source, a sharing
    /// violation or an error without an OS code is the rename's own.
    #[test]
    fn a_durable_rename_tries_again_only_after_access_denied() {
        assert!(rename_again(&io::Error::from_raw_os_error(5)));
        for code in [2, 3, 32, 183] {
            assert!(!rename_again(&io::Error::from_raw_os_error(code)), "{code}");
        }
        assert!(!rename_again(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
        assert!(!rename_again(&io::Error::other("no code")));
    }

    /// Every child runs without a window, in a group of its own when asked;
    /// only a job that allows breakaway gets the flag, and a refusal starts
    /// nothing and names why.
    #[test]
    fn a_child_breaks_away_only_where_its_job_allows_it() {
        assert_eq!(
            creation_flags(true, Placement::Breakaway).unwrap(),
            0x0900_0200
        );
        assert_eq!(
            creation_flags(false, Placement::Breakaway).unwrap(),
            0x0900_0000
        );
        for placed in [Placement::InJob, Placement::NoJob] {
            assert_eq!(
                creation_flags(true, placed).unwrap(),
                0x0800_0200,
                "{placed:?}"
            );
            assert_eq!(
                creation_flags(false, placed).unwrap(),
                0x0800_0000,
                "{placed:?}"
            );
        }
        for new_group in [false, true] {
            let e = creation_flags(new_group, Placement::Refuse("the reason")).unwrap_err();
            assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
            assert_eq!(e.to_string(), "the reason");
        }
    }

    /// A job's `LimitFlags` (winnt.h values): the three that decide where a
    /// child starts, every other limit changes nothing.
    #[test]
    fn a_jobs_limits_are_read_from_its_flags() {
        assert_eq!(
            (JOB_BREAKAWAY_OK, JOB_SILENT_BREAKAWAY_OK, JOB_ENDS_ON_CLOSE),
            (0x0800, 0x1000, 0x2000)
        );
        let plain = JobLimits {
            in_job: true,
            breakaway_ok: false,
            silent_breakaway_ok: false,
            kill_on_close: false,
        };
        assert_eq!(JobLimits::from_flags(0), plain);
        // A working-set and an active-process limit.
        assert_eq!(JobLimits::from_flags(0x0000_0001 | 0x0000_0008), plain);
        assert_eq!(
            JobLimits::from_flags(0x0800),
            JobLimits {
                breakaway_ok: true,
                ..plain
            }
        );
        assert_eq!(
            JobLimits::from_flags(0x1000),
            JobLimits {
                silent_breakaway_ok: true,
                ..plain
            }
        );
        assert_eq!(
            JobLimits::from_flags(0x2000),
            JobLimits {
                kill_on_close: true,
                ..plain
            }
        );
        assert_eq!(
            JobLimits::from_flags(0x3809),
            JobLimits {
                in_job: true,
                breakaway_ok: true,
                silent_breakaway_ok: true,
                kill_on_close: true,
            }
        );
        assert_eq!(
            JobLimits::default(),
            JobLimits {
                in_job: false,
                ..plain
            }
        );
    }

    /// Where a long-lived child starts, for every reading of its starter's
    /// job (S6 design note §5.1, I9; #9 2026-09-28: the PC's task job allows
    /// no breakaway): all five cases, every flag on both sides.
    #[test]
    fn the_job_as_read_decides_where_a_child_starts() {
        let job = |in_job, breakaway_ok, silent_breakaway_ok, kill_on_close| JobLimits {
            in_job,
            breakaway_ok,
            silent_breakaway_ok,
            kill_on_close,
        };
        let both = [false, true];
        for breakaway in both {
            for silent in both {
                for kill in both {
                    // In no job: nothing to leave, whatever the flags say.
                    assert_eq!(
                        placement(job(false, breakaway, silent, kill)),
                        Placement::NoJob,
                        "{breakaway} {silent} {kill}"
                    );
                }
            }
        }
        for silent in both {
            for kill in both {
                // Breakaway allowed: the child leaves the job, as before.
                assert_eq!(
                    placement(job(true, true, silent, kill)),
                    Placement::Breakaway,
                    "{silent} {kill}"
                );
            }
        }
        for kill in both {
            // Silent breakaway: every child starts outside the job anyway.
            assert_eq!(
                placement(job(true, false, true, kill)),
                Placement::NoJob,
                "{kill}"
            );
        }
        // Neither: inside a job that lives on while any of its processes
        // runs, so the child outlives its starter ...
        assert_eq!(placement(job(true, false, false, false)), Placement::InJob);
        // ... and nothing where the job's close would end the child.
        assert_eq!(
            placement(job(true, false, false, true)),
            Placement::Refuse(ENDS_WITH_THE_JOB)
        );
        assert!(
            ENDS_WITH_THE_JOB.contains("allows no breakaway")
                && ENDS_WITH_THE_JOB.contains("ends its processes when it closes"),
            "{ENDS_WITH_THE_JOB}"
        );
    }

    /// Helpers stay in the caller's job: a job that refuses breakaway (the
    /// PC's task job, #9 2026-09-28) must not refuse a `curl` check. A
    /// waiting one gets its own group, so Ctrl-Break reaches that helper
    /// only.
    #[test]
    fn helpers_stay_in_the_job_and_a_waiting_one_has_its_own_group() {
        assert_eq!(helper_flags(false), 0x0800_0000);
        assert_eq!(helper_flags(true), 0x0800_0200);
        assert_eq!(helper_flags(true) & CREATE_BREAKAWAY_FROM_JOB, 0);
        assert_eq!(
            helper_flags(true),
            CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP
        );
    }

    const USER: &str = "S-1-5-21-1-2-3-1001";

    #[test]
    fn dacl_sids_lists_the_accounts_of_plain_allow_aces() {
        assert_eq!(
            dacl_sids("D:P(A;;FA;;;S-1-5-21-1-2-3-1001)(A;;FA;;;SY)"),
            Some(vec![USER, "SY"])
        );
        assert_eq!(
            dacl_sids("D:P(A;;GA;;;S-1-5-21-1-2-3-1001)(A;;GA;;;SY)"),
            Some(vec![USER, "SY"])
        );
        assert_eq!(dacl_sids("D:PAI(A;ID;0x1f01ff;;;LA)"), Some(vec!["LA"]));
        assert_eq!(dacl_sids("D:(A;;GA;;;WD)"), Some(vec!["WD"]));
        for other in [
            "",
            "D:",
            "D:P",
            "D:NO_ACCESS_CONTROL",
            "O:SYD:P(A;;FA;;;SY)",
            "S:(A;;FA;;;SY)",
            "D:P(A;;FA;;;SY)S:(ML;;NW;;;LW)",
            "D:P(D;;FA;;;WD)(A;;FA;;;SY)",
            "D:P(A;;FA;;;SY)(D;;FA;;;WD)",
            "D:P(AU;;FA;;;SY)",
            "D:P(A;;FA;;SY)",
            "D:P(A;;FA;;;SY;(x))",
            "D:P(A;;FA;;;SY",
            "D:PA;;FA;;;SY)",
        ] {
            assert_eq!(dacl_sids(other), None, "{other}");
        }
    }

    #[test]
    fn a_private_dacl_admits_the_user_and_at_most_system() {
        let read_back = "D:P(A;;FA;;;S-1-5-21-1-2-3-1001)(A;;FA;;;SY)";
        assert_eq!(SYSTEM, "SY");
        assert!(sddl_is_private(read_back, USER));
        assert!(sddl_is_private("D:P(A;;FA;;;S-1-5-21-1-2-3-1001)", USER));
        assert!(sddl_is_private("D:P(A;;FA;;;LA)(A;;FA;;;SY)", "LA"));
        assert!(
            !sddl_is_private("D:P(A;;FA;;;SY)", USER),
            "without the user"
        );
        assert!(
            !sddl_is_private(read_back, "S-1-5-21-1-2-3-1002"),
            "another user's pipe"
        );
        assert!(
            !sddl_is_private(&format!("{read_back}(A;;FA;;;WD)"), USER),
            "everyone"
        );
        assert!(
            !sddl_is_private("D:P(A;;FA;;;BA)(A;;FA;;;S-1-5-21-1-2-3-1001)", USER),
            "administrators"
        );
        assert!(
            !sddl_is_private("D:P(D;;FA;;;WD)(A;;FA;;;S-1-5-21-1-2-3-1001)", USER),
            "a deny ACE is not this pipe's DACL"
        );
        assert!(!sddl_is_private("D:NO_ACCESS_CONTROL", USER), "a null DACL");
        assert!(!sddl_is_private("D:P", USER), "an empty DACL");
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

    #[test]
    fn a_guid_is_zero_padded_lower_case_hex() {
        assert_eq!(
            guid_text(0x0123_4567, 0x89ab, 0xcdef, [0, 1, 2, 3, 4, 5, 6, 7]),
            "01234567-89ab-cdef-0001-020304050607"
        );
        assert_eq!(
            guid_text(1, 2, 3, [0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32, 0x10]),
            "00000001-0002-0003-fedc-ba9876543210"
        );
        assert_eq!(
            guid_text(u32::MAX, u16::MAX, 0, [0xff; 8]),
            "ffffffff-ffff-0000-ffff-ffffffffffff"
        );
    }
}
