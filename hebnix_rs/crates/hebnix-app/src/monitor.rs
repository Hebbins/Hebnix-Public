//! background monitor: RL presence, statsapi availability, topmost tracking.
//! python's 3 polling threads folded into one ticking task.

use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::Sender;

use crate::messages::AppMsg;
use crate::statsapi_ini;

// wait for statsapi to be ready
const STARTUP_GRACE: Duration = Duration::from_secs(60);

/// shared between the ui thread and the monitor
pub struct MonitorShared {
    pub api_port: u16,
    pub statsapi_path: String,
    pub rl_path: String,
}

pub struct Monitor {
    #[cfg(not(feature = "lite"))]
    shared: Arc<Mutex<MonitorShared>>,
    running: Arc<AtomicBool>,
}

fn is_api_alive(port: u16) -> bool {
    format!("127.0.0.1:{port}")
        .parse()
        .ok()
        .and_then(|addr| TcpStream::connect_timeout(&addr, Duration::from_millis(500)).ok())
        .is_some()
}

impl Monitor {
    pub fn start(shared: MonitorShared, tx: Sender<AppMsg>, ctx: eframe::egui::Context) -> Self {
        let shared = Arc::new(Mutex::new(shared));
        let running = Arc::new(AtomicBool::new(true));

        {
            let shared = Arc::clone(&shared);
            let running = Arc::clone(&running);
            std::thread::Builder::new()
                .name("rl-monitor".into())
                .spawn(move || monitor_loop(&shared, &running, &tx, &ctx))
                .expect("failed to spawn monitor thread");
        }

        Self {
            #[cfg(not(feature = "lite"))]
            shared,
            running,
        }
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    #[cfg(not(feature = "lite"))]
    pub fn update_shared(&self, shared: MonitorShared) {
        if let Ok(mut current) = self.shared.lock() {
            *current = shared;
        }
    }
}

fn monitor_loop(
    shared: &Mutex<MonitorShared>,
    running: &AtomicBool,
    tx: &Sender<AppMsg>,
    ctx: &eframe::egui::Context,
) {
    tracing::info!("monitor thread started");
    let mut tick: u64 = 0;
    let mut last_topmost: Option<bool> = None;
    let mut last_window_mode: Option<hebnix_sdk::save_file::WindowMode> = None;
    // one statsapi fix/diagnose attempt per game session, once the game has had
    // long enough to open its socket
    let mut fix_attempted = false;
    let mut window_seen_at: Option<std::time::Instant> = None;

    while running.load(Ordering::Relaxed) {
        // every 0.5s: topmost tracking
        let rl_focused = hebnix_sdk::process::is_rocket_league_focused();
        // overlay must never outlive game focus. the ui loop hides it too but
        // can stall while the main window's hidden, so this thread is the real
        // enforcer.
        if !rl_focused {
            crate::overlay::enforce_hidden();
        }
        crate::winutil::note_foreground();
        let on_screen = !crate::winutil::main_window_hidden();
        let should_be_topmost =
            on_screen && (rl_focused || crate::winutil::foreground_window_is_ours());
        if last_topmost != Some(should_be_topmost) {
            last_topmost = Some(should_be_topmost);
            let _ = tx.send(AppMsg::Topmost(should_be_topmost));
            ctx.request_repaint();
        }

        // every 2.5s: RL / statsapi status
        if tick % 5 == 0 {
            let (port, statsapi_path, rl_path) = {
                let s = shared.lock().unwrap();
                (s.api_port, s.statsapi_path.clone(), s.rl_path.clone())
            };

            // liveness by process name. the exe path goes unreadable once eac
            // locks the process, so needing it made a running game look closed.
            let rl_open = hebnix_sdk::process::is_rocket_league_running();
            // path resolution is best-effort (works during startup before eac)
            let root_dir = if rl_open {
                hebnix_sdk::process::find_rocket_league()
                    .map(|info| info.root_dir.to_string_lossy().to_string())
            } else {
                None
            };
            let api_open = is_api_alive(port);

            if !rl_open {
                fix_attempted = false;
                window_seen_at = None;
            } else if api_open {
                window_seen_at = None;
            } else if !fix_attempted {
                // clock runs from the game's window, not its process
                if window_seen_at.is_none() && hebnix_sdk::process::rocket_league_hwnd().is_some() {
                    window_seen_at = Some(std::time::Instant::now());
                }
                match window_seen_at {
                    Some(seen) if seen.elapsed() >= STARTUP_GRACE => {
                        fix_attempted = true;
                        window_seen_at = None;
                        let ini_path = statsapi_ini::resolve_ini_path(&statsapi_path, &rl_path);
                        match std::fs::read_to_string(&ini_path) {
                            Ok(content) => {
                                let flat = content.to_lowercase().replace(' ', "");
                                if flat.contains("packetsendrate=0") {
                                    match statsapi_ini::update_ini_setting(
                                        &ini_path,
                                        "PacketSendRate",
                                        "20",
                                    ) {
                                        Ok(()) => {
                                            let _ = tx.send(AppMsg::StatsApiInitialised);
                                        }
                                        Err(e) => {
                                            let _ = tx.send(AppMsg::Log(format!(
                                                "[Monitor] Failed to update PacketSendRate: {e}"
                                            )));
                                        }
                                    }
                                } else {
                                    let _ = tx.send(AppMsg::Log(format!(
                                        "[Monitor] Rocket League has been up a while and port \
                                         {port} still isn't open. If you changed PacketSendRate \
                                         after launching, restart the game."
                                    )));
                                }
                            }
                            Err(_) => {
                                let _ = tx.send(AppMsg::Log(format!(
                                    "[Monitor] Cannot read {} to check PacketSendRate.",
                                    ini_path.display()
                                )));
                            }
                        }
                    }
                    _ => {}
                }
            }

            // TASystemSettings.ini, the game rewrites it on apply
            let window_mode = hebnix_sdk::utils::system_settings::window_mode();
            if window_mode.is_some() && window_mode != last_window_mode {
                last_window_mode = window_mode;
                if let Some(mode) = window_mode {
                    let _ = tx.send(AppMsg::WindowMode(mode));
                }
            }

            tracing::debug!(rl_open, api_open, port, ?root_dir, "monitor status tick");
            let _ = tx.send(AppMsg::RlStatus {
                rl_open,
                api_open,
                root_dir,
            });
            ctx.request_repaint();
        }

        tick += 1;
        std::thread::sleep(Duration::from_millis(500));
    }
}
