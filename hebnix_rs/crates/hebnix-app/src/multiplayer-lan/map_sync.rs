// tells tailnet peers which workshop map is in which slot, so a guest can see
// that they're missing the host's map and offer to install it. maps from the
// workshop cdn are described by id and can be downloaded from the cdn or, if
// the cdn is slow or down, fetched from the peer. maps a player imported
// themselves (`local_<hash>` ids) aren't on the cdn, so those only come from
// the peer -- see fetch_map_file for how that stays safe. every value read off the wire is
// untrusted and gets validated before it's used for anything.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::TsnetSidecarHandle;

pub const MAP_SYNC_PORT: u16 = 14790;
/// ids of maps a player imported themselves start with this, followed by the
/// first LOCAL_ID_HASH_CHARS hex characters of the file's sha256
pub const LOCAL_ID_PREFIX: &str = "local_";
const LOCAL_ID_HASH_CHARS: usize = 24;
/// hard cap on a map file received from a peer
pub const MAX_MAP_BYTES: u64 = 512 * 1024 * 1024;

/// first four bytes of an unreal package (0x9E2A83C1, little endian)
const UPK_MAGIC: [u8; 4] = [0xC1, 0x83, 0x2A, 0x9E];

const REQUEST: &str = "HEBNIX-MAPS 1";
const GET_REQUEST: &str = "HEBNIX-GET 1 ";
const MAX_REQUEST_LINE: usize = 96;
const MAX_HEADER_LINE: usize = 256;
const MAX_REPLY_BYTES: u64 = 16 * 1024;
const MAX_MAPS_PER_PEER: usize = 8;
const MAX_NAME_CHARS: usize = 64;
const MAX_DESCRIPTION_CHARS: usize = 500;
const MAX_CONNECTIONS: usize = 16;
const MAX_TRANSFERS: usize = 2;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const IO_TIMEOUT: Duration = Duration::from_secs(3);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_secs(10);
const ACCEPT_IDLE: Duration = Duration::from_millis(200);
/// anything smaller can't be a real map
const MIN_SHARED_BYTES: u64 = 1024;
/// the whole request line has to arrive within this, so a peer can't hold a
/// connection open by dripping one byte every few seconds
const REQUEST_DEADLINE: Duration = Duration::from_secs(5);

// per-player limits on the map sync port. every hebnix player shares one
// tailnet, so these keep one of them from tying up or flooding everyone
// else. a normal client polls once every POLL_INTERVAL and downloads a map
// now and then, nowhere near any of these.
const MAX_CONNECTIONS_PER_PEER: usize = 2;
const CONNECT_BURST: f64 = 5.0;
const CONNECT_REFILL: Duration = Duration::from_secs(2);
const GETS_PER_WINDOW: usize = 3;
const GET_WINDOW: Duration = Duration::from_secs(10 * 60);
/// online peers polled per round, so a flood of nodes can't balloon polling
const MAX_POLLED_PEERS: usize = 32;
/// a transfer that crawls below this is dropped (both directions)
const MIN_TRANSFER_RATE: u64 = 32 * 1024;
const RATE_WINDOW: Duration = Duration::from_secs(20);
/// overall transfer budget: this many bytes a second, plus TRANSFER_GRACE
const EXPECTED_TRANSFER_RATE: u64 = 128 * 1024;
const TRANSFER_GRACE: Duration = Duration::from_secs(30);
const MAX_TRANSFER_TIME: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct SlotMap {
    pub slot: String,
    pub id: String,
    pub name: String,
    /// imported by the player rather than from the workshop cdn (derived
    /// from the id on receive, never taken from the wire)
    #[serde(default)]
    pub local: bool,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
}

/// what one online peer says about itself
#[derive(Clone, Debug)]
pub struct PeerOffer {
    pub ip: IpAddr,
    /// their in-game name, empty if they haven't told us one
    pub player_name: String,
    /// tailnet hostname, the fallback label when there's no in-game name
    pub hostname: String,
    pub maps: Vec<SlotMap>,
}

