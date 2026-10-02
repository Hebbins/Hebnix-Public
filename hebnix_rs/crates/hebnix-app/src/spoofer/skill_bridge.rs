//! Local PsyNet websocket bridge used by rank spoofing and item spawning.
//!
//! The config response is rewritten to point PerConURL/PerConURLv2 here. The
//! bridge forwards every websocket frame to the real service and rewrites only
//! server-to-client text frames containing the Skills envelope.

use hebnix_sdk::rlapi::session::{REQUEST_PREFIX, SessionRequest, shared_game_session};
use std::collections::HashMap;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
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
    "Sec-WebSocket-Protocol",
];

pub struct SkillBridge {
    running: Arc<AtomicBool>,
    outbound: Sender<String>,
    connections: Arc<AtomicUsize>,
}

impl SkillBridge {
    pub fn start(
        ranks: Arc<Mutex<HashMap<i32, (i32, f64)>>>,
        tx: Sender<AppMsg>,
        dump_path: PathBuf,
        _base_dir: &std::path::Path,
    ) -> Result<Self, String> {
        let _ = std::fs::write(
            &dump_path,
            "# Rank spoofer WebSocket frames (credentials omitted)\n",
        );
        let listener = TcpListener::bind(LISTEN_ADDR).map_err(|error| {
            format!("cannot bind rank websocket bridge on {LISTEN_ADDR}: {error}")
        })?;
        let _ = std::fs::write(
            dump_path.with_file_name("rank_spoofer_status.log"),
            format!("Rank bridge listening on {LISTEN_ADDR}\n"),
        );
        let running = Arc::new(AtomicBool::new(true));
        let (outbound, outbound_rx) = unbounded::<String>();
        let connections = Arc::new(AtomicUsize::new(0));
        let thread_connections = Arc::clone(&connections);
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
                    let outbound_rx = outbound_rx.clone();
                    let connections = Arc::clone(&thread_connections);
                    let running = Arc::clone(&thread_running);
                    std::thread::spawn(move || {
                        if let Err(error) = handle_connection(
                            stream,
                            ranks,
                            outbound_rx,
                            connections,
                            running,
                            &dump_path,
                        ) {
                            let _ = tx.send(AppMsg::Log(format!(
                                "[Spoofer] Rank websocket bridge: {error}"
                            )));
                        }
                    });
                }
            })
            .map_err(|error| format!("cannot start rank websocket bridge: {error}"))?;
        Ok(Self {
            running,
            outbound,
            connections,
        })
    }

    pub fn is_connected(&self) -> bool {
        self.connections.load(Ordering::Relaxed) > 0
    }

    pub fn send_text(&self, message: String) -> Result<(), String> {
        self.outbound
            .send(message)
            .map_err(|_| "PsyNet websocket bridge is not running".into())
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        let _ = TcpStream::connect(LISTEN_ADDR);
    }
}

