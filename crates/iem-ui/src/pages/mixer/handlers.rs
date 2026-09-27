//! Handlers and memos for the mixer page — extracted from MixerPage body.

use leptos::prelude::*;

use crate::api::Channel;
use crate::components::category_tabs::Category;

use super::helpers::{DisplayChannel, display_channels};

/// Create the display_channels Memo — filters/sorts channels for the active tab.
pub(super) fn make_display_channels(
    channels: ReadSignal<Vec<Channel>>,
    active_category: ReadSignal<Category>,
    pinned_channels: ReadSignal<Vec<String>>,
    hidden_channels: ReadSignal<Vec<String>>,
) -> Memo<Vec<DisplayChannel>> {
    Memo::new(move |_| {
        channels.with(|chs| {
            pinned_channels.with(|pinned| {
                hidden_channels
                    .with(|hidden| display_channels(chs, active_category.get(), pinned, hidden))
            })
        })
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
