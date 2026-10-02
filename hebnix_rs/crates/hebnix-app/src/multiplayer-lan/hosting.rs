use std::net::SocketAddr;
use std::sync::{
    Arc,
    atomic::Ordering,
    mpsc::{self, Sender as StdSender},
};
use std::thread::{self, JoinHandle};

use crossbeam_channel::Sender;

use crate::messages::AppMsg;
use super::beacon::BeaconRelay;
use super::map_sync::{MapFileProvider, MapProvider, MapSync, PeerOffer};
use super::{PACKET_PUMP_INTERVAL, PEER_REFRESH_INTERVAL, TsnetSidecarHandle, TunnelStats};

pub struct HostSession {
    pub stats: Arc<TunnelStats>,
    stop_sender: StdSender<()>,
    worker: Option<JoinHandle<()>>,
    // kept alive only so the tailnet connection stays up for as long as
    // hosting does; also polled by the worker thread below for the live
    // peer list the relay sends to
    _sidecar: Arc<TsnetSidecarHandle>,
    // None if its port couldn't be bound; the relay works without it
    map_sync: Option<MapSync>,
}

impl std::fmt::Debug for HostSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostSession")
            .finish_non_exhaustive()
    }
}

impl HostSession {
    /// `host_tailnet_ip` is this machine's own address on the tailnet
    /// (learned from the sidecar before this is called). No room/PIN needed
    /// any more -- the relay just forwards to every peer currently on the
    /// tailnet (polled from the sidecar on the same cadence a room heartbeat
    /// used to run on), and Rocket League's own native LAN browser plus its
    /// own match password are what a guest actually uses to find and join.
    pub fn start(
        sidecar: Arc<TsnetSidecarHandle>,
        host_tailnet_ip: String,
        map_provider: MapProvider,
        map_files: MapFileProvider,
        tx: Sender<AppMsg>,
    ) -> Result<Self, String> {
        let host_octets = parse_ipv4(&host_tailnet_ip)?;
        let host_ip: std::net::IpAddr = host_tailnet_ip
            .parse()
            .map_err(|_| format!("invalid tailnet address: {host_tailnet_ip}"))?;
        let relay = BeaconRelay::bind(host_ip)?;
        let map_sync = match MapSync::start(sidecar.clone(), host_ip, map_provider, map_files) {
            Ok(sync) => Some(sync),
            Err(error) => {
                let _ = tx.send(AppMsg::Log(format!("[Core] Map sync unavailable: {error}")));
                None
            }
        };
        let stats = Arc::new(TunnelStats::default());
        let (stop_sender, stop_receiver) = mpsc::channel();
        let worker_stats = stats.clone();
        let worker_sidecar = sidecar.clone();
        let _ = tx.send(AppMsg::Log(format!(
            "[Core] Beacon relay bound to {host_ip}, watching UDP {:?}",
            super::RL_DISCOVERY_PORTS
        )));
        let worker = thread::spawn(move || {
            // refreshed from the tailnet's own peer list, not a room -
            // relaying to everyone currently connected is the point. Just
            // the IPs -- the destination port varies per packet, matching
            // whichever discovery port it was captured on (see beacon.rs).
            // Fetched once immediately rather than waiting for the first
            // PEER_REFRESH_INTERVAL tick, so an already-connected peer isn't
            // missed by beacons captured right at session start.
            let mut guest_ips: Vec<std::net::IpAddr> = worker_sidecar
                .peers_now()
                .map(|peers| {
                    peers
                        .into_iter()
                        .filter(|peer| peer.online)
                        .filter_map(|peer| peer.tailnet_ip.parse().ok())
                        .collect()
                })
                .unwrap_or_default();
            worker_stats.connected.store(!guest_ips.is_empty(), Ordering::Relaxed);
            let mut next_refresh = std::time::Instant::now() + PEER_REFRESH_INTERVAL;
            loop {
                if stop_receiver.try_recv().is_ok() {
                    // unblocks the beacon capture thread's WinDivert read
                    // directly, without touching the WinDivert service - see
                    // RawCapture's doc comment in beacon.rs
                    relay.stop_capture();
                    break;
                }
                if let Some((payload, source, port)) = relay.try_receive() {
                    let _ = tx.send(AppMsg::Log(format!(
                        "[Core] Beacon relay captured {} bytes on UDP {port} from {source} - {} known peer(s) to relay to",
                        payload.len(),
                        guest_ips.len()
                    )));
                    let rewritten = rewrite_lan_beacon_payload(payload, host_octets);
                    for &guest_ip in &guest_ips {
                        // don't echo the beacon back to whoever it came from
                        // -- WinDivert's sniff filter matches both directions
                        // (ip.SrcAddr OR ip.DstAddr == this host), so a
                        // packet this relay just *sent* to a peer shows up
                        // again as a "captured" packet from that peer's
                        // address once it lands on the wire, and would
                        // otherwise get bounced straight back to them
                        if guest_ip == source.ip() {
                            continue;
                        }
                        let guest = SocketAddr::new(guest_ip, port);
                        match relay.send_to(&rewritten, guest) {
                            Ok(()) => {
                                worker_stats.sent.fetch_add(1, Ordering::Relaxed);
                                if let Ok(mut value) = worker_stats.last_beacon_relayed.lock() {
                                    *value = format!("beacon → {guest}");
                                }
                            }
                            Err(error) => {
                                let _ = tx.send(AppMsg::Log(format!(
                                    "[Core] Beacon relay failed to send to {guest}: {error}"
                                )));
                            }
                        }
                    }
                }
                if std::time::Instant::now() >= next_refresh {
                    if let Ok(peers) = worker_sidecar.peers_now() {
                        guest_ips = peers
                            .into_iter()
                            .filter(|peer| peer.online)
                            .filter_map(|peer| peer.tailnet_ip.parse().ok())
                            .collect();
                        worker_stats
                            .connected
                            .store(!guest_ips.is_empty(), Ordering::Relaxed);
                    }
                    next_refresh += PEER_REFRESH_INTERVAL;
                }
                thread::sleep(PACKET_PUMP_INTERVAL);
            }
        });
        Ok(Self {
            stats,
            stop_sender,
            worker: Some(worker),
            _sidecar: sidecar,
            map_sync,
        })
    }

