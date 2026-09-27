//! Scheduling and memory of the calling process (S6 design note §3; S1c L5):
//! HIGH priority class, power throttling off, a default CPU Set and a locked
//! minimum working set. Each call changes only the calling process.

use std::io;

/// One mebibyte, the unit of [`lock_min_working_set`].
pub const MIB: usize = 1 << 20;

/// Sets the calling process to the HIGH priority class.
pub fn set_high_priority() -> io::Result<()> {
    imp::set_high_priority()
}

/// Turns power throttling off for the calling process: execution speed
/// (EcoQoS) and timer-resolution throttling.
pub fn disable_power_throttling() -> io::Result<()> {
    imp::disable_power_throttling()
}

/// Makes `ids` (CPU Set ids, not processor indices) the calling process's
/// default CPU Set; an empty slice clears it (every CPU).
pub fn set_cpu_sets(ids: &[u32]) -> io::Result<()> {
    imp::set_cpu_sets(ids)
}

/// Raises the calling process's minimum working set by `extra_mb` MiB and
/// makes that minimum hard (QUOTA_LIMITS_HARDWS_MIN_ENABLE), so the pages the
/// audio path touches are not trimmed.
pub fn lock_min_working_set(extra_mb: usize) -> io::Result<()> {
    imp::lock_min_working_set(extra_mb)
}

/// The new `(minimum, maximum)` working set for the current `(min, max)`:
/// both grow by `extra_mb` MiB, so the gap between them stays as Windows
/// accepted it; sums saturate.
pub fn working_set_target(min: usize, max: usize, extra_mb: usize) -> (usize, usize) {
    let extra = extra_mb.saturating_mul(MIB);
    (min.saturating_add(extra), max.saturating_add(extra))
}

#[cfg(not(windows))]
mod imp {
    use std::io;

    pub(super) fn set_high_priority() -> io::Result<()> {
        crate::unsupported()
    }

    pub(super) fn disable_power_throttling() -> io::Result<()> {
        crate::unsupported()
    }

    pub(super) fn set_cpu_sets(_ids: &[u32]) -> io::Result<()> {
        crate::unsupported()
    }

    pub(super) fn lock_min_working_set(_extra_mb: usize) -> io::Result<()> {
        crate::unsupported()
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::ptr;

    use windows_sys::Win32::System::Memory::{
        QUOTA_LIMITS_HARDWS_MAX_DISABLE, QUOTA_LIMITS_HARDWS_MIN_ENABLE, SetProcessWorkingSetSizeEx,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessWorkingSetSize, HIGH_PRIORITY_CLASS,
        PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION, PROCESS_POWER_THROTTLING_STATE,
        ProcessPowerThrottling, SetPriorityClass, SetProcessDefaultCpuSets, SetProcessInformation,
    };

    use crate::win::check;

    pub(super) fn set_high_priority() -> io::Result<()> {
        // SAFETY: the current-process pseudo handle and a documented class.
        check(unsafe { SetPriorityClass(GetCurrentProcess(), HIGH_PRIORITY_CLASS) })
    }

    pub(super) fn disable_power_throttling() -> io::Result<()> {
        // A bit in ControlMask with the same bit clear in StateMask turns
        // that throttling off.
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED
                | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
            StateMask: 0,
        };
        // SAFETY: `state` is the structure of this information class, with
        // its size; the call only reads it.
        check(unsafe {
            SetProcessInformation(
                GetCurrentProcess(),
                ProcessPowerThrottling,
                (&raw const state).cast(),
                size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            )
        })
    }

    pub(super) fn set_cpu_sets(ids: &[u32]) -> io::Result<()> {
        let count = u32::try_from(ids.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many CPU Set ids"))?;
        let list = if ids.is_empty() {
            ptr::null()
        } else {
            ids.as_ptr()
        };
        // SAFETY: `list` is null with a count of 0 (clears the default) or
        // points to `count` ids that the call only reads.
        check(unsafe { SetProcessDefaultCpuSets(GetCurrentProcess(), list, count) })
    }

    pub(super) fn lock_min_working_set(extra_mb: usize) -> io::Result<()> {
        let mut min = 0usize;
        let mut max = 0usize;
        // SAFETY: the current-process pseudo handle and two writable sizes.
        check(unsafe { GetProcessWorkingSetSize(GetCurrentProcess(), &mut min, &mut max) })?;
        let (min, max) = super::working_set_target(min, max, extra_mb);
        // SAFETY: the current-process pseudo handle and documented flags.
        check(unsafe {
            SetProcessWorkingSetSizeEx(
                GetCurrentProcess(),
                min,
                max,
                QUOTA_LIMITS_HARDWS_MIN_ENABLE | QUOTA_LIMITS_HARDWS_MAX_DISABLE,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[cfg(not(windows))]
    #[test]
    fn every_setting_is_unsupported_off_windows() {
        use crate::kind;
        use std::io::ErrorKind::Unsupported;

        assert_eq!(kind(set_high_priority()), Some(Unsupported));
        assert_eq!(kind(disable_power_throttling()), Some(Unsupported));
        assert_eq!(kind(set_cpu_sets(&[256, 257])), Some(Unsupported));
        assert_eq!(kind(lock_min_working_set(64)), Some(Unsupported));
    }

    #[cfg(windows)]
    #[test]
    fn the_settings_apply_to_our_own_process() {
        use windows_sys::Win32::System::Memory::{
            GetProcessWorkingSetSizeEx, QUOTA_LIMITS_HARDWS_MIN_ENABLE,
        };
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcess, GetPriorityClass, HIGH_PRIORITY_CLASS,
        };

        set_high_priority().unwrap();
        // SAFETY: the current-process pseudo handle.
        assert_eq!(
            unsafe { GetPriorityClass(GetCurrentProcess()) },
            HIGH_PRIORITY_CLASS
        );
        disable_power_throttling().unwrap();
        set_cpu_sets(&[]).unwrap();

        let working_set = || {
            let (mut min, mut max, mut flags) = (0usize, 0usize, 0u32);
            // SAFETY: the current-process pseudo handle and writable outputs.
            let ok = unsafe {
                GetProcessWorkingSetSizeEx(GetCurrentProcess(), &mut min, &mut max, &mut flags)
            };
            assert_ne!(ok, 0);
            (min, max, flags)
        };
        let (min, max, _) = working_set();
        lock_min_working_set(16).unwrap();
        let (min2, max2, flags2) = working_set();
        assert_eq!(min2, min + 16 * MIB);
        assert_eq!(max2, max + 16 * MIB);
        assert_ne!(flags2 & QUOTA_LIMITS_HARDWS_MIN_ENABLE, 0);
    }
}
