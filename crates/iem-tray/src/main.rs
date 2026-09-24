//! IEM Mixer Application Entry Point

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    iem_tray::run();
}
