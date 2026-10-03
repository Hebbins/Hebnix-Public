// Captures and relays Rocket League's LAN discovery beacon now that there's
// no TAP adapter to read raw Ethernet frames off of. Actual game traffic no
// longer goes through Hebnix at all -- once RL is bound to a tailnet address
// via -multihome, its own UDP sockets talk directly to the peer over the
// WireGuard tunnel tsnet provides. The only thing Hebnix still needs to do
// is make sure a guest's RL process learns the host's *tailnet* address in
// the first place, since RL's own beacon broadcasts the host's real LAN
// ip:port, which guests can't reach.
//
// CONFIRMED (packet capture, 2026-09-28): RL really does send a global UDP
// broadcast from its own tailnet-bound socket - `10.242.77.1.14001 >
// 255.255.255.255.14001`, port RL_DISCOVERY_PORTS below. The problem was
// never the port, the subnet, or the firewall (all confirmed working
// separately). It's that a second socket sharing that port with
// SO_REUSEADDR never actually got a copy of it -- Windows only guarantees
// SO_REUSEADDR lets multiple sockets *bind* to the same address without
// erroring, not that every bound socket receives every incoming datagram.
// In practice only one socket (RL's own) was ever getting delivery.
//
// First attempt at fixing this used Npcap to capture raw packets below the
// socket layer entirely -- but Npcap hooks the NDIS driver stack, and
// Tailscale's Wintun adapter doesn't register as a capturable NDIS device
// at all (confirmed: it's simply absent from Npcap's own device list).
// WinDivert works differently -- it intercepts at the Windows Filtering
// Platform (IP layer), which doesn't care what kind of adapter the traffic
// is on, so it sees this fine. `sniff` mode means it copies the packet
// rather than diverting/blocking it, so RL's own processing of its own
// broadcast is completely undisturbed; this is purely an observer sitting
// alongside it. Sending the rewritten copy back out still uses ordinary
// sockets -- that direction was always fine, confirmed working
// host-to-guest over the tunnel independently of this whole issue.

use std::ffi::{CString, c_void};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use libloading::Library;
use socket2::{Domain, Socket, Type};
use windows::Win32::Foundation::HANDLE;
use windows::core::BOOL;

const WINDIVERT_LAYER_NETWORK: u32 = 0;
const WINDIVERT_FLAG_SNIFF: u64 = 0x0001;
const WINDIVERT_FLAG_RECV_ONLY: u64 = 0x0004;
const WINDIVERT_SHUTDOWN_BOTH: u32 = 3;

pub struct BeaconRelay {
    // a single socket, bound to an OS-assigned ephemeral port, used only for
    // sending the relayed/rewritten copy back out - receiving happens via
    // the WinDivert capture thread below instead (see the module comment
    // for why). Deliberately NOT bound to any of the discovery ports
    // themselves any more: confirmed live that binding here to the same
    // port RL's own socket listens on (which this used to do, one socket
    // per discovery port, so the relayed packet's source port would match)
    // created exactly the same SO_REUSEADDR delivery-ambiguity problem this
    // module's own capture side already had to work around with WinDivert -
    // except this time on the *receiving* end, silently swallowing
    // delivery of our own correctly-sent packets before RL's socket ever
    // saw them. The actual join target RL uses is the ip:port rewritten
    // *inside the payload* (see hosting.rs's rewrite_lan_beacon_payload),
    // not the transport-level source port, so there's no need for our
    // outbound packet's source port to match the discovery port at all -
    // an ephemeral one works and stops competing with RL for that port.
    socket: std::net::UdpSocket,
    captured: Receiver<(Vec<u8>, SocketAddr, u16)>,
    capture: Arc<RawCapture>,
}

