//! xinput (xbox controller) state.

use windows::Win32::UI::Input::XboxController::{XINPUT_STATE, XInputGetState};

// Button masks

pub const XINPUT_DPAD_UP: u16 = 0x0001;
pub const XINPUT_DPAD_DOWN: u16 = 0x0002;
pub const XINPUT_DPAD_LEFT: u16 = 0x0004;
pub const XINPUT_DPAD_RIGHT: u16 = 0x0008;
pub const XINPUT_START: u16 = 0x0010;
/// "Back" / "View"
pub const XINPUT_SELECT: u16 = 0x0020;
/// Left Stick click
pub const XINPUT_LS: u16 = 0x0040;
/// Right Stick click
pub const XINPUT_RS: u16 = 0x0080;
/// Left Bumper
pub const XINPUT_LB: u16 = 0x0100;
/// Right Bumper
pub const XINPUT_RB: u16 = 0x0200;
pub const XINPUT_A: u16 = 0x1000;
pub const XINPUT_B: u16 = 0x2000;
pub const XINPUT_X: u16 = 0x4000;
pub const XINPUT_Y: u16 = 0x8000;

pub const XINPUT_BUTTON_DISPLAY: [(u16, &str); 14] = [
    (XINPUT_DPAD_UP, "D-Pad Up"),
    (XINPUT_DPAD_DOWN, "D-Pad Down"),
    (XINPUT_DPAD_LEFT, "D-Pad Left"),
    (XINPUT_DPAD_RIGHT, "D-Pad Right"),
    (XINPUT_START, "Start"),
    (XINPUT_SELECT, "Select"),
    (XINPUT_LS, "L-Stick"),
    (XINPUT_RS, "R-Stick"),
    (XINPUT_LB, "LB"),
    (XINPUT_RB, "RB"),
    (XINPUT_A, "A"),
    (XINPUT_B, "B"),
    (XINPUT_X, "X"),
    (XINPUT_Y, "Y"),
];

/// typed wrapper around the raw xinput state
#[derive(Debug, Clone, Copy, Default)]
pub struct XInputState {
    pub packet_number: u32,
    pub buttons: u16,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub thumb_lx: i16,
    pub thumb_ly: i16,
    pub thumb_rx: i16,
    pub thumb_ry: i16,
}

impl XInputState {
    pub fn is_pressed(&self, button_mask: u16) -> bool {
        (self.buttons & button_mask) == button_mask
    }
}

/// xinput state for controller user_index (0-3), None if not connected
pub fn get_xinput_state(user_index: u32) -> Option<XInputState> {
    let mut state = XINPUT_STATE::default();
    let result = unsafe { XInputGetState(user_index, &mut state) };
    if result != 0 {
        return None;
    }
    let g = state.Gamepad;
    Some(XInputState {
        packet_number: state.dwPacketNumber,
        buttons: g.wButtons.0,
        left_trigger: g.bLeftTrigger,
        right_trigger: g.bRightTrigger,
        thumb_lx: g.sThumbLX,
        thumb_ly: g.sThumbLY,
        thumb_rx: g.sThumbRX,
        thumb_ry: g.sThumbRY,
    })
}
