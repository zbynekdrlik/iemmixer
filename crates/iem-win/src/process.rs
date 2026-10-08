//! Process queries (S6 design note §3, §5.2, §5.3): processes by image name,
//! the holders of a module (the card's driver DLL), a process's start time
//! and image path, a handle to wait on a process and read its exit code, the
//! owner of a listening TCP port, and the system's boot time. Nothing here
//! ends a process: a caller opens a [`Handle`], asks the process to stop by
//! its own route, and then waits on the handle.
//!
//! Image and module names compare without ASCII case.

use std::io;
use std::time::{Duration, SystemTime};

/// The processes that have a module loaded (see [`module_scan`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModuleScan {
    /// `(pid, image name)` of every process with the module loaded.
    pub holders: Vec<(u32, String)>,
    /// Processes whose modules could not be listed (access denied, protected
    /// or already gone); they are skipped, not counted as holders.
    pub skipped: u32,
}

/// Every process as `(pid, image name)`, from one snapshot: the guard's
/// once-a-second look at the process list (S6 design note §5.1, P10).
pub fn list() -> io::Result<Vec<(u32, String)>> {
    imp::list()
}

/// Whether a process with this image name (e.g. `reaper.exe`) exists.
pub fn exists(image: &str) -> io::Result<bool> {
    imp::exists(image)
}

/// The ids of every process with this image name.
pub fn pids(image: &str) -> io::Result<Vec<u32>> {
    imp::pids(image)
}

/// `(pid, image name)` of every process that has `module` (e.g. a driver
/// DLL) loaded; processes whose modules cannot be listed are skipped.
pub fn module_holders(module: &str) -> io::Result<Vec<(u32, String)>> {
    imp::module_scan(module).map(|scan| scan.holders)
}

/// [`module_holders`] with the count of skipped processes.
pub fn module_scan(module: &str) -> io::Result<ModuleScan> {
    imp::module_scan(module)
}

/// The process's creation time in 100 ns units since 1601-01-01 (FILETIME).
/// With the pid it names one process across pid reuse.
pub fn start_time(pid: u32) -> io::Result<u64> {
    imp::start_time(pid)
}

/// The full path of the process's executable.
pub fn image_path(pid: u32) -> io::Result<String> {
    imp::image_path(pid)
}

/// The process's command line as Windows keeps it
/// (`NtQueryInformationProcess`, `ProcessCommandLineInformation`, Windows
/// 8.1 and later; limited query access is enough). The guard reads which
/// process a Windows Error Reporting report is for from it (`WerFault.exe
/// -u -p <pid> …`, #10).
pub fn command_line(pid: u32) -> io::Result<String> {
    imp::command_line(pid)
}

/// A handle to one process, opened to wait for its end and read its exit
/// code (`SYNCHRONIZE` and limited query access). Opened **before** the
/// process is asked to stop, it names that process only: a pid that ends and
/// is recycled can never answer for it (S6 design note §5.3).
#[derive(Debug)]
pub struct Handle {
    pid: u32,
    inner: imp::Handle,
}

impl Handle {
    /// Opens the running process `pid`.
    pub fn open_waitable(pid: u32) -> io::Result<Handle> {
        Ok(Handle {
            pid,
            inner: imp::Handle::open_waitable(pid)?,
        })
    }

    /// The process id the handle was opened for.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Waits up to `timeout` (whole milliseconds, never unbounded) for the
    /// process to end: its exit code once it has ended, `None` while it
    /// still runs.
    pub fn wait(&self, timeout: Duration) -> io::Result<Option<u32>> {
        self.inner.wait(crate::decide::wait_ms(timeout))
    }
}

/// The pid that listens on this TCP port (IPv4 first, then IPv6), if any.
pub fn listening(port: u16) -> io::Result<Option<u32>> {
    imp::listening(port)
}