impl PeerOffer {
    /// in-game name, else the tailnet hostname. never the address, so
    /// screenshots and streams don't hand out anyone's ip
    pub fn label(&self) -> String {
        if !self.player_name.is_empty() {
            self.player_name.clone()
        } else if !self.hostname.is_empty() {
            self.hostname.clone()
        } else {
            "Unknown player".to_string()
        }
    }
}

#[derive(Default)]
struct PeerUse {
    open: usize,
    tokens: f64,
    refilled: Option<Instant>,
    gets: VecDeque<Instant>,
    transferring: bool,
}

/// per-player connection and download limits for the map sync server
#[derive(Default)]
struct PeerLimits {
    peers: Mutex<HashMap<IpAddr, PeerUse>>,
}

impl PeerLimits {
    /// a new connection from `ip`, false if it's over its share
    fn try_open(&self, ip: IpAddr) -> bool {
        let Ok(mut peers) = self.peers.lock() else {
            return false;
        };
        let now = Instant::now();
        let peer = peers.entry(ip).or_default();
        let since = peer.refilled.map_or(Duration::MAX, |at| now - at);
        peer.tokens =
            (peer.tokens + since.as_secs_f64() / CONNECT_REFILL.as_secs_f64()).min(CONNECT_BURST);
        peer.refilled = Some(now);
        if peer.open >= MAX_CONNECTIONS_PER_PEER || peer.tokens < 1.0 {
            return false;
        }
        peer.tokens -= 1.0;
        peer.open += 1;
        true
    }

    fn close(&self, ip: IpAddr) {
        if let Ok(mut peers) = self.peers.lock() {
            if let Some(peer) = peers.get_mut(&ip) {
                peer.open = peer.open.saturating_sub(1);
            }
        }
    }

    /// one download at a time per player, and only a few per GET_WINDOW
    fn try_start_transfer(&self, ip: IpAddr) -> bool {
        let Ok(mut peers) = self.peers.lock() else {
            return false;
        };
        let now = Instant::now();
        let peer = peers.entry(ip).or_default();
        while peer.gets.front().is_some_and(|at| now - *at > GET_WINDOW) {
            peer.gets.pop_front();
        }
        if peer.transferring || peer.gets.len() >= GETS_PER_WINDOW {
            return false;
        }
        peer.gets.push_back(now);
        peer.transferring = true;
        true
    }

    fn end_transfer(&self, ip: IpAddr) {
        if let Ok(mut peers) = self.peers.lock() {
            if let Some(peer) = peers.get_mut(&ip) {
                peer.transferring = false;
            }
        }
    }
}

/// how long a transfer of `size` bytes may take in total
fn transfer_budget(size: u64) -> Duration {
    (Duration::from_secs(size / EXPECTED_TRANSFER_RATE) + TRANSFER_GRACE).min(MAX_TRANSFER_TIME)
}

/// drops transfers that blow their overall budget or crawl along so slowly
/// they'd hold a slot for ages
struct TransferGuard {
    deadline: Instant,
    window_start: Instant,
    window_bytes: u64,
}

impl TransferGuard {
    fn new(size: u64) -> Self {
        let now = Instant::now();
        Self {
            deadline: now + transfer_budget(size),
            window_start: now,
            window_bytes: 0,
        }
    }

    fn check(&mut self, moved: u64) -> Result<(), String> {
        let now = Instant::now();
        if now > self.deadline {
            return Err("the transfer took too long".to_string());
        }
        self.window_bytes += moved;
        let elapsed = now - self.window_start;
        if elapsed >= RATE_WINDOW {
            if self.window_bytes < MIN_TRANSFER_RATE * elapsed.as_secs() {
                return Err("the transfer is too slow".to_string());
            }
            self.window_start = now;
            self.window_bytes = 0;
        }
        Ok(())
    }
}

