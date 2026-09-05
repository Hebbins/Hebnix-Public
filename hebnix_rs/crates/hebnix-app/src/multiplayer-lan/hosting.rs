use super::tap::{arp_announcement, arp_reply_for_local, mac_address};
use super::{
    CreateRoomRequest, DirectHost, HOST_ADDRESS_BYTES, PACKET_PUMP_INTERVAL, RoomClient,
    RoomCredentials, SESSION_HEARTBEAT_INTERVAL, TapSession, TunnelStats, record_received_udp,
    record_sent_udp,
};
use std::sync::{
    Arc,
    mpsc::{self, Sender},
};
use std::thread::{self, JoinHandle};
pub struct HostSession {
    pub credentials: RoomCredentials,
    pub stats: Arc<TunnelStats>,
    client: RoomClient,
    stop_sender: Sender<()>,
    worker: Option<JoinHandle<()>>,
}
impl std::fmt::Debug for HostSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostSession")
            .finish_non_exhaustive()
    }
}
impl HostSession {
    pub fn start(
        client: RoomClient,
        request: CreateRoomRequest,
        tunnel: TapSession,
    ) -> Result<Self, String> {
        let tunnel_port = request.port;
        let tunnel_pin = format!("pending-{tunnel_port}");
        let udp_tunnel = DirectHost::bind(tunnel_port, tunnel_pin, "")?;
        let (room, credentials) = client.create_room(&request)?;
        let local_mac = mac_address()?;
        let (stop_sender, stop_receiver) = mpsc::channel();
        let refresh_client = client.clone();
        let pin = credentials.pin.clone();
        let host_secret = credentials.host_secret.clone();
        let join_token = room.join_token.clone();
        let stats = udp_tunnel.stats();
        let worker_stats = stats.clone();
        let worker = thread::spawn(move || {
            let mut udp = udp_tunnel.with_credentials(pin.clone(), join_token, local_mac);
            if let Ok(announcement) = arp_announcement(super::HOST_ADDRESS) {
                let _ = udp.relay(&announcement);
            }
            let mut next_heartbeat = std::time::Instant::now() + SESSION_HEARTBEAT_INTERVAL;
            loop {
                if stop_receiver.try_recv().is_ok() {
                    break;
                }
                if let Some(packet) = tunnel.try_receive() {
                    let packet = rewrite_lan_beacon_endpoint(packet, HOST_ADDRESS_BYTES);
                    record_sent_udp(&worker_stats, &packet);
                    let _ = udp.relay(&packet);
                }
                if let Ok(Some(packet)) = udp.poll() {
                    record_received_udp(&worker_stats, &packet);
                    match arp_reply_for_local(&packet, super::HOST_ADDRESS) {
                        Ok(Some(reply)) => {
                            let _ = udp.relay(&reply);
                        }
                        _ => {
                            if tunnel.send(&packet).is_ok() {
                                worker_stats
                                    .delivered
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    }
                }
                if std::time::Instant::now() >= next_heartbeat {
                    let _ = refresh_client.heartbeat(&pin, &host_secret);
                    next_heartbeat += SESSION_HEARTBEAT_INTERVAL;
                }
                thread::sleep(PACKET_PUMP_INTERVAL);
            }
        });
        Ok(Self {
            credentials,
            stats,
            client,
            stop_sender,
            worker: Some(worker),
        })
    }
    pub fn suspend(&mut self) {
        let _ = self.stop_sender.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
    pub fn stop(&mut self) -> Result<(), String> {
        self.suspend();
        self.client
            .close_room(&self.credentials.pin, &self.credentials.host_secret)
    }
}
pub(crate) fn rewrite_lan_beacon_endpoint(mut frame: Vec<u8>, address: [u8; 4]) -> Vec<u8> {
    if frame.len() < 42 || frame[12..14] != [0x08, 0x00] || frame[23] != 17 {
        return frame;
    }
    let ip_start = 14;
    let ip_header_len = usize::from(frame[ip_start] & 0x0f) * 4;
    let udp_start = ip_start + ip_header_len;
    if ip_header_len < 20 || frame.len() < udp_start + 8 {
        return frame;
    }
    let udp_len = usize::from(u16::from_be_bytes([
        frame[udp_start + 4],
        frame[udp_start + 5],
    ]));
    if udp_len < 8 || frame.len() < udp_start + udp_len {
        return frame;
    }
    let payload_start = udp_start + 8;
    let payload_end = udp_start + udp_len;
    let address_bytes = address;
    let address = format!(
        "{}.{}.{}.{}",
        address[0], address[1], address[2], address[3]
    );
    let delta = if let Some((offset, source_len, replacement)) =
        find_unreal_lan_endpoint(&frame[payload_start..payload_end], &address)
    {
        let start = payload_start + offset;
        let delta = replacement.len() as isize - source_len as isize;
        frame.splice(start..start + source_len, replacement);
        delta
    } else if replace_binary_lan_endpoint(&mut frame[payload_start..payload_end], address_bytes)
        || replace_equal_length_ascii_endpoint(&mut frame[payload_start..payload_end], &address)
    {
        0
    } else {
        return frame;
    };
    let old_ip_len = usize::from(u16::from_be_bytes([frame[16], frame[17]]));
    let Some(new_ip_len) = old_ip_len.checked_add_signed(delta) else {
        return frame;
    };
    let Some(new_udp_len) = udp_len.checked_add_signed(delta) else {
        return frame;
    };
    if new_ip_len > usize::from(u16::MAX) || new_udp_len > usize::from(u16::MAX) {
        return frame;
    }
    frame[16..18].copy_from_slice(&(new_ip_len as u16).to_be_bytes());
    frame[udp_start + 4..udp_start + 6].copy_from_slice(&(new_udp_len as u16).to_be_bytes());
    frame[24..26].fill(0);
    let ip_checksum = checksum(&frame[ip_start..ip_start + ip_header_len]);
    frame[24..26].copy_from_slice(&ip_checksum.to_be_bytes());
    frame[udp_start + 6..udp_start + 8].fill(0);
    let udp_checksum = udp_checksum(&frame, udp_start, new_udp_len);
    frame[udp_start + 6..udp_start + 8].copy_from_slice(&udp_checksum.to_be_bytes());
    frame
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
                            unreal_ansi_string(&format!("{replacement_ip}:7777")),
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
                            unreal_utf16_string(&format!("{replacement_ip}:7777")),
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
        if u16::from_be_bytes(next) == 7777 || u16::from_le_bytes(next) == 7777 {
            payload[offset..offset + 4].copy_from_slice(&replacement);
            return true;
        }
    }
    false
}
fn replace_equal_length_ascii_endpoint(payload: &mut [u8], replacement: &str) -> bool {
    let replacement = format!("{replacement}:7777");
    for end in 5..=payload.len() {
        if payload[end - 5..end] != *b":7777" {
            continue;
        }
        let mut start = end - 5;
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
    port == "7777" && address.parse::<std::net::Ipv4Addr>().is_ok()
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
fn checksum(bytes: &[u8]) -> u16 {
    let mut sum = 0u32;
    for chunk in bytes.chunks(2) {
        let word = if chunk.len() == 2 {
            u16::from_be_bytes([chunk[0], chunk[1]])
        } else {
            u16::from(chunk[0]) << 8
        };
        sum += u32::from(word);
        while sum > 0xffff {
            sum = (sum & 0xffff) + (sum >> 16);
        }
    }
    !(sum as u16)
}
fn udp_checksum(frame: &[u8], udp_start: usize, udp_len: usize) -> u16 {
    let mut bytes = Vec::with_capacity(12 + udp_len);
    bytes.extend_from_slice(&frame[26..34]);
    bytes.push(0);
    bytes.push(17);
    bytes.extend_from_slice(&(udp_len as u16).to_be_bytes());
    bytes.extend_from_slice(&frame[udp_start..udp_start + udp_len]);
    let value = checksum(&bytes);
    if value == 0 { 0xffff } else { value }
}
impl Drop for HostSession {
    fn drop(&mut self) {
        let _ = self.stop_sender.send(());
    }
}
#[cfg(test)]
mod tests {
    use super::{
        HOST_ADDRESS_BYTES, find_unreal_lan_endpoint, replace_binary_lan_endpoint,
        replace_equal_length_ascii_endpoint, unreal_ansi_string,
    };
    #[test]
    fn rewrites_the_physical_lan_endpoint_to_the_tap_host() {
        let payload = unreal_ansi_string("192.168.0.119:7777");
        let (_, _, replacement) = find_unreal_lan_endpoint(&payload, "192.10.192.1")
            .expect("the LAN endpoint should be found");
        assert_eq!(replacement, unreal_ansi_string("192.10.192.1:7777"));
    }
    #[test]
    fn rewrites_binary_and_equal_length_lan_endpoints() {
        let mut binary = [172, 31, 64, 1, 0x1e, 0x61];
        assert!(replace_binary_lan_endpoint(&mut binary, HOST_ADDRESS_BYTES));
        assert_eq!(&binary[..4], &HOST_ADDRESS_BYTES);
        let mut text = b"172.31.64.1:7777".to_vec();
        assert!(replace_equal_length_ascii_endpoint(
            &mut text,
            "192.10.192.1"
        ));
        assert_eq!(text, b"192.10.192.1:7777");
    }
}
