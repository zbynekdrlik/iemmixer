//! System tray icon and menu management

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

/// Icon size in pixels
const ICON_SIZE: u32 = 16;

/// The site config's links (`iem_core::Config::mixer_url`, `share_url`).
#[derive(Debug, Clone)]
pub struct Links {
    /// Open Mixer: the local server (`http://localhost:<port>`), a secure
    /// context for Copy URL's clipboard write in the same window.
    pub mixer: String,
    /// Copy URL: the public host, else the LAN URL; `None` disables the item.
    pub share: Option<String>,
}

/// Set up the tray icon with its menu. The tooltip says that no guard has
/// answered yet; the guard subscription ([`crate::guard`]) replaces it.
pub fn setup_tray(app: &AppHandle, links: Links) -> tauri::Result<TrayIcon> {
    // Display full version with git hash for unique deploy identification
    let version_label = format!("IEM Mixer v{}", iem_core::full_version());
    let version_item = MenuItem::with_id(app, "version", version_label, false, None::<&str>)?;

    let separator1 = PredefinedMenuItem::separator(app)?;

    // "Open Mixer" opens the mixer's landing page in the main window.
    let open_mixer_item = MenuItem::with_id(app, "open_mixer", "Open Mixer", true, None::<&str>)?;

    // Combined URL display + copy (click to copy); disabled without a
    // configured public host or LAN URL.
    let (copy_label, copy_enabled) = match &links.share {
        Some(url) => (format!("📋 {url}"), true),
        None => ("No public URL configured".to_string(), false),
    };
    let copy_url_item = MenuItem::with_id(app, "copy_url", copy_label, copy_enabled, None::<&str>)?;

    let separator2 = PredefinedMenuItem::separator(app)?;

    // Exit ends this tray only (F27): the server and the engine are the
    // guard's children and keep running.
    let quit_item = MenuItem::with_id(app, "quit", "Exit", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &version_item,
            &separator1,
            &open_mixer_item,
            &copy_url_item,
            &separator2,
            &quit_item,
        ],
    )?;

    let icon = make_tray_icon();

    TrayIconBuilder::with_id("main")
        .icon(icon)
        .tooltip(iem_guard::view::tooltip(None))
        .menu(&menu)
        .on_menu_event(move |app, event| {
            let id = event.id.as_ref();
            match id {
                "open_mixer" => {
                    open_mixer(app, &links.mixer);
                }
                "copy_url" => {
                    if let Some(url) = &links.share {
                        copy_url_to_clipboard(app, url);
                    }
                }
                "quit" => {
                    tracing::info!("Exit requested from the tray menu (the tray only)");
                    app.exit(0);
                }
                _ => {}
            }
        })
        .on_tray_icon_event(move |tray, event| {
            // Left-click opens mixer
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
                && let Some(window) = tray.app_handle().get_webview_window("main")
            {
                let _ = window.show();
                let _ = window.set_focus();
            }
        })
        .build(app)
}

/// The window the tray library created for the icon: the alarm
/// notification goes to its icon (`iem_win::window::balloon`).
#[cfg(windows)]
pub fn icon_window(icon: &TrayIcon) -> Option<isize> {
    match icon.with_inner_tray_icon(|inner| inner.window_handle() as isize) {
        Ok(hwnd) => Some(hwnd),
        Err(e) => {
            tracing::warn!(error = %e, "no window for the tray icon: alarms show in the tooltip only");
            None
        }
    }
}

/// Off Windows there is no icon window (the tray is built on Windows only).
#[cfg(not(windows))]
pub fn icon_window(_icon: &TrayIcon) -> Option<isize> {
    None
}

/// Open the mixer landing page in the main window
fn open_mixer(app: &AppHandle, url: &str) {
    tracing::info!(url, "Opening mixer");
    let Some(window) = app.get_webview_window("main") else {
        tracing::warn!("no main window to open the mixer in");
        return;
    };
    match url.parse::<tauri::Url>() {
        Ok(parsed) => {
            let _ = window.navigate(parsed);
        }
        Err(e) => {
            tracing::error!(url, error = %e, "the mixer URL does not parse");
            return;
        }
    }
    let _ = window.show();
    let _ = window.set_focus();
}

/// Copy the share URL to the clipboard (quoted as a JSON string literal).
/// The main window shows the local server (`Links::mixer`), a secure context,
/// so `navigator.clipboard` exists there.
fn copy_url_to_clipboard(app: &AppHandle, url: &str) {
    tracing::info!(url, "copying the share URL to the clipboard");
    let Some(window) = app.get_webview_window("main") else {
        tracing::warn!("no main window to copy the URL from");
        return;
    };
    let quoted = serde_json::to_string(url).unwrap_or_else(|_| "\"\"".to_string());
    let js = format!("navigator.clipboard.writeText({quoted})");
    if let Err(e) = window.eval(js) {
        tracing::warn!(error = %e, "the clipboard write did not run");
    }
}

/// Create the tray icon (headphones icon)
fn make_tray_icon() -> Image<'static> {
    let mut rgba = vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize];

    // Draw a simple headphones shape
    let color = (0x4Au8, 0x9Eu8, 0xFFu8); // Accent blue

    // Left ear cup (circle)
    draw_circle(&mut rgba, 4, 10, 3, color);

    // Right ear cup (circle)
    draw_circle(&mut rgba, 12, 10, 3, color);

    // Headband (arc at top)
    for x in 3..=13 {
        let y = if !(5..=11).contains(&x) {
            4
        } else if !(7..=9).contains(&x) {
            3
        } else {
            2
        };
        set_pixel(&mut rgba, x, y, color);
    }

    // Connect band to cups
    set_pixel(&mut rgba, 3, 5, color);
    set_pixel(&mut rgba, 3, 6, color);
    set_pixel(&mut rgba, 3, 7, color);
    set_pixel(&mut rgba, 13, 5, color);
    set_pixel(&mut rgba, 13, 6, color);
    set_pixel(&mut rgba, 13, 7, color);

    Image::new_owned(rgba, ICON_SIZE, ICON_SIZE)
}

fn set_pixel(rgba: &mut [u8], x: u32, y: u32, color: (u8, u8, u8)) {
    if x < ICON_SIZE && y < ICON_SIZE {
        let idx = ((y * ICON_SIZE + x) * 4) as usize;
        rgba[idx] = color.0;
        rgba[idx + 1] = color.1;
        rgba[idx + 2] = color.2;
        rgba[idx + 3] = 255;
    }
}

fn draw_circle(rgba: &mut [u8], cx: u32, cy: u32, r: u32, color: (u8, u8, u8)) {
    for dy in 0..=r {
        for dx in 0..=r {
            if dx * dx + dy * dy <= r * r {
                set_pixel(rgba, cx + dx, cy + dy, color);
                set_pixel(rgba, cx + dx, cy.saturating_sub(dy), color);
                set_pixel(rgba, cx.saturating_sub(dx), cy + dy, color);
                set_pixel(rgba, cx.saturating_sub(dx), cy.saturating_sub(dy), color);
            }
        }
    }
}
