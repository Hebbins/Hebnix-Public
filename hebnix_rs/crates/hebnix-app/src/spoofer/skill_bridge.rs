//! Local PsyNet websocket bridge used by rank spoofing.
//!
//! The config response is rewritten to point PerConURL/PerConURLv2 here. The
//! bridge forwards every websocket frame to the real service and rewrites only
//! server-to-client text frames containing the Skills envelope.

use std::collections::HashMap;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::Sender;
use tungstenite::client::IntoClientRequest;
use tungstenite::handshake::server::{Request, Response};
use tungstenite::http::{HeaderName, HeaderValue};
use tungstenite::{Error, Message, accept_hdr, connect};

use crate::messages::AppMsg;
use crate::spoofer::rules::{Body, RankRule, Rule};

const LISTEN_ADDR: &str = "127.0.0.1:8025";
const UPSTREAM_HOST: &str = "ws.rlpp.psynet.gg";
const FORWARD_HEADERS: &[&str] = &[
    "PsyToken",
    "PsySessionID",
    "PsyBuildID",
    "PsyEnvironment",
    "User-Agent",
];

pub struct SkillBridge {
    running: Arc<AtomicBool>,
}

impl SkillBridge {
    pub fn start(
        ranks: Arc<Mutex<HashMap<i32, (i32, f64)>>>,
        tx: Sender<AppMsg>,
        dump_path: PathBuf,
    ) -> Result<Self, String> {
        let _ = std::fs::write(&dump_path, "# Rank spoofer WebSocket frames (credentials omitted)\n");
        let listener = TcpListener::bind(LISTEN_ADDR).map_err(|error| {
            format!("cannot bind rank websocket bridge on {LISTEN_ADDR}: {error}")
        })?;
        let _ = std::fs::write(
            dump_path.with_file_name("rank_spoofer_status.log"),
            format!("Rank bridge listening on {LISTEN_ADDR}\n"),
        );
        let running = Arc::new(AtomicBool::new(true));
        let thread_running = Arc::clone(&running);
        let thread_tx = tx.clone();
        std::thread::Builder::new()
            .name("rank-skill-bridge".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if !thread_running.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let ranks = Arc::clone(&ranks);
                    let tx = thread_tx.clone();
                    let dump_path = dump_path.clone();
                    std::thread::spawn(move || {
                        if let Err(error) = handle_connection(stream, ranks, &dump_path) {
                            let _ = tx.send(AppMsg::Log(format!(
                                "[Spoofer] Rank websocket bridge: {error}"
                            )));
                        }
                    });
                }
            })
            .map_err(|error| format!("cannot start rank websocket bridge: {error}"))?;
        Ok(Self { running })
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        let _ = TcpStream::connect(LISTEN_ADDR);
    }
}

fn handle_connection(
    stream: TcpStream,
    ranks: Arc<Mutex<HashMap<i32, (i32, f64)>>>,
    dump_path: &std::path::Path,
) -> Result<(), String> {
    let request_state = Arc::new(Mutex::new(None::<(String, Vec<(String, String)>)>));
    let callback_state = Arc::clone(&request_state);
    let mut local = accept_hdr(stream, move |request: &Request, response: Response| {
        let headers = FORWARD_HEADERS
            .iter()
            .filter_map(|name| {
                request.headers().get(*name).and_then(|value| {
                    value
                        .to_str()
                        .ok()
                        .map(|value| ((*name).to_string(), value.to_string()))
                })
            })
            .collect();
        if let Ok(mut state) = callback_state.lock() {
            *state = Some((request.uri().to_string(), headers));
        }
        Ok(response)
    })
    .map_err(|error| format!("local websocket handshake failed: {error}"))?;

    let (path, headers) = request_state
        .lock()
        .ok()
        .and_then(|state| state.clone())
        .unwrap_or_else(|| ("/ws/gc2".into(), Vec::new()));
    let path = if path == "/" { "/ws/gc2" } else { &path };
    let mut request = format!("wss://{UPSTREAM_HOST}{path}")
        .into_client_request()
        .map_err(|error| format!("invalid upstream websocket URL: {error}"))?;
    for (name, value) in headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            request.headers_mut().insert(name, value);
        }
    }
    if !request.headers().contains_key("PsyEnvironment") {
        request
            .headers_mut()
            .insert("PsyEnvironment", HeaderValue::from_static("Prod"));
    }
    let (mut upstream, _) =
        connect(request).map_err(|error| format!("upstream websocket failed: {error}"))?;

    local
        .get_mut()
        .set_nonblocking(true)
        .map_err(|e| e.to_string())?;
    match upstream.get_mut() {
        tungstenite::stream::MaybeTlsStream::Plain(stream) => {
            stream.set_nonblocking(true).map_err(|e| e.to_string())?;
        }
        tungstenite::stream::MaybeTlsStream::NativeTls(stream) => {
            stream
                .get_mut()
                .set_nonblocking(true)
                .map_err(|e| e.to_string())?;
        }
        _ => {}
    }

    let rule = RankRule::new(ranks);
    loop {
        let mut progressed = false;
        match local.read() {
            Ok(message) => {
                progressed = true;
                dump_frame(dump_path, "REQUEST", &message);
                upstream
                    .send(message)
                    .map_err(|error| format!("forward to PsyNet failed: {error}"))?;
            }
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(Error::ConnectionClosed | Error::AlreadyClosed) => break,
            Err(error) => return Err(format!("local websocket read failed: {error}")),
        }
        match upstream.read() {
            Ok(mut message) => {
                progressed = true;
                dump_frame(dump_path, "RESPONSE", &message);
                if let Message::Text(text) = &message {
                    let mut body = Body::new("text/plain", text.as_bytes().to_vec());
                    if rule.rewrite(&mut body) {
                        if let Ok(rewritten) = String::from_utf8(body.bytes) {
                            message = Message::Text(rewritten);
                        }
                    }
                }
                local
                    .send(message)
                    .map_err(|error| format!("forward to game failed: {error}"))?;
            }
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(Error::ConnectionClosed | Error::AlreadyClosed) => break,
            Err(error) => return Err(format!("upstream websocket read failed: {error}")),
        }
        if !progressed {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    Ok(())
}

fn dump_frame(path: &std::path::Path, direction: &str, message: &Message) {
    let Message::Text(text) = message else { return };
    let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(path) else { return };
    use std::io::Write;
    let _ = writeln!(file, "\n--- {direction} ---\n{text}");
}
