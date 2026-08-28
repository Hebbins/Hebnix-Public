//! the hebnix lua api + the egui ui bridge for plugins.
//!
//! everything runs on the ui thread. ui fns grab the current Ui from a
//! thread-local stack the host pushes before on_settings/on_window, pops after.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Duration;

use crossbeam_channel::Sender;
use eframe::egui;
use mlua::{Lua, LuaSerdeExt, SerializeOptions, Table, Value as LuaValue, Variadic};

use hebnix_sdk::tracker::TrackerClient;

use crate::messages::AppMsg;
use crate::plugins::store::PluginStore;

use rodio::{Decoder, OutputStream, Sink};

/// app-wide state visible to plugins (set each frame)
#[derive(Debug, Clone)]
pub struct HostShared {
    pub is_gui_open: bool,
    pub rl_connected: bool,
    /// true from UpdateState until MatchEnded/MatchDestroyed/disconnect. gates
    /// hebnix.input.send so plugins can't turn it into a gameplay macro.
    pub in_match: bool,
    pub app_version: String,
    /// detected game platform ("steam", "epic", or "" if unknown). default for
    /// eos/rlapi calls that don't pass one.
    pub platform: String,
}

/// a window side, either a size in points or a share of the monitor RL is on
#[derive(Debug, Clone, Copy)]
pub enum SizeSpec {
    Fixed(f32),
    /// 0.0 to 1.0
    Percent(f32),
}

impl SizeSpec {
    /// monitor_px is that side of the RL monitor
    pub fn resolve(self, monitor_px: f32, ppp: f32) -> f32 {
        match self {
            Self::Fixed(v) => v,
            Self::Percent(p) => (monitor_px * p / ppp.max(0.1)).max(1.0),
        }
    }
}

/// number, or "50%" / "50 %" / 50.0 with a percent sign
fn parse_size_spec(v: &LuaValue) -> Option<SizeSpec> {
    match v {
        LuaValue::Integer(n) => Some(SizeSpec::Fixed(*n as f32)),
        LuaValue::Number(n) => Some(SizeSpec::Fixed(*n as f32)),
        LuaValue::String(s) => {
            let text = s.to_str().ok()?;
            let text = text.trim();
            match text.strip_suffix('%') {
                Some(num) => num
                    .trim()
                    .parse::<f32>()
                    .ok()
                    .map(|p| SizeSpec::Percent((p / 100.0).clamp(0.0, 1.0))),
                None => text.parse::<f32>().ok().map(SizeSpec::Fixed),
            }
        }
        _ => None,
    }
}

/// a plugin's floating always-on-top window
#[derive(Debug, Clone)]
pub struct WindowState {
    pub open: bool,
    pub title: String,
    pub width: SizeSpec,
    pub height: SizeSpec,
    pub opacity: f32,
    pub close_button: bool,
    /// where we ask egui to put the window, set on open only. an observed
    /// position fed back into the builder is a SetWindowPos mid drag, and over
    /// a dpi boundary the read and the write use different scales.
    pub pos: Option<(f32, f32)>,
    pub last_pos: Option<(f32, f32)>, // where it actually is, for persisting
    pub pos_dirty: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            open: false,
            title: String::new(),
            width: SizeSpec::Fixed(260.0),
            height: SizeSpec::Fixed(160.0),
            opacity: 0.9,
            close_button: false,
            pos: None,
            last_pos: None,
            pos_dirty: false,
        }
    }
}

/// per-plugin host context captured by the lua closures
pub struct HostCtx {
    pub slug: String,
    pub display_name: RefCell<String>,
    pub tx: Sender<AppMsg>,
    pub store: RefCell<PluginStore>,
    pub window: RefCell<WindowState>,
    pub shared: Rc<RefCell<HostShared>>,
    /// draft buffers for text inputs (key to current text)
    pub text_bufs: RefCell<std::collections::HashMap<String, String>>,
    /// the plugin's own folder, assets resolve under dir/assets
    pub dir: std::path::PathBuf,
    /// asset bytes by relative path. None means it failed and was already logged, the ui callbacks run every frame so it can only be said once.
    pub assets: RefCell<std::collections::HashMap<String, Option<std::sync::Arc<[u8]>>>>,
}

impl HostCtx {
    pub fn log(&self, msg: &str) {
        let name = self.display_name.borrow();
        let _ = self.tx.send(AppMsg::Log(format!("[{}] {}", name, msg)));
    }
}

// Plugin assets

// asset path where root is relative to plugin <slug>/assets/
fn asset_path(plugin_dir: &std::path::Path, rel: &str) -> Result<std::path::PathBuf, String> {
    let root = plugin_dir.join("assets");
    let cleaned = rel.replace('\\', "/");
    let cleaned = cleaned.trim_start_matches('/');
    if cleaned.is_empty() {
        return Err("empty asset path".to_string());
    }
    for part in cleaned.split('/') {
        if part == ".." {
            return Err(format!("{rel} points outside the assets folder"));
        }
        // drive letters
        if part.contains(':') {
            return Err(format!("{rel} is not a plain relative path"));
        }
    }

    let root = root
        .canonicalize()
        .map_err(|_| "this plugin has no assets folder".to_string())?;
    let full = root
        .join(cleaned)
        .canonicalize()
        .map_err(|_| format!("{rel} was not found in the assets folder"))?;
    // symlinks detection
    if !full.starts_with(&root) {
        return Err(format!("{rel} points outside the assets folder"));
    }
    Ok(full)
}

fn is_http_url(path: &str) -> bool {
    path.starts_with("https://") || path.starts_with("http://")
}

const REMOTE_IMAGE_MAX_BYTES: u64 = 4 * 1024 * 1024;

enum RemoteImage {
    Pending,
    Ready(std::sync::Arc<[u8]>),
    Failed,
}

fn remote_images() -> &'static std::sync::Mutex<std::collections::HashMap<String, RemoteImage>> {
    static MAP: OnceLock<std::sync::Mutex<std::collections::HashMap<String, RemoteImage>>> =
        OnceLock::new();
    MAP.get_or_init(Default::default)
}

/// bytes for a url ui.image is allowed to draw, fetching it once in the
/// background. None while it is on the way, so the draw call never blocks.
fn remote_image(url: &str) -> Option<std::sync::Arc<[u8]>> {
    if !crate::overlay::media_host_allowed(url) {
        return None;
    }
    let mut map = remote_images().lock().unwrap();
    match map.get(url) {
        Some(RemoteImage::Ready(bytes)) => return Some(bytes.clone()),
        Some(_) => return None,
        None => {}
    }
    map.insert(url.to_string(), RemoteImage::Pending);
    drop(map);

    let url = url.to_string();
    std::thread::spawn(move || {
        let fetched = ureq::get(&url)
            .timeout(Duration::from_secs(15))
            .call()
            .map_err(|error| error.to_string())
            .and_then(|response| {
                use std::io::Read;
                let mut bytes = Vec::new();
                response
                    .into_reader()
                    .take(REMOTE_IMAGE_MAX_BYTES)
                    .read_to_end(&mut bytes)
                    .map_err(|error| error.to_string())?;
                Ok(bytes)
            });
        let entry = match fetched {
            Ok(bytes) if !bytes.is_empty() => RemoteImage::Ready(bytes.into()),
            Ok(_) => RemoteImage::Failed,
            Err(error) => {
                tracing::warn!("ui.image {url} failed: {error}");
                RemoteImage::Failed
            }
        };
        remote_images().lock().unwrap().insert(url, entry);
    });
    None
}

/// cached asset bytes
fn load_asset(host: &HostCtx, rel: &str) -> Option<std::sync::Arc<[u8]>> {
    if let Some(cached) = host.assets.borrow().get(rel) {
        return cached.clone();
    }
    let loaded = asset_path(&host.dir, rel)
        .and_then(|p| std::fs::read(&p).map_err(|e| format!("{rel}: {e}")));
    let entry = match loaded {
        Ok(bytes) => Some(std::sync::Arc::<[u8]>::from(bytes)),
        Err(e) => {
            host.log(&e);
            None
        }
    };
    host.assets
        .borrow_mut()
        .insert(rel.to_string(), entry.clone());
    entry
}

// Current-Ui stack

thread_local! {
    static UI_STACK: RefCell<Vec<*mut egui::Ui>> = const { RefCell::new(Vec::new()) };
}

/// run f with ui as the current target for plugin ui calls
pub fn with_ui_scope<R>(ui: &mut egui::Ui, f: impl FnOnce() -> R) -> R {
    UI_STACK.with(|stack| stack.borrow_mut().push(ui as *mut egui::Ui));
    let result = f();
    UI_STACK.with(|stack| {
        stack.borrow_mut().pop();
    });
    result
}

/// current Ui, None if called outside a render scope
fn with_current_ui<R>(f: impl FnOnce(&mut egui::Ui) -> R) -> Option<R> {
    UI_STACK.with(|stack| {
        let ptr = stack.borrow().last().copied();
        // SAFETY: the pointer is only on the stack for the duration of
        // `with_ui_scope`, during which the &mut egui::Ui outlives all Lua
        // calls made inside it (everything is single-threaded).
        ptr.map(|p| unsafe { f(&mut *p) })
    })
}

/// serialize to lua with None becoming nil. mlua otherwise maps None to a
/// "null" light-userdata (NOT nil), which breaks checks like res.error ~= nil.
pub fn to_lua<T: serde::Serialize>(lua: &Lua, value: &T) -> mlua::Result<LuaValue> {
    lua.to_value_with(
        value,
        SerializeOptions::new()
            .serialize_none_to_null(false)
            .serialize_unit_to_null(false),
    )
}

pub fn tracker_client() -> &'static TrackerClient {
    static CLIENT: OnceLock<TrackerClient> = OnceLock::new();
    CLIENT.get_or_init(TrackerClient::default)
}

// Async tracker fetches (shared across plugins; results poll-able from Lua)

enum AsyncStats {
    Pending,
    Done(hebnix_sdk::tracker::PlayerStats),
}

fn async_stats() -> &'static std::sync::Mutex<std::collections::HashMap<String, AsyncStats>> {
    static MAP: OnceLock<std::sync::Mutex<std::collections::HashMap<String, AsyncStats>>> =
        OnceLock::new();
    MAP.get_or_init(Default::default)
}

/// what the tracker worker fetches (both resolve to a PlayerStats)
enum FetchSpec {
    /// full StatsAPI PrimaryId, e.g. "Steam|76561198..|0"
    PrimaryId { pid: String, name: String },
    /// tracker.gg platform slug + identifier (steam id64 or display name).
    Profile { slug: String, identifier: String },
}

/// single worker thread, 1.5s throttle between tracker.gg requests. results
/// land in async_stats under the request key.
fn stats_queue() -> &'static crossbeam_channel::Sender<(String, FetchSpec)> {
    static QUEUE: OnceLock<crossbeam_channel::Sender<(String, FetchSpec)>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let (tx, rx) = crossbeam_channel::unbounded::<(String, FetchSpec)>();
        std::thread::Builder::new()
            .name("tracker-worker".into())
            .spawn(move || {
                while let Ok((key, spec)) = rx.recv() {
                    let run = |spec: &FetchSpec| match spec {
                        FetchSpec::PrimaryId { pid, name } => tracker_client().fetch(pid, name),
                        FetchSpec::Profile { slug, identifier } => {
                            tracker_client().fetch_profile(slug, identifier)
                        }
                    };
                    // the tracker client rotates fingerprints + backs off on
                    // 429 internally, so just take the result here.
                    let result = run(&spec);
                    let stats = result.unwrap_or_else(|e| {
                        tracing::warn!("tracker fetch '{key}' refused: {e}");
                        hebnix_sdk::tracker::PlayerStats {
                            primary_id: key.clone(),
                            error: Some(e),
                            ..Default::default()
                        }
                    });
                    if let Some(err) = &stats.error {
                        tracing::info!("tracker fetch '{key}' error: {err}");
                    }
                    async_stats()
                        .lock()
                        .unwrap()
                        .insert(key, AsyncStats::Done(stats));
                    std::thread::sleep(Duration::from_millis(1500));
                }
            })
            .ok();
        tx
    })
}

