//! button-prompt icons for hotkey binds, shown next to a bound key/button in
//! plugin UI.
//!
//! binds work across xinput/dinput/playstation interchangeably (see
//! hebnix_sdk's is_hotkey_pressed cross-backend fallback - Rocket League's
//! own binds are always recorded xinput-shaped regardless of the physical
//! controller). The icon/label shown here follows the same idea: whichever
//! controller is actually connected right now decides the icon set, not
//! whatever type the bind happened to be captured as.
//!
//! source: Kenney's input-prompts pack (kenney.nl, CC0).

use hebnix_sdk::input::{DInputLayout, HotkeyBind, PsGen};

const XINPUT_ICONS: [(&str, &[u8]); 14] = [
    ("south", include_bytes!("../../assets/gamepad-icons/xinput/south.svg")),
    ("east", include_bytes!("../../assets/gamepad-icons/xinput/east.svg")),
    ("north", include_bytes!("../../assets/gamepad-icons/xinput/north.svg")),
    ("west", include_bytes!("../../assets/gamepad-icons/xinput/west.svg")),
    ("lb", include_bytes!("../../assets/gamepad-icons/xinput/lb.svg")),
    ("rb", include_bytes!("../../assets/gamepad-icons/xinput/rb.svg")),
    ("ls", include_bytes!("../../assets/gamepad-icons/xinput/ls.svg")),
    ("rs", include_bytes!("../../assets/gamepad-icons/xinput/rs.svg")),
    ("start", include_bytes!("../../assets/gamepad-icons/xinput/start.svg")),
    ("select", include_bytes!("../../assets/gamepad-icons/xinput/select.svg")),
    ("dpad_up", include_bytes!("../../assets/gamepad-icons/xinput/dpad_up.svg")),
    ("dpad_down", include_bytes!("../../assets/gamepad-icons/xinput/dpad_down.svg")),
    ("dpad_left", include_bytes!("../../assets/gamepad-icons/xinput/dpad_left.svg")),
    ("dpad_right", include_bytes!("../../assets/gamepad-icons/xinput/dpad_right.svg")),
];

const DINPUT_ICONS: [(&str, &[u8]); 17] = [
    ("south", include_bytes!("../../assets/gamepad-icons/dinput/south.svg")),
    ("east", include_bytes!("../../assets/gamepad-icons/dinput/east.svg")),
    ("north", include_bytes!("../../assets/gamepad-icons/dinput/north.svg")),
    ("west", include_bytes!("../../assets/gamepad-icons/dinput/west.svg")),
    ("l1", include_bytes!("../../assets/gamepad-icons/dinput/l1.svg")),
    ("r1", include_bytes!("../../assets/gamepad-icons/dinput/r1.svg")),
    ("l2", include_bytes!("../../assets/gamepad-icons/dinput/l2.svg")),
    ("r2", include_bytes!("../../assets/gamepad-icons/dinput/r2.svg")),
    ("l3", include_bytes!("../../assets/gamepad-icons/dinput/l3.svg")),
    ("r3", include_bytes!("../../assets/gamepad-icons/dinput/r3.svg")),
    ("select", include_bytes!("../../assets/gamepad-icons/dinput/select.svg")),
    ("start", include_bytes!("../../assets/gamepad-icons/dinput/start.svg")),
    ("mode", include_bytes!("../../assets/gamepad-icons/dinput/mode.svg")),
    ("dpad_up", include_bytes!("../../assets/gamepad-icons/dinput/dpad_up.svg")),
    ("dpad_down", include_bytes!("../../assets/gamepad-icons/dinput/dpad_down.svg")),
    ("dpad_left", include_bytes!("../../assets/gamepad-icons/dinput/dpad_left.svg")),
    ("dpad_right", include_bytes!("../../assets/gamepad-icons/dinput/dpad_right.svg")),
];