    /// workshop maps online peers say they have installed
    pub fn peer_offers(&self) -> Vec<PeerOffer> {
        self.map_sync.as_ref().map(MapSync::offers).unwrap_or_default()
    }

    pub fn suspend(&mut self) {
        // the worker thread calls relay.stop_capture() itself once it sees
        // this, which cancels the beacon capture thread's blocked WinDivert
        // read directly (see RawCapture in beacon.rs) and drops the handle,
        // releasing the lock on WinDivert64.sys/WinDivert.dll so a later
        // build can overwrite them. The WinDivert *service* itself is never
        // stopped or uninstalled here any more - that used to be how the
        // capture thread's read got unblocked (stopping the service
        // cancelled the pending call as a side effect), but repeated
        // start/stop cycles left it in a corrupted state (disabled, marked
        // for deletion, stuck stop-pending). It's left alone now, same as
        // HebnixTailscale is deliberately left running.
        let _ = self.stop_sender.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }

    pub fn stop(&mut self) -> Result<(), String> {
        self.suspend();
        Ok(())
    }
}

fn parse_ipv4(address: &str) -> Result<[u8; 4], String> {
    address
        .parse::<std::net::Ipv4Addr>()
        .map(|value| value.octets())
        .map_err(|_| format!("invalid tailnet address: {address}"))
}

/// Rewrites the host's real LAN ip:port embedded in Rocket League's LAN
/// discovery beacon to point at `address` (a tailnet IP) instead, so a
/// guest that can't reach the host's actual LAN address gets pointed at one
/// it can. Operates on the raw UDP payload only -- no more IP/UDP header or
/// checksum work needed, since this is now sent via an ordinary
/// `UdpSocket::send_to` rather than spliced into a captured Ethernet frame.
pub(crate) fn rewrite_lan_beacon_payload(mut payload: Vec<u8>, address: [u8; 4]) -> Vec<u8> {
    let address_string = format!(
        "{}.{}.{}.{}",
        address[0], address[1], address[2], address[3]
    );
    if let Some((offset, source_len, replacement)) =
        find_unreal_lan_endpoint(&payload, &address_string)
    {
        payload.splice(offset..offset + source_len, replacement);
    } else {
        let _ = replace_binary_lan_endpoint(&mut payload, address)
            || replace_equal_length_ascii_endpoint(&mut payload, &address_string);
    }
    payload
}

fn find_unreal_lan_endpoint(
    payload: &[u8],
    replacement_ip: &str,
) -> Option<(usize, usize, Vec<u8>)> {
    for offset in 0..payload.len().saturating_sub(4) {
        let length = i32::from_le_bytes(payload[offset..offset + 4].try_into().ok()?);
        if (2..=64).contains(&length) {
            let length = length as usize;
            let end = offset + 4 + length;
            if end <= payload.len() && payload[end - 1] == 0 {
                if let Ok(value) = std::str::from_utf8(&payload[offset + 4..end - 1]) {
                    if is_lan_game_endpoint(value) {
                        return Some((
                            offset,
                            4 + length,
                            unreal_ansi_string(&format!("{replacement_ip}:{}", super::RL_LAN_PORT)),
                        ));
                    }
                }
            }
        }
        if (-64..=-2).contains(&length) {
            let chars = (-length) as usize;
            let end = offset + 4 + chars * 2;
            if end <= payload.len() && payload[end - 2..end] == [0, 0] {
                let values = payload[offset + 4..end - 2]
                    .chunks_exact(2)
                    .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
                    .collect::<Vec<_>>();
                if let Ok(value) = String::from_utf16(&values) {
                    if is_lan_game_endpoint(&value) {
                        return Some((
                            offset,
                            4 + chars * 2,
                            unreal_utf16_string(&format!("{replacement_ip}:{}", super::RL_LAN_PORT)),
                        ));
                    }
                }
            }
        }
    }
    None
}