enum AsyncLog {
    Pending,
    Done(hebnix_sdk::log::LogInfo),
}

fn async_log() -> &'static std::sync::Mutex<std::collections::HashMap<String, AsyncLog>> {
    static MAP: OnceLock<std::sync::Mutex<std::collections::HashMap<String, AsyncLog>>> =
        OnceLock::new();
    MAP.get_or_init(Default::default)
}

/// single worker a verify=true parse waits on the stats api and the psynet
/// config, they are too slow for the ui thread lua runs on
fn log_queue() -> &'static crossbeam_channel::Sender<(String, bool)> {
    static QUEUE: OnceLock<crossbeam_channel::Sender<(String, bool)>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let (tx, rx) = crossbeam_channel::unbounded::<(String, bool)>();
        std::thread::Builder::new()
            .name("launch-log-worker".into())
            .spawn(move || {
                while let Ok((key, verify)) = rx.recv() {
                    let info = hebnix_sdk::log::parse_launch_log(None, verify, "INT");
                    async_log()
                        .lock()
                        .unwrap()
                        .insert(key, AsyncLog::Done(info));
                }
            })
            .ok();
        tx
    })
}

use std::io::Cursor;
fn play_audio(
    bytes: std::sync::Arc<[u8]>,
    volume: f32,
    tx: crossbeam_channel::Sender<AppMsg>,
    plugin_name: String,
) {
    std::thread::spawn(move || {
        let log = |msg: &str| {
            let _ = tx.send(AppMsg::Log(format!("[{}] {}", plugin_name, msg)));
        };

        let Ok((_stream, stream_handle)) = OutputStream::try_default() else {
            log("Audio Error: Failed to open default audio output stream.");
            return;
        };

        let Ok(sink) = Sink::try_new(&stream_handle) else {
            log("Audio Error: Failed to create audio sink.");
            return;
        };

        let cursor = Cursor::new(bytes.to_vec());
        let source = match Decoder::new(cursor) {
            Ok(s) => s,
            Err(e) => {
                log(&format!(
                    "Audio Error: Failed to decode file (Did you enable the feature in Cargo.toml?): {}",
                    e
                ));
                return;
            }
        };

        sink.set_volume(volume);
        sink.append(source);
        sink.sleep_until_end();
    });
}

// EOS token + RLAPI (PsyNet) access for plugins
//
// Both run off the UI thread (token acquisition and PsyNet calls block on
// network / the Steam DLL / the bridge subprocess). Plugins enqueue work and
// poll for the result, mirroring the tracker pattern above.

use hebnix_sdk::eos::{self, EOSToken, Platform as EosPlatform};
use hebnix_sdk::rlapi::RlApi;

/// parse a user platform string into an eos platform
fn parse_eos_platform(s: &str) -> Option<EosPlatform> {
    match s.trim().to_lowercase().as_str() {
        "steam" => Some(EosPlatform::Steam),
        "epic" | "epicgames" => Some(EosPlatform::Epic),
        _ => None,
    }
}

/// pick the platform for an eos/rlapi call: explicit arg wins, then the
/// detected game platform, then steam as a last resort.
fn resolve_platform(host: &Rc<HostCtx>, explicit: Option<String>) -> Option<EosPlatform> {
    if let Some(p) = explicit.as_deref().and_then(parse_eos_platform) {
        return Some(p);
    }
    let detected = host.shared.borrow().platform.clone();
    parse_eos_platform(&detected).or(Some(EosPlatform::Steam))
}

// --- EOS tokens (keyed by platform; cached, re-fetched when expired) ---

enum AsyncEos {
    Pending,
    Done(Option<EOSToken>),
}

fn async_eos() -> &'static std::sync::Mutex<std::collections::HashMap<String, AsyncEos>> {
    static MAP: OnceLock<std::sync::Mutex<std::collections::HashMap<String, AsyncEos>>> =
        OnceLock::new();
    MAP.get_or_init(Default::default)
}

/// process-wide cache of valid eos tokens, keyed by platform
fn eos_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, EOSToken>> {
    static MAP: OnceLock<std::sync::Mutex<std::collections::HashMap<String, EOSToken>>> =
        OnceLock::new();
    MAP.get_or_init(Default::default)
}

/// valid eos token for platform, fetching only if the cache is empty/expired.
/// the lock is held across the fetch on purpose: it serializes token grabs so
/// we never make two steam tickets at once (steam rejects the 2nd as
/// invalid_external_auth_token). concurrent callers block then reuse the token.
pub fn get_or_fetch_eos(platform: EosPlatform) -> Option<EOSToken> {
    let mut cache = eos_cache().lock().unwrap();
    if let Some(tok) = cache.get(platform.as_str()) {
        if !tok.expired() {
            return Some(tok.clone());
        }
    }
    let token = eos::get_eos_token(platform);
    if let Some(tok) = &token {
        cache.insert(platform.as_str().to_string(), tok.clone());
    }
    token
}

fn eos_queue() -> &'static crossbeam_channel::Sender<EosPlatform> {
    static QUEUE: OnceLock<crossbeam_channel::Sender<EosPlatform>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let (tx, rx) = crossbeam_channel::unbounded::<EosPlatform>();
        std::thread::Builder::new()
            .name("eos-worker".into())
            .spawn(move || {
                while let Ok(platform) = rx.recv() {
                    let token = get_or_fetch_eos(platform);
                    async_eos()
                        .lock()
                        .unwrap()
                        .insert(platform.as_str().to_string(), AsyncEos::Done(token));
                }
            })
            .ok();
        tx
    })
}

// --- RLAPI / PsyNet requests (one shared session, lazily connected) ---

struct RlApiJob {
    key: String,
    platform: EosPlatform,
    service: String,
    body: serde_json::Value,
}

enum AsyncRpc {
    Pending,
    Done {
        ok: bool,
        result: serde_json::Value,
        error: String,
    },
}

fn async_rpc() -> &'static std::sync::Mutex<std::collections::HashMap<String, AsyncRpc>> {
    static MAP: OnceLock<std::sync::Mutex<std::collections::HashMap<String, AsyncRpc>>> =
        OnceLock::new();
    MAP.get_or_init(Default::default)
}

/// is the shared psynet session connected
fn rlapi_connected_flag() -> &'static std::sync::atomic::AtomicBool {
    static FLAG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    &FLAG
}

fn store_rpc(key: &str, ok: bool, result: serde_json::Value, error: String) {
    async_rpc()
        .lock()
        .unwrap()
        .insert(key.to_string(), AsyncRpc::Done { ok, result, error });
}

fn rlapi_queue() -> &'static crossbeam_channel::Sender<RlApiJob> {
    use std::sync::atomic::Ordering;
    static QUEUE: OnceLock<crossbeam_channel::Sender<RlApiJob>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let (tx, rx) = crossbeam_channel::unbounded::<RlApiJob>();
        std::thread::Builder::new()
            .name("rlapi-worker".into())
            .spawn(move || {
                let mut session: Option<RlApi> = None;
                let mut session_platform: Option<EosPlatform> = None;

                // Connect using the shared EOS token cache (so we never race
                // the eos-worker for a fresh Steam ticket).
                let connect = |platform: EosPlatform| -> Result<RlApi, String> {
                    let token = get_or_fetch_eos(platform).ok_or_else(|| {
                        format!("could not obtain an EOS token for {}", platform.as_str())
                    })?;
                    RlApi::connect_with_token(&token, platform)
                };

                while let Ok(job) = rx.recv() {
                    // (Re)connect if we have no session or the platform changed.
                    if session.is_none() || session_platform != Some(job.platform) {
                        session = None;
                        rlapi_connected_flag().store(false, Ordering::Relaxed);
                        match connect(job.platform) {
                            Ok(api) => {
                                session = Some(api);
                                session_platform = Some(job.platform);
                                rlapi_connected_flag().store(true, Ordering::Relaxed);
                            }
                            Err(e) => {
                                store_rpc(&job.key, false, serde_json::Value::Null, e);
                                continue;
                            }
                        }
                    }

                    let api = session.as_mut().unwrap();
                    let mut result = api.request(&job.service, job.body.clone());

                    // If the bridge dropped, reconnect once and retry.
                    if is_disconnect(&result) {
                        rlapi_connected_flag().store(false, Ordering::Relaxed);
                        session = connect(job.platform).ok();
                        session_platform = session.as_ref().map(|_| job.platform);
                        rlapi_connected_flag().store(session.is_some(), Ordering::Relaxed);
                        if let Some(api) = session.as_mut() {
                            result = api.request(&job.service, job.body.clone());
                        }
                    }

                    match result {
                        Ok(value) => store_rpc(&job.key, true, value, String::new()),
                        Err(e) => store_rpc(&job.key, false, serde_json::Value::Null, e),
                    }
                }
            })
            .ok();
        tx
    })
}

fn is_disconnect(result: &Result<serde_json::Value, String>) -> bool {
    matches!(result, Err(e) if e.contains("closed") || e.contains("connection") || e.contains("write") || e.contains("flush"))
}

/// optional lua body table to a json object (empty obj when absent, so
/// no-arg psynet services get {} not [])
fn lua_body_to_json(lua: &Lua, body: Option<LuaValue>) -> serde_json::Value {
    match body {
        None | Some(LuaValue::Nil) => serde_json::json!({}),
        Some(v) => match lua.from_value::<serde_json::Value>(v) {
            Ok(serde_json::Value::Array(a)) if a.is_empty() => serde_json::json!({}),
            Ok(json) => json,
            Err(_) => serde_json::json!({}),
        },
    }
}

/// monotonic request-key gen for one-shot rlapi calls
fn next_rpc_key() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    format!("rpc:{}", N.fetch_add(1, Ordering::Relaxed))
}

/// normalize platform names to tracker.gg slugs
fn normalize_slug(platform: &str) -> String {
    match platform.trim().to_lowercase().as_str() {
        "steam" => "steam",
        "epic" | "epicgames" => "epic",
        "xbl" | "xbox" | "xboxone" => "xbl",
        "psn" | "ps4" | "ps5" | "playstation" => "psn",
        "switch" | "nintendo" => "switch",
        other => return other.to_string(),
    }
    .to_string()
}

// Async bind capture (one capture at a time is plenty)

enum CaptureState {
    Idle,
    Pending,
    Done(Option<String>),
}

fn capture_state() -> &'static std::sync::Mutex<CaptureState> {
    static STATE: OnceLock<std::sync::Mutex<CaptureState>> = OnceLock::new();
    STATE.get_or_init(|| std::sync::Mutex::new(CaptureState::Idle))
}

fn parse_hex_color(s: &str) -> Option<egui::Color32> {
    let s = s.trim().trim_start_matches('#');
    match s.len() {
        6 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            Some(egui::Color32::from_rgb(r, g, b))
        }
        8 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            let a = u8::from_str_radix(&s[6..8], 16).ok()?;
            Some(egui::Color32::from_rgba_unmultiplied(r, g, b, a))
        }
        _ => None,
    }
}

fn opt_f32(opts: &Option<Table>, key: &str, default: f32) -> f32 {
    opts.as_ref()
        .and_then(|t| t.get::<f32>(key).ok())
        .unwrap_or(default)
}

fn opt_bool(opts: &Option<Table>, key: &str, default: bool) -> bool {
    opts.as_ref()
        .and_then(|t| t.get::<bool>(key).ok())
        .unwrap_or(default)
}

fn opt_num(opts: &Option<Table>, key: &str) -> Option<f32> {
    opts.as_ref().and_then(|t| t.get::<f32>(key).ok())
}

/// browser UA, reqwest's default gets 403'd by some hosts
fn http_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .build()
            .unwrap_or_default()
    })
}

