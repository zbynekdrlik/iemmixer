//! Window messages (S6 design note §3, §5.3): a window of a class owned by a
//! process, the owner's menu command posted to it, a dialog check, the
//! session-end window of the engine's owner thread, and the notification on
//! the tray's icon (S6 plan Task 11).

use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// The class of standard dialog boxes.
pub const DIALOG_CLASS: &str = "#32770";

/// The top-level window (hidden ones included) of `class` owned by `pid`.
pub fn find_owned(class: &str, pid: u32) -> io::Result<Option<isize>> {
    imp::find_owned(class, pid)
}

/// Posts `WM_COMMAND` with `id` (a menu item: wParam = id, lParam = 0) to
/// `hwnd`, as the window's own menu does when the item is clicked. Refuses
/// the null window (a thread message) and the broadcast handle.
pub fn post_command(hwnd: isize, id: u16) -> io::Result<()> {
    crate::decide::one_window(hwnd)?;
    imp::post_command(hwnd, id)
}

/// Whether `pid` owns a visible top-level dialog box (class `#32770`).
pub fn has_dialog(pid: u32) -> io::Result<bool> {
    imp::has_dialog(pid)
}

/// The icon ids [`balloon`] tries.
pub const ICON_IDS: u32 = 64;

/// Shows a notification (a balloon; a toast on Windows 10 and later) with a
/// warning icon on the notification-area icon of `hwnd`, the window the
/// tray library created for that icon. `title` and `text` are cut to the
/// shell's 63 and 255 UTF-16 units.
///
/// The tray library keeps its icon's id private, so the ids 1 to
/// [`ICON_IDS`] are tried in turn: the window holds one icon, and the shell
/// refuses an id the window does not hold without changing anything.
/// `NotFound` when no id matched (no icon, or no shell).
pub fn balloon(hwnd: isize, title: &str, text: &str) -> io::Result<()> {
    crate::decide::one_window(hwnd)?;
    imp::balloon(
        hwnd,
        &crate::decide::fixed_wide::<64>(title),
        &crate::decide::fixed_wide::<256>(text),
    )
}

/// A hidden top-level window on the calling thread for the end of the
/// Windows session (S6 design note §3). It answers `WM_QUERYENDSESSION` with
/// TRUE; on `WM_ENDSESSION` with TRUE it sets `ended` and runs `on_end` once,
/// while a `ShutdownBlockReasonCreate` text tells the user what is finishing.
///
/// The thread must pump its messages (the engine's owner thread does). The
/// window belongs to that thread, so the type is not `Send`; dropping it
/// destroys the window.
#[derive(Debug)]
pub struct SessionEndWindow {
    inner: imp::SessionEndWindow,
}

impl SessionEndWindow {
    /// Creates the window. `reason` is the text Windows shows while
    /// `on_end` runs.
    pub fn create(
        reason: &str,
        ended: Arc<AtomicBool>,
        on_end: impl FnOnce() + 'static,
    ) -> io::Result<Self> {
        imp::SessionEndWindow::create(reason, ended, Box::new(on_end)).map(|inner| Self { inner })
    }

    /// The window handle (for logs and tests).
    pub fn hwnd(&self) -> isize {
        self.inner.hwnd()
    }
}

#[cfg(not(windows))]
mod imp {
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    pub(super) fn find_owned(_class: &str, _pid: u32) -> io::Result<Option<isize>> {
        crate::unsupported()
    }

    pub(super) fn post_command(_hwnd: isize, _id: u16) -> io::Result<()> {
        crate::unsupported()
    }

    pub(super) fn has_dialog(_pid: u32) -> io::Result<bool> {
        crate::unsupported()
    }

    pub(super) fn balloon(_hwnd: isize, _title: &[u16; 64], _text: &[u16; 256]) -> io::Result<()> {
        crate::unsupported()
    }

    /// Never created off Windows.
    #[derive(Debug)]
    pub(super) enum SessionEndWindow {}