/// what one machine tells its peers about itself
#[derive(Clone, Debug, Default)]
pub struct LocalInfo {
    pub player_name: String,
    pub maps: Vec<SlotMap>,
}

#[derive(Deserialize, Serialize)]
struct Reply {
    #[serde(default)]
    player_name: String,
    maps: Vec<SlotMap>,
}

pub struct PeerInfoReply {
    pub player_name: String,
    pub maps: Vec<SlotMap>,
}

/// supplies this machine's in-game name and installed workshop maps
pub type MapProvider = Arc<dyn Fn() -> LocalInfo + Send + Sync>;

/// looks up the cached file for a map id, None if we don't have it
pub type MapFileProvider = Arc<dyn Fn(&str) -> Option<PathBuf> + Send + Sync>;

/// live progress of a download from a peer, polled by the ui
#[derive(Default)]
pub struct TransferProgress {
    pub done: AtomicU64,
    pub total: AtomicU64,
}

/// map ids end up in a cdn url and a cache filename, so only allow the
/// characters a plain id needs
pub fn valid_map_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn is_local_map_id(id: &str) -> bool {
    valid_map_id(id) && id.starts_with(LOCAL_ID_PREFIX)
}

/// `local_<first 24 hex of the sha256>` -- the same file always gets the same
/// id on every machine, which is also how a received file is verified
pub fn local_map_id(sha256_hex: &str) -> String {
    let end = sha256_hex.len().min(LOCAL_ID_HASH_CHARS);
    format!("{LOCAL_ID_PREFIX}{}", &sha256_hex[..end])
}

pub fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn clean_text(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(max_chars)
        .collect()
}

fn clean_name(name: &str) -> String {
    clean_text(name, MAX_NAME_CHARS)
}

/// peers are only trusted if they sit in the same /24 as our own tailnet
/// address (the tailnet is one shared subnet, see tsnet_sidecar.rs)
fn peer_allowed(local: IpAddr, peer: IpAddr) -> bool {
    match (local, peer) {
        (IpAddr::V4(local), IpAddr::V4(peer)) => local.octets()[..3] == peer.octets()[..3],
        _ => false,
    }
}

pub struct MapSync {
    stop: Arc<AtomicBool>,
    offers: Arc<Mutex<Vec<PeerOffer>>>,
    /// players blocked this session, with the name they had when blocked:
    /// their maps are hidden and their connections dropped
    blocked: Arc<Mutex<HashMap<IpAddr, String>>>,
}

