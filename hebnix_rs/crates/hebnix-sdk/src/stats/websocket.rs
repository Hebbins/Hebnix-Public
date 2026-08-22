use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tungstenite::{Message, client::client};

use crate::stats::models::StatsEvent;
use crate::stats::parser::parse_message;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsCommand {
    #[serde(rename = "Command")]
    pub command: String,

    #[serde(rename = "Data", skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl WsCommand {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            data: None,
        }
    }

    pub fn with_data<T: Serialize>(
        command: impl Into<String>,
        data: T,
    ) -> Result<Self, serde_json::Error> {
        Ok(Self {
            command: command.into(),
            data: Some(serde_json::to_value(data)?),
        })
    }
}

pub struct WsStatsClient {
    host: String,
    port: Arc<AtomicU16>,
    running: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
    cmd_tx: Sender<WsCommand>,
    cmd_rx: Receiver<WsCommand>,
}

impl WsStatsClient {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        Self {
            host: host.into(),
            port: Arc::new(AtomicU16::new(port)),
            running: Arc::new(AtomicBool::new(false)),
            connected: Arc::new(AtomicBool::new(false)),
            thread: Mutex::new(None),
            cmd_tx,
            cmd_rx,
        }
    }

    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub fn port(&self) -> u16 {
        self.port.load(Ordering::Relaxed)
    }

    pub fn set_port(&self, port: u16) {
        self.port.store(port, Ordering::Relaxed);
    }

    pub fn send_command(
        &self,
        cmd: WsCommand,
    ) -> Result<(), crossbeam_channel::SendError<WsCommand>> {
        self.cmd_tx.send(cmd)
    }

    pub fn start(&self, event_sender: Sender<StatsEvent>) {
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let host = self.host.clone();
        let port = Arc::clone(&self.port);
        let running = Arc::clone(&self.running);
        let connected = Arc::clone(&self.connected);
        let cmd_rx = self.cmd_rx.clone();

        let handle = std::thread::Builder::new()
            .name("ws-stats-rw".into())
            .spawn(move || {
                reader_writer_loop(&host, &port, &running, &connected, &event_sender, &cmd_rx)
            })
            .expect("failed to spawn websocket rw thread");
        *self.thread.lock().unwrap() = Some(handle);
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        self.connected.store(false, Ordering::SeqCst);
        if let Some(handle) = self.thread.lock().unwrap().take() {
            let _ = handle.join();
        }
    }
}

impl Drop for WsStatsClient {
    fn drop(&mut self) {
        self.stop();
    }
}

fn reader_writer_loop(
    host: &str,
    port: &AtomicU16,
    running: &AtomicBool,
    connected: &AtomicBool,
    event_sender: &Sender<StatsEvent>,
    cmd_rx: &Receiver<WsCommand>,
) {
    while running.load(Ordering::Relaxed) {
        let current_port = port.load(Ordering::Relaxed);
        let addr = format!("{}:{}", host, current_port);

        let stream = match TcpStream::connect(&addr) {
            Ok(s) => s,
            Err(_) => {
                connected.store(false, Ordering::Relaxed);
                // Sleep to prevent tight spinloop when game is closed[cite: 11]
                for _ in 0..20 {
                    if !running.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                continue;
            }
        };

        let _ = stream.set_read_timeout(Some(Duration::from_millis(10)));
        let url = format!("ws://{}/", addr);

        let mut socket = match client(url, stream) {
            Ok((s, _)) => s,
            Err(_) => {
                connected.store(false, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };

        connected.store(true, Ordering::Relaxed);

        while running.load(Ordering::Relaxed) {
            match socket.read() {
                Ok(Message::Text(text)) => {
                    if let Ok(event) = parse_message(text.as_bytes()) {
                        if event_sender.send(event).is_err() {
                            running.store(false, Ordering::Relaxed);
                            break;
                        }
                    }
                }
                Ok(Message::Binary(bin)) => {
                    if let Ok(event) = parse_message(&bin) {
                        if event_sender.send(event).is_err() {
                            running.store(false, Ordering::Relaxed);
                            break;
                        }
                    }
                }
                Ok(Message::Close(_)) => break,
                Err(tungstenite::Error::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(_) => break,
                _ => {}
            }

            while let Ok(cmd) = cmd_rx.try_recv() {
                if let Ok(json) = serde_json::to_string(&cmd) {
                    if socket.send(Message::Text(json.into())).is_err() {
                        break;
                    }
                }
            }
        }

        connected.store(false, Ordering::Relaxed);
    }
}
