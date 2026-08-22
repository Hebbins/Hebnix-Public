use std::collections::{HashMap, VecDeque};
use std::net::{SocketAddr, UdpSocket};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;
const MAX_PACKET_BYTES: usize = 2_048;
const DATA_MAGIC: &[u8; 4] = b"HBD1";
const DATA_TAG_BYTES: usize = 32;
const DATA_HEADER_BYTES: usize = 16;
const DATA_CHUNK_BYTES: usize = 1_100;
const MAX_FRAGMENTS: usize = 8;
const MAX_PENDING_FRAMES: usize = 512;

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Payload {
    Join {
        pin: String,
        nonce: [u8; 16],
        tap_mac: String,
        tap_ip: String,
    },
    Accept {
        nonce: [u8; 16],
        tap_mac: String,
    },
    Data {
        packet: Vec<u8>,
    },
    DataFragment {
        packet_id: u64,
        index: u16,
        count: u16,
        data: String,
    },
}
#[derive(Serialize, Deserialize)]
struct Frame {
    sequence: u64,
    payload: Payload,
    tag: String,
}

#[derive(Default)]
pub struct TunnelStats {
    pub connected: AtomicBool,
    pub sent: AtomicU64,
    pub received: AtomicU64,
    pub delivered: AtomicU64,
    pub last_sent_udp: Mutex<String>,
    pub last_received_udp: Mutex<String>,
    pub last_sent_broadcast_udp: Mutex<String>,
    pub last_received_broadcast_udp: Mutex<String>,
    pub last_sent_lan_udp: Mutex<String>,
    pub last_received_lan_udp: Mutex<String>,
}

pub fn record_sent_udp(stats: &TunnelStats, frame: &[u8]) {
    record_udp(
        &stats.last_sent_udp,
        &stats.last_sent_broadcast_udp,
        &stats.last_sent_lan_udp,
        frame,
    );
}

pub fn record_received_udp(stats: &TunnelStats, frame: &[u8]) {
    record_udp(
        &stats.last_received_udp,
        &stats.last_received_broadcast_udp,
        &stats.last_received_lan_udp,
        frame,
    );
}
pub struct DirectHost {
    socket: UdpSocket,
    pin: String,
    key: Vec<u8>,
    peers: HashMap<SocketAddr, u64>,
    sequence: u64,
    stats: Arc<TunnelStats>,
    tap_mac: String,
    assemblies: HashMap<(SocketAddr, u64), Assembly>,
    pending: VecDeque<Vec<u8>>,
}
pub struct DirectGuest {
    socket: UdpSocket,
    host: SocketAddr,
    pin: String,
    key: Vec<u8>,
    sequence: u64,
    stats: Arc<TunnelStats>,
    tap_mac: String,
    local_ip: String,
    last_sequence: u64,
    assemblies: HashMap<(SocketAddr, u64), Assembly>,
}

struct Assembly {
    created: Instant,
    fragments: Vec<Option<Vec<u8>>>,
}

