//! generic dinput/hid controller support via gilrs (backed by the community
//! SDL_GameControllerDB), covering anything that isn't a native XInput device:
//! PS4/PS5 pads, 8BitDo pads in dinput mode, Switch-style pads, and any other
//! controller with an SDL community mapping.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use gilrs::{Gamepad, GamepadId, GilrsBuilder};

// gilrs 0.10.10 vendors a snapshot of SDL_GameControllerDB that's missing
// newer controllers (e.g. no "8BitDo Ultimate 2" entry, only the older
// "Ultimate"/"Ultimate C"/"Ultimate 2C" pads), so recognised-but-outdated
// devices fall back to gilrs' raw/positional layout instead of the real one.
// pulled fresh from https://github.com/mdqinc/SDL_GameControllerDB
// (zlib licensed) - update this file periodically for newly released pads.
const GAME_CONTROLLER_DB: &str = include_str!("../../assets/gamecontrollerdb.txt");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Xbox,
    PlayStation,
    Nintendo,
}

/// which playstation generation, only meaningful when Layout::PlayStation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PsGen {
    Ds4,
    Ds5,
}

const SONY_VID: u16 = 0x054C;
const NINTENDO_VID: u16 = 0x057E;

// (pid, gen) - DualSense/DualSense Edge (PS5) vs DualShock 4 incl v2 and the
// USB wireless adapter (PS4)
const SONY_PIDS: [(u16, PsGen); 5] = [
    (0x0CE6, PsGen::Ds5),
    (0x0DF2, PsGen::Ds5),
    (0x05C4, PsGen::Ds4),
    (0x09CC, PsGen::Ds4),
    (0x0BA0, PsGen::Ds4),
];

fn detect_layout(name: &str, vendor: Option<u16>) -> Layout {
    if vendor == Some(SONY_VID) {
        return Layout::PlayStation;
    }
    if vendor == Some(NINTENDO_VID) {
        return Layout::Nintendo;
    }
    let n = name.to_ascii_lowercase();
    if n.contains("dualshock") || n.contains("dualsense") || n.contains("playstation") || n.contains("sony") {
        Layout::PlayStation
    } else if n.contains("switch") || n.contains("joy-con") || n.contains("joycon") || n.contains("nintendo") {
        Layout::Nintendo
    } else {
        // covers xbox-style pads, including 8bitdo's dinput mode (already
        // labelled/positioned the xbox way on the physical controller)
        Layout::Xbox
    }
}

// defaults to Ds5 (current-gen) when the pid isn't recognised but the
// device is still a sony pad (e.g. connected over a generic bt stack)
fn detect_ps_gen(product: Option<u16>) -> PsGen {
    product
        .and_then(|pid| SONY_PIDS.iter().find(|(p, _)| *p == pid).map(|(_, g)| *g))
        .unwrap_or(PsGen::Ds5)
}

// canonical button names, stable regardless of controller brand/layout.
// order doubles as the persisted index used in HotkeyBind::controller_button.
pub const DINPUT_BUTTONS: [&str; 17] = [
    "south", "east", "north", "west", "l1", "r1", "l2", "r2", "l3", "r3", "select", "start",
    "mode", "dpad_up", "dpad_down", "dpad_left", "dpad_right",
];

const XBOX_LABELS: [(&str, &str); 17] = [
    ("south", "A"),
    ("east", "B"),
    ("north", "Y"),
    ("west", "X"),
    ("l1", "LB"),
    ("r1", "RB"),
    ("l2", "LT"),
    ("r2", "RT"),
    ("l3", "L-Stick"),
    ("r3", "R-Stick"),
    ("select", "Back"),
    ("start", "Start"),
    ("mode", "Guide"),
    ("dpad_up", "D-Pad Up"),
    ("dpad_down", "D-Pad Down"),
    ("dpad_left", "D-Pad Left"),
    ("dpad_right", "D-Pad Right"),
];

const PLAYSTATION_LABELS: [(&str, &str); 17] = [
    ("south", "Cross"),
    ("east", "Circle"),
    ("north", "Triangle"),
    ("west", "Square"),
    ("l1", "L1"),
    ("r1", "R1"),
    ("l2", "L2"),
    ("r2", "R2"),
    ("l3", "L3"),
    ("r3", "R3"),
    ("select", "Share"),
    ("start", "Options"),
    ("mode", "PS"),
    ("dpad_up", "D-Pad Up"),
    ("dpad_down", "D-Pad Down"),
    ("dpad_left", "D-Pad Left"),
    ("dpad_right", "D-Pad Right"),
];