const PS4_ICONS: [(&str, &[u8]); 16] = [
    ("south", include_bytes!("../../assets/gamepad-icons/playstation4/south.svg")),
    ("east", include_bytes!("../../assets/gamepad-icons/playstation4/east.svg")),
    ("north", include_bytes!("../../assets/gamepad-icons/playstation4/north.svg")),
    ("west", include_bytes!("../../assets/gamepad-icons/playstation4/west.svg")),
    ("l1", include_bytes!("../../assets/gamepad-icons/playstation4/l1.svg")),
    ("r1", include_bytes!("../../assets/gamepad-icons/playstation4/r1.svg")),
    ("l2", include_bytes!("../../assets/gamepad-icons/playstation4/l2.svg")),
    ("r2", include_bytes!("../../assets/gamepad-icons/playstation4/r2.svg")),
    ("l3", include_bytes!("../../assets/gamepad-icons/playstation4/l3.svg")),
    ("r3", include_bytes!("../../assets/gamepad-icons/playstation4/r3.svg")),
    ("select", include_bytes!("../../assets/gamepad-icons/playstation4/select.svg")),
    ("start", include_bytes!("../../assets/gamepad-icons/playstation4/start.svg")),
    ("dpad_up", include_bytes!("../../assets/gamepad-icons/playstation4/dpad_up.svg")),
    ("dpad_down", include_bytes!("../../assets/gamepad-icons/playstation4/dpad_down.svg")),
    ("dpad_left", include_bytes!("../../assets/gamepad-icons/playstation4/dpad_left.svg")),
    ("dpad_right", include_bytes!("../../assets/gamepad-icons/playstation4/dpad_right.svg")),
];

const PS5_ICONS: [(&str, &[u8]); 16] = [
    ("south", include_bytes!("../../assets/gamepad-icons/playstation5/south.svg")),
    ("east", include_bytes!("../../assets/gamepad-icons/playstation5/east.svg")),
    ("north", include_bytes!("../../assets/gamepad-icons/playstation5/north.svg")),
    ("west", include_bytes!("../../assets/gamepad-icons/playstation5/west.svg")),
    ("l1", include_bytes!("../../assets/gamepad-icons/playstation5/l1.svg")),
    ("r1", include_bytes!("../../assets/gamepad-icons/playstation5/r1.svg")),
    ("l2", include_bytes!("../../assets/gamepad-icons/playstation5/l2.svg")),
    ("r2", include_bytes!("../../assets/gamepad-icons/playstation5/r2.svg")),
    ("l3", include_bytes!("../../assets/gamepad-icons/playstation5/l3.svg")),
    ("r3", include_bytes!("../../assets/gamepad-icons/playstation5/r3.svg")),
    ("select", include_bytes!("../../assets/gamepad-icons/playstation5/select.svg")),
    ("start", include_bytes!("../../assets/gamepad-icons/playstation5/start.svg")),
    ("dpad_up", include_bytes!("../../assets/gamepad-icons/playstation5/dpad_up.svg")),
    ("dpad_down", include_bytes!("../../assets/gamepad-icons/playstation5/dpad_down.svg")),
    ("dpad_left", include_bytes!("../../assets/gamepad-icons/playstation5/dpad_left.svg")),
    ("dpad_right", include_bytes!("../../assets/gamepad-icons/playstation5/dpad_right.svg")),
];

// old dualsense-only binds stored an index into the pre-dinput DS4_BUTTONS
// order (see bindings.rs' LEGACY_DS4_ORDER) instead of a DINPUT_BUTTONS
// index; same translation, duplicated here since bindings.rs keeps it private.
const LEGACY_DS4_ORDER: [u32; 19] = [
    0, 1, 3, 2, 4, 5, 6, 7, 8, 9, 11, 10, 12, 12, 12, 13, 14, 15, 16,
];

enum Backend {
    XInput,
    PlayStation(PsGen),
    Generic,
}

fn xinput_present() -> bool {
    (0..4).any(|i| hebnix_sdk::input::get_xinput_state(i).is_some())
}

