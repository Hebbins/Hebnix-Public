//! Requests over Rocket League's captured connection. This module never logs in.
use crossbeam_channel::{Receiver, Sender, bounded};
use serde_json::Value;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
pub const REQUEST_PREFIX: &str = "HebnixRLAPI_";

pub fn shared_game_session() -> Arc<GameSession> {
    static SESSION: OnceLock<Arc<GameSession>> = OnceLock::new();
    Arc::clone(SESSION.get_or_init(|| Arc::new(GameSession::default())))
}

#[derive(Default)]
pub struct GameSession {
    enabled: AtomicBool,
    next_id: AtomicU64,
    connection: Mutex<Option<(u64, Sender<SessionRequest>)>>,
    error: Mutex<String>,
}

pub struct SessionRequest {
    pub id: String,
    pub service: String,
    pub body: Value,
    pub deadline: Instant,
    pub reply: Sender<Result<Value, String>>,
}

pub struct GameConnection {
    owner: Arc<GameSession>,
    generation: u64,
    pub requests: Receiver<SessionRequest>,
}

impl GameSession {
    pub fn reset(&self) {
        self.set_enabled(false);
        *self.connection.lock().unwrap() = None;
    }
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
        if enabled {
            self.set_error(String::new());
        }
    }
    pub fn connected(&self) -> bool {
        self.enabled() && self.connection.lock().unwrap().is_some()
    }
    pub fn has_connection(&self) -> bool {
        self.connection.lock().unwrap().is_some()
    }
    pub fn set_error(&self, error: String) {
        *self.error.lock().unwrap() = error;
    }
    pub fn status(&self) -> String {
        if !self.enabled() {
            return "Disabled".into();
        }
        if self.has_connection() {
            return "Session captured · Connected to Rocket League".into();
        }
        let error = self.error.lock().unwrap();
        if !error.is_empty() {
            return error.clone();
        }
        "Waiting for Rocket League's next login".into()
    }
    /// Called only after forwarding the game's token-bearing handshake and
    /// successfully opening the one upstream connection on its behalf.
    pub fn attach(self: &Arc<Self>) -> GameConnection {
        let generation = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, requests) = bounded(32);
        *self.connection.lock().unwrap() = Some((generation, tx));
        self.set_error(String::new());
        GameConnection {
            owner: Arc::clone(self),
            generation,
            requests,
        }
    }
    pub fn request(&self, service: &str, body: Value) -> Result<Value, String> {
        validate_request(service, &body)?;
        if !self.enabled() {
            return Err("Enable RLAPI and capture Rocket League's session first.".into());
        }
        let sender = self
            .connection
            .lock()
            .unwrap()
            .as_ref()
            .map(|(_, tx)| tx.clone())
            .ok_or("No captured game connection. Enable RLAPI before launching Rocket League.")?;
        let id = format!(
            "{REQUEST_PREFIX}{}_{}",
            std::process::id(),
            self.next_id.fetch_add(1, Ordering::Relaxed)
        );
        let (reply, response) = bounded(1);
        sender
            .try_send(SessionRequest {
                id,
                service: service.into(),
                body,
                deadline: Instant::now() + REQUEST_TIMEOUT,
                reply,
            })
            .map_err(|_| {
                "Game session is busy or disconnected. Request was not sent.".to_string()
            })?;
        response.recv_timeout(REQUEST_TIMEOUT).map_err(|_| {
            "Game session request timed out or disconnected; it was not retried.".to_string()
        })?
    }
}
impl GameConnection {
    pub fn current(&self) -> bool {
        self.owner.enabled()
            && self
                .owner
                .connection
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|(id, _)| *id == self.generation)
    }
}
impl Drop for GameConnection {
    fn drop(&mut self) {
        let mut active = self.owner.connection.lock().unwrap();
        if active
            .as_ref()
            .is_some_and(|(id, _)| *id == self.generation)
        {
            *active = None;
        }
        for job in self.requests.try_iter() {
            let _ = job.reply.send(Err(
                "Game connection closed; request was not retried.".into()
            ));
        }
    }
}

pub fn validate_request(service: &str, body: &Value) -> Result<(), String> {
    if service.is_empty() || service.len() > 256 || service.chars().any(char::is_control) {
        return Err("Invalid RLAPI endpoint.".into());
    }
    let Some((name, version)) = service.rsplit_once(' ') else {
        return Err("Use Namespace/Endpoint v1 format.".into());
    };
    if !name.contains('/')
        || name.contains(char::is_whitespace)
        || !version.starts_with('v')
        || version.len() < 2
        || !version[1..].bytes().all(|b| b.is_ascii_digit())
    {
        return Err("Use Namespace/Endpoint v1 format.".into());
    }
    if name
        .split('/')
        .next()
        .is_some_and(|n| n.eq_ignore_ascii_case("Auth"))
    {
        return Err("Authentication endpoints cannot use the shared game session.".into());
    }
    if !body.is_object() {
        return Err("Payload must be a JSON object.".into());
    }
    if body.to_string().len() > 1024 * 1024 {
        return Err("Payload must be smaller than 1 MiB.".into());
    }
    Ok(())
}
