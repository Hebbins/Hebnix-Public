use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SEQUENCE: &[&str] = &[
    "up", "up", "down", "down", "left", "right", "left", "right", "b", "a",
];
const URL: &str = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";
const TIMEOUT: Duration = Duration::from_millis(1500);

pub struct SecretSequenceListener {
    stop: Arc<AtomicBool>,
}

impl SecretSequenceListener {
    pub fn new() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn start(&self) {
        let stop = Arc::clone(&self.stop);
        std::thread::Builder::new()
            .name("secretsequence-listener".into())
            .spawn(move || {
                let mut was_down = [false; 6];
                let mut progress = 0usize;
                let mut last_key_at: Option<Instant> = None;

                while !stop.load(Ordering::Relaxed) {
                    if !crate::winutil::foreground_window_is_ours() {
                        progress = 0;
                        last_key_at = None;
                        was_down = [false; 6];
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }

                    if let Some(t) = last_key_at {
                        if t.elapsed() > TIMEOUT {
                            progress = 0;
                        }
                    }

                    const KEYS: [&str; 6] = ["up", "down", "left", "right", "b", "a"];
                    for (i, key) in KEYS.iter().enumerate() {
                        let is_down = hebnix_sdk::input::is_key_pressed(key);
                        if is_down && !was_down[i] {
                            if *key == SEQUENCE[progress] {
                                progress += 1;
                                last_key_at = Some(Instant::now());
                                if progress == SEQUENCE.len() {
                                    let _ = std::process::Command::new("cmd")
                                        .args(["/C", "start", "", URL])
                                        .spawn();
                                    progress = 0;
                                    last_key_at = None;
                                }
                            } else if progress > 0 {
                                progress = 0;
                            }
                        }
                        was_down[i] = is_down;
                    }

                    std::thread::sleep(Duration::from_millis(10));
                }
            })
            .ok();
    }
}

impl Drop for SecretSequenceListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
