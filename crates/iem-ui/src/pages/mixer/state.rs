//! Reactive state for the MixerPage component.
//!
//! Bundles all signal pairs into a single struct so that
//! `connect_websocket` can take one reference instead of 44 parameters.

use leptos::prelude::*;
use std::collections::{HashMap, HashSet};
use wasm_bindgen::prelude::*;

use crate::api::Channel;
use crate::components::category_tabs::Category;
use crate::components::eq_modal::EqBandState;
use crate::components::settings_modal::UserSettings;
use crate::components::talk_button::TalkState;

/// All reactive state owned by MixerPage.
#[derive(Clone, Copy)]
pub(super) struct MixerState {
    pub channels: (ReadSignal<Vec<Channel>>, WriteSignal<Vec<Channel>>),
    pub meters: (
        ReadSignal<HashMap<usize, [f32; 2]>>,
        WriteSignal<HashMap<usize, [f32; 2]>>,
    ),
    pub connected: (ReadSignal<bool>, WriteSignal<bool>),
    pub loading: (ReadSignal<bool>, WriteSignal<bool>),
    pub fader_touched: (
        ReadSignal<HashMap<usize, bool>>,
        WriteSignal<HashMap<usize, bool>>,
    ),
    pub global_touched: (ReadSignal<bool>, WriteSignal<bool>),
    pub stems_touched: (ReadSignal<bool>, WriteSignal<bool>),
    pub global_level: (ReadSignal<f32>, WriteSignal<f32>),
    pub global_muted: (ReadSignal<bool>, WriteSignal<bool>),
    pub stems_level: (ReadSignal<f32>, WriteSignal<f32>),
    pub stems_muted: (ReadSignal<bool>, WriteSignal<bool>),
    pub stems_bus_idx: (ReadSignal<Option<usize>>, WriteSignal<Option<usize>>),
    pub eq_open: (
        ReadSignal<Option<(usize, String)>>,
        WriteSignal<Option<(usize, String)>>,
    ),
    pub eq_bands: (ReadSignal<Vec<EqBandState>>, WriteSignal<Vec<EqBandState>>),
    pub eq_loading: (ReadSignal<bool>, WriteSignal<bool>),
    pub limiter_open: (
        ReadSignal<Option<(usize, String)>>,
        WriteSignal<Option<(usize, String)>>,
    ),
    pub limiter_limit_db: (ReadSignal<f32>, WriteSignal<f32>),
    pub limiter_limit_norm: (ReadSignal<f32>, WriteSignal<f32>),
    pub limiter_enabled: (ReadSignal<bool>, WriteSignal<bool>),
    pub limiter_loading: (ReadSignal<bool>, WriteSignal<bool>),
    pub limiter_active_seconds: (ReadSignal<f64>, WriteSignal<f64>),
    pub active_category: (ReadSignal<Category>, WriteSignal<Category>),
    pub data_pulse: (ReadSignal<bool>, WriteSignal<bool>),
    pub pinned_channels: (ReadSignal<Vec<usize>>, WriteSignal<Vec<usize>>),
    pub hidden_channels: (ReadSignal<Vec<usize>>, WriteSignal<Vec<usize>>),
    pub network_mode: (ReadSignal<String>, WriteSignal<String>),
    pub output_track_idx: (ReadSignal<Option<usize>>, WriteSignal<Option<usize>>),
    pub soloed: (ReadSignal<HashSet<usize>>, WriteSignal<HashSet<usize>>),
    pub pre_solo_mutes: (
        ReadSignal<HashMap<usize, bool>>,
        WriteSignal<HashMap<usize, bool>>,
    ),
    pub double_tap_fader: (ReadSignal<bool>, WriteSignal<bool>),
    pub has_photo: (ReadSignal<bool>, WriteSignal<bool>),
    pub preset_modal_visible: (ReadSignal<bool>, WriteSignal<bool>),
    pub pin_modal_visible: (ReadSignal<bool>, WriteSignal<bool>),
    pub settings_modal_visible: (ReadSignal<bool>, WriteSignal<bool>),
    pub snapshot_modal_visible: (ReadSignal<bool>, WriteSignal<bool>),
    pub alert_data: (
        ReadSignal<Option<(String, String)>>,
        WriteSignal<Option<(String, String)>>,
    ),
    pub alert_active: (ReadSignal<bool>, WriteSignal<bool>),
    pub talk_state: (ReadSignal<TalkState>, WriteSignal<TalkState>),
    pub engineer_talking: (ReadSignal<bool>, WriteSignal<bool>),
    /// Internet access (Cloudflare tunnel) status from the server (reaperiem#202);
    /// `None` until the first `TunnelStatus` message arrives.
    pub tunnel: (
        ReadSignal<Option<iem_core::TunnelStatusInfo>>,
        WriteSignal<Option<iem_core::TunnelStatusInfo>>,
    ),
    pub ws: (
        ReadSignal<Option<web_sys::WebSocket>>,
        WriteSignal<Option<web_sys::WebSocket>>,
    ),
}