/// status 0 when it never landed
fn send_req(req: reqwest::blocking::RequestBuilder) -> (u16, String) {
    match req.send() {
        Ok(res) => (res.status().as_u16(), res.text().unwrap_or_default()),
        Err(e) => (0, e.to_string()),
    }
}

/// separate client with redirects disabled — needed for OAuth flows (e.g.
/// PSN's NPSSO exchange) that 302-redirect with the payload (an auth code)
/// in the Location header itself; http_client()'s default policy follows
/// redirects automatically, which would silently discard that header and
/// hand back the followed-to page's body instead.
fn http_client_no_redirect() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .build()
            .unwrap_or_default()
    })
}

/// like send_req but returns the response's Location header instead of the
/// body — status 0 (empty location) when the request never landed.
fn send_req_location(req: reqwest::blocking::RequestBuilder) -> (u16, String) {
    match req.send() {
        Ok(res) => {
            let status = res.status().as_u16();
            let location = res
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            (status, location)
        }
        Err(_) => (0, String::new()),
    }
}

/// like send_req but preserves raw bytes instead of decoding as UTF-8 text —
/// res.text() mangles/truncates binary responses (e.g. avatar images), since
/// it forces the body into a Rust String. status 0 when it never landed.
fn send_req_bytes(req: reqwest::blocking::RequestBuilder) -> (u16, Vec<u8>) {
    match req.send() {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.bytes().map(|b| b.to_vec()).unwrap_or_default();
            (status, body)
        }
        Err(e) => (0, e.to_string().into_bytes()),
    }
}

const UI_TABLE_REGISTRY: &str = "hebnix_ui_table";
const DRAW_TABLE_REGISTRY: &str = "hebnix_draw_table";

