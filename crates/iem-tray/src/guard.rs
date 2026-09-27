//! The tray's subscription to the guard (S6 plan Task 11, F27; design note
//! §5.1): `Request::Subscribe` on the guard pipe, then one frame per change
//! (`iem_guard::proto::Update`). What the tray shows is decided in
//! `iem_guard::view` (tested and mutated on Linux); this module only carries
//! it to the icon. Nothing here ends a process: the guard's `Quit` ends this
//! tray, and a missing guard only greys the tooltip until it answers again.

use std::thread;
use std::time::Duration;

use iem_guard::proto::{self, FrameError, Reply, Request, Update};
use iem_guard::view::{self, Seen};
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, Stream};
use tauri::AppHandle;
use tauri::tray::TrayIcon;

/// The wait before the next connection while no guard answers.
const RETRY: Duration = Duration::from_secs(2);

/// How a subscription ended.
enum Ended {
    /// The guard asked the tray to quit.
    Quit,
    /// The guard closed the pipe (it stopped or hands over to a new guard).
    Closed,
}

/// Starts the subscription thread. `icon` shows the tooltip, `hwnd` (the
/// icon's window) the alarm notifications; either may be missing, the guard's
/// `Quit` still ends the tray.
pub fn spawn(app: AppHandle, icon: Option<TrayIcon>, hwnd: Option<isize>) {
    let started = thread::Builder::new()
        .name("guard-subscription".into())
        .spawn(move || run(&app, icon.as_ref(), hwnd));
    if let Err(e) = started {
        tracing::error!(error = %e, "the guard subscription did not start: no status in the tray");
    }
}

fn run(app: &AppHandle, icon: Option<&TrayIcon>, hwnd: Option<isize>) {
    let mut seen = Seen::default();
    // Whether the current outage is logged already (one line per outage, not
    // one per attempt).
    let mut logged = false;
    loop {
        match subscribe(icon, hwnd, &mut seen, &mut logged) {
            Ok(Ended::Quit) => {
                tracing::info!("the guard asked the tray to quit");
                app.exit(0);
                return;
            }
            Ok(Ended::Closed) => {
                tracing::warn!("the guard closed the subscription; reconnecting");
                logged = true;
            }
            Err(e) => {
                if !logged {
                    tracing::warn!(error = %e, "no guard subscription; retrying every 2 s");
                    logged = true;
                }
            }
        }
        show_tooltip(icon, &view::tooltip(None));
        thread::sleep(RETRY);
    }
}

/// One subscription: connect, subscribe, then show every update until the
/// guard quits the tray or closes the pipe. An error before the first
/// update is a missing guard; one after it a broken pipe.
fn subscribe(
    icon: Option<&TrayIcon>,
    hwnd: Option<isize>,
    seen: &mut Seen,
    logged: &mut bool,
) -> Result<Ended, FrameError> {
    let name = proto::NAME.to_ns_name::<GenericNamespaced>()?;
    let mut pipe = Stream::connect(name)?;
    proto::write_frame(&mut pipe, &Request::Subscribe)?;
    tracing::info!("subscribed to the guard");
    *logged = false;
    loop {
        match proto::read_update(&mut pipe) {
            Ok(Update::Quit) => return Ok(Ended::Quit),
            Ok(Update::State(reply)) => show(icon, hwnd, seen, &reply),
            Err(FrameError::Closed) => return Ok(Ended::Closed),
            Err(e) => return Err(e),
        }
    }
}

/// The tooltip for `reply`, and one notification for its new alarms.
fn show(icon: Option<&TrayIcon>, hwnd: Option<isize>, seen: &mut Seen, reply: &Reply) {
    let tip = view::tooltip(Some(reply));
    tracing::debug!(tooltip = %tip, "guard update");
    show_tooltip(icon, &tip);
    let fresh = seen.fresh(reply);
    for alarm in &fresh {
        tracing::warn!(id = alarm.id, text = %alarm.text, "new guard alarm");
    }
    let Some((title, text)) = view::notice(&fresh) else {
        return;
    };
    match hwnd {
        Some(hwnd) => {
            if let Err(e) = iem_win::window::balloon(hwnd, &title, &text) {
                tracing::warn!(error = %e, "the alarm notification did not show");
            }
        }
        None => tracing::warn!("no tray icon window: the alarm shows in the tooltip only"),
    }
}

fn show_tooltip(icon: Option<&TrayIcon>, text: &str) {
    if let Some(icon) = icon
        && let Err(e) = icon.set_tooltip(Some(text))
    {
        tracing::warn!(error = %e, "the tooltip did not update");
    }
}
