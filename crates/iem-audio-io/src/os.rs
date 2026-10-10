//! Windows process placement, priority, clock and trace markers (S1c design
//! note §4.1, §6.2 L5) for the S1a spike only: the spike's `Host` path in
//! `asio/spike.rs` and `examples/asio_spike/`. The S6 engine places and
//! prioritises itself through `iem_win::power` and never calls this module.
//! The driver's callback thread is never re-prioritised: it inherits the
//! process's default CPU Set like every other thread of the process, nothing
//! else.

use core::ffi::c_void;
use core::ptr;
use std::io;

use windows_sys::Win32::System::Diagnostics::Etw::{
    EventRegister, EventUnregister, EventWriteString, REGHANDLE,
};
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_sys::Win32::System::SystemInformation::{
    GetSystemCpuSetInformation, SYSTEM_CPU_SET_INFORMATION, SYSTEM_CPU_SET_INFORMATION_REALTIME,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessorNumber, GetCurrentThread, GetCurrentThreadId,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION, PROCESS_POWER_THROTTLING_STATE,
    ProcessPowerThrottling, SetProcessDefaultCpuSets, SetProcessInformation, SetThreadPriority,
    SetThreadSelectedCpuSets, THREAD_PRIORITY_TIME_CRITICAL,
};
use windows_sys::core::{BOOL, GUID};

use crate::cpuset::{self, CpuSet};

/// The trace-marker provider; `scripts/pc-tuning/IemMeasure.psm1` enables it
/// by this GUID.
pub const MARKER_PROVIDER: GUID = GUID::from_u128(0x3b6c_1e0a_5d2f_4c8e_9a71_0e4f_2d9b_8c11);
/// ETW level "information".
const LEVEL_INFO: u8 = 4;

fn check(ok: BOOL, what: &str) -> io::Result<()> {
    if ok == 0 {
        let e = io::Error::last_os_error();
        return Err(io::Error::new(e.kind(), format!("{what}: {e}")));
    }
    Ok(())
}

fn len32(ids: &[u32]) -> u32 {
    u32::try_from(ids.len()).unwrap_or(u32::MAX)
}

fn list(ids: &[u32]) -> *const u32 {
    if ids.is_empty() {
        ptr::null()
    } else {
        ids.as_ptr()
    }
}

/// The system's CPU Sets (every group).
pub fn system_cpu_sets() -> io::Result<Vec<CpuSet>> {
    let mut len = 0_u32;
    // SAFETY: a null buffer of length 0 only asks for the needed length.
    unsafe { GetSystemCpuSetInformation(ptr::null_mut(), 0, &mut len, ptr::null_mut(), 0) };
    let entry = size_of::<SYSTEM_CPU_SET_INFORMATION>();
    let count = usize::try_from(len).unwrap_or(0).div_ceil(entry).max(1);
    let mut buf = vec![SYSTEM_CPU_SET_INFORMATION::default(); count];
    let bytes = u32::try_from(count.saturating_mul(entry)).unwrap_or(u32::MAX);
    // SAFETY: `buf` holds `bytes` bytes of aligned entries for the call.
    let ok = unsafe {
        GetSystemCpuSetInformation(buf.as_mut_ptr(), bytes, &mut len, ptr::null_mut(), 0)
    };
    check(ok, "GetSystemCpuSetInformation")?;
    let filled = usize::try_from(len).unwrap_or(0) / entry;
    Ok(buf
        .iter()
        .take(filled)
        // Type 0 (CpuSetInformation) entries of the size this build knows.
        .filter(|e| e.Type == 0 && usize::try_from(e.Size).ok() == Some(entry))
        .map(|e| {
            // SAFETY: a Type 0 entry holds the CpuSet member of the union.
            let c = unsafe { e.Anonymous.CpuSet };
            // SAFETY: AllFlags is the byte view of the flags union.
            let flags = unsafe { c.Anonymous1.AllFlags };
            CpuSet {
                id: c.Id,
                group: c.Group,
                lp: c.LogicalProcessorIndex,
                core: c.CoreIndex,
                realtime: u32::from(flags) & SYSTEM_CPU_SET_INFORMATION_REALTIME != 0,
            }
        })
        .collect())
}