/// install the hebnix global + build the ui bridge table
pub fn install_api(lua: &Lua, host: Rc<HostCtx>) -> mlua::Result<()> {
    let hebnix = lua.create_table()?;

    // Logging
    {
        let host = Rc::clone(&host);
        hebnix.set(
            "log",
            lua.create_function(move |_, args: Variadic<LuaValue>| {
                let parts: Vec<String> = args
                    .iter()
                    .map(|v| match v {
                        LuaValue::String(s) => s.to_string_lossy().to_string(),
                        other => format!("{other:?}"),
                    })
                    .collect();
                host.log(&parts.join(" "));
                Ok(())
            })?,
        )?;
    }

    // Persisted settings
    {
        let host = Rc::clone(&host);
        hebnix.set(
            "get_bool",
            lua.create_function(move |_, (key, default): (String, Option<bool>)| {
                Ok(host.store.borrow().get_bool(&key, default.unwrap_or(false)))
            })?,
        )?;
    }
    {
        let host = Rc::clone(&host);
        hebnix.set(
            "get_string",
            lua.create_function(move |_, (key, default): (String, Option<String>)| {
                Ok(host
                    .store
                    .borrow()
                    .get_string(&key, default.as_deref().unwrap_or("")))
            })?,
        )?;
    }
    {
        let host = Rc::clone(&host);
        hebnix.set(
            "get_number",
            lua.create_function(move |_, (key, default): (String, Option<f64>)| {
                Ok(host.store.borrow().get_number(&key, default.unwrap_or(0.0)))
            })?,
        )?;
    }
    {
        let host = Rc::clone(&host);
        hebnix.set(
            "set",
            lua.create_function(move |_, (key, value): (String, LuaValue)| {
                let mut store = host.store.borrow_mut();
                match value {
                    LuaValue::Boolean(b) => store.set_bool(&key, b),
                    LuaValue::Integer(i) => store.set_number(&key, i as f64),
                    LuaValue::Number(n) => store.set_number(&key, n),
                    LuaValue::String(s) => store.set_string(&key, &s.to_string_lossy()),
                    other => {
                        return Err(mlua::Error::runtime(format!(
                            "hebnix.set: unsupported value type {}",
                            other.type_name()
                        )));
                    }
                }
                Ok(())
            })?,
        )?;
    }

    // App state
    {
        let host = Rc::clone(&host);
        hebnix.set(
            "is_gui_open",
            lua.create_function(move |_, ()| Ok(host.shared.borrow().is_gui_open))?,
        )?;
    }
    {
        let host = Rc::clone(&host);
        hebnix.set(
            "rl_connected",
            lua.create_function(move |_, ()| Ok(host.shared.borrow().rl_connected))?,
        )?;
    }
    {
        let host = Rc::clone(&host);
        hebnix.set(
            "app_version",
            lua.create_function(move |_, ()| Ok(host.shared.borrow().app_version.clone()))?,
        )?;
    }
    {
        let host = Rc::clone(&host);
        hebnix.set(
            "slug",
            lua.create_function(move |_, ()| Ok(host.slug.clone()))?,
        )?;
    }
    // Absolute path to this plugin's own folder — same base used to
    // resolve draw.image/ui.image's relative paths (base_dir/plugins/
    // <slug>). Lua's plain io.* library has no notion of "the plugin's
    // folder" — it resolves relative paths against the process's
    // current working directory, which the host never chdir()s, so it
    // isn't reliably anything in particular. Plugins that need io.open
    // (e.g. writing a downloaded file before draw.image can show it)
    // should join this with a relative path instead of using a bare
    // relative path directly.
    {
        let host = Rc::clone(&host);
        hebnix.set(
            "plugin_dir",
            lua.create_function(move |_, ()| {
                let dir = crate::config::base_dir().join("plugins").join(&host.slug);
                Ok(dir.to_string_lossy().to_string())
            })?,
        )?;
    }

    // SDK helpers
    hebnix.set(
        "shorten_rank",
        lua.create_function(|_, rank: String| Ok(hebnix_sdk::utils::shorten_rank(&rank)))?,
    )?;
    hebnix.set(
        "tier_name",
        lua.create_function(|_, id: usize| Ok(hebnix_sdk::utils::get_tier_name(id).to_string()))?,
    )?;
    hebnix.set(
        "platform_tag",
        lua.create_function(|_, pid: String| {
            Ok(hebnix_sdk::utils::get_platform_tag(&pid).to_string())
        })?,
    )?;
    hebnix.set(
        "is_bot",
        lua.create_function(|_, pid: String| Ok(hebnix_sdk::utils::is_bot(&pid)))?,
    )?;
    hebnix.set(
        "is_bind_pressed",
        lua.create_function(|_, bind: String| Ok(hebnix_sdk::input::is_bind_pressed(&bind)))?,
    )?;
    hebnix.set(
        "get_action_binds",
        lua.create_function(|lua, action: String| {
            let result = lua.create_table()?;
            for (index, bind) in hebnix_sdk::input::action_binds(&action).iter().enumerate() {
                result.set(index + 1, bind.clone())?;
            }
            Ok(result)
        })?,
    )?;
    hebnix.set(
        "is_action_pressed",
        lua.create_function(|_, action: String| Ok(hebnix_sdk::input::is_action_pressed(&action)))?,
    )?;
    hebnix.set(
        "refresh_action_binds",
        lua.create_function(|_, ()| {
            hebnix_sdk::input::clear_action_bind_cache();
            Ok(())
        })?,
    )?;
    hebnix.set(
        "monotonic_seconds",
        lua.create_function(|_, ()| {
            static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
            Ok(START
                .get_or_init(std::time::Instant::now)
                .elapsed()
                .as_secs_f64())
        })?,
    )?;

    // connected pads (Universal Analog Support)
    hebnix.set(
        "controllers",
        lua.create_function(|lua, ()| {
            // Use a static OnceLock so the hardware context stays alive across frames
            static GAMEPADS: std::sync::OnceLock<std::sync::Mutex<gamepads::Gamepads>> =
                std::sync::OnceLock::new();
            let mut gp = GAMEPADS
                .get_or_init(|| std::sync::Mutex::new(gamepads::Gamepads::new()))
                .lock()
                .unwrap();

            // Poll hardware for fresh inputs
            gp.poll();

            let list = lua.create_table()?;
            let mut i = 1;

            for gamepad in gp.all() {
                let pad = lua.create_table()?;

                pad.set("id", format!("{:?}", gamepad.id()))?;
                pad.set("name", format!("Gamepad {:?}", gamepad.id()))?;
                pad.set("kind", "universal")?;

                // Analog Axes (-1.0 to 1.0)
                pad.set("lx", gamepad.left_stick_x())?;
                pad.set("ly", gamepad.left_stick_y())?;
                pad.set("rx", gamepad.right_stick_x())?;
                pad.set("ry", gamepad.right_stick_y())?;

                // Analog Triggers (0.0 to 1.0)
                pad.set("lt", gamepad.left_trigger())?;
                pad.set("rt", gamepad.right_trigger())?;

                // Standard SDL Mapped Buttons (Cross-Platform)
                pad.set(
                    "btn_south",
                    gamepad.is_currently_pressed(gamepads::Button::ActionDown),
                )?; // A / Cross
                pad.set(
                    "btn_east",
                    gamepad.is_currently_pressed(gamepads::Button::ActionRight),
                )?; // B / Circle
                pad.set(
                    "btn_west",
                    gamepad.is_currently_pressed(gamepads::Button::ActionLeft),
                )?; // X / Square
                pad.set(
                    "btn_north",
                    gamepad.is_currently_pressed(gamepads::Button::ActionUp),
                )?; // Y / Triangle

                pad.set(
                    "dpad_up",
                    gamepad.is_currently_pressed(gamepads::Button::DPadUp),
                )?;
                pad.set(
                    "dpad_down",
                    gamepad.is_currently_pressed(gamepads::Button::DPadDown),
                )?;
                pad.set(
                    "dpad_left",
                    gamepad.is_currently_pressed(gamepads::Button::DPadLeft),
                )?;
                pad.set(
                    "dpad_right",
                    gamepad.is_currently_pressed(gamepads::Button::DPadRight),
                )?;

                pad.set(
                    "bumper_l",
                    gamepad.is_currently_pressed(gamepads::Button::FrontLeftUpper),
                )?;
                pad.set(
                    "bumper_r",
                    gamepad.is_currently_pressed(gamepads::Button::FrontRightUpper),
                )?;
                pad.set(
                    "trigger_l",
                    gamepad.is_currently_pressed(gamepads::Button::FrontLeftLower),
                )?;
                pad.set(
                    "trigger_r",
                    gamepad.is_currently_pressed(gamepads::Button::FrontRightLower),
                )?;
                pad.set(
                    "stick_l",
                    gamepad.is_currently_pressed(gamepads::Button::LeftStick),
                )?;
                pad.set(
                    "stick_r",
                    gamepad.is_currently_pressed(gamepads::Button::RightStick),
                )?;
                // Center controls are exposed with neutral names so plugins
                // work with both DualShock/DualSense and Xbox layouts.
                pad.set(
                    "select",
                    gamepad.is_currently_pressed(gamepads::Button::LeftCenterCluster),
                )?;
                pad.set(
                    "start",
                    gamepad.is_currently_pressed(gamepads::Button::RightCenterCluster),
                )?;
                // SDL's standard mapping reports the DualShock touchpad
                // click as the Mode button. Xbox pads simply report false.
                pad.set(
                    "touchpad",
                    gamepad.is_currently_pressed(gamepads::Button::Mode),
                )?;

                list.set(i, pad)?;
                i += 1;
            }
            Ok(list)
        })?,
    )?;

    // fullscreen / windowed / borderless, out of TASystemSettings.ini
    hebnix.set(
        "window_mode",
        lua.create_function(|_, ()| {
            Ok(hebnix_sdk::utils::system_settings::window_mode().map(|m| m.as_str().to_string()))
        })?,
    )?;

    // tracker.gg (blocking network call, cached ~5 min per player)
    hebnix.set(
        "fetch_stats",
        lua.create_function(|lua, (primary_id, display_name): (String, String)| {
            match tracker_client().fetch(&primary_id, &display_name) {
                Ok(stats) => Ok(to_lua(lua, &stats)?),
                Err(_) => Ok(LuaValue::Nil),
            }
        })?,
    )?;
    hebnix.set(
        "cached_stats",
        lua.create_function(|lua, primary_id: String| {
            match tracker_client().get_cached(&primary_id) {
                Some(stats) => Ok(to_lua(lua, &stats)?),
                None => Ok(LuaValue::Nil),
            }
        })?,
    )?;

    {
        let host = Rc::clone(&host);
        hebnix.set(
            "list_assets",
            lua.create_function(move |lua, ()| {
                let assets_dir = host.dir.join("assets");
                let list = lua.create_table()?;
                let mut i = 1;

                if let Ok(entries) = std::fs::read_dir(assets_dir) {
                    for entry in entries.flatten() {
                        if let Ok(file_type) = entry.file_type() {
                            if file_type.is_file() {
                                if let Some(name) = entry.file_name().to_str() {
                                    list.set(i, name)?;
                                    i += 1;
                                }
                            }
                        }
                    }
                }
                Ok(list)
            })?,
        )?;
    }

    // hebnix.settings.* - small settings-page helpers
    let settings = lua.create_table()?;
    {
        let host = Rc::clone(&host);
        settings.set(
            "open_assets",
            lua.create_function(move |_, ()| {
                let assets_dir = host.dir.join("assets");
                if let Err(err) = std::fs::create_dir_all(&assets_dir) {
                    host.log(&format!(
                        "open_assets: couldn't create assets folder: {err}"
                    ));
                    return Ok(());
                }
                let _ = std::process::Command::new("cmd")
                    .args(["/C", "start", "", &assets_dir.to_string_lossy()])
                    .spawn();
                Ok(())
            })?,
        )?;
    }
    hebnix.set("settings", settings)?;

    // non-blocking tracker fetch by StatsAPI PrimaryId ("Steam|..|0")
    // Poll with hebnix.stats_result(primary_id).
    hebnix.set(
        "fetch_stats_async",
        lua.create_function(|_, (primary_id, display_name): (String, String)| {
            if primary_id.is_empty() || hebnix_sdk::utils::is_bot(&primary_id) {
                return Ok(false);
            }
            let mut map = async_stats().lock().unwrap();
            if map.contains_key(&primary_id) {
                return Ok(false); // already pending or done
            }
            map.insert(primary_id.clone(), AsyncStats::Pending);
            drop(map);
            let _ = stats_queue().send((
                primary_id.clone(),
                FetchSpec::PrimaryId {
                    pid: primary_id,
                    name: display_name,
                },
            ));
            Ok(true)
        })?,
    )?;

    // Non-blocking tracker fetch by platform + identifier:
    //   hebnix.fetch_profile_async("steam", "76561198..")   -- id64
    //   hebnix.fetch_profile_async("epic", "DisplayName")  -- any platform
    // Returns the key to poll with hebnix.stats_result(key), or nil.
    hebnix.set(
        "fetch_profile_async",
        lua.create_function(|_, (platform, identifier): (String, String)| {
            let slug = normalize_slug(&platform);
            let identifier = identifier.trim().to_string();
            if slug.is_empty() || identifier.is_empty() {
                return Ok(None);
            }
            let key = format!("{slug}:{identifier}");
            let mut map = async_stats().lock().unwrap();
            if !map.contains_key(&key) {
                map.insert(key.clone(), AsyncStats::Pending);
                drop(map);
                let _ = stats_queue().send((key.clone(), FetchSpec::Profile { slug, identifier }));
            }
            Ok(Some(key))
        })?,
    )?;

    // Returns nil (never requested), "pending", or a stats table (check
    // .error on it for failures).
    hebnix.set(
        "stats_result",
        lua.create_function(|lua, primary_id: String| {
            let map = async_stats().lock().unwrap();
            match map.get(&primary_id) {
                None => Ok(LuaValue::Nil),
                Some(AsyncStats::Pending) => Ok(LuaValue::String(lua.create_string("pending")?)),
                Some(AsyncStats::Done(stats)) => Ok(to_lua(lua, stats)?),
            }
        })?,
    )?;

    hebnix.set(
        "clear_stats_cache",
        lua.create_function(|_, ()| {
            async_stats().lock().unwrap().clear();
            tracker_client().clear_cache();
            Ok(())
        })?,
    )?;

    // eos tokens + rlapi (psynet)

    // The detected platform of the running game ("steam" / "epic" / "").
    {
        let host = Rc::clone(&host);
        hebnix.set(
            "detected_platform",
            lua.create_function(move |_, ()| Ok(host.shared.borrow().platform.clone()))?,
        )?;
    }

    // Non-blocking EOS token acquisition. `platform` is optional and defaults
    // to the detected platform (then Steam). Returns the platform key to poll
    // with hebnix.eos_result(key), or nil if no platform could be determined.
    {
        let host = Rc::clone(&host);
        hebnix.set(
            "eos_token_async",
            lua.create_function(move |_, platform: Option<String>| {
                let Some(plat) = resolve_platform(&host, platform) else {
                    return Ok(None);
                };
                let key = plat.as_str().to_string();
                let mut map = async_eos().lock().unwrap();
                let needs_fetch = match map.get(&key) {
                    Some(AsyncEos::Pending) => false,
                    Some(AsyncEos::Done(Some(tok))) => tok.expired(),
                    _ => true,
                };
                if needs_fetch {
                    map.insert(key.clone(), AsyncEos::Pending);
                    drop(map);
                    let _ = eos_queue().send(plat);
                }
                Ok(Some(key))
            })?,
        )?;
    }

    // Returns nil (never requested), "pending", nil-on-failure, or a token
    // table {access_token, refresh_token, account_id, steam_id, expires_at,
    // platform}.
    hebnix.set(
        "eos_result",
        lua.create_function(|lua, key: String| {
            let map = async_eos().lock().unwrap();
            match map.get(&key) {
                None => Ok(LuaValue::Nil),
                Some(AsyncEos::Pending) => Ok(LuaValue::String(lua.create_string("pending")?)),
                Some(AsyncEos::Done(None)) => Ok(LuaValue::Boolean(false)),
                Some(AsyncEos::Done(Some(tok))) => Ok(to_lua(lua, tok)?),
            }
        })?,
    )?;

    // Non-blocking PsyNet request. `service` e.g. "Skills/GetPlayerSkill v1";
    // `body` an optional table; `platform` optional (defaults to detected).
    // Returns a request key to poll with hebnix.rlapi_result(key).
    // The session auto-authenticates (acquiring an EOS token) on first use.
    {
        let host = Rc::clone(&host);
        hebnix.set(
            "rlapi_request_async",
            lua.create_function(
                move |lua, (service, body, platform): (String, Option<LuaValue>, Option<String>)| {
                    let service = service.trim().to_string();
                    if service.is_empty() {
                        return Ok(None);
                    }
                    let Some(plat) = resolve_platform(&host, platform) else {
                        return Ok(None);
                    };
                    let json_body = lua_body_to_json(lua, body);
                    let key = next_rpc_key();
                    async_rpc().lock().unwrap().insert(key.clone(), AsyncRpc::Pending);
                    let _ = rlapi_queue().send(RlApiJob {
                        key: key.clone(),
                        platform: plat,
                        service,
                        body: json_body,
                    });
                    Ok(Some(key))
                },
            )?,
        )?;
    }

    // Returns nil (unknown key), "pending", or a result table
    // {ok=bool, result=table, error=string}. A completed result is consumed
    // (removed) on read, so poll once you get a table.
    hebnix.set(
        "rlapi_result",
        lua.create_function(|lua, key: String| {
            let mut map = async_rpc().lock().unwrap();
            match map.get(&key) {
                None => Ok(LuaValue::Nil),
                Some(AsyncRpc::Pending) => Ok(LuaValue::String(lua.create_string("pending")?)),
                Some(AsyncRpc::Done { .. }) => {
                    let Some(AsyncRpc::Done { ok, result, error }) = map.remove(&key) else {
                        unreachable!()
                    };
                    let table = lua.create_table()?;
                    table.set("ok", ok)?;
                    table.set("result", to_lua(lua, &result)?)?;
                    table.set("error", error)?;
                    Ok(LuaValue::Table(table))
                }
            }
        })?,
    )?;

    // Whether the shared PsyNet session is currently connected.
    hebnix.set(
        "rlapi_connected",
        lua.create_function(|_, ()| {
            Ok(rlapi_connected_flag().load(std::sync::atomic::Ordering::Relaxed))
        })?,
    )?;

    // Audio Control
    let audio = lua.create_table()?;
    {
        let host = Rc::clone(&host);
        audio.set(
            "play",
            lua.create_function(move |_, (path, volume): (String, Option<f32>)| {
                let Some(bytes) = load_asset(&host, &path) else {
                    host.log(&format!("Audio file not found: {}", path));
                    return Ok(false);
                };

                let vol = volume.unwrap_or(1.0).clamp(0.0, 5.0);

                play_audio(
                    bytes,
                    vol,
                    host.tx.clone(),
                    host.display_name.borrow().clone(),
                );

                Ok(true)
            })?,
        )?;
    }
    hebnix.set("audio", audio)?;

    // Synthetic keyboard input.
    //
    // input.send taps arbitrary keys and is disabled while a match is in
    // progress, so it can't be turned into a gameplay macro (jump/boost/flip
    // sequences, etc). chat.send is a separate, narrower endpoint that only
    // opens a chat channel, types a message, and hits enter — that's allowed
    // mid-match because sending chat isn't a competitive advantage, it's the
    // point of the feature.
    let input = lua.create_table()?;
    {
        let host = Rc::clone(&host);
        input.set(
            "send",
            lua.create_function(move |_, keys: Variadic<String>| {
                if host.shared.borrow().in_match {
                    return Err(mlua::Error::runtime(
                        "hebnix.input.send is disabled while a match is in progress",
                    ));
                }
                for key in keys.iter() {
                    if !hebnix_sdk::input::tap_key(key) {
                        return Err(mlua::Error::runtime(format!(
                            "hebnix.input.send: unknown key '{key}'"
                        )));
                    }
                }
                Ok(())
            })?,
        )?;
    }
    hebnix.set("input", input)?;

    let chat = lua.create_table()?;
    chat.set(
        "send",
        lua.create_function(|_, (channel, message): (String, String)| {
            let open_key = match channel.to_lowercase().as_str() {
                "global" => "t",
                "team" => "y",
                "party" => "u",
                other => {
                    return Err(mlua::Error::runtime(format!(
                        "hebnix.chat.send: unknown channel '{other}', expected global, team or party"
                    )));
                }
            };
            hebnix_sdk::input::tap_key(open_key);
            std::thread::sleep(Duration::from_millis(100));
            hebnix_sdk::input::type_text(&message);
            std::thread::sleep(Duration::from_millis(30));
            hebnix_sdk::input::tap_key("enter");
            Ok(())
        })?,
    )?;
    hebnix.set("chat", chat)?;

    // Read UTF-8 text bundled beneath a plugin's assets directory.  This keeps
    // plugin data files sandboxed while allowing small lookup tables.
    {
        let host = Rc::clone(&host);
        hebnix.set(
            "read_asset_text",
            lua.create_function(move |_, path: String| {
                let Some(bytes) = load_asset(&host, &path) else {
                    return Ok(None::<String>);
                };
                Ok(std::str::from_utf8(&bytes).ok().map(str::to_owned))
            })?,
        )?;
    }

    // Non-blocking bind capture (keyboard, XInput or PlayStation button).
    hebnix.set(
        "capture_bind_async",
        lua.create_function(|_, timeout: Option<f64>| {
            {
                let mut state = capture_state().lock().unwrap();
                if matches!(*state, CaptureState::Pending) {
                    return Ok(false);
                }
                *state = CaptureState::Pending;
            }
            let timeout = Duration::from_secs_f64(timeout.unwrap_or(10.0).clamp(1.0, 60.0));
            std::thread::spawn(move || {
                let result = hebnix_sdk::input::detect_any_hotkey(Some(timeout))
                    .map(|bind| hebnix_sdk::input::bind_to_string(&bind));
                *capture_state().lock().unwrap() = CaptureState::Done(result);
            });
            Ok(true)
        })?,
    )?;

    // Returns (status, bind): ("idle", nil) / ("pending", nil) /
    // ("done", "tab") / ("timeout", nil). "done"/"timeout" reset to idle.
    hebnix.set(
        "capture_bind_result",
        lua.create_function(|_, ()| {
            let mut state = capture_state().lock().unwrap();
            match &*state {
                CaptureState::Idle => Ok(("idle".to_string(), None::<String>)),
                CaptureState::Pending => Ok(("pending".to_string(), None)),
                CaptureState::Done(result) => {
                    let out = match result {
                        Some(bind) => ("done".to_string(), Some(bind.clone())),
                        None => ("timeout".to_string(), None),
                    };
                    *state = CaptureState::Idle;
                    Ok(out)
                }
            }
        })?,
    )?;

    hebnix.set(
        "parse_launch_log",
        lua.create_function(|lua, verify: Option<bool>| {
            let info = hebnix_sdk::log::parse_launch_log(None, verify.unwrap_or(true), "INT");
            to_lua(lua, &info)
        })?,
    )?;
    // Launch.log verify=true (default) confirms the match against the stats
    // api Returns the key to poll with hebnix.launch_log_result(key).
    hebnix.set(
        "parse_launch_log_async",
        lua.create_function(|_, verify: Option<bool>| {
            let verify = verify.unwrap_or(true);
            let key = format!("launchlog:{verify}");
            let mut map = async_log().lock().unwrap();
            if !map.contains_key(&key) {
                map.insert(key.clone(), AsyncLog::Pending);
                drop(map);
                let _ = log_queue().send((key.clone(), verify));
            }
            Ok(key)
        })?,
    )?;

    // Returns nil (never requested), "pending", or the log table.
    hebnix.set(
        "launch_log_result",
        lua.create_function(|lua, key: String| {
            let map = async_log().lock().unwrap();
            match map.get(&key) {
                None => Ok(LuaValue::Nil),
                Some(AsyncLog::Pending) => Ok(LuaValue::String(lua.create_string("pending")?)),
                Some(AsyncLog::Done(info)) => Ok(to_lua(lua, info)?),
            }
        })?,
    )?;

    // cleans the parseso the next _async call re-reads the log
    hebnix.set(
        "clear_launch_log",
        lua.create_function(|_, ()| {
            async_log().lock().unwrap().clear();
            Ok(())
        })?,
    )?;

    // Process detection
    hebnix.set(
        "find_rocket_league",
        lua.create_function(|lua, ()| match hebnix_sdk::process::find_rocket_league() {
            Some(rl) => {
                let t = lua.create_table()?;
                t.set("pid", rl.pid)?;
                t.set("exe_path", rl.exe_path.to_string_lossy().to_string())?;
                t.set("root_dir", rl.root_dir.to_string_lossy().to_string())?;
                t.set("platform", rl.platform.as_str())?;
                t.set(
                    "save_data_path",
                    rl.save_data_path.to_string_lossy().to_string(),
                )?;
                Ok(LuaValue::Table(t))
            }
            None => Ok(LuaValue::Nil),
        })?,
    )?;
    hebnix.set(
        "rocket_league_window_rect",
        lua.create_function(|lua, ()| {
            let Some((left, top, right, bottom)) =
                hebnix_sdk::process::get_rocket_league_window_rect()
            else {
                return Ok(LuaValue::Nil);
            };
            let rect = lua.create_table()?;
            rect.set("left", left)?;
            rect.set("top", top)?;
            rect.set("right", right)?;
            rect.set("bottom", bottom)?;
            Ok(LuaValue::Table(rect))
        })?,
    )?;

    // save file access. blocking: decrypt + parse takes a
    // moment, so call from a button/on_load, not every frame.
    hebnix.set(
        "find_save_file",
        lua.create_function(|_, ()| {
            Ok(
                hebnix_sdk::save_file::find_save_file(None)
                    .map(|p| p.to_string_lossy().to_string()),
            )
        })?,
    )?;
    hebnix.set(
        "load_save_summary",
        lua.create_function(|lua, path: Option<String>| {
            let path = match path.map(std::path::PathBuf::from) {
                Some(p) => p,
                None => match hebnix_sdk::save_file::find_save_file(None) {
                    Some(p) => p,
                    None => {
                        let t = lua.create_table()?;
                        t.set("error", "no .save file found")?;
                        return Ok(LuaValue::Table(t));
                    }
                },
            };
            match hebnix_sdk::save_file::load(&path, false) {
                Ok(save) => Ok(LuaValue::Table(build_save_summary(lua, &save, &path)?)),
                Err(e) => {
                    let t = lua.create_table()?;
                    t.set("error", e.to_string())?;
                    Ok(LuaValue::Table(t))
                }
            }
        })?,
    )?;

    hebnix.set(
        "find_save_accounts",
        lua.create_function(|lua, ()| {
            to_lua(lua, &hebnix_sdk::save_file::find_save_accounts(None))
        })?,
    )?;
    hebnix.set(
        "find_save_files",
        lua.create_function(|lua, ()| {
            let files = lua.create_table()?;
            let Some(dir) = hebnix_sdk::save_file::detect_save_data_path() else {
                return Ok(files);
            };
            let mut paths: Vec<_> = std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("save"))
                })
                .collect();
            paths.sort();
            for (index, path) in paths.iter().enumerate() {
                files.set(index + 1, path.to_string_lossy().to_string())?;
            }
            Ok(files)
        })?,
    )?;
    hebnix.set(
        "load_save_configuration",
        lua.create_function(|lua, path: Option<String>| {
            let Some(path) = path
                .map(std::path::PathBuf::from)
                .or_else(|| hebnix_sdk::save_file::find_save_file(None))
            else {
                let result = lua.create_table()?;
                result.set("error", "no .save file found")?;
                return Ok(result);
            };
            let result = lua.create_table()?;
            result.set("path", path.to_string_lossy().to_string())?;
            match hebnix_sdk::save_file::list_configuration(&path) {
                Ok(values) => result.set("values", to_lua(lua, &values)?)?,
                Err(error) => result.set("error", error.to_string())?,
            }
            Ok(result)
        })?,
    )?;
    hebnix.set(
        "update_save_configuration",
        lua.create_function(|lua, (path, id, value): (String, String, String)| {
            let result = lua.create_table()?;
            match hebnix_sdk::save_file::update_configuration(
                std::path::Path::new(&path),
                &id,
                &value,
            ) {
                Ok(backup) => {
                    result.set("ok", true)?;
                    result.set("backup", backup.to_string_lossy().to_string())?;
                }
                Err(error) => result.set("error", error.to_string())?,
            }
            Ok(result)
        })?,
    )?;
    hebnix.set(
        "restore_save_configuration",
        lua.create_function(|lua, (path, backup): (String, String)| {
            let result = lua.create_table()?;
            match hebnix_sdk::save_file::restore_configuration(
                std::path::Path::new(&path),
                std::path::Path::new(&backup),
            ) {
                Ok(safety_backup) => {
                    result.set("ok", true)?;
                    result.set("backup", safety_backup.to_string_lossy().to_string())?;
                }
                Err(error) => result.set("error", error.to_string())?,
            }
            Ok(result)
        })?,
    )?;

    hebnix.set(
        "backup_save_configuration",
        lua.create_function(|lua, path: String| {
            let result = lua.create_table()?;
            let source = std::path::Path::new(&path);
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let backup = std::path::PathBuf::from(format!(
                "{}.backup-{stamp}.save",
                source.to_string_lossy()
            ));
            match std::fs::copy(source, &backup) {
                Ok(_) => {
                    result.set("ok", true)?;
                    result.set("backup", backup.to_string_lossy().to_string())?;
                }
                Err(error) => result.set("error", error.to_string())?,
            }
            Ok(result)
        })?,
    )?;

    // Floating window control
    let window = lua.create_table()?;
    {
        let host = Rc::clone(&host);
        window.set(
            "open",
            lua.create_function(move |_, opts: Option<Table>| {
                let mut win = host.window.borrow_mut();
                let was_open = win.open;
                win.open = true;
                if let Some(opts) = opts {
                    if let Ok(t) = opts.get::<String>("title") {
                        win.title = t;
                    }
                    // number or percent of the monitor RL is on, "50%"
                    if let Ok(w) = opts.get::<LuaValue>("width") {
                        if let Some(spec) = parse_size_spec(&w) {
                            win.width = spec;
                        }
                    }
                    if let Ok(h) = opts.get::<LuaValue>("height") {
                        if let Some(spec) = parse_size_spec(&h) {
                            win.height = spec;
                        }
                    }
                    if let Ok(o) = opts.get::<f32>("opacity") {
                        win.opacity = o.clamp(0.0, 1.0); // 0 for a bare overlay
                    }
                    if let Ok(close_button) = opts.get::<bool>("close_button") {
                        win.close_button = close_button;
                    }
                }
                if win.title.is_empty() {
                    win.title = host.display_name.borrow().clone();
                }
                // Restore the last saved position, only on a fresh open, so
                // plugins that call open() every tick don't fight the user's
                // drag with stale coordinates.
                if !was_open {
                    let store = host.store.borrow();
                    let x = store.get_number("__win_x", -1.0);
                    let y = store.get_number("__win_y", -1.0);
                    if x >= 0.0 && y >= 0.0 {
                        win.pos = Some((x as f32, y as f32));
                        win.last_pos = win.pos;
                    }
                }
                Ok(())
            })?,
        )?;
    }
    {
        let host = Rc::clone(&host);
        window.set(
            "close",
            lua.create_function(move |_, ()| {
                host.window.borrow_mut().open = false;
                Ok(())
            })?,
        )?;
    }
    {
        let host = Rc::clone(&host);
        window.set(
            "is_open",
            lua.create_function(move |_, ()| Ok(host.window.borrow().open))?,
        )?;
    }
    {
        let host = Rc::clone(&host);
        window.set(
            "get_position",
            lua.create_function(move |lua, ()| {
                let position = host.window.borrow().last_pos;
                let Some((x, y)) = position else {
                    return Ok(LuaValue::Nil);
                };
                let result = lua.create_table()?;
                result.set("x", x)?;
                result.set("y", y)?;
                Ok(LuaValue::Table(result))
            })?,
        )?;
    }
    {
        let host = Rc::clone(&host);
        window.set(
            "set_title",
            lua.create_function(move |_, title: String| {
                host.window.borrow_mut().title = title;
                Ok(())
            })?,
        )?;
    }
    {
        let host = Rc::clone(&host);
        window.set(
            "set_close_button",
            lua.create_function(move |_, enabled: bool| {
                host.window.borrow_mut().close_button = enabled;
                Ok(())
            })?,
        )?;
    }
    hebnix.set("window", window)?;

    // byte-safe download variant of http_get_async — result lands in this
    // plugin's on_http_download_response(req_id, status, body), where body
    // is a raw byte string (not decoded as UTF-8), for binary responses like
    // avatar images that http_get_async's text-based body would corrupt.
    {
        let host = Rc::clone(&host);
        hebnix.set(
            "http_download_async",
            lua.create_function(
                move |_,
                      (req_id, url, headers): (
                    String,
                    String,
                    Option<std::collections::HashMap<String, String>>,
                )| {
                    let thread_tx = host.tx.clone();
                    let slug = host.slug.clone();

                    std::thread::spawn(move || {
                        let mut req = http_client().get(&url);

                        if let Some(hdrs) = headers {
                            for (k, v) in hdrs {
                                req = req.header(k, v);
                            }
                        }

                        let (status, body) = send_req_bytes(req);
                        let _ = thread_tx.send(AppMsg::PluginHttpDownloadRes {
                            slug,
                            req_id,
                            status,
                            body,
                        });
                    });
                    Ok(())
                },
            )?,
        )?;
    }

    // no-redirect GET — result lands in this plugin's
    // on_http_redirect_response(req_id, status, location), where location
    // is the response's Location header (empty string if the response
    // wasn't a redirect, or had no such header). Needed for OAuth flows
    // that hand back a payload (e.g. an auth code) IN the Location header
    // of a 302 rather than in a followable page body — http_get_async
    // would auto-follow that redirect via http_client()'s default policy
    // and lose the header entirely.
    {
        let host = Rc::clone(&host);
        hebnix.set(
            "http_get_no_redirect_async",
            lua.create_function(
                move |_,
                      (req_id, url, headers): (
                    String,
                    String,
                    Option<std::collections::HashMap<String, String>>,
                )| {
                    let thread_tx = host.tx.clone();
                    let slug = host.slug.clone();

                    std::thread::spawn(move || {
                        let mut req = http_client_no_redirect().get(&url);

                        if let Some(hdrs) = headers {
                            for (k, v) in hdrs {
                                req = req.header(k, v);
                            }
                        }

                        let (status, location) = send_req_location(req);
                        let _ = thread_tx.send(AppMsg::PluginHttpRedirectRes {
                            slug,
                            req_id,
                            status,
                            location,
                        });
                    });
                    Ok(())
                },
            )?,
        )?;
    }

    // result lands in this plugin's on_http_response(req_id, status, body)
    {
        let host = Rc::clone(&host);
        hebnix.set(
            "http_get_async",
            lua.create_function(
                move |_,
                      (req_id, url, headers): (
                    String,
                    String,
                    Option<std::collections::HashMap<String, String>>,
                )| {
                    let thread_tx = host.tx.clone();
                    let slug = host.slug.clone();

                    std::thread::spawn(move || {
                        let mut req = http_client().get(&url);

                        if let Some(hdrs) = headers {
                            for (k, v) in hdrs {
                                req = req.header(k, v);
                            }
                        }

                        let (status, body) = send_req(req);
                        let _ = thread_tx.send(AppMsg::PluginHttpRes {
                            slug,
                            req_id,
                            status,
                            body,
                        });
                    });
                    Ok(())
                },
            )?,
        )?;
    }

    {
        let host = Rc::clone(&host);
        hebnix.set(
            "http_post_async",
            lua.create_function(
                move |_,
                      (req_id, url, body, headers): (
                    String,
                    String,
                    String,
                    Option<std::collections::HashMap<String, String>>,
                )| {
                    let thread_tx = host.tx.clone();
                    let slug = host.slug.clone();

                    std::thread::spawn(move || {
                        let mut req = http_client().post(&url).body(body);

                        if let Some(hdrs) = headers {
                            for (k, v) in hdrs {
                                req = req.header(k, v);
                            }
                        }

                        let (status, body) = send_req(req);
                        let _ = thread_tx.send(AppMsg::PluginHttpRes {
                            slug,
                            req_id,
                            status,
                            body,
                        });
                    });
                    Ok(())
                },
            )?,
        )?;
    }

    // Browser / URL Launcher
    hebnix.set(
        "open_url",
        lua.create_function(|_, url: String| {
            // Zero-dependency Windows native way to open the default web browser
            let _ = std::process::Command::new("cmd")
                .args(["/C", "start", "", &url])
                .spawn();
            Ok(())
        })?,
    )?;

    // JSON Data Handling
    hebnix.set(
        "json_decode",
        lua.create_function(|lua, json_str: String| {
            let json: serde_json::Value =
                serde_json::from_str(&json_str).map_err(mlua::Error::external)?;
            to_lua(lua, &json)
        })?,
    )?;

    hebnix.set(
        "json_encode",
        lua.create_function(|lua, val: LuaValue| {
            let json = lua.from_value::<serde_json::Value>(val)?;
            serde_json::to_string(&json).map_err(mlua::Error::external)
        })?,
    )?;

    // Data Processing (Base64 + Zlib)
    hebnix.set(
        "base64_encode",
        lua.create_function(|_, data: mlua::String| {
            use base64::{Engine as _, engine::general_purpose::STANDARD};
            Ok(STANDARD.encode(data.as_bytes().as_ref()))
        })?,
    )?;

    hebnix.set(
        "zlib_compress",
        lua.create_function(|lua, data: mlua::String| {
            use std::io::Write;
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            encoder
                .write_all(data.as_bytes().as_ref())
                .map_err(mlua::Error::external)?;
            let compressed = encoder.finish().map_err(mlua::Error::external)?;
            lua.create_string(&compressed)
        })?,
    )?;

    // Crypto
    hebnix.set(
        "crypto_hmac_sha256",
        lua.create_function(|_, (key, message): (String, mlua::String)| {
            use hmac::{Hmac, Mac};
            use sha2::Sha256;

            type HmacSha256 = Hmac<Sha256>;

            let mut mac =
                HmacSha256::new_from_slice(key.as_bytes()).map_err(mlua::Error::external)?;
            mac.update(message.as_bytes().as_ref());
            let result = mac.finalize();
            Ok(hex::encode(result.into_bytes()))
        })?,
    )?;

    // WebSocket Send
    {
        let host = Rc::clone(&host);
        hebnix.set(
            "ws_send",
            lua.create_function(move |lua, (command, data): (String, Option<LuaValue>)| {
                let json_body = lua_body_to_json(lua, data);

                let cmd = if json_body.as_object().map_or(false, |o| o.is_empty()) {
                    hebnix_sdk::stats::websocket::WsCommand::new(command)
                } else {
                    hebnix_sdk::stats::websocket::WsCommand {
                        command,
                        data: Some(json_body),
                    }
                };

                let _ = host.tx.send(AppMsg::SendWsCommand(cmd));
                Ok(())
            })?,
        )?;
    }

    // Analog Gamepads Crate Integration
    hebnix.set(
        "poll_gamepads",
        lua.create_function(|lua, ()| {
            // Use a static OnceLock so the hardware context stays alive across frames
            static GAMEPADS: std::sync::OnceLock<std::sync::Mutex<gamepads::Gamepads>> =
                std::sync::OnceLock::new();
            let mut gp = GAMEPADS
                .get_or_init(|| std::sync::Mutex::new(gamepads::Gamepads::new()))
                .lock()
                .unwrap();

            // Poll hardware for fresh inputs
            gp.poll();

            let list = lua.create_table()?;
            let mut i = 1;

            for gamepad in gp.all() {
                let pad = lua.create_table()?;

                pad.set("id", format!("{:?}", gamepad.id()))?;

                // Analog Axes (-1.0 to 1.0)
                pad.set("lx", gamepad.left_stick_x())?;
                pad.set("ly", gamepad.left_stick_y())?;
                pad.set("rx", gamepad.right_stick_x())?;
                pad.set("ry", gamepad.right_stick_y())?;

                // Analog Triggers (0.0 to 1.0)
                pad.set("lt", gamepad.left_trigger())?;
                pad.set("rt", gamepad.right_trigger())?;

                // Standard SDL Mapped Buttons (Cross-Platform)
                pad.set(
                    "btn_south",
                    gamepad.is_currently_pressed(gamepads::Button::ActionDown),
                )?; // A / Cross
                pad.set(
                    "btn_east",
                    gamepad.is_currently_pressed(gamepads::Button::ActionRight),
                )?; // B / Circle
                pad.set(
                    "btn_west",
                    gamepad.is_currently_pressed(gamepads::Button::ActionLeft),
                )?; // X / Square
                pad.set(
                    "btn_north",
                    gamepad.is_currently_pressed(gamepads::Button::ActionUp),
                )?; // Y / Triangle

                pad.set(
                    "dpad_up",
                    gamepad.is_currently_pressed(gamepads::Button::DPadUp),
                )?;
                pad.set(
                    "dpad_down",
                    gamepad.is_currently_pressed(gamepads::Button::DPadDown),
                )?;
                pad.set(
                    "dpad_left",
                    gamepad.is_currently_pressed(gamepads::Button::DPadLeft),
                )?;
                pad.set(
                    "dpad_right",
                    gamepad.is_currently_pressed(gamepads::Button::DPadRight),
                )?;

                pad.set(
                    "bumper_l",
                    gamepad.is_currently_pressed(gamepads::Button::FrontLeftUpper),
                )?;
                pad.set(
                    "bumper_r",
                    gamepad.is_currently_pressed(gamepads::Button::FrontRightUpper),
                )?;
                pad.set(
                    "stick_l",
                    gamepad.is_currently_pressed(gamepads::Button::LeftStick),
                )?;
                pad.set(
                    "stick_r",
                    gamepad.is_currently_pressed(gamepads::Button::RightStick),
                )?;
                pad.set(
                    "select",
                    gamepad.is_currently_pressed(gamepads::Button::LeftCenterCluster),
                )?;
                pad.set(
                    "start",
                    gamepad.is_currently_pressed(gamepads::Button::RightCenterCluster),
                )?;
                pad.set(
                    "touchpad",
                    gamepad.is_currently_pressed(gamepads::Button::Mode),
                )?;

                list.set(i, pad)?;
                i += 1;
            }
            Ok(list)
        })?,
    )?;

    // goes through AppMsg, lua never reaches into the browser itself
    {
        let overlay = lua.create_table()?;
        let host = Rc::clone(&host);
        overlay.set(
            "send",
            lua.create_function(move |lua, value: LuaValue| {
                let data: serde_json::Value = lua.from_value(value)?;
                let _ = host.tx.send(AppMsg::OverlayPost {
                    slug: host.slug.clone(),
                    data,
                });
                Ok(())
            })?,
        )?;
        hebnix.set("overlay", overlay)?;
    }

    lua.globals().set("hebnix", hebnix)?;

    // Build the ui bridge table and stash it in the registry.
    let ui_table = build_ui_table(lua, Rc::clone(&host))?;
    lua.set_named_registry_value(UI_TABLE_REGISTRY, ui_table)?;

    // Build the overlay draw table (used by plugin.on_overlay).
    let draw_table = build_draw_table(lua, Rc::clone(&host))?;
    lua.set_named_registry_value(DRAW_TABLE_REGISTRY, draw_table)?;

    Ok(())
}

