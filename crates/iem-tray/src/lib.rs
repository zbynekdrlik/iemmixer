//! The iemmixer tray (F27; S6 plan Task 11): an icon with the guard's status
//! and a window on the mixer.
//!
//! The tray runs no server: `iem-server` is the guard's child (S6 design note
//! §5.2 step 8), and the tray reads only the site config's links for Open
//! Mixer and Copy URL. A background thread subscribes to the guard
//! ([`guard`]): the mode and the alarms update the tooltip, a new alarm
//! raises a notification, and the guard's `Quit` ends the tray. The menu's
//! Exit ends the tray only, never the server or the engine.
//!
//! The site config is `$IEMMIXER_CONFIG` (default `iemmixer.toml`), as for
//! `iem-server`; without a readable one the links fall back to the local
//! server. The log is `%LOCALAPPDATA%\iemmixer\logs\iem-tray.log.<date>`.

pub mod guard;
pub mod tray;

use std::path::PathBuf;

use iem_core::Config;
use tauri::{Manager, RunEvent, WindowEvent};

/// The site config, as `iem-server` finds it.
fn config_path() -> PathBuf {
    PathBuf::from(std::env::var("IEMMIXER_CONFIG").unwrap_or_else(|_| "iemmixer.toml".to_string()))
}

/// Run the tray until the menu's Exit or the guard's `Quit`.
pub fn run() {
    std::panic::set_hook(Box::new(|info| {
        tracing::error!("PANIC: {}", info);
    }));

    let log_dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("iemmixer")
        .join("logs");
    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        eprintln!("iem-tray: cannot create {}: {e}", log_dir.display());
    }

    let file_appender = tracing_appender::rolling::daily(&log_dir, "iem-tray.log");
    // Dropped at the event loop's exit, so the last lines (the guard's Quit
    // included) reach the file.
    let (non_blocking, log_flush) = tracing_appender::non_blocking(file_appender);
    let mut log_flush = Some(log_flush);

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false);

    let env_filter = tracing_subscriber::EnvFilter::from_default_env()
        .add_directive("iem_tray=debug".parse().unwrap())
        .add_directive("iem_core=debug".parse().unwrap());

    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    tracing_subscriber::registry()
        .with(env_filter)
        .with(file_layer)
        .init();

    tracing::info!(log_dir = %log_dir.display(), "Logging initialized");
    tracing::info!("Starting the iemmixer tray v{}", iem_core::VERSION);

    let path = config_path();
    let config = match Config::load(&path) {
        Ok(config) => config,
        Err(e) => {
            tracing::error!(
                error = %e,
                path = %path.display(),
                "the site config did not load; the links fall back to the local server"
            );
            Config::default()
        }
    };
    let links = tray::Links {
        mixer: config.mixer_url(),
        share: config.share_url(),
    };
    tracing::info!(mixer = %links.mixer, share = ?links.share, "site links");

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // Focus existing window when second instance tries to start
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
                let _ = window.show();
            }
        }))
        .plugin(tauri_plugin_shell::init())
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Hide instead of close
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            let icon = match tray::setup_tray(&handle, links) {
                Ok(icon) => Some(icon),
                Err(e) => {
                    tracing::error!("Failed to set up the tray icon: {e}");
                    None
                }
            };
            let hwnd = icon.as_ref().and_then(tray::icon_window);
            // Even without an icon the guard's Quit must reach this process.
            guard::spawn(handle, icon, hwnd);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the iemmixer tray");

    app.run(move |_app, event| {
        if let RunEvent::Exit = event {
            tracing::info!("the tray exits");
            drop(log_flush.take());
        }
    });
}