/// Thin, self-owned wrapper around the raw WinDivertOpen/Recv/Shutdown/Close
/// calls -- deliberately NOT using the `windivert` crate's own `WinDivert`
/// type for this handle. That wrapper requires `&mut self` to call
/// `shutdown()`/`close()`, which makes it impossible to cancel a `recv()`
/// that's blocked forever on another thread without fighting the borrow
/// checker. The underlying WinDivert C API is explicit that
/// WinDivertShutdown() is safe to call on the SAME handle from a different
/// thread than the one blocked in WinDivertRecv() - that's its documented
/// purpose. Going around the wrapper crate to call these directly lets the
/// capture thread be shut down cleanly and directly, instead of the old
/// approach of stopping/uninstalling the whole WinDivert *service* to force
/// the blocked read to unblock as a side effect - which left the service in
/// a genuinely corrupted state (disabled, marked for deletion, stuck
/// stop-pending) after repeated start/stop cycles. The service itself is
/// now never touched at shutdown at all.
struct RawCapture {
    handle: HANDLE,
    // Keep the DLL mapped for every call and until after WinDivertClose().
    _library: Library,
    recv: unsafe extern "C" fn(HANDLE, *mut c_void, u32, *mut u32, *mut c_void) -> BOOL,
    shutdown: unsafe extern "C" fn(HANDLE, u32) -> BOOL,
    close: unsafe extern "C" fn(HANDLE) -> BOOL,
}

// HANDLE is just a wrapper around an isize - safe to share and use
// concurrently from multiple threads, which is exactly what WinDivert's own
// API contract for WinDivertShutdown() requires.
unsafe impl Send for RawCapture {}
unsafe impl Sync for RawCapture {}

impl RawCapture {
    /// Loads WinDivert only once Hebnix is elevated and the runtime bundle has
    /// been extracted to its AppData folder. This avoids an eager process-load
    /// dependency on a DLL/driver that may not exist yet.
    fn open(filter: &str, flags: u64) -> Result<Self, String> {
        if !crate::spoofer::is_admin() {
            return Err("WinDivert capture requires Hebnix to run as administrator".to_string());
        }

        let base_dir = crate::config::base_dir();
        crate::multiplayer_assets::ensure_present(&base_dir)
            .map_err(|error| format!("could not extract multiplayer components: {error}"))?;
        let dll_path = base_dir.join("multiplayer-lan").join("WinDivert.dll");
        let library = unsafe { Library::new(&dll_path) }
            .map_err(|error| format!("could not load {}: {error}", dll_path.display()))?;

        // Function pointers are copied out while `library` remains owned by
        // RawCapture, so the DLL cannot unload before the final Close call.
        let open = unsafe {
            *library
                .get::<unsafe extern "C" fn(*const i8, u32, i16, u64) -> HANDLE>(b"WinDivertOpen\0")
                .map_err(|error| format!("could not find WinDivertOpen: {error}"))?
        };
        let recv = unsafe {
            *library
                .get::<unsafe extern "C" fn(HANDLE, *mut c_void, u32, *mut u32, *mut c_void) -> BOOL>(b"WinDivertRecv\0")
                .map_err(|error| format!("could not find WinDivertRecv: {error}"))?
        };
        let shutdown = unsafe {
            *library
                .get::<unsafe extern "C" fn(HANDLE, u32) -> BOOL>(b"WinDivertShutdown\0")
                .map_err(|error| format!("could not find WinDivertShutdown: {error}"))?
        };
        let close = unsafe {
            *library
                .get::<unsafe extern "C" fn(HANDLE) -> BOOL>(b"WinDivertClose\0")
                .map_err(|error| format!("could not find WinDivertClose: {error}"))?
        };

        let filter = CString::new(filter).map_err(|error| error.to_string())?;
        let handle = unsafe { open(filter.as_ptr(), WINDIVERT_LAYER_NETWORK, 0, flags) };
        if handle.is_invalid() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(Self {
            handle,
            _library: library,
            recv,
            shutdown,
            close,
        })
    }

