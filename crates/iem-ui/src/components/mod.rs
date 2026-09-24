//! Reusable UI components

pub mod alert_button;
pub mod alert_toast;
pub mod audio_player;
pub mod backup_section;
pub mod category_tabs;
pub mod confirm_dialog;
pub mod eq_modal;
pub mod fader;
pub mod limiter_modal;
pub mod meter;
pub mod pan;
pub mod pin_change_modal;
pub mod preset_modal;
pub mod settings_modal;
pub mod snapshot_modal;
pub mod talk_button;
pub mod toolbar;
pub mod tunnel_status;

/// A JS callback owned by the component that installed it; replacing or
/// dropping the `Closure` releases it. Named because the nested type trips
/// clippy `type_complexity`.
pub(crate) type CallbackSlot =
    std::rc::Rc<std::cell::RefCell<Option<wasm_bindgen::closure::Closure<dyn FnMut()>>>>;

/// A `window` mouse listener owned by a dragging component (removed on
/// mouseup); named for the same reason as [`CallbackSlot`].
pub(crate) type MouseCallbackSlot = std::rc::Rc<
    std::cell::RefCell<Option<wasm_bindgen::closure::Closure<dyn FnMut(web_sys::MouseEvent)>>>,
>;
