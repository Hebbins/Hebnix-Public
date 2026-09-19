//! NAT traversal for hosting Workshop LAN without manual router setup.
//!
//! The tunnel is plain UDP straight to the address the room API publishes, so
//! a host needs a public UDP port. This module tries, in order:
//!   1. UPnP (IGD) to ask the router to open the tunnel port.
//!   2. STUN, run on the tunnel socket itself, to learn the public
//!      `ip:port` the NAT assigned to that socket. That port is what gets
//!      published as the room port, and a keepalive holds the mapping open.
//! It also reports what it found (CGNAT, per-destination "symmetric" ports)
//! so the UI can tell the host why guests might not be able to connect.
//! It does not do true hole punching or relaying; those need the room API to
//! carry guest endpoints.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use rand::RngCore;

const STUN_MAGIC: u32 = 0x2112_A442;
const STUN_SERVERS: [&str; 2] = ["stun.l.google.com:19302", "stun.cloudflare.com:3478"];
const STUN_TIMEOUT: Duration = Duration::from_millis(1_500);
const STUN_RESEND_INTERVAL: Duration = Duration::from_millis(500);
const STUN_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);
const UPNP_SEARCH_TIMEOUT: Duration = Duration::from_secs(3);
const UPNP_LEASE_SECS: u32 = 3_600;
const UPNP_RENEW_INTERVAL: Duration = Duration::from_secs(1_800);
const UPNP_DESCRIPTION: &str = "Hebnix Workshop LAN";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reachability {
    /// the router opened the tunnel port for us
    UpnpOpened,
    /// STUN gave us a public port; works if the NAT accepts unsolicited inbound
    StunMapped,
    /// the router's WAN address is private/shared, so we are behind carrier NAT
    Cgnat,
    /// the NAT picks a different public port per destination
    SymmetricNat,
    /// nothing worked; the host must forward the port manually
    Unknown,
}

#[derive(Clone, Debug)]
pub struct HostReachability {
    pub kind: Reachability,
    /// port to publish in the room; equals the local port unless a mapping
    /// changed it
    pub public_port: u16,
}

impl HostReachability {
    pub fn summary(&self) -> String {
        match self.kind {
            Reachability::UpnpOpened => {
                format!("UDP port {} opened on your router via UPnP.", self.public_port)
            }
            Reachability::StunMapped => format!(
                "Public UDP port {} found via STUN. Guests can connect if your NAT allows \
                 inbound traffic; otherwise forward this port on your router.",
                self.public_port
            ),
            Reachability::Cgnat => "Your router has a private/shared WAN address (CGNAT). \
                Guests will likely be unable to connect to you."
                .to_string(),
            Reachability::SymmetricNat => "Your network assigns a different public port per \
                destination. Guests will likely be unable to connect to you."
                .to_string(),
            Reachability::Unknown => format!(
                "Could not open a public port automatically. Forward UDP port {} on your \
                 router if guests cannot connect.",
                self.public_port
            ),
        }
    }
}

/// Keeps the NAT mapping (and UPnP lease) alive for as long as it is held.
/// Dropping it removes any UPnP mapping it created.
pub struct NatKeepalive {
    upnp: Option<UpnpMapping>,
    stun_target: Option<SocketAddr>,
    next_stun: Instant,
    next_renew: Instant,
}

impl NatKeepalive {
    /// Call regularly from the packet pump; does nothing until a timer is due.
    pub fn tick(&mut self, socket: &UdpSocket) {
        let now = Instant::now();
        if now >= self.next_stun {
            if let Some(target) = self.stun_target {
                let _ = socket.send_to(&binding_request(random_transaction()), target);
            }
            self.next_stun = now + STUN_KEEPALIVE_INTERVAL;
        }
        if now >= self.next_renew {
            if let Some(mapping) = &self.upnp {
                if let Err(error) = mapping.renew() {
                    tracing::warn!("workshop lan: UPnP lease renewal failed: {error}");
                }
            }
            self.next_renew = now + UPNP_RENEW_INTERVAL;
        }
    }
}