/// whichever controller is actually connected right now - real xinput takes
/// priority when both happen to be plugged in, since that's the lower-latency
/// native path
fn effective_backend() -> Backend {
    if xinput_present() {
        return Backend::XInput;
    }
    match hebnix_sdk::input::current_dinput_layout() {
        Some(DInputLayout::PlayStation) => {
            Backend::PlayStation(hebnix_sdk::input::current_ps_gen().unwrap_or(PsGen::Ds5))
        }
        _ => Backend::Generic,
    }
}

/// the dinput canonical button name for a bind, regardless of which backend
/// it was originally captured on - None for dinput_raw (paddles/misc
/// buttons), which have no cross-backend equivalent to translate through.
fn dinput_name_for(bind: &HotkeyBind) -> Option<&'static str> {
    match bind.controller_type.as_str() {
        "xinput" => hebnix_sdk::input::xinput_mask_to_dinput_name(bind.controller_button as u16),
        "dinput" => hebnix_sdk::input::DINPUT_BUTTONS.get(bind.controller_button as usize).copied(),
        "dualsense" => {
            let idx = LEGACY_DS4_ORDER
                .get(bind.controller_button as usize)
                .copied()
                .unwrap_or(0);
            hebnix_sdk::input::DINPUT_BUTTONS.get(idx as usize).copied()
        }
        _ => None,
    }
}

// dinput canonical name -> xinput icon key. mostly identical strings; only
// the shoulder/stick-click names differ (l1/r1/l3/r3 vs lb/rb/ls/rs). l2/r2
// (analog-only on xinput) and mode (no guide button in XINPUT_GAMEPAD) have
// no xinput icon - same set dinput_name_to_xinput_mask can't bridge either.
fn xinput_icon_key_from_dinput(name: &'static str) -> Option<&'static str> {
    Some(match name {
        "south" | "east" | "north" | "west" | "start" | "select" | "dpad_up" | "dpad_down"
        | "dpad_left" | "dpad_right" => name,
        "l1" => "lb",
        "r1" => "rb",
        "l3" => "ls",
        "r3" => "rs",
        _ => return None,
    })
}

/// label to show next to the icon so the user can see which input method is
/// currently satisfying a bind: "(Xinput)" / "(Playstation)" / "(Dinput)"
pub fn bind_type_label(bind: &HotkeyBind) -> &'static str {
    if !bind.is_controller {
        return "";
    }
    match effective_backend() {
        Backend::XInput => "(Xinput)",
        Backend::PlayStation(_) => "(Playstation)",
        Backend::Generic => "(Dinput)",
    }
}

/// (svg bytes, cache key) for a bind's icon, matching whichever controller
/// is currently connected. None for keyboard binds, dinput_raw (paddles -
/// no icon in the pack), or a button with no equivalent on the connected
/// backend (e.g. a dinput L2/R2 bind while only an xinput pad is plugged in).
pub fn bind_icon(bind: &HotkeyBind) -> Option<(&'static [u8], String)> {
    if !bind.is_controller {
        return None;
    }
    let key = dinput_name_for(bind)?;
    match effective_backend() {
        Backend::XInput => {
            let xkey = xinput_icon_key_from_dinput(key)?;
            XINPUT_ICONS
                .iter()
                .find(|(k, _)| *k == xkey)
                .map(|(k, b)| (*b, format!("xinput_{k}")))
        }
        Backend::PlayStation(psgen) => {
            let set: &[(&str, &[u8])] = match psgen {
                PsGen::Ds4 => &PS4_ICONS,
                PsGen::Ds5 => &PS5_ICONS,
            };
            let tag = match psgen {
                PsGen::Ds4 => "ps4",
                PsGen::Ds5 => "ps5",
            };
            set.iter()
                .find(|(k, _)| *k == key)
                .map(|(k, b)| (*b, format!("{tag}_{k}")))
        }
        Backend::Generic => DINPUT_ICONS
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(k, b)| (*b, format!("din_{k}"))),
    }
}
