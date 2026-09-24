//! Handlers and memos for the mixer page — extracted from MixerPage body.

use leptos::prelude::*;
use std::collections::HashMap;

use crate::api::Channel;
use crate::components::category_tabs::Category;
use crate::components::eq_modal::EqBandState;
use crate::components::preset_modal::{ChannelState, PresetData};

use super::helpers::{DisplayChannel, ws_send};

/// Create the display_channels Memo — filters/sorts channels for the active tab.
pub(super) fn make_display_channels(
    channels: ReadSignal<Vec<Channel>>,
    member_id: Signal<String>,
    active_category: ReadSignal<Category>,
    pinned_channels: ReadSignal<Vec<usize>>,
    hidden_channels: ReadSignal<Vec<usize>>,
) -> Memo<Vec<DisplayChannel>> {
    Memo::new(move |_| {
        let chs = channels.get();
        let member = member_id.get();
        let my_input = format!("{} MIC", member.to_uppercase());
        let active_cat = active_category.get();
        let pinned = pinned_channels.get();
        let hidden = hidden_channels.get();

        let mut result = Vec::new();
        let mut seen_pairs: std::collections::HashSet<String> = std::collections::HashSet::new();

        for ch in &chs {
            if ch.stereo_side.as_deref() == Some("R") {
                continue;
            }

            let is_my_input = ch.name.to_uppercase() == my_input;
            let is_pinned = pinned.contains(&ch.track_index);
            let is_hidden = hidden.contains(&ch.track_index);

            // Hidden tab: only show hidden channels
            if active_cat == Category::Hidden {
                if !is_hidden {
                    continue;
                }
            } else if active_cat == Category::Main {
                // Main tab: show "me" channel + pinned channels
                // Hidden does NOT remove pinned channels from Main — only from category tabs
                if !is_my_input && !is_pinned {
                    continue;
                }
            } else {
                // Category tabs: filter by category, skip hidden
                if !active_cat.matches(&ch.category) {
                    continue;
                }
                if is_hidden {
                    continue;
                }
            }

            let (is_stereo, partner_index) = if ch.stereo_side.as_deref() == Some("L") {
                if let Some(ref pair_name) = ch.stereo_pair {
                    if seen_pairs.contains(pair_name) {
                        continue;
                    }
                    seen_pairs.insert(pair_name.clone());
                    let partner = chs
                        .iter()
                        .find(|c| {
                            c.stereo_pair.as_ref() == Some(pair_name)
                                && c.stereo_side.as_deref() == Some("R")
                        })
                        .map(|c| c.track_index);
                    (true, partner)
                } else {
                    (false, None)
                }
            } else {
                (false, None)
            };

            let display_name = if is_stereo {
                ch.name.trim_end_matches(" L").to_string()
            } else {
                ch.name.clone()
            };

            result.push(DisplayChannel {
                track_index: ch.track_index,
                display_name,
                is_stereo,
                partner_index,
                is_my_input,
            });
        }

        // Sort Stems: Click first, Guide second, then rest
        if active_cat == Category::Stems {
            result.sort_by_key(|ch| {
                let name_upper = ch.display_name.to_uppercase();
                if name_upper == "CLICK" {
                    0
                } else if name_upper == "GUIDE" {
                    1
                } else {
                    2
                }
            });
        }

        // Sort Main: own channel first, pinned channels after
        if active_cat == Category::Main {
            result.sort_by_key(|ch| if ch.is_my_input { 0 } else { 1 });
        }

        result
    })
}

