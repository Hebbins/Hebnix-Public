//! playstation controller support via hidapi, buttons only
//
// DualSense/DualSense Edge (PS5) and DualShock 4 (PS4), USB + bluetooth.
// button names match the python port ("btn_cross", "btn_l1", ...). DS4 Share maps to "btn_create".

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use hidapi::HidApi;

const SONY_VID: u16 = 0x054C;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PsModel {
    // DualSense / DualSense Edge (PS5)
    Ds5,
    // DualShock 4 (PS4), incl v2 and the USB wireless adapter
    Ds4,
}

impl PsModel {
    pub fn as_str(self) -> &'static str {
        match self {
            PsModel::Ds5 => "ds5",
            PsModel::Ds4 => "ds4",
        }
    }
}

// (pid, model)
const PS_PIDS: [(u16, PsModel); 5] = [
    (0x0CE6, PsModel::Ds5), // DualSense
    (0x0DF2, PsModel::Ds5), // DualSense Edge
    (0x05C4, PsModel::Ds4), // DualShock 4 v1
    (0x09CC, PsModel::Ds4), // DualShock 4 v2
    (0x0BA0, PsModel::Ds4), // DS4 USB wireless adapter
];

pub const DS4_BUTTONS: [&str; 19] = [
    "btn_cross",
    "btn_circle",
    "btn_square",
    "btn_triangle",
    "btn_l1",
    "btn_r1",
    "btn_l2",
    "btn_r2",
    "btn_l3",
    "btn_r3",
    "btn_options",
    "btn_create",
    "btn_ps",
    "btn_touchpad",
    "btn_mute",
    "btn_up",
    "btn_down",
    "btn_left",
    "btn_right",
];

pub const DS4_BUTTON_DISPLAY: [(&str, &str); 19] = [
    ("btn_cross", "Cross"),
    ("btn_circle", "Circle"),
    ("btn_square", "Square"),
    ("btn_triangle", "Triangle"),
    ("btn_l1", "L1"),
    ("btn_r1", "R1"),
    ("btn_l2", "L2"),
    ("btn_r2", "R2"),
    ("btn_l3", "L3"),
    ("btn_r3", "R3"),
    ("btn_options", "Options"),
    ("btn_create", "Create"),
    ("btn_ps", "PS"),
    ("btn_touchpad", "Touchpad"),
    ("btn_mute", "Mute"),
    ("btn_up", "D-Pad Up"),
    ("btn_down", "D-Pad Down"),
    ("btn_left", "D-Pad Left"),
    ("btn_right", "D-Pad Right"),
];

fn pressed_set() -> &'static Mutex<HashSet<String>> {
    static PRESSED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    PRESSED.get_or_init(|| Mutex::new(HashSet::new()))
}

static CONNECTED: AtomicBool = AtomicBool::new(false);
static MONITOR_STARTED: AtomicBool = AtomicBool::new(false);
static MODEL: AtomicU8 = AtomicU8::new(0); // 0 none, 1 ds4, 2 ds5

/// true if a controller is connected (only meaningful after start_dualsense_monitor)
pub fn is_dualsense_connected() -> bool {
    CONNECTED.load(Ordering::Relaxed)
}

/// which playstation pad the monitor has open
pub fn ps_model() -> Option<PsModel> {
    match MODEL.load(Ordering::Relaxed) {
        1 => Some(PsModel::Ds4),
        2 => Some(PsModel::Ds5),
        _ => None,
    }
}

fn set_connected(model: Option<PsModel>) {
    MODEL.store(
        match model {
            Some(PsModel::Ds4) => 1,
            Some(PsModel::Ds5) => 2,
            None => 0,
        },
        Ordering::Relaxed,
    );
    CONNECTED.store(model.is_some(), Ordering::Relaxed);
}

/// sorted list of currently held button names
pub fn get_dualsense_inputs() -> Vec<String> {
    let mut list: Vec<String> = pressed_set().lock().unwrap().iter().cloned().collect();
    list.sort();
    list
}