/// When the system started: now minus the time since the start, sleep and
/// hibernation included (`GetTickCount64`). A restart moves it; a shutdown
/// with Fast Startup hibernates the kernel and may keep the earlier time,
/// which is why the guard's reboot rule also looks at the processes that run
/// (S6 design note §5.2).
pub fn boot_time() -> io::Result<SystemTime> {
    crate::decide::boot_time(SystemTime::now(), imp::since_boot()?)
}

#[cfg(not(windows))]
mod imp {
    use std::io;
    use std::time::Duration;

    use super::ModuleScan;

    pub(super) fn list() -> io::Result<Vec<(u32, String)>> {
        crate::unsupported()
    }

    pub(super) fn exists(_image: &str) -> io::Result<bool> {
        crate::unsupported()
    }

    pub(super) fn pids(_image: &str) -> io::Result<Vec<u32>> {
        crate::unsupported()
    }

    pub(super) fn module_scan(_module: &str) -> io::Result<ModuleScan> {
        crate::unsupported()
    }

    pub(super) fn start_time(_pid: u32) -> io::Result<u64> {
        crate::unsupported()
    }

    pub(super) fn image_path(_pid: u32) -> io::Result<String> {
        crate::unsupported()
    }

    pub(super) fn command_line(_pid: u32) -> io::Result<String> {
        crate::unsupported()
    }

    pub(super) fn listening(_port: u16) -> io::Result<Option<u32>> {
        crate::unsupported()
    }

    pub(super) fn since_boot() -> io::Result<Duration> {
        crate::unsupported()
    }

    /// Never opened off Windows.
    #[derive(Debug)]
    pub(super) enum Handle {}

    impl Handle {
        pub(super) fn open_waitable(_pid: u32) -> io::Result<Self> {
            crate::unsupported()
        }