/// per-state overlay draw table
pub fn draw_table(lua: &Lua) -> mlua::Result<Table> {
    lua.named_registry_value(DRAW_TABLE_REGISTRY)
}

/// parse a "#rrggbb"/"#rrggbbaa" option into an rgba color. dcomp honors alpha,
/// the gdi fallback ignores it.
fn opt_rgba(
    opts: &Option<Table>,
    key: &str,
    default: crate::overlay::Rgba,
) -> crate::overlay::Rgba {
    opts.as_ref()
        .and_then(|t| t.get::<String>(key).ok())
        .and_then(|s| parse_hex_color(&s))
        .map(|c| crate::overlay::Rgba(c.r(), c.g(), c.b(), c.a()))
        .unwrap_or(default)
}

/// screen-space draw fns for the click-through overlay. coords are physical
/// pixels from the game window's top-left. calls outside an on_overlay frame
/// are no-ops (no active canvas).
fn build_draw_table(lua: &Lua, host: Rc<HostCtx>) -> mlua::Result<Table> {
    use crate::overlay;
    let draw = lua.create_table()?;
    const WHITE: overlay::Rgba = overlay::Rgba(255, 255, 255, 255);

    // draw.line(x1, y1, x2, y2, {color="#rrggbb[aa]", width=1})
    draw.set(
        "line",
        lua.create_function(
            |_, (x1, y1, x2, y2, opts): (f32, f32, f32, f32, Option<Table>)| {
                overlay::line(
                    x1,
                    y1,
                    x2,
                    y2,
                    opt_rgba(&opts, "color", WHITE),
                    opt_f32(&opts, "width", 1.0),
                );
                Ok(())
            },
        )?,
    )?;

    // draw.rect(x, y, w, h, {color=, filled=false, border=0, border_color=, radius=0})
    draw.set(
        "rect",
        lua.create_function(
            |_, (x, y, w, h, opts): (f32, f32, f32, f32, Option<Table>)| {
                let filled = opt_bool(&opts, "filled", false);
                let border = opts
                    .as_ref()
                    .and_then(|table| table.get::<f32>("border").ok())
                    .or_else(|| {
                        opts.as_ref()
                            .and_then(|table| table.get::<f32>("border_size").ok())
                    })
                    .unwrap_or_else(|| {
                        if filled {
                            0.0
                        } else {
                            opt_f32(&opts, "width", 1.0)
                        }
                    });
                let radius = opts
                    .as_ref()
                    .and_then(|table| table.get::<f32>("radius").ok())
                    .or_else(|| {
                        opts.as_ref()
                            .and_then(|table| table.get::<f32>("border_radius").ok())
                    })
                    .unwrap_or(0.0);
                overlay::rect(
                    x,
                    y,
                    w,
                    h,
                    opt_rgba(&opts, "color", WHITE),
                    opt_rgba(&opts, "border_color", opt_rgba(&opts, "color", WHITE)),
                    border,
                    filled,
                    radius,
                );
                Ok(())
            },
        )?,
    )?;

    // draw.circle(x, y, radius, {color=, width=, filled=false})
    draw.set(
        "circle",
        lua.create_function(|_, (x, y, radius, opts): (f32, f32, f32, Option<Table>)| {
            overlay::circle(
                x,
                y,
                radius,
                opt_rgba(&opts, "color", WHITE),
                opt_f32(&opts, "width", 1.0),
                opt_bool(&opts, "filled", false),
            );
            Ok(())
        })?,
    )?;

    // draw.text(x, y, "string", {color=, size=14, halign="left"|"center"|"right"})
    draw.set(
        "text",
        lua.create_function(|_, (x, y, s, opts): (f32, f32, String, Option<Table>)| {
            let halign = opts
                .as_ref()
                .and_then(|t| t.get::<String>("halign").ok())
                .unwrap_or_default();
            overlay::text(
                x,
                y,
                &s,
                opt_rgba(&opts, "color", WHITE),
                opt_f32(&opts, "size", 14.0),
                &halign,
            );
            Ok(())
        })?,
    )?;

    // draw.polygon({{x,y}, {x,y}, ...}, {color=})  -- filled polygon
    draw.set(
        "polygon",
        lua.create_function(|_, (points, opts): (Table, Option<Table>)| {
            let mut pts: Vec<(f32, f32)> = Vec::new();
            for pair in points.sequence_values::<Table>() {
                let pair = pair?;
                let x: f32 = pair.get(1)?;
                let y: f32 = pair.get(2)?;
                pts.push((x, y));
            }
            overlay::polygon(&pts, opt_rgba(&opts, "color", WHITE));
            Ok(())
        })?,
    )?;

    // draw.image("asset.png", x, y, width, height, {opacity=1.0})
    let host_clone = Rc::clone(&host);
    draw.set(
        "image",
        lua.create_function(
            move |_, (path, x, y, w, h, opts): (String, f32, f32, f32, f32, Option<Table>)| {
                // Ensure images are resolved cleanly relative to the plugin's folder so they can just bundle them!
                let full_path = crate::config::base_dir()
                    .join("plugins")
                    .join(&host_clone.slug)
                    .join(&path);

                let path_str = full_path.to_string_lossy().to_string();
                overlay::image(&path_str, x, y, w, h, opt_f32(&opts, "opacity", 1.0));
                Ok(())
            },
        )?,
    )?;

    Ok(draw)
}

