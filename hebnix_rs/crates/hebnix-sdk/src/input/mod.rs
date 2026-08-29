//! keyboard, xinput (xbox) + dualsense/ds4 input detection.

pub mod bindings;
pub mod dualsense;
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
pub use keyboard::{detect_hotkey, is_key_pressed};
pub use xinput::{XINPUT_BUTTON_DISPLAY, XInputState, get_xinput_state};