impl MapSync {
    pub fn start(
        sidecar: Arc<TsnetSidecarHandle>,
        bind_ip: IpAddr,
        provider: MapProvider,
        files: MapFileProvider,
    ) -> Result<Self, String> {
        let (sync, _) = Self::start_server(bind_ip, MAP_SYNC_PORT, provider, files)?;
        let stop = sync.stop.clone();
        let offers = sync.offers.clone();
        let blocked = sync.blocked.clone();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let mut found = Vec::new();
                if let Ok(peers) = sidecar.peers_now() {
                    let peers = peers
                        .into_iter()
                        .filter(|peer| peer.online)
                        .take(MAX_POLLED_PEERS);
                    for peer in peers {
                        let Ok(ip) = peer.tailnet_ip.parse::<IpAddr>() else {
                            continue;
                        };
                        if !peer_allowed(bind_ip, ip) || is_blocked(&blocked, ip) {
                            continue;
                        }
                        if let Ok(info) = fetch_peer_info(ip, MAP_SYNC_PORT) {
                            found.push(PeerOffer {
                                ip,
                                player_name: info.player_name,
                                hostname: clean_name(&peer.hostname),
                                maps: info.maps,
                            });
                        }
                    }
                }
                if let Ok(mut current) = offers.lock() {
                    *current = found;
                }
                let mut waited = Duration::ZERO;
                while waited < POLL_INTERVAL && !stop.load(Ordering::Relaxed) {
                    thread::sleep(ACCEPT_IDLE);
                    waited += ACCEPT_IDLE;
                }
            }
        });
        Ok(sync)
    }

    fn start_server(
        bind_ip: IpAddr,
        port: u16,
        provider: MapProvider,
        files: MapFileProvider,
    ) -> Result<(Self, u16), String> {
        let listener = TcpListener::bind(SocketAddr::new(bind_ip, port))
            .map_err(|error| format!("could not bind the map sync port: {error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let blocked = Arc::new(Mutex::new(HashMap::new()));
        let server_stop = stop.clone();
        let server_blocked = blocked.clone();
        let connections = Arc::new(AtomicUsize::new(0));
        let transfers = Arc::new(AtomicUsize::new(0));
        let limits = Arc::new(PeerLimits::default());
        thread::spawn(move || {
            while !server_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, addr)) => {
                        let ip = addr.ip();
                        // anything over a limit is just dropped here, before a
                        // thread is spent on it
                        if !peer_allowed(bind_ip, ip)
                            || is_blocked(&server_blocked, ip)
                            || connections.load(Ordering::Relaxed) >= MAX_CONNECTIONS
                            || !limits.try_open(ip)
                        {
                            continue;
                        }
                        // one thread per connection so a big file transfer
                        // doesn't hold up everyone else's map-info requests
                        connections.fetch_add(1, Ordering::Relaxed);
                        let provider = provider.clone();
                        let files = files.clone();
                        let connections = connections.clone();
                        let transfers = transfers.clone();
                        let limits = limits.clone();
                        thread::spawn(move || {
                            let _ = serve(stream, ip, &provider, &files, &transfers, &limits);
                            limits.close(ip);
                            connections.fetch_sub(1, Ordering::Relaxed);
                        });
                    }
                    Err(_) => thread::sleep(ACCEPT_IDLE),
                }
            }
        });
        let sync = Self {
            stop,
            offers: Arc::new(Mutex::new(Vec::new())),
            blocked,
        };
        Ok((sync, port))
    }

    pub fn offers(&self) -> Vec<PeerOffer> {
        self.offers.lock().map(|o| o.clone()).unwrap_or_default()
    }

    /// hides a player's maps and refuses their connections until this
    /// session ends
    pub fn block(&self, ip: IpAddr, label: String) {
        if let Ok(mut blocked) = self.blocked.lock() {
            blocked.insert(ip, label);
        }
        if let Ok(mut offers) = self.offers.lock() {
            offers.retain(|offer| offer.ip != ip);
        }
    }

    /// lets a blocked player back in; their maps show up on the next poll
    pub fn unblock(&self, ip: IpAddr) {
        if let Ok(mut blocked) = self.blocked.lock() {
            blocked.remove(&ip);
        }
    }

    /// everyone blocked this session, by name
    pub fn blocked(&self) -> Vec<(IpAddr, String)> {
        let mut list: Vec<(IpAddr, String)> = self
            .blocked
            .lock()
            .map(|blocked| {
                blocked
                    .iter()
                    .map(|(ip, label)| (*ip, label.clone()))
                    .collect()
            })
            .unwrap_or_default();
        list.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
        list
    }
}

fn is_blocked(blocked: &Mutex<HashMap<IpAddr, String>>, ip: IpAddr) -> bool {
    blocked
        .lock()
        .map(|set| set.contains_key(&ip))
        .unwrap_or(false)
}

impl Drop for MapSync {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// reads one '\n'-terminated line a byte at a time, so nothing past the line
/// is consumed from the stream. the whole line has to arrive by `deadline`.
fn read_line(stream: &mut TcpStream, max: usize, deadline: Instant) -> std::io::Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if Instant::now() > deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "line took too long",
            ));
        }
        stream.read_exact(&mut byte)?;
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
        if line.len() > max {
            return Err(std::io::Error::other("line too long"));
        }
    }
    String::from_utf8(line).map_err(std::io::Error::other)
}