/// per-state ui bridge table
pub fn ui_table(lua: &Lua) -> mlua::Result<Table> {
    lua.named_registry_value(UI_TABLE_REGISTRY)
}

/// compact lua table for a parsed .save. the typed models carry the full raw
/// trees which are way too big to hand to plugins, so we pick the useful bits.
fn build_save_summary(
    lua: &Lua,
    save: &hebnix_sdk::save_file::SaveData,
    path: &std::path::Path,
) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("path", path.to_string_lossy().to_string())?;
    t.set("engine_version", save.header.engine_version)?;
    t.set("license_version", save.header.licensee_version)?;
    t.set("objects", save.objects.len())?;

    // 3 values per stat id, slots unconfirmed
    if let Some(stats) = save.stats() {
        let s = lua.create_table()?;
        for (id, values) in &stats.stats {
            let v = lua.create_table()?;
            for (i, n) in values.iter().enumerate() {
                v.set(i + 1, *n)?;
            }
            s.set(id.clone(), v)?;
        }
        t.set("stats", s)?;
        t.set("product_stat_count", stats.product_stats.len())?;
    }

    if let Some(xp) = save.xp() {
        let x = lua.create_table()?;
        x.set("level", xp.level)?;
        x.set("xp", xp.xp)?;
        x.set("total_xp", xp.total_xp)?;
        x.set("next_threshold", xp.next_threshold)?;
        t.set("xp", x)?;
    }

    if let Some(video) = save.video() {
        let v = lua.create_table()?;
        v.set("window_mode", video.window_mode.as_str())?;
        v.set("resolution", video.resolution.clone())?;
        v.set("width", video.res_width)?;
        v.set("height", video.res_height)?;
        v.set("max_fps", video.max_fps)?;
        let opts = lua.create_table()?;
        for (k, val) in &video.options {
            opts.set(k.clone(), val.clone())?;
        }
        v.set("options", opts)?;
        t.set("video", v)?;
    }

    if let Some(camera) = save.camera() {
        let c = lua.create_table()?;
        c.set("fov", camera.fov)?;
        c.set("height", camera.height)?;
        c.set("angle", camera.angle)?;
        c.set("distance", camera.distance)?;
        c.set("stiffness", camera.stiffness)?;
        c.set("swivel_speed", camera.swivel_speed)?;
        c.set("transition_speed", camera.transition_speed)?;
        c.set("ball_cam_default", camera.prefers_secondary_camera)?;
        t.set("camera", c)?;
    }

    let skills = save.skills();
    if !skills.is_empty() {
        let sk = lua.create_table()?;
        for (playlist_id, skill) in &skills {
            let entry = lua.create_table()?;
            entry.set("tier", skill.tier)?;
            entry.set(
                "tier_name",
                hebnix_sdk::utils::get_tier_name(skill.tier.max(0) as usize),
            )?;
            entry.set("matches_played", skill.matches_played)?;
            sk.set(*playlist_id, entry)?;
        }
        t.set("skills", sk)?;
    }

    // slots past 3 unknown, so pass the raw array too
    if let Some(loadout) = save.loadout() {
        let l = lua.create_table()?;
        l.set("body", loadout.body())?;
        l.set("decal", loadout.decal())?;
        l.set("wheels", loadout.wheels())?;
        l.set("boost", loadout.boost())?;
        let slots = lua.create_table()?;
        for (i, p) in loadout.products.iter().enumerate() {
            slots.set(i, *p)?;
        }
        l.set("slots", slots)?;
        l.set("team_color_id", loadout.team_paint.team_color_id)?;
        l.set("custom_color_id", loadout.team_paint.custom_color_id)?;
        t.set("loadout", l)?;
    }

    if let Some(profile) = save.profile() {
        t.set("profile_name", profile.profile_name)?;
        t.set("player_title", profile.player_title)?;
    }

    t.set("inventory_count", save.inventory().len())?;
    t.set("recent_players_count", save.recent_players().len())?;
    t.set("observed_players_count", save.observed_players().len())?;

    Ok(t)
}