impl DirectHost {
    pub fn bind(
        port: u16,
        pin: impl Into<String>,
        join_token: impl Into<String>,
    ) -> Result<Self, String> {
        let socket = UdpSocket::bind(("0.0.0.0", port)).map_err(|e| e.to_string())?;
        socket.set_nonblocking(true).map_err(|e| e.to_string())?;
        Ok(Self {
            socket,
            pin: pin.into(),
            key: join_token.into().into_bytes(),
            peers: HashMap::new(),
            sequence: 0,
            stats: Arc::new(TunnelStats::default()),
            tap_mac: String::new(),
            assemblies: HashMap::new(),
            pending: VecDeque::new(),
        })
    }
    pub fn with_credentials(
        mut self,
        pin: impl Into<String>,
        join_token: impl Into<String>,
        tap_mac: impl Into<String>,
    ) -> Self {
        self.pin = pin.into();
        self.key = join_token.into().into_bytes();
        self.tap_mac = tap_mac.into();
        self
    }
    pub fn stats(&self) -> Arc<TunnelStats> {
        self.stats.clone()
    }
    pub fn poll(&mut self) -> Result<Option<Vec<u8>>, String> {
        let Some((payload, peer, sequence)) = receive(&self.socket, &self.key)? else {
            return Ok(None);
        };
        match payload {
            Payload::Join {
                pin,
                nonce,
                tap_mac,
                tap_ip,
            } if pin == self.pin => {
                validate_guest_ip(&tap_ip)?;
                super::tap::add_neighbor(&tap_ip, &tap_mac)?;
                self.peers.insert(peer, sequence);
                self.stats.connected.store(true, Ordering::Relaxed);
                self.sequence += 1;
                send(
                    &self.socket,
                    peer,
                    &self.key,
                    self.sequence,
                    Payload::Accept {
                        nonce,
                        tap_mac: self.tap_mac.clone(),
                    },
                )?;
                if let Ok(announcement) = super::tap::arp_announcement(super::HOST_ADDRESS) {
                    self.sequence += 1;
                    send(
                        &self.socket,
                        peer,
                        &self.key,
                        self.sequence,
                        Payload::Data {
                            packet: announcement,
                        },
                    )?;
                    self.stats.sent.fetch_add(1, Ordering::Relaxed);
                }
                while let Some(packet) = self.pending.pop_front() {
                    self.relay(&packet)?;
                }
                Ok(None)
            }
            Payload::Data { packet }
                if self.peers.get(&peer).is_some_and(|last| sequence > *last) =>
            {
                self.peers.insert(peer, sequence);
                if tap_frame(&packet) {
                    self.stats.received.fetch_add(1, Ordering::Relaxed);
                    Ok(Some(packet))
                } else {
                    Ok(None)
                }
            }
            Payload::DataFragment {
                packet_id,
                index,
                count,
                data,
            } if self.peers.get(&peer).is_some_and(|last| packet_id > *last) => {
                let packet =
                    push_fragment(&mut self.assemblies, peer, packet_id, index, count, &data)?;
                if let Some(packet) = packet {
                    self.peers.insert(peer, packet_id);
                    if tap_frame(&packet) {
                        self.stats.received.fetch_add(1, Ordering::Relaxed);
                        return Ok(Some(packet));
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }
    pub fn relay(&mut self, packet: &[u8]) -> Result<(), String> {
        if !tap_frame(packet) {
            return Ok(());
        };
        if self.peers.is_empty() {
            if self.pending.len() < MAX_PENDING_FRAMES {
                self.pending.push_back(packet.to_vec());
            }
            return Ok(());
        }
        for peer in self.peers.keys().copied().collect::<Vec<_>>() {
            self.sequence += 1;
            send_data(&self.socket, peer, &self.key, self.sequence, packet)?;
            self.stats.sent.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

impl DirectGuest {
    pub fn new(
        host: SocketAddr,
        pin: impl Into<String>,
        join_token: impl Into<String>,
        tap_mac: impl Into<String>,
        local_ip: impl Into<String>,
    ) -> Result<Self, String> {
        let socket = UdpSocket::bind(("0.0.0.0", 0)).map_err(|e| e.to_string())?;
        socket.set_nonblocking(true).map_err(|e| e.to_string())?;
        Ok(Self {
            socket,
            host,
            pin: pin.into(),
            key: join_token.into().into_bytes(),
            sequence: 0,
            stats: Arc::new(TunnelStats::default()),
            tap_mac: tap_mac.into(),
            local_ip: local_ip.into(),
            last_sequence: 0,
            assemblies: HashMap::new(),
        })
    }
    pub fn stats(&self) -> Arc<TunnelStats> {
        self.stats.clone()
    }
    pub fn begin(&mut self) -> Result<(), String> {
        let mut nonce = [0; 16];
        rand::thread_rng().fill_bytes(&mut nonce);
        self.sequence += 1;
        send(
            &self.socket,
            self.host,
            &self.key,
            self.sequence,
            Payload::Join {
                pin: self.pin.clone(),
                nonce,
                tap_mac: self.tap_mac.clone(),
                tap_ip: self.local_ip.clone(),
            },
        )
    }
    pub fn send_packet(&mut self, packet: &[u8]) -> Result<(), String> {
        if !tap_frame(packet) {
            return Ok(());
        };
        self.sequence += 1;
        send_data(&self.socket, self.host, &self.key, self.sequence, packet)?;
        self.stats.sent.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    pub fn poll(&mut self) -> Result<Option<Vec<u8>>, String> {
        let Some((payload, peer, sequence)) = receive(&self.socket, &self.key)? else {
            return Ok(None);
        };
        if peer != self.host {
            return Ok(None);
        };
        Ok(match payload {
            Payload::Accept { tap_mac, .. } => {
                super::tap::add_neighbor(super::HOST_ADDRESS, &tap_mac)?;
                self.stats.connected.store(true, Ordering::Relaxed);
                if let Ok(announcement) = super::tap::arp_announcement(&self.local_ip) {
                    self.send_packet(&announcement)?;
                }
                None
            }
            Payload::Data { packet } if tap_frame(&packet) => {
                self.stats.received.fetch_add(1, Ordering::Relaxed);
                Some(packet)
            }
            Payload::DataFragment {
                packet_id,
                index,
                count,
                data,
            } if packet_id > self.last_sequence => {
                let packet =
                    push_fragment(&mut self.assemblies, peer, packet_id, index, count, &data)?;
                if let Some(packet) = packet {
                    self.last_sequence = packet_id.max(sequence);
                    if tap_frame(&packet) {
                        self.stats.received.fetch_add(1, Ordering::Relaxed);
                        Some(packet)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        })
    }
}

fn validate_guest_ip(address: &str) -> Result<(), String> {
    let Some(last) = address
        .strip_prefix(&format!("{}.", super::VPN_SUBNET))
        .and_then(|value| value.parse::<u8>().ok())
    else {
        return Err("invalid Workshop LAN guest address".to_string());
    };
    super::guest_address(last).map(|_| ())
}

fn send(
    socket: &UdpSocket,
    peer: SocketAddr,
    key: &[u8],
    sequence: u64,
    payload: Payload,
) -> Result<(), String> {
    let raw = serde_json::to_vec(&(sequence, &payload)).map_err(|e| e.to_string())?;
    let mut mac = HmacSha256::new_from_slice(key).map_err(|e| e.to_string())?;
    mac.update(&raw);
    let frame = Frame {
        sequence,
        payload,
        tag: hex::encode(mac.finalize().into_bytes()),
    };
    socket
        .send_to(
            &serde_json::to_vec(&frame).map_err(|e| e.to_string())?,
            peer,
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn send_data(
    socket: &UdpSocket,
    peer: SocketAddr,
    key: &[u8],
    packet_id: u64,
    packet: &[u8],
) -> Result<(), String> {
    let count = packet.len().div_ceil(DATA_CHUNK_BYTES);
    if count == 0 || count > MAX_FRAGMENTS {
        return Err("invalid tunnel Ethernet frame size".to_string());
    }
    for (index, chunk) in packet.chunks(DATA_CHUNK_BYTES).enumerate() {
        send_data_fragment(
            socket,
            peer,
            key,
            packet_id,
            index as u16,
            count as u16,
            chunk,
        )?;
    }
    Ok(())
}

fn send_data_fragment(
    socket: &UdpSocket,
    peer: SocketAddr,
    key: &[u8],
    packet_id: u64,
    index: u16,
    count: u16,
    chunk: &[u8],
) -> Result<(), String> {
    let mut bytes = Vec::with_capacity(DATA_HEADER_BYTES + chunk.len() + DATA_TAG_BYTES);
    bytes.extend_from_slice(DATA_MAGIC);
    bytes.extend_from_slice(&packet_id.to_le_bytes());
    bytes.extend_from_slice(&index.to_le_bytes());
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(chunk);
    let mut mac = HmacSha256::new_from_slice(key).map_err(|error| error.to_string())?;
    mac.update(&bytes);
    bytes.extend_from_slice(&mac.finalize().into_bytes());
    socket
        .send_to(&bytes, peer)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn push_fragment(
    assemblies: &mut HashMap<(SocketAddr, u64), Assembly>,
    peer: SocketAddr,
    packet_id: u64,
    index: u16,
    count: u16,
    data: &str,
) -> Result<Option<Vec<u8>>, String> {
    let count = count as usize;
    let index = index as usize;
    if count == 0 || count > MAX_FRAGMENTS || index >= count {
        return Err("invalid tunnel fragment".to_string());
    }
    assemblies.retain(|_, assembly| assembly.created.elapsed() < Duration::from_secs(3));
    let assembly = assemblies
        .entry((peer, packet_id))
        .or_insert_with(|| Assembly {
            created: Instant::now(),
            fragments: vec![None; count],
        });
    if assembly.fragments.len() != count {
        assemblies.remove(&(peer, packet_id));
        return Err("inconsistent tunnel fragment count".to_string());
    }
    assembly.fragments[index] = Some(
        BASE64
            .decode(data)
            .map_err(|_| "invalid tunnel fragment encoding".to_string())?,
    );
    if assembly.fragments.iter().any(Option::is_none) {
        return Ok(None);
    }
    let assembly = assemblies
        .remove(&(peer, packet_id))
        .ok_or_else(|| "tunnel fragment assembly disappeared".to_string())?;
    let packet = assembly.fragments.into_iter().flatten().flatten().collect();
    Ok(Some(packet))
}

fn receive(socket: &UdpSocket, key: &[u8]) -> Result<Option<(Payload, SocketAddr, u64)>, String> {
    let mut bytes = [0; MAX_PACKET_BYTES];
    let (len, peer) = match socket.recv_from(&mut bytes) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if len >= DATA_HEADER_BYTES + DATA_TAG_BYTES && bytes[..4] == *DATA_MAGIC {
        let signed = &bytes[..len - DATA_TAG_BYTES];
        let tag = &bytes[len - DATA_TAG_BYTES..len];
        let mut mac = HmacSha256::new_from_slice(key).map_err(|error| error.to_string())?;
        mac.update(signed);
        mac.verify_slice(tag)
            .map_err(|_| "invalid tunnel authentication".to_string())?;
        let packet_id = u64::from_le_bytes(signed[4..12].try_into().unwrap());
        let index = u16::from_le_bytes(signed[12..14].try_into().unwrap());
        let count = u16::from_le_bytes(signed[14..16].try_into().unwrap());
        return Ok(Some((
            Payload::DataFragment {
                packet_id,
                index,
                count,
                data: BASE64.encode(&signed[DATA_HEADER_BYTES..]),
            },
            peer,
            packet_id,
        )));
    }
    let frame: Frame = serde_json::from_slice(&bytes[..len]).map_err(|e| e.to_string())?;
    let raw = serde_json::to_vec(&(frame.sequence, &frame.payload)).map_err(|e| e.to_string())?;
    let mut mac = HmacSha256::new_from_slice(key).map_err(|e| e.to_string())?;
    mac.update(&raw);
    let tag = hex::decode(frame.tag).map_err(|_| "invalid tunnel tag".to_string())?;
    mac.verify_slice(&tag)
        .map_err(|_| "invalid tunnel authentication".to_string())?;
    Ok(Some((frame.payload, peer, frame.sequence)))
}
fn tap_frame(frame: &[u8]) -> bool {
    (14..=MAX_PACKET_BYTES).contains(&frame.len())
}

fn record_udp(
    latest: &Mutex<String>,
    broadcast: &Mutex<String>,
    rocket_league: &Mutex<String>,
    frame: &[u8],
) {
    if frame.len() < 42 || frame[12..14] != 0x0800u16.to_be_bytes() || frame[23] != 17 {
        return;
    }
    let source = format!("{}.{}.{}.{}", frame[26], frame[27], frame[28], frame[29]);
    let destination = format!("{}.{}.{}.{}", frame[30], frame[31], frame[32], frame[33]);
    let source_port = u16::from_be_bytes([frame[34], frame[35]]);
    let destination_port = u16::from_be_bytes([frame[36], frame[37]]);
    let flow = format!("{source}:{source_port} → {destination}:{destination_port}");
    if let Ok(mut value) = latest.lock() {
        *value = flow.clone();
    }
    if is_non_system_broadcast(&frame[30..34], destination_port) {
        if let Ok(mut value) = broadcast.lock() {
            *value = flow.clone();
        }
    }
    if !is_rocket_league_port(source_port) && !is_rocket_league_port(destination_port) {
        return;
    }
    if let Ok(mut value) = rocket_league.lock() {
        *value = flow;
    }
}

fn is_rocket_league_port(port: u16) -> bool {
    matches!(port, 7777 | 7778 | 14_000..=14_010)
}

fn is_non_system_broadcast(destination: &[u8], port: u16) -> bool {
    (destination == [255, 255, 255, 255]
        || destination[3] == 255
        || (224..=239).contains(&destination[0]))
        && !matches!(port, 1900 | 5353)
}