    fn recv(&self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let mut recv_len: u32 = 0;
        let ok = unsafe {
            (self.recv)(
                self.handle,
                buffer.as_mut_ptr() as *mut _,
                buffer.len() as u32,
                &mut recv_len,
                std::ptr::null_mut(),
            )
        };
        if ok.as_bool() {
            Ok(recv_len as usize)
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// unblocks a `recv()` call in progress on another thread - see the
    /// struct doc comment for why this is safe to call concurrently
    fn shutdown(&self) {
        unsafe {
            let _ = (self.shutdown)(self.handle, WINDIVERT_SHUTDOWN_BOTH);
        }
    }
}

impl Drop for RawCapture {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.close)(self.handle);
        }
    }
}

impl BeaconRelay {
    /// `host_tailnet_ip` is this machine's own tailnet address - the send
    /// socket binds there specifically (on an ephemeral port - see the
    /// struct doc comment for why), and it's used to find the matching
    /// network adapter to capture packets from.
    pub fn bind(host_tailnet_ip: IpAddr) -> Result<Self, String> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, None)
            .map_err(|error| format!("could not create the beacon relay socket: {error}"))?;
        socket.set_broadcast(true).map_err(|error| {
            format!("could not enable broadcast on the beacon relay socket: {error}")
        })?;
        socket
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let address: SocketAddr = (host_tailnet_ip, 0).into();
        socket.bind(&address.into()).map_err(|error| {
            format!("could not bind the beacon relay socket to {host_tailnet_ip}: {error}")
        })?;

        let (tx, rx) = mpsc::channel();
        let capture = spawn_capture_thread(tx)?;

        Ok(Self {
            socket: socket.into(),
            captured: rx,
            capture,
        })
    }

    /// non-blocking - returns the next captured beacon packet, if any,
    /// along with the peer that sent it and which discovery port it was on
    pub fn try_receive(&self) -> Option<(Vec<u8>, SocketAddr, u16)> {
        self.captured.try_recv().ok()
    }

    /// cleanly unblocks and stops the capture thread. Doesn't touch the
    /// WinDivert *service* at all - just this handle - so it can't leave
    /// the service in a bad state the way stopping the service used to.
    pub fn stop_capture(&self) {
        self.capture.shutdown();
    }

    /// sends `payload` to `destination` (the peer's ip and whichever
    /// discovery port the original beacon was captured on)
    pub fn send_to(&self, payload: &[u8], destination: SocketAddr) -> Result<(), String> {
        self.socket
            .send_to(payload, destination)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

/// opens a WinDivert handle scoped to the discovery ports on the tailnet
/// address, in sniff mode (packets are copied, not diverted, so RL's own
/// handling of its own broadcast is completely undisturbed), and starts a
/// background thread parsing out the UDP payload and pushing it to `tx`.
fn spawn_capture_thread(tx: Sender<(Vec<u8>, SocketAddr, u16)>) -> Result<Arc<RawCapture>, String> {
    let port_filter = super::RL_DISCOVERY_PORTS
        .iter()
        .map(|port| format!("udp.DstPort == {port}"))
        .collect::<Vec<_>>()
        .join(" or ");
    // `outbound and ip.DstAddr == 255.255.255.255` - i.e. RL's own genuine
    // discovery broadcast, and ONLY that. Confirmed live this needed to be
    // this narrow: the previous filter matched on address
    // (`ip.SrcAddr == host_tailnet_ip or ip.DstAddr == host_tailnet_ip`),
    // which - being address-based, not direction-based - also matched this
    // relay's OWN outbound unicast sends to peers (source address is this
    // same host either way), so every relayed packet got immediately
    // recaptured as if it were a brand new beacon and relayed again,
    // forever, with no actual network round trip needed to sustain the
    // loop. `outbound` + broadcast destination captures exactly "RL is
    // announcing itself" and nothing this relay itself ever sends (which is
    // always unicast to a specific peer, never to the broadcast address).
    let filter = format!("outbound and udp and ({port_filter}) and ip.DstAddr == 255.255.255.255");

    let flags = WINDIVERT_FLAG_SNIFF | WINDIVERT_FLAG_RECV_ONLY;
    let capture = Arc::new(
        RawCapture::open(&filter, flags)
            .map_err(|error| format!("could not start the beacon capture (WinDivert): {error}"))?,
    );

    let thread_capture = capture.clone();
    std::thread::Builder::new()
        .name("beacon-capture".into())
        .spawn(move || {
            // plenty for a LAN discovery beacon - real game traffic never
            // goes through this capture at all, only the broadcast does
            let mut buffer = vec![0u8; 4096];
            loop {
                match thread_capture.recv(&mut buffer) {
                    Ok(len) => {
                        if let Some(parsed) = parse_udp_payload(&buffer[..len]) {
                            if tx.send(parsed).is_err() {
                                break; // BeaconRelay was dropped
                            }
                        }
                    }
                    // stop_capture()'s shutdown() call unblocks recv() with
                    // an error - that's the expected, clean exit path, not
                    // just a failure case
                    Err(_) => break,
                }
            }
        })
        .map_err(|error| format!("could not start the beacon capture thread: {error}"))?;

    Ok(capture)
}

/// pulls the UDP payload, source address, and destination port out of a raw
/// IP packet - WinDivert's network layer hands us the packet starting at
/// the IP header (no Ethernet header, it's already above that), so this is
/// the same job a normal socket's recv_from would otherwise do
fn parse_udp_payload(ip: &[u8]) -> Option<(Vec<u8>, SocketAddr, u16)> {
    const UDP_PROTOCOL: u8 = 17;

    if ip.len() < 20 {
        return None;
    }
    let version = ip[0] >> 4;
    if version != 4 {
        return None;
    }
    let header_len = (ip[0] & 0x0F) as usize * 4;
    if header_len < 20 || ip.len() < header_len + 8 {
        return None;
    }
    if ip[9] != UDP_PROTOCOL {
        return None;
    }
    let source_ip = Ipv4Addr::new(ip[12], ip[13], ip[14], ip[15]);

    let udp = &ip[header_len..];
    let source_port = u16::from_be_bytes([udp[0], udp[1]]);
    let destination_port = u16::from_be_bytes([udp[2], udp[3]]);
    let udp_length = u16::from_be_bytes([udp[4], udp[5]]) as usize;
    if udp_length < 8 || udp.len() < 8 {
        return None;
    }
    let payload_end = udp_length.min(udp.len());
    let payload = udp[8..payload_end].to_vec();

    Some((
        payload,
        SocketAddr::new(source_ip.into(), source_port),
        destination_port,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_udp_payload_out_of_a_raw_ip_packet() {
        let mut ip = vec![0u8; 20];
        ip[0] = 0x45; // version 4, header length 20
        ip[9] = 17; // UDP
        ip[12..16].copy_from_slice(&[10, 242, 77, 1]);
        let mut udp = vec![0u8; 8];
        udp[0..2].copy_from_slice(&14001u16.to_be_bytes());
        udp[2..4].copy_from_slice(&14001u16.to_be_bytes());
        let payload = b"hello beacon";
        udp[4..6].copy_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
        udp.extend_from_slice(payload);
        ip.extend_from_slice(&udp);

        let (parsed_payload, source, destination_port) =
            parse_udp_payload(&ip).expect("should parse a well-formed UDP packet");
        assert_eq!(parsed_payload, payload);
        assert_eq!(source, "10.242.77.1:14001".parse().unwrap());
        assert_eq!(destination_port, 14001);
    }

    #[test]
    fn ignores_non_udp_and_truncated_packets() {
        assert!(parse_udp_payload(&[0u8; 10]).is_none()); // too short for an IP header
        let mut non_udp = vec![0u8; 20];
        non_udp[0] = 0x45;
        non_udp[9] = 6; // TCP, not UDP
        assert!(parse_udp_payload(&non_udp).is_none());
    }
}