impl MixerState {
    // --- Modal toggles ---

    pub fn open_preset_modal(&self) {
        let _ = self.preset_modal_visible.1.try_set(true);
    }
    pub fn close_preset_modal(&self) {
        let _ = self.preset_modal_visible.1.try_set(false);
    }
    pub fn open_snapshot_modal(&self) {
        let _ = self.snapshot_modal_visible.1.try_set(true);
    }
    pub fn close_snapshot_modal(&self) {
        let _ = self.snapshot_modal_visible.1.try_set(false);
    }
    pub fn open_settings_modal(&self) {
        let _ = self.settings_modal_visible.1.try_set(true);
    }
    pub fn close_settings_modal(&self) {
        let _ = self.settings_modal_visible.1.try_set(false);
    }
    pub fn open_pin_change_modal(&self) {
        let _ = self.pin_modal_visible.1.try_set(true);
    }
    pub fn close_pin_change_modal(&self) {
        let _ = self.pin_modal_visible.1.try_set(false);
    }

    // --- Category ---

    pub fn select_category(&self, cat: crate::components::category_tabs::Category) {
        let _ = self.active_category.1.try_set(cat);
    }

    // --- Photo ---

    pub fn update_has_photo(&self, has: bool) {
        let _ = self.has_photo.1.try_set(has);
    }

    // --- Solo clear ---

    pub fn clear_solo(&self) {
        let saved = self.pre_solo_mutes.0.get();
        let _ = self.channels.1.try_update(|chs| {
            for c in chs.iter_mut() {
                let should_be_muted = saved.get(&c.track_index).copied().unwrap_or(false);
                c.muted = should_be_muted;
            }
        });
        let _ = self.pre_solo_mutes.1.try_set(HashMap::new());
        let _ = self.soloed.1.try_set(HashSet::new());
    }

    // --- EQ close (deferred to next macrotask) ---

    pub fn close_eq(&self) {
        let set_eq_open = self.eq_open.1;
        let set_eq_bands = self.eq_bands.1;
        let cb = wasm_bindgen::closure::Closure::once_into_js(move || {
            let _ = set_eq_open.try_set(None);
            let _ = set_eq_bands.try_set(Vec::new());
        });
        web_sys::window()
            .unwrap()
            .set_timeout_with_callback(cb.as_ref().unchecked_ref())
            .unwrap();
    }

    // --- Limiter ---

    pub fn reset_limiter_activity(&self) {
        let _ = self.limiter_active_seconds.1.try_set(0.0);
    }

    /// Limiter range: normalized 0.0–1.0 maps to -6 dB to 0 dB (value * 6.0 - 6.0).
    pub fn set_limiter_param(&self, value: f32) {
        let _ = self.limiter_limit_norm.1.try_set(value);
        let _ = self.limiter_limit_db.1.try_set(value * 6.0 - 6.0);
    }

    pub fn set_limiter_enabled_state(&self, en: bool) {
        let _ = self.limiter_enabled.1.try_set(en);
    }

    pub fn close_limiter(&self) {
        let set_limiter_open = self.limiter_open.1;
        let cb = wasm_bindgen::closure::Closure::once_into_js(move || {
            let _ = set_limiter_open.try_set(None);
        });
        web_sys::window()
            .unwrap()
            .set_timeout_with_callback(cb.as_ref().unchecked_ref())
            .unwrap();
    }

    pub fn new(member_id: &str) -> Self {
        let user_settings = UserSettings::load(member_id);
        Self {
            channels: signal(Vec::new()),
            meters: signal(HashMap::new()),
            connected: signal(false),
            loading: signal(true),
            fader_touched: signal(HashMap::new()),
            global_touched: signal(false),
            stems_touched: signal(false),
            global_level: signal(0.0),
            global_muted: signal(false),
            stems_level: signal(0.0),
            stems_muted: signal(false),
            stems_bus_idx: signal(None),
            eq_open: signal(None),
            eq_bands: signal(Vec::new()),
            eq_loading: signal(false),
            limiter_open: signal(None),
            limiter_limit_db: signal(-6.0),
            limiter_limit_norm: signal(0.0),
            limiter_enabled: signal(true),
            limiter_loading: signal(false),
            limiter_active_seconds: signal(0.0),
            active_category: signal(Category::Main),
            data_pulse: signal(false),
            pinned_channels: signal(Vec::new()),
            hidden_channels: signal(Vec::new()),
            network_mode: signal(String::new()),
            output_track_idx: signal(None),
            soloed: signal(HashSet::new()),
            pre_solo_mutes: signal(HashMap::new()),
            double_tap_fader: signal(user_settings.double_tap_fader),
            has_photo: signal(false),
            preset_modal_visible: signal(false),
            pin_modal_visible: signal(false),
            settings_modal_visible: signal(false),
            snapshot_modal_visible: signal(false),
            alert_data: signal(None),
            alert_active: signal(false),
            talk_state: signal(TalkState::Idle),
            engineer_talking: signal(false),
            tunnel: signal(None),
            ws: signal(None),
        }
    }
}