fn ids(lps: &[u8]) -> io::Result<Vec<u32>> {
    cpuset::ids_for(lps, &system_cpu_sets()?).map_err(io::Error::other)
}

/// Makes `lps` this process's default CPU Set: every thread without its own
/// selection (the driver's callback thread included) runs there. Empty = no
/// default. Returns the IDs.
pub fn set_process_cpus(lps: &[u8]) -> io::Result<Vec<u32>> {
    let ids = ids(lps)?;
    // SAFETY: `ids` lives for the call; a null list with count 0 clears the default.
    let ok = unsafe { SetProcessDefaultCpuSets(GetCurrentProcess(), list(&ids), len32(&ids)) };
    check(ok, "SetProcessDefaultCpuSets").map(|()| ids)
}

/// Selects `lps` for the calling thread. Returns the IDs.
pub fn set_thread_cpus(lps: &[u8]) -> io::Result<Vec<u32>> {
    let ids = ids(lps)?;
    // SAFETY: as in `set_process_cpus`, for the pseudo-handle of this thread.
    let ok = unsafe { SetThreadSelectedCpuSets(GetCurrentThread(), list(&ids), len32(&ids)) };
    check(ok, "SetThreadSelectedCpuSets").map(|()| ids)
}

/// Never EcoQoS, and timer-resolution requests are always honoured (Windows
/// 11 ignores them for hidden processes otherwise).
pub fn disable_power_throttling() -> io::Result<()> {
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED
            | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        StateMask: 0,
    };
    let size = u32::try_from(size_of::<PROCESS_POWER_THROTTLING_STATE>()).unwrap_or(0);
    // SAFETY: `state` is a valid PROCESS_POWER_THROTTLING_STATE of `size` bytes.
    let ok = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            ptr::from_ref(&state).cast::<c_void>(),
            size,
        )
    };
    check(ok, "SetProcessInformation(ProcessPowerThrottling)")
}

/// The calling thread at TIME_CRITICAL (the hwlat scanner only).
pub fn set_thread_time_critical() -> io::Result<()> {
    // SAFETY: the pseudo-handle of the calling thread and a valid priority.
    let ok = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) };
    check(ok, "SetThreadPriority")
}

/// The logical processor the caller runs on (no system call).
pub fn current_processor() -> u32 {
    // SAFETY: no arguments; reads the processor number.
    unsafe { GetCurrentProcessorNumber() }
}

/// The caller's thread id (no system call).
pub fn current_thread_id() -> u32 {
    // SAFETY: no arguments; reads the thread environment block.
    unsafe { GetCurrentThreadId() }
}

/// (QPC count, QPC frequency).
pub fn qpc() -> io::Result<(i64, i64)> {
    let (mut count, mut freq) = (0_i64, 0_i64);
    // SAFETY: both out-parameters are valid for the calls.
    check(
        unsafe { QueryPerformanceCounter(&mut count) },
        "QueryPerformanceCounter",
    )?;
    // SAFETY: as above.
    check(
        unsafe { QueryPerformanceFrequency(&mut freq) },
        "QueryPerformanceFrequency",
    )?;
    Ok((count, freq))
}

/// Trace markers: one string event per glitch on [`MARKER_PROVIDER`]; cheap
/// when no trace session listens.
pub struct Markers(REGHANDLE);

impl Markers {
    pub fn register() -> io::Result<Self> {
        let mut handle: REGHANDLE = 0;
        // SAFETY: the GUID and the out-handle outlive the call; no callback.
        let status = unsafe { EventRegister(&MARKER_PROVIDER, None, ptr::null(), &mut handle) };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(
                i32::try_from(status).unwrap_or(-1),
            ));
        }
        Ok(Self(handle))
    }

    pub fn write(&self, text: &str) {
        let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
        // SAFETY: `wide` is NUL-terminated and lives for the call.
        unsafe { EventWriteString(self.0, LEVEL_INFO, 0, wide.as_ptr()) };
    }
}

impl Drop for Markers {
    fn drop(&mut self) {
        // SAFETY: the handle came from EventRegister and is released once.
        unsafe { EventUnregister(self.0) };
    }
}