fn serve(
    mut stream: TcpStream,
    peer: IpAddr,
    provider: &MapProvider,
    files: &MapFileProvider,
    transfers: &AtomicUsize,
    limits: &PeerLimits,
) -> std::io::Result<()> {
    // accepted sockets can inherit the listener's non-blocking mode on windows
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(TRANSFER_TIMEOUT))?;
    let request = read_line(
        &mut stream,
        MAX_REQUEST_LINE,
        Instant::now() + REQUEST_DEADLINE,
    )?;
    if request == REQUEST {
        let mut info = provider();
        info.maps.truncate(MAX_MAPS_PER_PEER);
        let body = serde_json::to_vec(&Reply {
            player_name: info.player_name,
            maps: info.maps,
        })
        .map_err(std::io::Error::other)?;
        return stream.write_all(&body);
    }
    if let Some(id) = request.strip_prefix(GET_REQUEST) {
        return serve_file(&mut stream, peer, id, files, transfers, limits);
    }
    Ok(())
}

fn serve_file(
    stream: &mut TcpStream,
    peer: IpAddr,
    id: &str,
    files: &MapFileProvider,
    transfers: &AtomicUsize,
    limits: &PeerLimits,
) -> std::io::Result<()> {
    let refuse = |stream: &mut TcpStream, reason: &str| {
        let header = serde_json::json!({ "error": reason });
        stream.write_all(format!("{header}\n").as_bytes())
    };
    // valid_map_id keeps the id to plain filename characters, so nothing
    // outside the map cache can be named
    if !valid_map_id(id) {
        return refuse(stream, "not a shareable map");
    }
    let Some(path) = files(id) else {
        return refuse(stream, "map not found");
    };
    if !limits.try_start_transfer(peer) {
        return refuse(stream, "too many downloads, try again later");
    }
    if transfers.fetch_add(1, Ordering::Relaxed) >= MAX_TRANSFERS {
        transfers.fetch_sub(1, Ordering::Relaxed);
        limits.end_transfer(peer);
        return refuse(stream, "busy");
    }
    let result = (|| {
        let mut file = std::fs::File::open(path)?;
        let size = file.metadata()?.len();
        if size > MAX_MAP_BYTES {
            return refuse(stream, "map too large to share");
        }
        // only ever hand out unreal packages (.upk/.udk share the header),
        // whatever else might end up in the cache
        let mut magic = [0u8; 4];
        if size < MIN_SHARED_BYTES || file.read_exact(&mut magic).is_err() || magic != UPK_MAGIC {
            return refuse(stream, "not a map package");
        }
        let header = serde_json::json!({ "size": size });
        stream.write_all(format!("{header}\n").as_bytes())?;
        stream.write_all(&magic)?;
        let mut guard = TransferGuard::new(size);
        let mut buffer = [0u8; 65536];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                return Ok(());
            }
            stream.write_all(&buffer[..count])?;
            guard.check(count as u64).map_err(std::io::Error::other)?;
        }
    })();
    transfers.fetch_sub(1, Ordering::Relaxed);
    limits.end_transfer(peer);
    result
}

fn connect(peer: IpAddr, port: u16, read_timeout: Duration) -> Result<TcpStream, String> {
    let stream = TcpStream::connect_timeout(&SocketAddr::new(peer, port), CONNECT_TIMEOUT)
        .map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(read_timeout))
        .and_then(|_| stream.set_write_timeout(Some(IO_TIMEOUT)))
        .map_err(|error| error.to_string())?;
    Ok(stream)
}

