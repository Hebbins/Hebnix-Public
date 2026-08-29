//! keyboard, xinput (xbox) + dualsense/ds4 input detection.

pub mod bindings;
pub mod dualsense;
pub mod game_actions;
pub mod keyboard;
pub mod xinput;

pub use bindings::{
    ControllerType, HotkeyBind, bind_to_string, detect_any_hotkey, get_button_display,
    init_controllers, is_bind_pressed, is_hotkey_pressed, resolve_bind_string,
};
pub use dualsense::{
    DS4_BUTTON_DISPLAY, DS4_BUTTONS, PsModel, get_dualsense_inputs, is_dualsense_connected,
    ps_model,
};
pub use game_actions::{action_binds, clear_action_bind_cache, is_action_pressed};
pub use keyboard::{detect_hotkey, is_key_pressed, tap_key, type_text};
pub use xinput::{XINPUT_BUTTON_DISPLAY, XInputState, get_xinput_state};