/// Works out how guests can reach `socket` (bound to `local_port`) and returns
/// the port to publish plus a keepalive to hold the mapping open.
///
/// `socket` must be non-blocking; this may block for a few seconds while the
/// router and STUN servers answer.
pub fn establish(socket: &UdpSocket, local_port: u16) -> (HostReachability, NatKeepalive) {
    let stun_target = STUN_SERVERS.iter().find_map(|server| resolve_v4(server));
    let now = Instant::now();
    let keepalive = |upnp| NatKeepalive {
        upnp,
        stun_target,
        next_stun: now + STUN_KEEPALIVE_INTERVAL,
        next_renew: now + UPNP_RENEW_INTERVAL,
    };

    let mut cgnat = false;
    match UpnpMapping::open(local_port) {
        UpnpOutcome::Mapped(mapping) => {
            let public_port = mapping.external_port;
            tracing::info!("workshop lan: UPnP mapped UDP {public_port}");
            let report = HostReachability {
                kind: Reachability::UpnpOpened,
                public_port,
            };
            return (report, keepalive(Some(mapping)));
        }
        UpnpOutcome::Cgnat(ip) => {
            tracing::info!("workshop lan: router WAN address {ip} is not public");
            cgnat = true;
        }
        UpnpOutcome::Unavailable(reason) => {
            tracing::info!("workshop lan: UPnP unavailable: {reason}");
        }
    }

    let mapped = stun_mapped_endpoints(socket);
    tracing::info!("workshop lan: STUN mapped endpoints: {mapped:?}");
    let (kind, public_port) = classify_stun(&mapped, cgnat, local_port);
    (
        HostReachability { kind, public_port },
        keepalive(None),
    )
}

fn classify_stun(mapped: &[SocketAddr], cgnat: bool, local_port: u16) -> (Reachability, u16) {
    let Some(first) = mapped.first() else {
        let kind = if cgnat {
            Reachability::Cgnat
        } else {
            Reachability::Unknown
        };
        return (kind, local_port);
    };
    let port = first.port();
    if mapped.iter().any(|other| other.port() != port) {
        return (Reachability::SymmetricNat, port);
    }
    if cgnat {
        (Reachability::Cgnat, port)
    } else {
        (Reachability::StunMapped, port)
    }
}

// ---- STUN (RFC 5389 binding request, IPv4 only) ----

fn random_transaction() -> [u8; 12] {
    let mut id = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut id);
    id
}

fn binding_request(transaction: [u8; 12]) -> [u8; 20] {
    let mut packet = [0u8; 20];
    packet[0..2].copy_from_slice(&0x0001u16.to_be_bytes());
    // bytes 2..4: message length 0
    packet[4..8].copy_from_slice(&STUN_MAGIC.to_be_bytes());
    packet[8..20].copy_from_slice(&transaction);
    packet
}

fn parse_binding_response(bytes: &[u8], transaction: &[u8; 12]) -> Option<SocketAddr> {
    if bytes.len() < 20
        || bytes[0..2] != 0x0101u16.to_be_bytes()
        || bytes[4..8] != STUN_MAGIC.to_be_bytes()
        || bytes[8..20] != transaction[..]
    {
        return None;
    }
    let body_len = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
    let body = bytes.get(20..20 + body_len)?;
    let mut plain = None;
    let mut offset = 0;
    while offset + 4 <= body.len() {
        let kind = u16::from_be_bytes([body[offset], body[offset + 1]]);
        let len = u16::from_be_bytes([body[offset + 2], body[offset + 3]]) as usize;
        let value = body.get(offset + 4..offset + 4 + len)?;
        match kind {
            // XOR-MAPPED-ADDRESS
            0x0020 if len >= 8 && value[1] == 0x01 => {
                let port = u16::from_be_bytes([value[2], value[3]]) ^ (STUN_MAGIC >> 16) as u16;
                let ip = u32::from_be_bytes([value[4], value[5], value[6], value[7]]) ^ STUN_MAGIC;
                return Some(SocketAddr::from((Ipv4Addr::from(ip), port)));
            }
            // MAPPED-ADDRESS, used only if no XOR variant is present
            0x0001 if len >= 8 && value[1] == 0x01 => {
                let port = u16::from_be_bytes([value[2], value[3]]);
                let ip = Ipv4Addr::new(value[4], value[5], value[6], value[7]);
                plain = Some(SocketAddr::from((ip, port)));
            }
            _ => {}
        }
        offset += 4 + len.div_ceil(4) * 4;
    }
    plain
}

fn resolve_v4(server: &str) -> Option<SocketAddr> {
    server.to_socket_addrs().ok()?.find(SocketAddr::is_ipv4)
}

