mod direct_udp;
mod firewall;
mod guest;
mod hosting;
mod models;
mod room_api;
mod tap;

use std::time::Duration;

pub use direct_udp::{DirectGuest, DirectHost, TunnelStats, record_received_udp, record_sent_udp};
pub use firewall::{ensure_host_rule, ensure_join_rule_if_needed, ensure_rocket_league_lan_rule};
pub use guest::GuestSession;
pub use hosting::HostSession;
pub use models::{
    CreateRoomRequest, JoinRoomRequest, JoinedRoom, LeaveRoomRequest, MapDescriptor, Room,
    RoomCredentials, UpdatePlayerRequest,
};
pub use room_api::RoomClient;
pub use tap::{TapSession, configure_existing, ensure_adapter, is_configured};

pub const VPN_SUBNET: &str = "192.10.192";
pub const HOST_ADDRESS: &str = "192.10.192.1";
pub const HOST_ADDRESS_BYTES: [u8; 4] = [192, 10, 192, 1];
pub const FIRST_GUEST_ADDRESS: &str = "192.10.192.2";
pub const GUEST_ADDRESS_RANGE: &str = "192.10.192.2-192.10.192.8";
pub const PACKET_PUMP_INTERVAL: Duration = Duration::from_millis(2);
pub const SESSION_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(300);

pub fn guest_address(slot: u8) -> Result<String, String> {
    if (2..=8).contains(&slot) {
        Ok(format!("{VPN_SUBNET}.{slot}"))
    } else {
        Err("invalid Workshop LAN player slot".to_string())
    }
}

pub fn cleanup_system_state() -> Result<(), String> {
    let mut errors = Vec::new();
    if let Err(error) = firewall::remove_rules() {
        errors.push(error);
    }
    if let Err(error) = tap::clear_configuration() {
        errors.push(error);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}
