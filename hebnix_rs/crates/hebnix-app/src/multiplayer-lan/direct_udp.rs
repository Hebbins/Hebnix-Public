// Session-level connection stats for the UI's throughput/status display.
// Previously this file also carried a hand-rolled HMAC-framed, fragmented
// UDP tunnel protocol (DirectHost/DirectGuest) that relayed raw Ethernet
// frames captured off a TAP adapter. None of that exists anymore: RL's game
// traffic now flows directly over the tsnet-provided network interface, so
// there's nothing for Hebnix to relay or frame at that layer -- the only
// thing left to track here is what the beacon relay (see beacon.rs) is
// doing, for the UI panel.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64};

#[derive(Default)]
pub struct TunnelStats {
    /// at least one peer's tailnet address is known and the session is
    /// considered live (host: a guest reported in; guest: joined and the
    /// tailnet is up)
    pub connected: AtomicBool,
    /// the sidecar could not bring the tailnet up, or the room could not be
    /// reached, within a reasonable time
    pub join_failed: AtomicBool,
    pub sent: AtomicU64,
    pub received: AtomicU64,
    pub last_beacon_relayed: Mutex<String>,
}