/// Create the get_current_state Callback for preset save.
pub(super) fn make_get_current_state(
    channels: ReadSignal<Vec<Channel>>,
    stems_bus_idx: ReadSignal<Option<usize>>,
    stems_level: ReadSignal<f32>,
    eq_bands: ReadSignal<Vec<EqBandState>>,
    eq_open: ReadSignal<Option<(usize, String)>>,
) -> Callback<(), PresetData> {
    Callback::new(move |_: ()| {
        let chs = channels.get();
        let mut channel_states = HashMap::new();
        for ch in &chs {
            channel_states.insert(
                ch.track_index,
                ChannelState {
                    vol: ch.level_db,
                    mute: ch.muted,
                    pan: ch.pan,
                },
            );
        }
        let current_stems_level = if stems_bus_idx.get_untracked().is_some() {
            Some(stems_level.get_untracked())
        } else {
            None
        };
        // Include cached EQ data if the user has loaded EQ for a track
        let eq_data = {
            let bands = eq_bands.get_untracked();
            let open = eq_open.get_untracked();
            if !bands.is_empty() {
                if let Some((track_idx, _)) = open {
                    let mut eq_map = HashMap::new();
                    eq_map.insert(
                        track_idx,
                        bands
                            .iter()
                            .map(|b| crate::components::preset_modal::EqBandPreset {
                                band_type: b.band_type.clone(),
                                freq_hz: b.freq_hz,
                                gain_db: b.gain_db,
                                bw: b.bw,
                                freq_norm: b.freq_norm,
                                gain_norm: b.gain_norm,
                                bw_norm: b.bw_norm,
                                enabled: b.enabled,
                            })
                            .collect(),
                    );
                    Some(eq_map)
                } else {
                    None
                }
            } else {
                None
            }
        };
        PresetData {
            channels: channel_states,
            created_at: None,
            updated_at: None,
            stems_level_db: current_stems_level,
            eq_bands: eq_data,
        }
    })
}

/// Create the on_load_preset Callback for preset load.
pub(super) fn make_on_load_preset(
    connected: ReadSignal<bool>,
    set_channels: WriteSignal<Vec<Channel>>,
    ws: ReadSignal<Option<web_sys::WebSocket>>,
) -> Callback<PresetData, ()> {
    Callback::new(move |preset: PresetData| {
        if !connected.get() {
            web_sys::console::warn_1(&"Preset loading blocked: not connected to REAPER".into());
            return;
        }

        // Update local state
        let _ = set_channels.try_update(|chs| {
            for ch in chs.iter_mut() {
                if let Some(state) = preset.channels.get(&ch.track_index) {
                    ch.level_db = state.vol;
                    ch.muted = state.mute;
                    ch.pan = state.pan;
                }
            }
        });

        // Send all changes via WebSocket
        for (track_index, state) in &preset.channels {
            ws_send(
                ws,
                &iem_core::ClientMsg::SetLevel {
                    track_index: *track_index,
                    level_db: state.vol,
                },
            );
            ws_send(
                ws,
                &iem_core::ClientMsg::SetMute {
                    track_index: *track_index,
                    muted: state.mute,
                },
            );
            ws_send(
                ws,
                &iem_core::ClientMsg::SetPan {
                    track_index: *track_index,
                    pan: state.pan,
                },
            );
        }

        // reaperiem#205: apply the stems bus level if the preset carries one (previously
        // captured but never applied on load).
        if let Some(stems_db) = preset.stems_level_db {
            ws_send(
                ws,
                &iem_core::ClientMsg::SetStemsLevel { level_db: stems_db },
            );
        }

        // reaperiem#205: apply EQ bands via the LEGACY norm protocol (param=freq|gain|bw),
        // matching the server-side preset replay path — NOT the value-domain
        // protocol used by the interactive slider. Previously eq_bands was
        // dropped on load entirely.
        if let Some(ref eq_bands) = preset.eq_bands {
            for (track_index, bands) in eq_bands {
                for (band_idx, band) in bands.iter().enumerate() {
                    for (param, value) in [
                        ("freq", band.freq_norm),
                        ("gain", band.gain_norm),
                        ("bw", band.bw_norm),
                        ("enabled", if band.enabled { 1.0 } else { 0.0 }),
                    ] {
                        ws_send(
                            ws,
                            &iem_core::ClientMsg::SetEqBand {
                                track_index: *track_index,
                                band: band_idx as u8,
                                param: param.to_string(),
                                value,
                            },
                        );
                    }
                }
            }
        }
    })
}

/// Create the on_mute_all Callback.
pub(super) fn make_on_mute_all(member_id: Signal<String>) -> Callback<(), ()> {
    Callback::new(move |_: ()| {
        let member = member_id.get();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = crate::api::batch_mute_all(&member).await {
                web_sys::console::error_1(&format!("Mute all failed: {}", e).into());
            }
        });
    })
}