        pub(super) fn wait(&self, _ms: u32) -> io::Result<Option<u32>> {
            match *self {}
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::mem::offset_of;
    use std::os::windows::io::{AsRawHandle, OwnedHandle};
    use std::ptr;
    use std::time::Duration;

    use windows_sys::Wdk::System::Threading::{
        NtQueryInformationProcess, ProcessCommandLineInformation,
    };
    use windows_sys::Win32::Foundation::{
        ERROR_BAD_LENGTH, ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_FILES, FALSE, FILETIME,
        STATUS_BUFFER_OVERFLOW, STATUS_BUFFER_TOO_SMALL, STATUS_INFO_LENGTH_MISMATCH,
        UNICODE_STRING, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID,
        MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW, PROCESSENTRY32W,
        Process32FirstW, Process32NextW, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::SystemInformation::GetTickCount64;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_ACCESS_RIGHTS,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        QueryFullProcessImageNameW, WaitForSingleObject,
    };

    use super::ModuleScan;
    use crate::win::{check, from_wide, owned, same_name};

    /// Address families of `GetExtendedTcpTable` (ws2def.h).
    const AF_INET: u32 = 2;
    const AF_INET6: u32 = 23;

    /// Every process as `(pid, image name)`, from one Toolhelp32 snapshot.
    fn processes() -> io::Result<Vec<(u32, String)>> {
        // SAFETY: a plain snapshot request; the handle is owned below.
        let snapshot = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) })?;
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut out = Vec::new();
        // SAFETY: a valid snapshot handle and an entry with dwSize set.
        let mut more = unsafe { Process32FirstW(snapshot.as_raw_handle(), &mut entry) };
        while more != 0 {
            out.push((entry.th32ProcessID, from_wide(&entry.szExeFile)));
            // SAFETY: as above.
            more = unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) };
        }
        end_of_list(out)
    }

    /// A Toolhelp32 walk ends with ERROR_NO_MORE_FILES; any other error is real.
    fn end_of_list<T>(items: Vec<T>) -> io::Result<Vec<T>> {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
            Ok(items)
        } else {
            Err(err)
        }
    }

    pub(super) fn list() -> io::Result<Vec<(u32, String)>> {
        processes()
    }

    pub(super) fn exists(image: &str) -> io::Result<bool> {
        Ok(processes()?.iter().any(|(_, name)| same_name(name, image)))
    }

    pub(super) fn pids(image: &str) -> io::Result<Vec<u32>> {
        Ok(processes()?
            .into_iter()
            .filter(|(_, name)| same_name(name, image))
            .map(|(pid, _)| pid)
            .collect())
    }

    pub(super) fn module_scan(module: &str) -> io::Result<ModuleScan> {
        let mut scan = ModuleScan::default();
        for (pid, name) in processes()? {
            // The idle process (pid 0) has no modules.
            if pid == 0 {
                continue;
            }
            match modules_of(pid) {
                Ok(modules) => {
                    if modules.iter().any(|m| same_name(m, module)) {
                        scan.holders.push((pid, name));
                    }
                }
                Err(_) => scan.skipped += 1,
            }
        }
        Ok(scan)
    }

    /// The module names of one process (64- and 32-bit modules).
    fn modules_of(pid: u32) -> io::Result<Vec<String>> {
        let snapshot = module_snapshot(pid)?;
        let mut entry = MODULEENTRY32W {
            dwSize: size_of::<MODULEENTRY32W>() as u32,
            ..Default::default()
        };
        let mut out = Vec::new();
        // SAFETY: a valid snapshot handle and an entry with dwSize set.
        let mut more = unsafe { Module32FirstW(snapshot.as_raw_handle(), &mut entry) };
        while more != 0 {
            out.push(from_wide(&entry.szModule));
            // SAFETY: as above.
            more = unsafe { Module32NextW(snapshot.as_raw_handle(), &mut entry) };
        }
        end_of_list(out)
    }

    fn module_snapshot(pid: u32) -> io::Result<OwnedHandle> {
        // ERROR_BAD_LENGTH: the module list changed during the call; the
        // documentation says to call again.
        let mut attempts = 0;
        loop {
            // SAFETY: a plain snapshot request; the handle is owned below.
            let handle =
                unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid) };
            match owned(handle) {
                Err(e) if e.raw_os_error() == Some(ERROR_BAD_LENGTH as i32) && attempts < 8 => {
                    attempts += 1;
                }
                other => return other,
            }
        }
    }

    fn open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> io::Result<OwnedHandle> {
        // SAFETY: a plain call; null is its failure, which `owned` reports.
        owned(unsafe { OpenProcess(access, FALSE, pid) })
    }

    pub(super) fn start_time(pid: u32) -> io::Result<u64> {
        let process = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: a valid process handle and four writable FILETIMEs.
        check(unsafe {
            GetProcessTimes(
                process.as_raw_handle(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        })?;
        Ok((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }

    pub(super) fn image_path(pid: u32) -> io::Result<String> {
        let process = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        let mut buf = vec![0u16; 32_768];
        let mut len = buf.len() as u32;
        // SAFETY: `buf` holds `len` units; the call writes at most that many
        // and sets `len` to the length without the NUL.
        check(unsafe {
            QueryFullProcessImageNameW(
                process.as_raw_handle(),
                PROCESS_NAME_WIN32,
                buf.as_mut_ptr(),
                &mut len,
            )
        })?;
        Ok(String::from_utf16_lossy(
            buf.get(..len as usize).unwrap_or_default(),
        ))
    }

    pub(super) fn command_line(pid: u32) -> io::Result<String> {
        let process = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        // A UNICODE_STRING followed by its text, in 8-byte aligned storage;
        // when the buffer is too small the call names the size it needs.
        let mut buf: Vec<u64> = vec![0; 64];
        for _ in 0..8 {
            let size = u32::try_from(size_of_val(buf.as_slice())).map_err(io::Error::other)?;
            let mut needed = 0u32;
            // SAFETY: a valid process handle with limited query access;
            // `buf` holds `size` writable bytes, aligned for a
            // UNICODE_STRING, and the call writes at most that many.
            let status = unsafe {
                NtQueryInformationProcess(
                    process.as_raw_handle(),
                    ProcessCommandLineInformation,
                    buf.as_mut_ptr().cast(),
                    size,
                    &mut needed,
                )
            };
            if status >= 0 {
                return unicode_text(&buf);
            }
            let too_small = matches!(
                status,
                STATUS_INFO_LENGTH_MISMATCH | STATUS_BUFFER_TOO_SMALL | STATUS_BUFFER_OVERFLOW
            );
            if !too_small || needed <= size {
                return Err(io::Error::other(format!(
                    "the command line of process {pid}: NTSTATUS {status:#010x}"
                )));
            }
            buf = vec![0; (needed as usize).div_ceil(8)];
        }
        Err(io::Error::other(format!(
            "the command line of process {pid} kept growing"
        )))
    }

    /// The text of the UNICODE_STRING at the start of `buf`, read only where
    /// it lies inside `buf` (the call writes it right after the header).
    fn unicode_text(buf: &[u64]) -> io::Result<String> {
        let bytes = size_of_val(buf);
        if bytes < size_of::<UNICODE_STRING>() {
            return Err(io::Error::other("no room for the command line's header"));
        }
        // SAFETY: `buf` holds at least one UNICODE_STRING and is aligned for
        // it (8-byte storage); the call wrote it.
        let header = unsafe { buf.as_ptr().cast::<UNICODE_STRING>().read() };
        let units = usize::from(header.Length) / 2;
        if units == 0 {
            return Ok(String::new());
        }
        let start = buf.as_ptr().addr();
        let text = header.Buffer.addr();
        let inside = text >= start && text.is_multiple_of(2) && text - start + units * 2 <= bytes;
        if !inside {
            return Err(io::Error::other("the command line lies outside its buffer"));
        }
        // SAFETY: `units` UTF-16 units at `header.Buffer` lie inside `buf`
        // (checked above) and are aligned for u16; the call wrote them.
        let wide = unsafe { std::slice::from_raw_parts(header.Buffer.cast_const(), units) };
        Ok(String::from_utf16_lossy(wide))
    }

    #[derive(Debug)]
    pub(super) struct Handle(OwnedHandle);

    impl Handle {
        pub(super) fn open_waitable(pid: u32) -> io::Result<Self> {
            open(pid, PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION).map(Self)
        }

        /// `ms` is below INFINITE (`decide::wait_ms`), so the wait is bounded.
        pub(super) fn wait(&self, ms: u32) -> io::Result<Option<u32>> {
            // SAFETY: a valid process handle with SYNCHRONIZE access.
            match unsafe { WaitForSingleObject(self.0.as_raw_handle(), ms) } {
                WAIT_OBJECT_0 => {
                    let mut code = 0u32;
                    // SAFETY: a valid process handle with query access and a
                    // writable code.
                    check(unsafe { GetExitCodeProcess(self.0.as_raw_handle(), &mut code) })?;
                    Ok(Some(code))
                }
                WAIT_TIMEOUT => Ok(None),
                _ => Err(io::Error::last_os_error()),
            }
        }
    }

    pub(super) fn since_boot() -> io::Result<Duration> {
        // SAFETY: a plain call without arguments.
        Ok(Duration::from_millis(unsafe { GetTickCount64() }))
    }

    pub(super) fn listening(port: u16) -> io::Result<Option<u32>> {
        let v4 = listeners(AF_INET)?;
        let found = rows::<MIB_TCPROW_OWNER_PID>(&v4, offset_of!(MIB_TCPTABLE_OWNER_PID, table))
            .into_iter()
            .find(|row| local_port(row.dwLocalPort) == port)
            .map(|row| row.dwOwningPid);
        if found.is_some() {
            return Ok(found);
        }
        let v6 = listeners(AF_INET6)?;
        Ok(
            rows::<MIB_TCP6ROW_OWNER_PID>(&v6, offset_of!(MIB_TCP6TABLE_OWNER_PID, table))
                .into_iter()
                .find(|row| local_port(row.dwLocalPort) == port)
                .map(|row| row.dwOwningPid),
        )
    }

    /// The low 16 bits of a table's port field hold the port in network byte
    /// order.
    fn local_port(field: u32) -> u16 {
        u16::from_be(field as u16)
    }

    /// The listener table of one address family (8-byte aligned storage).
    fn listeners(family: u32) -> io::Result<Vec<u64>> {
        let mut buf: Vec<u64> = Vec::new();
        let mut size = 0u32;
        // The table may grow between the size query and the read: ask again.
        for _ in 0..8 {
            let table: *mut core::ffi::c_void = if buf.is_empty() {
                ptr::null_mut()
            } else {
                buf.as_mut_ptr().cast()
            };
            // SAFETY: `table` is null or `buf`, which holds at least `size`
            // bytes; the call writes at most `size` bytes and otherwise
            // reports the size it needs.
            let rc = unsafe {
                GetExtendedTcpTable(
                    table,
                    &mut size,
                    FALSE,
                    family,
                    TCP_TABLE_OWNER_PID_LISTENER,
                    0,
                )
            };
            if rc == 0 {
                return Ok(buf);
            }
            if rc != ERROR_INSUFFICIENT_BUFFER {
                return Err(io::Error::from_raw_os_error(rc as i32));
            }
            buf = vec![0u64; (size as usize).div_ceil(8)];
        }
        Err(io::Error::other("the TCP listener table kept growing"))
    }

    /// The rows of a table that starts with a `u32` count and has its rows at
    /// `offset`; rows beyond the buffer are not read.
    fn rows<R: Copy>(buf: &[u64], offset: usize) -> Vec<R> {
        let bytes = size_of_val(buf);
        if bytes < size_of::<u32>() {
            return Vec::new();
        }
        let base = buf.as_ptr().cast::<u8>();
        // SAFETY: the buffer holds at least 4 bytes, aligned for u32.
        let count = unsafe { base.cast::<u32>().read() } as usize;
        let mut out = Vec::with_capacity(count.min(bytes / size_of::<R>().max(1)));
        for i in 0..count {
            let start = offset + i * size_of::<R>();
            if start + size_of::<R>() > bytes {
                break;
            }
            // SAFETY: `start..start + size_of::<R>()` lies inside `buf`; R is
            // a plain `repr(C)` row the call wrote there.
            out.push(unsafe { base.add(start).cast::<R>().read_unaligned() });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #[cfg(not(windows))]
    #[test]
    fn every_query_is_unsupported_off_windows() {
        use super::*;
        use crate::kind;
        use std::io::ErrorKind::Unsupported;

        assert_eq!(kind(list()), Some(Unsupported));
        assert_eq!(kind(exists("reaper.exe")), Some(Unsupported));
        assert_eq!(kind(pids("reaper.exe")), Some(Unsupported));
        assert_eq!(kind(module_holders("test-card.dll")), Some(Unsupported));
        assert_eq!(kind(module_scan("test-card.dll")), Some(Unsupported));
        assert_eq!(kind(start_time(4242)), Some(Unsupported));
        assert_eq!(kind(image_path(4242)), Some(Unsupported));
        assert_eq!(kind(command_line(4242)), Some(Unsupported));
        assert_eq!(kind(Handle::open_waitable(4242)), Some(Unsupported));
        assert_eq!(kind(listening(8080)), Some(Unsupported));
        assert_eq!(kind(boot_time()), Some(Unsupported));
    }

    #[cfg(windows)]
    fn own_image() -> String {
        std::env::current_exe()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    #[cfg(windows)]
    #[test]
    fn processes_are_found_by_image_name() {
        use super::*;

        assert!(!exists("definitely-not-running.exe").unwrap());
        assert!(pids("definitely-not-running.exe").unwrap().is_empty());
        let me = std::process::id();
        assert!(exists(&own_image()).unwrap());
        assert!(pids(&own_image()).unwrap().contains(&me));
        assert!(pids(&own_image().to_uppercase()).unwrap().contains(&me));
        let all = list().unwrap();
        assert!(
            all.iter()
                .any(|(pid, name)| *pid == me && name.eq_ignore_ascii_case(&own_image())),
            "{all:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn our_own_process_holds_kernel32() {
        use super::*;

        let me = std::process::id();
        let holders = module_holders("kernel32.dll").unwrap();
        assert!(holders.iter().any(|(pid, _)| *pid == me), "{holders:?}");
        let scan = module_scan("KERNEL32.DLL").unwrap();
        assert!(
            scan.holders
                .iter()
                .any(|(pid, name)| *pid == me && name.eq_ignore_ascii_case(&own_image()))
        );
        let none = module_scan("definitely-not-loaded.dll").unwrap();
        assert!(none.holders.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn start_time_and_image_path_of_our_own_process() {
        use super::*;

        let me = std::process::id();
        let first = start_time(me).unwrap();
        assert!(first > 0);
        assert_eq!(start_time(me).unwrap(), first);
        let path = image_path(me).unwrap();
        let exe = std::env::current_exe().unwrap();
        assert!(
            path.eq_ignore_ascii_case(&exe.to_string_lossy()),
            "{path} vs {exe:?}"
        );
    }

    /// The guard tells a WerFault report of REAPER's crash by its command
    /// line (`-p <pid>`, #10): our own process's and a running child's.
    #[cfg(windows)]
    #[test]
    fn the_command_line_of_our_own_process_and_of_a_child_is_read() {
        use super::*;

        let me = command_line(std::process::id()).unwrap();
        assert!(
            me.to_ascii_lowercase()
                .contains(&own_image().to_ascii_lowercase()),
            "{me}"
        );
        // ping ends by itself after about two seconds; its arguments are
        // read while it runs.
        let mut child = std::process::Command::new("ping")
            .args(["-n", "3", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let line = command_line(child.id());
        let _ = child.wait();
        let line = line.unwrap();
        assert!(line.contains("-n 3 127.0.0.1"), "{line}");
        // No process has this pid (pids are multiples of 4).
        assert!(command_line(u32::MAX - 2).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn a_waitable_handle_times_out_on_a_running_process_and_reads_the_exit_code() {
        use super::*;

        let me = Handle::open_waitable(std::process::id()).unwrap();
        assert_eq!(me.pid(), std::process::id());
        assert_eq!(me.wait(Duration::from_millis(10)).unwrap(), None);
        for code in [0u32, 7] {
            // The Child keeps its own handle open, so the pid stays this
            // process's until both handles are closed.
            let exit = format!("exit {code}");
            let mut child = std::process::Command::new("cmd")
                .args(["/C", exit.as_str()])
                .spawn()
                .unwrap();
            let handle = Handle::open_waitable(child.id()).unwrap();
            assert_eq!(handle.pid(), child.id());
            assert_eq!(handle.wait(Duration::from_secs(10)).unwrap(), Some(code));
            assert_eq!(handle.wait(Duration::ZERO).unwrap(), Some(code));
            assert_eq!(child.wait().unwrap().code(), Some(code as i32));
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_boot_time_is_in_the_past_and_stable() {
        use super::*;

        let first = boot_time().unwrap();
        assert!(first < SystemTime::now());
        let second = boot_time().unwrap();
        let apart = match second.duration_since(first) {
            Ok(d) => d,
            Err(e) => e.duration(),
        };
        assert!(apart < Duration::from_secs(1), "{apart:?}");
    }

    #[cfg(windows)]
    #[test]
    fn listening_names_the_owner_of_a_bound_port() {
        use super::*;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert_eq!(listening(port).unwrap(), Some(std::process::id()));
        drop(listener);
        assert_eq!(listening(port).unwrap(), None);
    }
}