fn build_ui_table(lua: &Lua, host: Rc<HostCtx>) -> mlua::Result<Table> {
    let ui = lua.create_table()?;

    ui.set(
        "label",
        lua.create_function(|_, text: String| {
            with_current_ui(|ui| {
                ui.label(text);
            });
            Ok(())
        })?,
    )?;

    ui.set(
        "heading",
        lua.create_function(|_, text: String| {
            with_current_ui(|ui| {
                ui.heading(text);
            });
            Ok(())
        })?,
    )?;

    ui.set(
        "colored_label",
        lua.create_function(|_, (hex, text): (String, String)| {
            with_current_ui(|ui| {
                let color = parse_hex_color(&hex).unwrap_or(egui::Color32::GRAY);
                ui.colored_label(color, text);
            });
            Ok(())
        })?,
    )?;

    ui.set(
        "copy_to_clipboard",
        lua.create_function(|_, text: String| {
            with_current_ui(|ui| ui.ctx().copy_text(text));
            Ok(())
        })?,
    )?;
    // Persisted colour picker: ui.color_picker("key", "Label", "#rrggbb[aa]") -> hex
    // The alpha byte, when supplied, is preserved while selecting the RGB tint.
    {
        let host = Rc::clone(&host);
        ui.set(
            "color_picker",
            lua.create_function(
                move |_, (key, label, default): (String, String, Option<String>)| {
                    let fallback = default.unwrap_or_else(|| "#ffffff".to_string());
                    let mut value = host.store.borrow().get_string(&key, &fallback);
                    let had_alpha = value.trim().trim_start_matches('#').len() == 8;
                    let mut color = parse_hex_color(&value)
                        .or_else(|| parse_hex_color(&fallback))
                        .unwrap_or(egui::Color32::WHITE);
                    let alpha = color.a();
                    let mut rgb = [color.r(), color.g(), color.b()];
                    let changed = with_current_ui(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(&label);
                            ui.color_edit_button_srgb(&mut rgb).changed()
                        })
                        .inner
                    })
                    .unwrap_or(false);
                    if changed {
                        color = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                        value = if had_alpha {
                            format!(
                                "#{:02x}{:02x}{:02x}{alpha:02x}",
                                color.r(),
                                color.g(),
                                color.b()
                            )
                        } else {
                            format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b())
                        };
                        host.store.borrow_mut().set_string(&key, &value);
                    }
                    Ok(value)
                },
            )?,
        )?;
    }

    ui.set(
        "separator",
        lua.create_function(|_, ()| {
            with_current_ui(|ui| {
                ui.separator();
            });
            Ok(())
        })?,
    )?;

    ui.set(
        "space",
        lua.create_function(|_, px: Option<f32>| {
            with_current_ui(|ui| ui.add_space(px.unwrap_or(8.0)));
            Ok(())
        })?,
    )?;

    ui.set(
        "button",
        lua.create_function(|_, text: String| {
            Ok(with_current_ui(|ui| ui.button(text).clicked()).unwrap_or(false))
        })?,
    )?;

    {
        let host = Rc::clone(&host);
        ui.set(
            "slider",
            lua.create_function(
                move |_, (key, label, min, max, default): (String, String, f32, f32, Option<f32>)| {
                    let mut value = host.store.borrow().get_number(&key, default.unwrap_or(min) as f64) as f32;

                    let changed = with_current_ui(|ui| {
                        let mut is_changed = false;
                        ui.horizontal(|ui| {
                            ui.label(&label);
                            is_changed = ui.add(egui::Slider::new(&mut value, min..=max)).changed();
                        });
                        is_changed
                    }).unwrap_or(false);

                    if changed {
                        host.store.borrow_mut().set_number(&key, value as f64);
                    }
                    Ok(value)
                }
            )?
        )?;
    }

    {
        let host = Rc::clone(&host);
        ui.set(
            "combo_box",
            lua.create_function(move |_, (key, label, options): (String, String, Table)| {
                let mut opts = Vec::new();
                for val in options.sequence_values::<String>() {
                    if let Ok(s) = val {
                        opts.push(s);
                    }
                }

                let mut current_val = host.store.borrow().get_string(&key, "");
                let mut changed = false;

                if !opts.contains(&current_val) && !opts.is_empty() {
                    current_val = opts[0].clone();
                    changed = true;
                }

                let _ = with_current_ui(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(&label);
                        egui::ComboBox::from_id_salt(&key)
                            .selected_text(&current_val)
                            .show_ui(ui, |ui| {
                                for opt in &opts {
                                    if ui
                                        .selectable_value(&mut current_val, opt.clone(), opt)
                                        .changed()
                                    {
                                        changed = true;
                                    }
                                }
                            });
                    });
                });

                if changed {
                    host.store.borrow_mut().set_string(&key, &current_val);
                }
                Ok(current_val)
            })?,
        )?;
    }

    // caller-owned combo box for live data paths
    ui.set(
        "combo_box_value",
        lua.create_function(
            |_, (id, label, current, options): (String, String, String, Table)| {
                let opts: Vec<String> = options
                    .sequence_values::<String>()
                    .filter_map(Result::ok)
                    .collect();
                let mut selected = if current.is_empty() {
                    opts.first().cloned().unwrap_or_default()
                } else {
                    current
                };
                let _ = with_current_ui(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(&label);
                        egui::ComboBox::from_id_salt(&id)
                            .selected_text(&selected)
                            .show_ui(ui, |ui| {
                                for option in &opts {
                                    ui.selectable_value(&mut selected, option.clone(), option);
                                }
                            });
                    });
                });
                Ok(selected)
            },
        )?,
    )?;
    // Persisted checkbox: ui.checkbox("key", "Label", default) -> bool
    {
        let host = Rc::clone(&host);
        ui.set(
            "checkbox",
            lua.create_function(
                move |_, (key, label, default): (String, String, Option<bool>)| {
                    let mut value = host.store.borrow().get_bool(&key, default.unwrap_or(false));
                    let changed = with_current_ui(|ui| ui.checkbox(&mut value, label).changed())
                        .unwrap_or(false);
                    if changed {
                        host.store.borrow_mut().set_bool(&key, value);
                    }
                    Ok(value)
                },
            )?,
        )?;
    }

    // Persisted text input: ui.text_input("key", "placeholder") -> string
    {
        let host = Rc::clone(&host);
        ui.set(
            "text_input",
            lua.create_function(move |_, (key, placeholder): (String, Option<String>)| {
                let mut buf = {
                    let bufs = host.text_bufs.borrow();
                    match bufs.get(&key) {
                        Some(b) => b.clone(),
                        None => host.store.borrow().get_string(&key, ""),
                    }
                };
                let committed = with_current_ui(|ui| {
                    let mut edit = egui::TextEdit::singleline(&mut buf);
                    if let Some(ph) = &placeholder {
                        edit = edit.hint_text(ph.clone());
                    }
                    let response = ui.add(edit);
                    response.lost_focus()
                })
                .unwrap_or(false);
                host.text_bufs.borrow_mut().insert(key.clone(), buf.clone());
                if committed {
                    host.store.borrow_mut().set_string(&key, &buf);
                }
                Ok(buf)
            })?,
        )?;
    }

    {
        ui.set(
            "readonly_textbox",
            lua.create_function(move |_, (text, height): (String, Option<f32>)| {
                let _ = with_current_ui(|ui| {
                    let display_text = text;
                    let height = height.unwrap_or(260.0).max(48.0);
                    egui::ScrollArea::vertical()
                        .max_height(height)
                        .show(ui, |ui| {
                            ui.monospace(&display_text);
                        });
                });
                Ok(())
            })?,
        )?;
    }

    // ui.image("logo.png", {width=, height=, x=, y=, tint="#rrggbb[aa]"})
    // false when the file isn't in the plugin's assets folder, or for a url
    // hebnix holds no cached avatar for. x and y need both sizes, they paint
    // in place so images can stack instead of pushing the layout cursor along.
    {
        let host = Rc::clone(&host);
        ui.set(
            "image",
            lua.create_function(move |_, (path, opts): (String, Option<Table>)| {
                let (bytes, uri) = if is_http_url(&path) {
                    // an avatar hebnix already cached, or an allowed host
                    let found = tracker_client()
                        .avatar_bytes(&path)
                        .or_else(|| remote_image(&path));
                    match found {
                        Some(bytes) => (bytes, format!("bytes://remote/{path}")),
                        None => return Ok(false),
                    }
                } else {
                    match load_asset(&host, &path) {
                        Some(bytes) => (bytes, format!("bytes://plugin/{}/{}", host.slug, path)),
                        None => return Ok(false),
                    }
                };
                let w = opt_num(&opts, "width");
                let h = opt_num(&opts, "height");
                let at = match (opt_num(&opts, "x"), opt_num(&opts, "y")) {
                    (Some(x), Some(y)) => Some(egui::vec2(x, y)),
                    _ => None,
                };
                let tint = opts
                    .as_ref()
                    .and_then(|t| t.get::<String>("tint").ok())
                    .and_then(|s| parse_hex_color(&s));

                let drawn = with_current_ui(|ui| {
                    let mut img = egui::Image::from_bytes(uri, bytes);
                    if let Some(c) = tint {
                        img = img.tint(c);
                    }
                    match (w, h) {
                        (Some(w), Some(h)) => {
                            img = img.fit_to_exact_size(egui::vec2(w, h));
                        }
                        (Some(w), None) => img = img.max_width(w),
                        _ => {}
                    }
                    match (at, w, h) {
                        (Some(offset), Some(w), Some(h)) => {
                            // off the cursor, not min_rect, so a stack lands
                            // below whatever was drawn before it
                            let min = ui.cursor().min + offset;
                            img.paint_at(ui, egui::Rect::from_min_size(min, egui::vec2(w, h)));
                        }
                        _ => {
                            ui.add(img);
                        }
                    }
                    true
                })
                .unwrap_or(false);
                Ok(drawn)
            })?,
        )?;
    }

    // ui.horizontal(function() ... end)
    ui.set(
        "horizontal",
        lua.create_function(|lua, f: mlua::Function| {
            let ui_tbl: Table = ui_table(lua)?;
            with_current_ui(|outer| {
                outer.horizontal(|inner| {
                    with_ui_scope(inner, || {
                        if let Err(e) = f.call::<()>(ui_tbl.clone()) {
                            tracing::warn!("plugin ui.horizontal callback error: {e}");
                        }
                    });
                });
            });
            Ok(())
        })?,
    )?;

    Ok(ui)
}