const NINTENDO_LABELS: [(&str, &str); 17] = [
    ("south", "B"),
    ("east", "A"),
    ("north", "X"),
    ("west", "Y"),
    ("l1", "L"),
    ("r1", "R"),
    ("l2", "ZL"),
    ("r2", "ZR"),
    ("l3", "L-Stick"),
    ("r3", "R-Stick"),
    ("select", "Minus"),
    ("start", "Plus"),
    ("mode", "Home"),
    ("dpad_up", "D-Pad Up"),
    ("dpad_down", "D-Pad Down"),
    ("dpad_left", "D-Pad Left"),
    ("dpad_right", "D-Pad Right"),
];

fn labels_for(layout: Layout) -> &'static [(&'static str, &'static str); 17] {
    match layout {
        Layout::Xbox => &XBOX_LABELS,
        Layout::PlayStation => &PLAYSTATION_LABELS,
        Layout::Nintendo => &NINTENDO_LABELS,
    }
}

/// readable label for a canonical button name, using the detected pad's
/// layout if one is connected, else falling back to xbox-style labels.
pub fn button_display(name: &str) -> String {
    let layout = current_layout().unwrap_or(Layout::Xbox);
    labels_for(layout)
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| name.to_string())
}

fn pressed_set() -> &'static Mutex<HashSet<&'static str>> {
    static PRESSED: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    PRESSED.get_or_init(|| Mutex::new(HashSet::new()))
}

// raw platform-native codes for buttons gilrs can't name (Button::Unknown) -
// paddles, misc/back buttons, etc. gilrs' SDL mapping parser recognises
// "paddle1".."paddle4" and "misc1" in the mapping string but has no Button
// enum slot for them, so they all collapse to Button::Unknown; the only way
// to tell them apart is by this raw code, which is stable for a given
// device+platform+connection mode but not portable/human-named.
fn raw_pressed_set() -> &'static Mutex<HashSet<u32>> {
    static RAW: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();
    RAW.get_or_init(|| Mutex::new(HashSet::new()))
}

static CONNECTED: AtomicBool = AtomicBool::new(false);
static MONITOR_STARTED: AtomicBool = AtomicBool::new(false);
// 0 none, 1 xbox, 2 playstation, 3 nintendo
static LAYOUT: AtomicU8 = AtomicU8::new(0);
// 0 unset, 1 ds4, 2 ds5 - only meaningful when LAYOUT is playstation
static PS_GEN: AtomicU8 = AtomicU8::new(0);

fn current_layout() -> Option<Layout> {
    match LAYOUT.load(Ordering::Relaxed) {
        1 => Some(Layout::Xbox),
        2 => Some(Layout::PlayStation),
        3 => Some(Layout::Nintendo),
        _ => None,
    }
}

/// which playstation pad is currently connected, if any (only meaningful
/// after start_dinput_monitor and while a playstation pad is connected)
pub fn current_ps_gen() -> Option<PsGen> {
    if current_layout() != Some(Layout::PlayStation) {
        return None;
    }
    match PS_GEN.load(Ordering::Relaxed) {
        1 => Some(PsGen::Ds4),
        2 => Some(PsGen::Ds5),
        _ => None,
    }
}

/// which brand/layout the currently connected dinput pad uses, if any
pub fn current_dinput_layout() -> Option<Layout> {
    current_layout()
}

fn set_layout(layout: Option<Layout>, ps_gen: Option<PsGen>) {
    LAYOUT.store(
        match layout {
            Some(Layout::Xbox) => 1,
            Some(Layout::PlayStation) => 2,
            Some(Layout::Nintendo) => 3,
            None => 0,
        },
        Ordering::Relaxed,
    );
    PS_GEN.store(
        match ps_gen {
            Some(PsGen::Ds4) => 1,
            Some(PsGen::Ds5) => 2,
            None => 0,
        },
        Ordering::Relaxed,
    );
    CONNECTED.store(layout.is_some(), Ordering::Relaxed);
}

/// true if a dinput controller is connected (only meaningful after
/// start_dinput_monitor)
pub fn is_dinput_connected() -> bool {
    CONNECTED.load(Ordering::Relaxed)
}