/// Sends binding requests from `socket` to `server` until a matching response
/// arrives or the timeout expires. The socket must be non-blocking.
fn stun_query(socket: &UdpSocket, server: SocketAddr) -> Option<SocketAddr> {
    let transaction = random_transaction();
    let request = binding_request(transaction);
    let deadline = Instant::now() + STUN_TIMEOUT;
    let mut next_send = Instant::now();
    let mut buffer = [0u8; 512];
    while Instant::now() < deadline {
        if Instant::now() >= next_send {
            let _ = socket.send_to(&request, server);
            next_send = Instant::now() + STUN_RESEND_INTERVAL;
        }
        match socket.recv_from(&mut buffer) {
            Ok((len, from)) if from.ip() == server.ip() => {
                if let Some(mapped) = parse_binding_response(&buffer[..len], &transaction) {
                    return Some(mapped);
                }
            }
            Ok(_) => {}
            // Windows reports a previous send's ICMP unreachable as a
            // ConnectionReset on the next recv; it is not fatal for the socket
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::ConnectionReset
                ) =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
    None
}

/// Public endpoint of `socket` as seen by each STUN server that answered.
pub fn stun_mapped_endpoints(socket: &UdpSocket) -> Vec<SocketAddr> {
    STUN_SERVERS
        .iter()
        .filter_map(|server| resolve_v4(server))
        .filter_map(|server| stun_query(socket, server))
        .collect()
}

// ---- UPnP ----

enum UpnpOutcome {
    Mapped(UpnpMapping),
    /// the router answered but its WAN address is not publicly routable
    Cgnat(Ipv4Addr),
    Unavailable(String),
}

struct UpnpMapping {
    gateway: igd_next::Gateway,
    external_port: u16,
    local: SocketAddr,
}

impl UpnpMapping {
    fn open(local_port: u16) -> UpnpOutcome {
        let gateway = match igd_next::search_gateway(igd_next::SearchOptions {
            timeout: Some(UPNP_SEARCH_TIMEOUT),
            ..Default::default()
        }) {
            Ok(gateway) => gateway,
            Err(error) => return UpnpOutcome::Unavailable(error.to_string()),
        };
        if let Ok(IpAddr::V4(external)) = gateway.get_external_ip() {
            if !is_publicly_routable(external) {
                return UpnpOutcome::Cgnat(external);
            }
        }
        let Some(local_ip) = local_ip_towards(gateway.addr) else {
            return UpnpOutcome::Unavailable("could not determine local address".to_string());
        };
        let local = SocketAddr::V4(SocketAddrV4::new(local_ip, local_port));
        let external_port = match gateway.add_port(
            igd_next::PortMappingProtocol::UDP,
            local_port,
            local,
            UPNP_LEASE_SECS,
            UPNP_DESCRIPTION,
        ) {
            Ok(()) => local_port,
            Err(_) => match gateway.add_any_port(
                igd_next::PortMappingProtocol::UDP,
                local,
                UPNP_LEASE_SECS,
                UPNP_DESCRIPTION,
            ) {
                Ok(port) => port,
                Err(error) => return UpnpOutcome::Unavailable(error.to_string()),
            },
        };
        UpnpOutcome::Mapped(Self {
            gateway,
            external_port,
            local,
        })
    }

    fn renew(&self) -> Result<(), String> {
        self.gateway
            .add_port(
                igd_next::PortMappingProtocol::UDP,
                self.external_port,
                self.local,
                UPNP_LEASE_SECS,
                UPNP_DESCRIPTION,
            )
            .map_err(|error| error.to_string())
    }
}

impl Drop for UpnpMapping {
    fn drop(&mut self) {
        let _ = self
            .gateway
            .remove_port(igd_next::PortMappingProtocol::UDP, self.external_port);
    }
}

/// The local address the OS would use to reach `gateway`, found by connecting
/// a throwaway UDP socket (no packets are sent).
fn local_ip_towards(gateway: SocketAddr) -> Option<Ipv4Addr> {
    let probe = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    probe.connect(gateway).ok()?;
    match probe.local_addr().ok()?.ip() {
        IpAddr::V4(ip) => Some(ip),
        IpAddr::V6(_) => None,
    }
}

fn is_publicly_routable(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    let shared = octets[0] == 100 && (64..=127).contains(&octets[1]);
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || shared)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(transaction: [u8; 12], attributes: &[u8]) -> Vec<u8> {
        let mut packet = Vec::new();
        packet.extend_from_slice(&0x0101u16.to_be_bytes());
        packet.extend_from_slice(&(attributes.len() as u16).to_be_bytes());
        packet.extend_from_slice(&STUN_MAGIC.to_be_bytes());
        packet.extend_from_slice(&transaction);
        packet.extend_from_slice(attributes);
        packet
    }

    fn xor_mapped(ip: Ipv4Addr, port: u16) -> Vec<u8> {
        let mut attribute = Vec::new();
        attribute.extend_from_slice(&0x0020u16.to_be_bytes());
        attribute.extend_from_slice(&8u16.to_be_bytes());
        attribute.extend_from_slice(&[0, 1]);
        attribute.extend_from_slice(&(port ^ (STUN_MAGIC >> 16) as u16).to_be_bytes());
        attribute.extend_from_slice(&(u32::from(ip) ^ STUN_MAGIC).to_be_bytes());
        attribute
    }

    #[test]
    fn binding_request_has_stun_header() {
        let transaction = [7u8; 12];
        let packet = binding_request(transaction);
        assert_eq!(packet[0..2], [0x00, 0x01]);
        assert_eq!(packet[2..4], [0x00, 0x00]);
        assert_eq!(packet[4..8], [0x21, 0x12, 0xA4, 0x42]);
        assert_eq!(packet[8..20], transaction);
    }

    #[test]
    fn parses_xor_mapped_address() {
        let transaction = [9u8; 12];
        let bytes = response(transaction, &xor_mapped(Ipv4Addr::new(203, 0, 113, 5), 40_123));
        assert_eq!(
            parse_binding_response(&bytes, &transaction),
            Some("203.0.113.5:40123".parse().unwrap())
        );
    }

    #[test]
    fn skips_unknown_attributes_and_padding() {
        let transaction = [3u8; 12];
        let mut attributes = Vec::new();
        // SOFTWARE attribute, 5 bytes of value padded to 8
        attributes.extend_from_slice(&0x8022u16.to_be_bytes());
        attributes.extend_from_slice(&5u16.to_be_bytes());
        attributes.extend_from_slice(b"hello\0\0\0");
        attributes.extend_from_slice(&xor_mapped(Ipv4Addr::new(198, 51, 100, 7), 1234));
        let bytes = response(transaction, &attributes);
        assert_eq!(
            parse_binding_response(&bytes, &transaction),
            Some("198.51.100.7:1234".parse().unwrap())
        );
    }

    #[test]
    fn falls_back_to_plain_mapped_address() {
        let transaction = [4u8; 12];
        let mut attribute = Vec::new();
        attribute.extend_from_slice(&0x0001u16.to_be_bytes());
        attribute.extend_from_slice(&8u16.to_be_bytes());
        attribute.extend_from_slice(&[0, 1]);
        attribute.extend_from_slice(&5555u16.to_be_bytes());
        attribute.extend_from_slice(&[192, 0, 2, 9]);
        let bytes = response(transaction, &attribute);
        assert_eq!(
            parse_binding_response(&bytes, &transaction),
            Some("192.0.2.9:5555".parse().unwrap())
        );
    }

    #[test]
    fn rejects_wrong_transaction_or_truncated_input() {
        let transaction = [1u8; 12];
        let bytes = response(transaction, &xor_mapped(Ipv4Addr::new(203, 0, 113, 5), 1));
        assert_eq!(parse_binding_response(&bytes, &[2u8; 12]), None);
        assert_eq!(parse_binding_response(&bytes[..10], &transaction), None);
        let mut truncated = bytes.clone();
        truncated.truncate(24);
        assert_eq!(parse_binding_response(&truncated, &transaction), None);
    }

    #[test]
    fn classifies_private_and_shared_addresses() {
        assert!(!is_publicly_routable(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(!is_publicly_routable(Ipv4Addr::new(192, 168, 1, 1)));
        assert!(!is_publicly_routable(Ipv4Addr::new(172, 16, 5, 5)));
        assert!(!is_publicly_routable(Ipv4Addr::new(100, 64, 0, 1)));
        assert!(!is_publicly_routable(Ipv4Addr::new(100, 127, 255, 254)));
        assert!(is_publicly_routable(Ipv4Addr::new(100, 128, 0, 1)));
        assert!(is_publicly_routable(Ipv4Addr::new(8, 8, 8, 8)));
    }

    #[test]
    fn classifies_stun_results() {
        let a: SocketAddr = "203.0.113.5:40000".parse().unwrap();
        let b: SocketAddr = "203.0.113.5:40001".parse().unwrap();
        assert_eq!(classify_stun(&[], false, 7000), (Reachability::Unknown, 7000));
        assert_eq!(classify_stun(&[], true, 7000), (Reachability::Cgnat, 7000));
        assert_eq!(classify_stun(&[a, a], false, 7000), (Reachability::StunMapped, 40000));
        assert_eq!(classify_stun(&[a, b], false, 7000), (Reachability::SymmetricNat, 40000));
        assert_eq!(classify_stun(&[a], true, 7000), (Reachability::Cgnat, 40000));
    }
}