/// save the plugin window pos for next session. two disk writes, dont call it
/// per frame.
pub fn persist_window_pos(host: &HostCtx, x: f32, y: f32) {
    if x >= 0.0 && y >= 0.0 {
        let mut store = host.store.borrow_mut();
        store.set_number("__win_x", x as f64);
        store.set_number("__win_y", y as f64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_spec_takes_numbers_and_percents() {
        let lua = Lua::new();
        let spec = |code: &str| {
            let v: LuaValue = lua.load(code).eval().unwrap();
            parse_size_spec(&v)
        };

        assert!(matches!(spec("return 650").unwrap(), SizeSpec::Fixed(v) if v == 650.0));
        assert!(matches!(spec("return 650.5").unwrap(), SizeSpec::Fixed(v) if v == 650.5));
        assert!(matches!(spec("return '650'").unwrap(), SizeSpec::Fixed(v) if v == 650.0));
        assert!(matches!(spec("return '50%'").unwrap(), SizeSpec::Percent(p) if p == 0.5));
        assert!(matches!(spec("return ' 25 % '").unwrap(), SizeSpec::Percent(p) if p == 0.25));
        assert!(matches!(spec("return '200%'").unwrap(), SizeSpec::Percent(p) if p == 1.0));

        assert!(spec("return 'wide'").is_none());
        assert!(spec("return true").is_none());
        assert!(spec("return nil").is_none());
    }

    // percent is a share of the monitor in pixels, the builder wants points
    #[test]
    fn percent_resolves_against_the_monitor() {
        assert_eq!(SizeSpec::Percent(0.5).resolve(1920.0, 1.0), 960.0);
        assert_eq!(SizeSpec::Percent(0.5).resolve(1920.0, 1.25), 768.0);
        assert_eq!(SizeSpec::Percent(0.25).resolve(1080.0, 1.0), 270.0);

        // a fixed size is the same everywhere, thats the whole point of it
        assert_eq!(SizeSpec::Fixed(650.0).resolve(1920.0, 1.0), 650.0);
        assert_eq!(SizeSpec::Fixed(650.0).resolve(3840.0, 1.25), 650.0);

        // never zero, egui wont make a window out of it
        assert!(SizeSpec::Percent(0.0).resolve(1920.0, 1.0) >= 1.0);
    }

    // regression: None must reach lua as real nil, not mlua's null sentinel.
    // plugins check res.error ~= nil to detect failures.
    #[test]
    fn none_serializes_to_lua_nil() {
        let lua = Lua::new();
        let stats = hebnix_sdk::tracker::PlayerStats {
            primary_id: "Steam|1|0".to_string(),
            display_name: "Test".to_string(),
            error: None,
            ..Default::default()
        };
        let value = to_lua(&lua, &stats).unwrap();
        lua.globals().set("res", value).unwrap();
        let error_is_nil: bool = lua.load("return res.error == nil").eval().unwrap();
        assert!(error_is_nil, "None must be Lua nil");

        let stats_err = hebnix_sdk::tracker::PlayerStats {
            error: Some("boom".to_string()),
            ..Default::default()
        };
        let value = to_lua(&lua, &stats_err).unwrap();
        lua.globals().set("res", value).unwrap();
        let error_is_nil: bool = lua.load("return res.error == nil").eval().unwrap();
        assert!(!error_is_nil, "Some(err) must be non-nil");
    }

    #[test]
    fn assets_stay_inside_the_assets_folder() {
        let dir = std::env::temp_dir().join("hebnix_asset_guard");
        let assets = dir.join("assets");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(assets.join("skin")).unwrap();
        std::fs::write(assets.join("logo.png"), b"x").unwrap();
        std::fs::write(assets.join("skin").join("base.png"), b"x").unwrap();
        // a secret one folder up, the thing a traversal would be after
        std::fs::write(dir.join("secret.txt"), b"x").unwrap();

        assert!(asset_path(&dir, "logo.png").is_ok());
        assert!(asset_path(&dir, "skin/base.png").is_ok());
        assert!(
            asset_path(&dir, "skin\\base.png").is_ok(),
            "backslashes too"
        );
        assert!(
            asset_path(&dir, "/logo.png").is_ok(),
            "leading slash is noise"
        );
        assert!(asset_path(&dir, "//logo.png").is_ok());

        for bad in [
            "../secret.txt",
            "..\\secret.txt",
            "skin/../../secret.txt",
            "/../secret.txt",
            "C:/Windows/win.ini",
            "logo.png:stream",
            "",
            "missing.png",
        ] {
            assert!(asset_path(&dir, bad).is_err(), "{bad:?} should not resolve");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_assets_folder_is_an_error_not_a_panic() {
        let dir = std::env::temp_dir().join("hebnix_asset_none");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(asset_path(&dir, "logo.png").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn platform_parsing() {
        assert_eq!(parse_eos_platform("Steam"), Some(EosPlatform::Steam));
        assert_eq!(parse_eos_platform(" epic "), Some(EosPlatform::Epic));
        assert_eq!(parse_eos_platform("epicgames"), Some(EosPlatform::Epic));
        assert_eq!(parse_eos_platform("xbox"), None);
        assert_eq!(parse_eos_platform(""), None);
    }

    #[test]
    fn body_conversion() {
        let lua = Lua::new();
        // Absent / nil / empty table all become an empty JSON object (so
        // no-arg PsyNet services get `{}`, never `[]`).
        assert_eq!(lua_body_to_json(&lua, None), serde_json::json!({}));
        assert_eq!(
            lua_body_to_json(&lua, Some(LuaValue::Nil)),
            serde_json::json!({})
        );
        let empty = lua.create_table().unwrap();
        assert_eq!(
            lua_body_to_json(&lua, Some(LuaValue::Table(empty))),
            serde_json::json!({})
        );

        // A populated table becomes a JSON object with matching keys.
        let t = lua.create_table().unwrap();
        t.set("PlayerID", "Steam|1|0").unwrap();
        let json = lua_body_to_json(&lua, Some(LuaValue::Table(t)));
        assert_eq!(json["PlayerID"], serde_json::json!("Steam|1|0"));
    }
}
