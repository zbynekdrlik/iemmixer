//! Reusable UI components

pub mod alert_button;
pub mod alert_toast;
pub mod audio_player;
pub mod back_to_reaper;
pub mod backup_section;
pub mod category_tabs;
pub mod confirm_dialog;
pub mod console_section;
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

/// Runs `f` in the next macrotask, after the current event has finished its
/// dispatch. A click that unmounts the component it bubbles through (closing
/// a modal, navigating away) must defer that change: dropping the handlers
/// of the elements the event still has to reach makes it call freed closures.
pub(crate) fn after_event(f: impl FnOnce() + 'static) {
    use wasm_bindgen::JsCast;
    let cb = wasm_bindgen::closure::Closure::once_into_js(f);
    if let Some(w) = web_sys::window() {
        let _ = w.set_timeout_with_callback(cb.unchecked_ref());
    }
}

/// Keeps `value` (a callback the browser holds) alive while the current
/// component is mounted, and drops it when the component unmounts — after
/// its `on_cleanup`, which stops the browser from calling it. A
/// `Closure::forget` instead would leak one callback per mount.
pub(crate) fn keep_while_mounted<T: 'static>(value: T) {
    let _ = leptos::prelude::StoredValue::new_local(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use leptos::prelude::Owner;
    use std::cell::Cell;
    use std::rc::Rc;

    /// Sets its flag when dropped.
    struct Tracked(Rc<Cell<bool>>);

    impl Drop for Tracked {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    #[test]
    fn a_kept_value_lives_until_its_component_is_cleaned_up() {
        let dropped = Rc::new(Cell::new(false));
        let component = Owner::new();
        component.with(|| keep_while_mounted(Tracked(Rc::clone(&dropped))));
        assert!(!dropped.get(), "alive while mounted");
        // Settings closed: the console section unmounts.
        component.cleanup();
        assert!(dropped.get(), "released when the component unmounts");
    }
}