fn replace_binary_lan_endpoint(payload: &mut [u8], replacement: [u8; 4]) -> bool {
    if payload.len() < 6 {
        return false;
    }
    let port = super::RL_LAN_PORT;
    for offset in 0..=payload.len() - 6 {
        let candidate = [
            payload[offset],
            payload[offset + 1],
            payload[offset + 2],
            payload[offset + 3],
        ];
        if candidate == replacement || !std::net::Ipv4Addr::from(candidate).is_private() {
            continue;
        }
        let next = [payload[offset + 4], payload[offset + 5]];
        if u16::from_be_bytes(next) == port || u16::from_le_bytes(next) == port {
            payload[offset..offset + 4].copy_from_slice(&replacement);
            return true;
        }
    }
    false
}

fn replace_equal_length_ascii_endpoint(payload: &mut [u8], replacement: &str) -> bool {
    let port_suffix = format!(":{}", super::RL_LAN_PORT);
    let replacement = format!("{replacement}{port_suffix}");
    let suffix_bytes = port_suffix.as_bytes();
    let suffix_len = suffix_bytes.len();
    if payload.len() < suffix_len {
        return false;
    }
    for end in suffix_len..=payload.len() {
        if payload[end - suffix_len..end] != *suffix_bytes {
            continue;
        }
        let mut start = end - suffix_len;
        while start > 0 && (payload[start - 1].is_ascii_digit() || payload[start - 1] == b'.') {
            start -= 1;
        }
        let value = std::str::from_utf8(&payload[start..end]).ok();
        if value.is_some_and(is_lan_game_endpoint) && end - start == replacement.len() {
            payload[start..end].copy_from_slice(replacement.as_bytes());
            return true;
        }
    }
    false
}

fn is_lan_game_endpoint(value: &str) -> bool {
    let Some((address, port)) = value.rsplit_once(':') else {
        return false;
    };
    port.parse::<u16>().is_ok_and(|port| port == super::RL_LAN_PORT)
        && address.parse::<std::net::Ipv4Addr>().is_ok()
}

fn unreal_ansi_string(value: &str) -> Vec<u8> {
    let mut bytes = ((value.len() + 1) as i32).to_le_bytes().to_vec();
    bytes.extend_from_slice(value.as_bytes());
    bytes.push(0);
    bytes
}

fn unreal_utf16_string(value: &str) -> Vec<u8> {
    let chars = value.encode_utf16().count() + 1;
    let mut bytes = (-(chars as i32)).to_le_bytes().to_vec();
    for character in value.encode_utf16().chain(std::iter::once(0)) {
        bytes.extend_from_slice(&character.to_le_bytes());
    }
    bytes
}

impl Drop for HostSession {
    fn drop(&mut self) {
        let _ = self.stop_sender.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::{find_unreal_lan_endpoint, replace_binary_lan_endpoint, replace_equal_length_ascii_endpoint, unreal_ansi_string};

    #[test]
    fn rewrites_the_physical_lan_endpoint_to_the_tailnet_host() {
        let payload = unreal_ansi_string("192.168.0.119:7777");
        let (_, _, replacement) = find_unreal_lan_endpoint(&payload, "100.64.0.1")
            .expect("the LAN endpoint should be found");
        assert_eq!(replacement, unreal_ansi_string("100.64.0.1:7777"));
    }

    #[test]
    fn rewrites_binary_and_equal_length_lan_endpoints() {
        let tailnet_octets = [100, 64, 0, 1];
        let mut binary = [172, 31, 64, 1, 0x1e, 0x61];
        assert!(replace_binary_lan_endpoint(&mut binary, tailnet_octets));
        assert_eq!(&binary[..4], &tailnet_octets);
        // same byte length as the source address -- this rewrite only fires
        // on an exact-length match, by design (see replace_equal_length_ascii_endpoint)
        let mut text = b"172.31.64.1:7777".to_vec();
        assert!(replace_equal_length_ascii_endpoint(&mut text, "100.64.77.1"));
        assert_eq!(text, b"100.64.77.1:7777");
    }
}