    impl SessionEndWindow {
        pub(super) fn create(
            _reason: &str,
            _ended: Arc<AtomicBool>,
            _on_end: Box<dyn FnOnce()>,
        ) -> io::Result<Self> {
            crate::unsupported()
        }

        pub(super) fn hwnd(&self) -> isize {
            match *self {}
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::cell::RefCell;
    use std::io;
    use std::ptr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows_sys::Win32::Foundation::{
        ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, TRUE, WPARAM,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Shutdown::{
        ShutdownBlockReasonCreate, ShutdownBlockReasonDestroy,
    };
    use windows_sys::Win32::UI::Shell::{
        NIF_INFO, NIIF_WARNING, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, EnumWindows, GWLP_USERDATA, GetClassNameW,
        GetWindowLongPtrW, GetWindowThreadProcessId, IsWindowVisible, PostMessageW,
        RegisterClassExW, SetWindowLongPtrW, WM_COMMAND, WM_ENDSESSION, WM_QUERYENDSESSION,
        WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
    };
    use windows_sys::core::BOOL;

    use crate::win::{check, from_wide, same_name, wide};

    /// One top-level window as `EnumWindows` reports it.
    struct TopLevel {
        hwnd: HWND,
        pid: u32,
        class: String,
        visible: bool,
    }

    fn top_level() -> io::Result<Vec<TopLevel>> {
        let mut out: Vec<TopLevel> = Vec::new();
        // SAFETY: `collect` runs synchronously inside EnumWindows and writes
        // only to `out` through the pointer, which outlives the call.
        check(unsafe { EnumWindows(Some(collect), (&raw mut out) as LPARAM) })?;
        Ok(out)
    }

    unsafe extern "system" fn collect(hwnd: HWND, out: LPARAM) -> BOOL {
        // SAFETY: `out` is the `Vec<TopLevel>` of `top_level`, alive and not
        // otherwise borrowed during the enumeration.
        let out = unsafe { &mut *(out as *mut Vec<TopLevel>) };
        let mut pid = 0u32;
        // SAFETY: a window handle from the enumeration and a writable pid.
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        let mut class = [0u16; 256];
        // SAFETY: `class` holds 256 units; the call writes at most that many.
        let len = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), 256) };
        let class = from_wide(
            class
                .get(..usize::try_from(len).unwrap_or(0))
                .unwrap_or_default(),
        );
        // SAFETY: a window handle from the enumeration.
        let visible = unsafe { IsWindowVisible(hwnd) } != 0;
        out.push(TopLevel {
            hwnd,
            pid,
            class,
            visible,
        });
        TRUE
    }

    pub(super) fn find_owned(class: &str, pid: u32) -> io::Result<Option<isize>> {
        Ok(top_level()?
            .into_iter()
            .find(|w| w.pid == pid && same_name(&w.class, class))
            .map(|w| w.hwnd as isize))
    }

    pub(super) fn post_command(hwnd: isize, id: u16) -> io::Result<()> {
        // SAFETY: PostMessageW validates the handle; WM_COMMAND carries a
        // menu id (high word 0) and no control handle.
        check(unsafe { PostMessageW(hwnd as HWND, WM_COMMAND, WPARAM::from(id), 0) })
    }

    pub(super) fn has_dialog(pid: u32) -> io::Result<bool> {
        Ok(top_level()?
            .iter()
            .any(|w| w.pid == pid && w.visible && same_name(&w.class, super::DIALOG_CLASS)))
    }

