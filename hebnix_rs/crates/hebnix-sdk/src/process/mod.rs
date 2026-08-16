//! RL process detection + window helpers.

pub mod detector;
pub mod window;

pub use detector::{
    RlPlatform, RlProcessInfo, detect_platform, find_rocket_league, get_save_data_path,
    is_rocket_league_running,
};
pub use window::{
    get_rocket_league_window_rect, is_cursor_inside_rl_window, is_rocket_league_focused,
    rocket_league_hwnd, rocket_league_monitor_size,
};