// decode the 3 button bytes, same layout on DS4 and DualSense
fn decode_buttons(b1: u8, b2: u8, b3: u8) -> HashSet<String> {
    let mut held: HashSet<String> = HashSet::new();
    let mut add = |name: &str| {
        held.insert(name.to_string());
    };

    // face buttons, high nibble of b1
    if b1 & 0x10 != 0 {
        add("btn_square");
    }
    if b1 & 0x20 != 0 {
        add("btn_cross");
    }
    if b1 & 0x40 != 0 {
        add("btn_circle");
    }
    if b1 & 0x80 != 0 {
        add("btn_triangle");
    }

    // d-pad, low nibble of b1, hat encoding (8 = released)
    match b1 & 0x0F {
        0 => add("btn_up"),
        1 => {
            add("btn_up");
            add("btn_right");
        }
        2 => add("btn_right"),
        3 => {
            add("btn_down");
            add("btn_right");
        }
        4 => add("btn_down"),
        5 => {
            add("btn_down");
            add("btn_left");
        }
        6 => add("btn_left"),
        7 => {
            add("btn_up");
            add("btn_left");
        }
        _ => {}
    }

    if b2 & 0x01 != 0 {
        add("btn_l1");
    }
    if b2 & 0x02 != 0 {
        add("btn_r1");
    }
    if b2 & 0x04 != 0 {
        add("btn_l2");
    }
    if b2 & 0x08 != 0 {
        add("btn_r2");
    }
    if b2 & 0x10 != 0 {
        add("btn_create");
    } // Share on DS4
    if b2 & 0x20 != 0 {
        add("btn_options");
    }
    if b2 & 0x40 != 0 {
        add("btn_l3");
    }
    if b2 & 0x80 != 0 {
        add("btn_r3");
    }

    if b3 & 0x01 != 0 {
        add("btn_ps");
    }
    if b3 & 0x02 != 0 {
        add("btn_touchpad");
    }
    if b3 & 0x04 != 0 {
        add("btn_mute");
    } // DualSense only

    held
}

// pull the held button names out of an input report.
// button byte offset depends on device + report id:
//   DualSense: USB report 0x01 -> byte 8, BT report 0x31 -> byte 9
//   DS4: USB/basic-BT report 0x01 -> byte 5, full BT report 0x11 -> byte 7
fn parse_report(model: PsModel, report: &[u8]) -> Option<HashSet<String>> {
    let base = match (model, report.first()?) {
        (PsModel::Ds5, 0x01) if report.len() >= 11 => 8usize,
        (PsModel::Ds5, 0x31) if report.len() >= 12 => 9usize,
        (PsModel::Ds4, 0x01) if report.len() >= 8 => 5usize,
        (PsModel::Ds4, 0x11) if report.len() >= 10 => 7usize,
        _ => return None,
    };
    let b1 = *report.get(base)?;
    let b2 = *report.get(base + 1)?;
    let b3 = *report.get(base + 2)?;
    Some(decode_buttons(b1, b2, b3))
}

/// spawn the bg thread that reads controller state and reconnects on drop. safe to call twice.
pub fn start_dualsense_monitor() {
    if MONITOR_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("ps-controller-monitor".into())
        .spawn(monitor_loop)
        .ok();
}

fn monitor_loop() {
    loop {
        let device = HidApi::new().ok().and_then(|api| {
            PS_PIDS
                .iter()
                .find_map(|(pid, model)| api.open(SONY_VID, *pid).ok().map(|d| (d, *model)))
        });

        let Some((device, model)) = device else {
            set_connected(None);
            pressed_set().lock().unwrap().clear();
            std::thread::sleep(Duration::from_secs(2));
            continue;
        };

        set_connected(Some(model));
        tracing::info!("PlayStation controller connected ({model:?})");
        let mut buf = [0u8; 128];

        loop {
            match device.read_timeout(&mut buf, 100) {
                Ok(0) => {} // timeout, nothing
                Ok(n) => {
                    if let Some(held) = parse_report(model, &buf[..n]) {
                        *pressed_set().lock().unwrap() = held;
                    }
                }
                Err(_) => {
                    // disconnected, drop it and retry
                    set_connected(None);
                    pressed_set().lock().unwrap().clear();
                    tracing::info!("PlayStation controller disconnected");
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}