fn handle_connection(
    stream: TcpStream,
    ranks: Arc<Mutex<HashMap<i32, (i32, f64)>>>,
    outbound: Receiver<String>,
    connections: Arc<AtomicUsize>,
    running: Arc<AtomicBool>,
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
        .and_then(|mut state| state.take())
        .unwrap_or_else(|| ("/ws/gc2".into(), Vec::new()));
    let path = if path == "/" { "/ws/gc2" } else { &path };
    let mut request = format!("wss://{UPSTREAM_HOST}{path}")
        .into_client_request()
        .map_err(|error| format!("invalid upstream websocket URL: {error}"))?;
    let has_session = ["PsyToken", "PsySessionID"].iter().all(|required| {
        headers
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case(required) && !value.is_empty())
    });
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
    connections.fetch_add(1, Ordering::SeqCst);
    struct ConnectionGuard(Arc<AtomicUsize>);
    impl Drop for ConnectionGuard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let _connection_guard = ConnectionGuard(connections);

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

    let session = shared_game_session();
    let game_connection = has_session.then(|| session.attach());
    let mut pending = PendingRequests::default();
    let rule = RankRule::new(ranks);
    while running.load(Ordering::Relaxed) {
        let mut progressed = false;
        flush_socket(&mut local)?;
        flush_socket(&mut upstream)?;
        pending.expire();
        if let Some(connection) = &game_connection {
            for job in connection.requests.try_iter() {
                if !connection.current() || job.deadline <= Instant::now() {
                    let _ = job.reply.send(Err(
                        "Game session changed or request expired; request was not sent.".into(),
                    ));
                    continue;
                }
                let message = request_frame(&job);
                let id = job.id.clone();
                pending.0.insert(id, job);
                send_frame(&mut upstream, Message::Text(message))?;
                progressed = true;
            }
        }
        while let Ok(text) = outbound.try_recv() {
            send_frame(&mut local, Message::Text(text))?;
            progressed = true;
        }
        match local.read() {
            Ok(message) => {
                progressed = true;
                dump_frame(dump_path, "REQUEST", &message);
                send_frame(&mut upstream, message)?;
            }
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(Error::ConnectionClosed | Error::AlreadyClosed) => break,
            Err(error) => return Err(format!("local websocket read failed: {error}")),
        }
        match upstream.read() {
            Ok(mut message) => {
                progressed = true;
                if let Message::Text(text) = &message {
                    if pending.consume(text) {
                        continue;
                    }
                }
                dump_frame(dump_path, "RESPONSE", &message);
                if let Message::Text(text) = &message {
                    let mut body = Body::new("text/plain", text.as_bytes().to_vec());
                    if rule.rewrite(&mut body) {
                        if let Ok(rewritten) = String::from_utf8(body.bytes) {
                            message = Message::Text(rewritten);
                        }
                    }
                }
                send_frame(&mut local, message)?;
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
    let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(path) else {
        return;
    };
    use std::io::Write;
    let _ = writeln!(file, "\n--- {direction} ---\n{text}");
}

fn request_frame(job: &SessionRequest) -> String {
    use base64::Engine;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let body = job.body.to_string();
    let mut mac = Hmac::<Sha256>::new_from_slice(b"c338bd36fb8c42b1a431d30add939fc7").unwrap();
    mac.update(b"-");
    mac.update(body.as_bytes());
    let signature = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
    format!(
        "PsyService: {}\r\nPsyRequestID: {}\r\nPsySig: {signature}\r\n\r\n{body}",
        job.service, job.id
    )
}

#[derive(Default)]
struct PendingRequests(HashMap<String, SessionRequest>);
impl PendingRequests {
    fn consume(&mut self, text: &str) -> bool {
        let Some((head, body)) = text.split_once("\r\n\r\n") else {
            return false;
        };
        let Some(id) = head.lines().find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("PsyResponseID"))
                .map(|(_, value)| value.trim())
        }) else {
            return false;
        };
        // Never deliver our responses to the game, including a late response
        // after the caller timed out. The game's own frames pass unchanged.
        if !id.starts_with(REQUEST_PREFIX) {
            return false;
        }
        if let Some(job) = self.0.remove(id) {
            let parsed = serde_json::from_str::<serde_json::Value>(body)
                .map_err(|_| "RLAPI returned invalid JSON.".to_string())
                .and_then(|value| {
                    if let Some(error) = value.get("Error").filter(|e| !e.is_null()) {
                        Err(error.to_string())
                    } else {
                        value
                            .get("Result")
                            .cloned()
                            .ok_or("RLAPI response has no Result.".into())
                    }
                });
            let _ = job.reply.send(parsed);
        }
        true
    }
    fn expire(&mut self) {
        let now = Instant::now();
        self.0.retain(|_, job| {
            if job.deadline <= now {
                let _ = job
                    .reply
                    .send(Err("RLAPI request timed out; it was not retried.".into()));
                false
            } else {
                true
            }
        });
    }
}
impl Drop for PendingRequests {
    fn drop(&mut self) {
        for (_, job) in self.0.drain() {
            let _ = job.reply.send(Err(
                "Game connection closed; request was not retried.".into()
            ));
        }
    }
}
fn send_frame<S: std::io::Read + std::io::Write>(
    socket: &mut tungstenite::WebSocket<S>,
    message: Message,
) -> Result<(), String> {
    match socket.send(message) {
        Ok(()) => Ok(()),
        // Tungstenite retains the encoded frame; flush it on the next loop.
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
        Err(_) => Err("PsyNet websocket send failed".into()),
    }
}
fn flush_socket<S: std::io::Read + std::io::Write>(
    socket: &mut tungstenite::WebSocket<S>,
) -> Result<(), String> {
    match socket.flush() {
        Ok(()) => Ok(()),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
        Err(_) => Err("PsyNet websocket flush failed".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn consumes_only_our_responses_including_late_ones() {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let id = format!("{REQUEST_PREFIX}test");
        let mut pending = PendingRequests::default();
        pending.0.insert(
            id.clone(),
            SessionRequest {
                id: id.clone(),
                service: "Population/GetPopulation v1".into(),
                body: serde_json::json!({}),
                deadline: Instant::now() + Duration::from_secs(20),
                reply: tx,
            },
        );
        assert!(!pending.consume("PsyResponseID: PsyNetMessage_X_2\r\n\r\n{\"Result\":{}}"));
        let reply = format!("PsyResponseID: {id}\r\n\r\n{{\"Result\":{{\"Count\":9}}}}");
        assert!(pending.consume(&reply));
        assert_eq!(rx.try_recv().unwrap().unwrap()["Count"], 9);
        assert!(pending.consume(&reply));
        assert!(pending.0.is_empty());
    }
    #[test]
    fn disconnect_completes_pending_request() {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let mut pending = PendingRequests::default();
        pending.0.insert(
            "test".into(),
            SessionRequest {
                id: "test".into(),
                service: "Population/GetPopulation v1".into(),
                body: serde_json::json!({}),
                deadline: Instant::now() + Duration::from_secs(20),
                reply: tx,
            },
        );
        drop(pending);
        assert!(rx.try_recv().unwrap().unwrap_err().contains("not retried"));
    }
}