/// sorted list of currently held canonical button names
pub fn get_dinput_inputs() -> Vec<String> {
    let mut list: Vec<String> = pressed_set()
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.to_string())
        .collect();
    list.sort();
    list
}

/// sorted list of currently held raw codes (paddles/misc buttons gilrs
/// can't name - see raw_pressed_set)
pub fn get_dinput_raw_inputs() -> Vec<u32> {
    let mut list: Vec<u32> = raw_pressed_set().lock().unwrap().iter().copied().collect();
    list.sort_unstable();
    list
}

/// is this raw code currently held
pub fn is_dinput_raw_pressed(code: u32) -> bool {
    raw_pressed_set().lock().unwrap().contains(&code)
}

fn read_pressed(gamepad: &Gamepad) -> HashSet<&'static str> {
    use gilrs::Button;
    const MAP: [(Button, &str); 17] = [
        (Button::South, "south"),
        (Button::East, "east"),
        (Button::North, "north"),
        (Button::West, "west"),
        (Button::LeftTrigger, "l1"),
        (Button::RightTrigger, "r1"),
        (Button::LeftTrigger2, "l2"),
        (Button::RightTrigger2, "r2"),
        (Button::LeftThumb, "l3"),
        (Button::RightThumb, "r3"),
        (Button::Select, "select"),
        (Button::Start, "start"),
        (Button::Mode, "mode"),
        (Button::DPadUp, "dpad_up"),
        (Button::DPadDown, "dpad_down"),
        (Button::DPadLeft, "dpad_left"),
        (Button::DPadRight, "dpad_right"),
    ];
    MAP.iter()
        .filter(|(btn, _)| gamepad.is_pressed(*btn))
        .map(|(_, name)| *name)
        .collect()
}

/// spawn the bg thread that reads controller state and reconnects on drop.
/// safe to call twice.
pub fn start_dinput_monitor() {
    if MONITOR_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("dinput-controller-monitor".into())
        .spawn(monitor_loop)
        .ok();
}

fn monitor_loop() {
    let built = GilrsBuilder::new()
        // use our up-to-date db instead of gilrs' bundled (older) snapshot
        .add_included_mappings(false)
        .add_mappings(GAME_CONTROLLER_DB)
        .build();
    let Ok(mut gilrs) = built else {
        tracing::warn!("gilrs init failed, dinput controller support disabled");
        return;
    };

    let mut active: Option<GamepadId> = None;

    loop {
        // drain pending events so gilrs' internal state is current, pick up
        // connects/disconnects, and track raw codes for buttons gilrs can't
        // name (paddles etc - see raw_pressed_set)
        while let Some(event) = gilrs.next_event() {
            match event.event {
                gilrs::EventType::Connected if active.is_none() => {
                    active = Some(event.id);
                }
                gilrs::EventType::Disconnected if active == Some(event.id) => {
                    active = None;
                    raw_pressed_set().lock().unwrap().clear();
                }
                gilrs::EventType::ButtonPressed(gilrs::Button::Unknown, code)
                    if active == Some(event.id) =>
                {
                    raw_pressed_set().lock().unwrap().insert(code.into_u32());
                }
                gilrs::EventType::ButtonReleased(gilrs::Button::Unknown, code)
                    if active == Some(event.id) =>
                {
                    raw_pressed_set().lock().unwrap().remove(&code.into_u32());
                }
                _ => {}
            }
        }

        // fall back to the first still-connected pad if we haven't latched
        // onto one yet (covers pads already plugged in at startup)
        if active.is_none() {
            active = gilrs.gamepads().find(|(_, g)| g.is_connected()).map(|(id, _)| id);
        }

        match active.and_then(|id| gilrs.connected_gamepad(id)) {
            Some(gamepad) => {
                let layout = detect_layout(gamepad.name(), gamepad.vendor_id());
                let ps_gen = (layout == Layout::PlayStation)
                    .then(|| detect_ps_gen(gamepad.product_id()));
                set_layout(Some(layout), ps_gen);
                *pressed_set().lock().unwrap() = read_pressed(&gamepad);
            }
            None => {
                active = None;
                set_layout(None, None);
                pressed_set().lock().unwrap().clear();
                raw_pressed_set().lock().unwrap().clear();
            }
        }

        std::thread::sleep(Duration::from_millis(16));
    }
}
