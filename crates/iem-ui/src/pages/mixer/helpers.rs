use leptos::prelude::*;
use wasm_bindgen::prelude::*;

/// Post-release guard duration in milliseconds.
/// With server-side echo suppression, this only needs to cover WebSocket round-trip (~10-20ms).
pub(super) const POST_RELEASE_GUARD_MS: i32 = 100;

/// Minimum interval between WebSocket sends per track (ms).
/// Limits to ~20 commands/sec to avoid overwhelming the server.
pub(super) const THROTTLE_INTERVAL_MS: f64 = 50.0;

/// Processed channel for display (handles stereo pairs)
/// Note: level_db, pan, muted are read via derived signals from channels
#[derive(Debug, Clone, PartialEq)]
pub(super) struct DisplayChannel {
    pub track_index: usize,
    pub display_name: String,
    pub is_stereo: bool,
    pub partner_index: Option<usize>,
    pub is_my_input: bool,
}

/// Send a command via WebSocket (synchronous, non-blocking)
pub(super) fn ws_send(ws: ReadSignal<Option<web_sys::WebSocket>>, cmd: &iem_core::ClientMsg) {
    if let Some(ws) = ws.get_untracked() {
        if ws.ready_state() == web_sys::WebSocket::OPEN {
            if let Ok(json) = serde_json::to_string(cmd) {
                let _ = ws.send_with_str(&json);
            }
        }
    }
}

/// Storage for WebSocket closures to prevent memory leaks on reconnect.
/// Dropping a Closure that was passed to JS via `as_ref().unchecked_ref()` properly
/// releases the WASM-side allocation. Without this, `Closure::forget()` leaks on every reconnect.
/// Uses Rc<RefCell<>> because wasm_bindgen::Closure is !Send (WASM is single-threaded).
pub(super) type WsClosures = (
    Closure<dyn FnMut(web_sys::MessageEvent)>,
    Closure<dyn FnMut(web_sys::CloseEvent)>,
);
pub(super) type WsClosureStore = std::rc::Rc<std::cell::RefCell<Option<WsClosures>>>;

/// Counter for consecutive WebSocket failures without receiving data.
/// Shared across connect_websocket calls via Rc<Cell<>>.
pub(super) type WsFailCounter = std::rc::Rc<std::cell::Cell<u32>>;

/// Max consecutive WS failures before redirecting to login
pub(super) const MAX_WS_FAILURES: u32 = 3;

/// Parse track name into main and type parts
pub(super) fn parse_track_name(name: &str) -> (String, String) {
    let parts: Vec<&str> = name.split_whitespace().collect();
    if parts.len() >= 2 {
        (parts[0].to_string(), parts[1..].join(" "))
    } else {
        (name.to_string(), String::new())
    }
}

/// Format dB value for display with unit suffix
pub(super) fn format_db(db: f32) -> String {
    if db <= -60.0 {
        "-\u{221E}dB".to_string()
    } else if db >= 0.0 {
        format!("+{:.1}dB", db)
    } else {
        format!("{:.1}dB", db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_db_uses_proper_notation() {
        assert!(format_db(0.0).ends_with("dB"), "Must use 'dB' not 'db'");
        assert!(format_db(-6.0).ends_with("dB"));
        assert!(format_db(-60.0).ends_with("dB")); // -inf case
    }

    #[test]
    fn test_format_db_max_length() {
        let cases = [0.0, 6.0, 12.0, -6.0, -12.5, -59.9, -60.0, -100.0];
        for db in cases {
            let s = format_db(db);
            assert!(
                s.chars().count() <= 7,
                "format_db({db}) = \"{s}\" exceeds 7 chars"
            );
        }
    }
}