    pub(super) fn balloon(hwnd: isize, title: &[u16; 64], text: &[u16; 256]) -> io::Result<()> {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd as HWND,
            uFlags: NIF_INFO,
            szInfo: *text,
            szInfoTitle: *title,
            dwInfoFlags: NIIF_WARNING,
            ..Default::default()
        };
        for id in 1..=super::ICON_IDS {
            data.uID = id;
            // SAFETY: a complete NOTIFYICONDATAW carrying its own size, with
            // NUL-terminated texts; the shell reads it during the call and
            // changes only the balloon of the icon (hwnd, id), if one exists.
            if unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) } != 0 {
                return Ok(());
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the window holds no notification-area icon",
        ))
    }

    const CLASS: &str = "iemmixer-session-end";

    struct State {
        ended: Arc<AtomicBool>,
        reason: Vec<u16>,
        /// Taken on the first `WM_ENDSESSION(TRUE)`; a nested or later one
        /// finds it empty.
        on_end: RefCell<Option<Box<dyn FnOnce()>>>,
    }

    #[derive(Debug)]
    pub(super) struct SessionEndWindow {
        hwnd: HWND,
        state: *mut State,
    }

    impl SessionEndWindow {
        pub(super) fn create(
            reason: &str,
            ended: Arc<AtomicBool>,
            on_end: Box<dyn FnOnce()>,
        ) -> io::Result<Self> {
            let class = wide(CLASS);
            // SAFETY: the module handle of this executable (null name).
            let instance = unsafe { GetModuleHandleW(ptr::null()) };
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(session_proc),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..Default::default()
            };
            // SAFETY: a complete class description; the system copies the
            // name. A second window of the process finds the class there.
            if unsafe { RegisterClassExW(&wc) } == 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() != Some(ERROR_CLASS_ALREADY_EXISTS as i32) {
                    return Err(err);
                }
            }
            // SAFETY: a registered class; no parent, so the window is
            // top-level and receives the session-end messages; never shown.
            let hwnd = unsafe {
                CreateWindowExW(
                    WS_EX_TOOLWINDOW,
                    class.as_ptr(),
                    ptr::null(),
                    WS_OVERLAPPED,
                    0,
                    0,
                    0,
                    0,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    instance,
                    ptr::null(),
                )
            };
            if hwnd.is_null() {
                return Err(io::Error::last_os_error());
            }
            let state = Box::into_raw(Box::new(State {
                ended,
                reason: wide(reason),
                on_end: RefCell::new(Some(on_end)),
            }));
            // SAFETY: our own window; the pointer stays valid until Drop
            // clears it and destroys the window.
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize) };
            Ok(Self { hwnd, state })
        }

        pub(super) fn hwnd(&self) -> isize {
            self.hwnd as isize
        }
    }

    impl Drop for SessionEndWindow {
        fn drop(&mut self) {
            // SAFETY: our window, dropped on its own thread (the type is not
            // Send). Once the pointer is cleared and the window destroyed no
            // message reaches `session_proc` with this state, so the box is
            // freed last.
            unsafe {
                SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
                DestroyWindow(self.hwnd);
                drop(Box::from_raw(self.state));
            }
        }
    }

    unsafe extern "system" fn session_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_QUERYENDSESSION => 1,
            WM_ENDSESSION => {
                // SAFETY: GWLP_USERDATA holds 0 or the live State of this
                // window (cleared before the State is freed).
                let state =
                    unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const State).as_ref() };
                if let Some(state) = state.filter(|_| wparam != 0) {
                    session_ends(hwnd, state);
                }
                0
            }
            // SAFETY: default handling of every other message.
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    fn session_ends(hwnd: HWND, state: &State) {
        state.ended.store(true, Ordering::SeqCst);
        // The borrow ends with this statement, so a nested WM_ENDSESSION
        // while `on_end` runs finds `None` instead of a held borrow.
        let on_end = state.on_end.borrow_mut().take();
        if let Some(on_end) = on_end {
            // SAFETY: our own window and a NUL-terminated reason.
            unsafe { ShutdownBlockReasonCreate(hwnd, state.reason.as_ptr()) };
            on_end();
            // SAFETY: our own window.
            unsafe { ShutdownBlockReasonDestroy(hwnd) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind;

    #[test]
    fn a_command_is_never_posted_to_the_null_or_broadcast_handle() {
        assert_eq!(kind(post_command(0, 7)), Some(io::ErrorKind::InvalidInput));
        assert_eq!(
            kind(post_command(0xFFFF, 7)),
            Some(io::ErrorKind::InvalidInput)
        );
    }

    #[test]
    fn a_balloon_never_goes_to_the_null_or_broadcast_handle() {
        assert_eq!(
            kind(balloon(0, "iemmixer", "alarm")),
            Some(io::ErrorKind::InvalidInput)
        );
        assert_eq!(
            kind(balloon(0xFFFF, "iemmixer", "alarm")),
            Some(io::ErrorKind::InvalidInput)
        );
    }

    /// A made-up handle holds no notification-area icon, with or without a
    /// shell in the session: no id matches and nothing is shown.
    #[cfg(windows)]
    #[test]
    fn a_window_without_an_icon_gets_no_balloon() {
        assert_eq!(
            kind(balloon(0x1234, "iemmixer", "alarm")),
            Some(io::ErrorKind::NotFound)
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn every_window_call_is_unsupported_off_windows() {
        use std::io::ErrorKind::Unsupported;

        assert_eq!(kind(find_owned("TestClass", 4242)), Some(Unsupported));
        assert_eq!(kind(post_command(0x1234, 7)), Some(Unsupported));
        assert_eq!(kind(post_command(0xFFFE, 7)), Some(Unsupported));
        assert_eq!(kind(post_command(0x1_0000, 7)), Some(Unsupported));
        assert_eq!(kind(has_dialog(4242)), Some(Unsupported));
        assert_eq!(
            kind(balloon(0x1234, "iemmixer", "alarm")),
            Some(Unsupported)
        );
        let ended = Arc::new(AtomicBool::new(false));
        assert_eq!(
            kind(SessionEndWindow::create("saving", ended, || {})),
            Some(Unsupported)
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_session_end_window_runs_its_job_once_and_is_found_by_class() {
        use std::cell::Cell;
        use std::rc::Rc;
        use std::sync::atomic::Ordering;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SendMessageW, WM_ENDSESSION, WM_QUERYENDSESSION,
        };

        let ended = Arc::new(AtomicBool::new(false));
        let runs = Rc::new(Cell::new(0u32));
        let counter = Rc::clone(&runs);
        let window =
            SessionEndWindow::create("iemmixer saves its state", Arc::clone(&ended), move || {
                counter.set(counter.get() + 1);
            })
            .unwrap();
        let hwnd = window.hwnd() as windows_sys::Win32::Foundation::HWND;
        let me = std::process::id();

        assert_eq!(
            find_owned("iemmixer-session-end", me).unwrap(),
            Some(window.hwnd())
        );
        assert_eq!(find_owned("iemmixer-session-end", me + 1).unwrap(), None);
        assert!(!has_dialog(me).unwrap());
        post_command(window.hwnd(), 7).unwrap();

        // SAFETY: our own window on this thread (SendMessageW calls its
        // procedure directly).
        let answer = unsafe { SendMessageW(hwnd, WM_QUERYENDSESSION, 0, 0) };
        assert_eq!(answer, 1);
        // SAFETY: as above; FALSE: the session does not end.
        unsafe { SendMessageW(hwnd, WM_ENDSESSION, 0, 0) };
        assert!(!ended.load(Ordering::SeqCst));
        assert_eq!(runs.get(), 0);
        // SAFETY: as above; TRUE: the session ends.
        unsafe { SendMessageW(hwnd, WM_ENDSESSION, 1, 0) };
        assert!(ended.load(Ordering::SeqCst));
        assert_eq!(runs.get(), 1);
        // SAFETY: as above.
        unsafe { SendMessageW(hwnd, WM_ENDSESSION, 1, 0) };
        assert_eq!(runs.get(), 1);

        let handle = window.hwnd();
        drop(window);
        assert_eq!(find_owned("iemmixer-session-end", me).unwrap(), None);
        assert!(post_command(handle, 7).is_err());
    }
}
