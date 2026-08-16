//! tcp client for RL's Stats API.
//!
//! connects to the game's local json-over-tcp socket (only open when
//! PacketSendRate > 0 in DefaultStatsAPI.ini), parses events and pushes them
//! over a crossbeam channel.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::Sender;

use crate::stats::models::StatsEvent;
use crate::stats::parser::{extract_json_objects, parse_message};

const HANDSHAKE: &[u8] = b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n";

/// client for RL's built-in Stats API.
///
/// ```no_run
/// use hebnix_sdk::stats::StatsClient;
///
/// let (tx, rx) = crossbeam_channel::unbounded();
/// let client = StatsClient::new("127.0.0.1", 49123);
/// client.start(tx);
/// while let Ok(event) = rx.recv() {
///     println!("{}", event.event_type);
/// }
/// ```
pub struct StatsClient {
    host: String,
    port: Arc<AtomicU16>,
    running: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    thread: std::sync::Mutex<Option<JoinHandle<()>>>,
}

impl StatsClient {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port: Arc::new(AtomicU16::new(port)),
            running: Arc::new(AtomicBool::new(false)),
            connected: Arc::new(AtomicBool::new(false)),
            thread: std::sync::Mutex::new(None),
        }
    }

    /// true while the reader thread has an open connection to the game.
    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub fn port(&self) -> u16 {
        self.port.load(Ordering::Relaxed)
    }

    /// change the port. picked up on next (re)connect.
    pub fn set_port(&self, port: u16) {
        self.port.store(port, Ordering::Relaxed);
    }

    /// start the bg reader thread. no-op if already running.
    pub fn start(&self, sender: Sender<StatsEvent>) {
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let host = self.host.clone();
        let port = Arc::clone(&self.port);
        let running = Arc::clone(&self.running);
        let connected = Arc::clone(&self.connected);

        let handle = std::thread::Builder::new()
            .name("stats-reader".into())
            .spawn(move || reader_loop(&host, &port, &running, &connected, &sender))
            .expect("failed to spawn stats reader thread");
        *self.thread.lock().unwrap() = Some(handle);
    }

    /// stop the reader thread and disconnect.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        self.connected.store(false, Ordering::SeqCst);
        if let Some(handle) = self.thread.lock().unwrap().take() {
            let _ = handle.join();
        }
    }
}

impl Drop for StatsClient {
    fn drop(&mut self) {
        self.stop();
    }
}

fn try_connect(host: &str, port: u16) -> std::io::Result<TcpStream> {
    let addr = format!("{host}:{port}");
    let stream = TcpStream::connect_timeout(
        &addr.parse().map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("bad addr: {e}"))
        })?,
        Duration::from_secs(5),
    )?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    let mut s = stream;
    // RL Stats API wants a bare HTTP GET upgrade, not a websocket.
    s.write_all(HANDSHAKE)?;
    Ok(s)
}

fn reader_loop(
    host: &str,
    port: &AtomicU16,
    running: &AtomicBool,
    connected: &AtomicBool,
    sender: &Sender<StatsEvent>,
) {
    let mut stream: Option<TcpStream> = None;
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 65536];

    while running.load(Ordering::Relaxed) {
        if stream.is_none() {
            match try_connect(host, port.load(Ordering::Relaxed)) {
                Ok(s) => {
                    stream = Some(s);
                    connected.store(true, Ordering::Relaxed);
                    buf.clear();
                }
                Err(_) => {
                    connected.store(false, Ordering::Relaxed);
                    // sleep in short slices so stop() stays responsive
                    for _ in 0..20 {
                        if !running.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    continue;
                }
            }
        }

        let s = stream.as_mut().unwrap();
        match s.read(&mut chunk) {
            Ok(0) => {
                // game closed the connection
                stream = None;
                connected.store(false, Ordering::Relaxed);
                buf.clear();
            }
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                let (objects, rest) = extract_json_objects(&buf);
                buf = rest;
                for raw in objects {
                    if let Ok(event) = parse_message(&raw) {
                        if sender.send(event).is_err() {
                            // receiver's gone, nothing to send to
                            running.store(false, Ordering::Relaxed);
                            connected.store(false, Ordering::Relaxed);
                            return;
                        }
                    }
                }
                if buf.len() > 1_000_000 {
                    buf.clear();
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                // read timeout, just loop around and recheck running
            }
            Err(_) => {
                stream = None;
                connected.store(false, Ordering::Relaxed);
                buf.clear();
            }
        }
    }
    connected.store(false, Ordering::Relaxed);
}