pub fn fetch_peer_info(peer: IpAddr, port: u16) -> Result<PeerInfoReply, String> {
    let mut stream = connect(peer, port, IO_TIMEOUT)?;
    stream
        .write_all(format!("{REQUEST}\n").as_bytes())
        .map_err(|error| error.to_string())?;
    let mut body = Vec::new();
    stream
        .take(MAX_REPLY_BYTES)
        .read_to_end(&mut body)
        .map_err(|error| error.to_string())?;
    let reply: Reply = serde_json::from_slice(&body).map_err(|error| error.to_string())?;
    Ok(PeerInfoReply {
        player_name: clean_name(&reply.player_name),
        maps: reply
            .maps
            .into_iter()
            .filter(|map| valid_map_id(&map.id))
            .take(MAX_MAPS_PER_PEER)
            .map(|map| SlotMap {
                local: is_local_map_id(&map.id),
                name: clean_name(&map.name),
                author: clean_name(&map.author),
                description: clean_text(&map.description, MAX_DESCRIPTION_CHARS),
                ..map
            })
            .collect(),
    })
}

/// downloads a map from a peer into `dest`. the file only lands at `dest` if
/// it is no bigger than `max_bytes` and starts like an unreal package. a
/// `local_` map must also match the sha256 baked into its id, so a peer can't
/// hand over anything other than the exact file the id names. cdn maps have
/// no hash to check. the partial file is deleted on any failure.
pub fn fetch_map_file(
    peer: IpAddr,
    port: u16,
    id: &str,
    dest: &Path,
    max_bytes: u64,
    progress: &TransferProgress,
) -> Result<(), String> {
    if !valid_map_id(id) {
        return Err("not a valid map id".to_string());
    }
    let partial = dest.with_extension("part");
    let result = download(peer, port, id, &partial, max_bytes, progress);
    match result {
        Ok(()) => std::fs::rename(&partial, dest).map_err(|error| {
            let _ = std::fs::remove_file(&partial);
            error.to_string()
        }),
        Err(error) => {
            let _ = std::fs::remove_file(&partial);
            Err(error)
        }
    }
}

fn starts_with_upk_magic(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut magic))
        .is_ok()
        && magic == UPK_MAGIC
}

fn download(
    peer: IpAddr,
    port: u16,
    id: &str,
    partial: &Path,
    max_bytes: u64,
    progress: &TransferProgress,
) -> Result<(), String> {
    let mut stream = connect(peer, port, TRANSFER_TIMEOUT)?;
    stream
        .write_all(format!("{GET_REQUEST}{id}\n").as_bytes())
        .map_err(|error| error.to_string())?;
    let header = read_line(
        &mut stream,
        MAX_HEADER_LINE,
        Instant::now() + TRANSFER_TIMEOUT,
    )
    .map_err(|error| error.to_string())?;
    let header: serde_json::Value =
        serde_json::from_str(&header).map_err(|error| error.to_string())?;
    if let Some(error) = header.get("error").and_then(|v| v.as_str()) {
        return Err(format!("peer refused: {}", clean_name(error)));
    }
    let size = header
        .get("size")
        .and_then(|v| v.as_u64())
        .ok_or("peer sent no file size")?;
    if size < MIN_SHARED_BYTES || size > max_bytes {
        return Err(format!(
            "map size {size} bytes is outside the allowed limit"
        ));
    }
    progress.total.store(size, Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);

    let mut file = std::fs::File::create(partial).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut received = 0u64;
    let mut guard = TransferGuard::new(size);
    while received < size {
        let want = buffer.len().min((size - received) as usize);
        let count = stream
            .read(&mut buffer[..want])
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return Err("peer closed the connection early".to_string());
        }
        hasher.update(&buffer[..count]);
        file.write_all(&buffer[..count])
            .map_err(|error| error.to_string())?;
        received += count as u64;
        progress.done.store(received, Ordering::Relaxed);
        guard.check(count as u64)?;
    }
    file.flush().map_err(|error| error.to_string())?;
    drop(file);
    // only unreal packages (.upk/.udk) are accepted, imported or not
    if !starts_with_upk_magic(partial) {
        return Err("received file is not a map".to_string());
    }
    if is_local_map_id(id) && local_map_id(&hex::encode(hasher.finalize())) != id {
        return Err("received file does not match its map id".to_string());
    }
    Ok(())
}
