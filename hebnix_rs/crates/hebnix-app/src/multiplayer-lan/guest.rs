use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use super::hosting::rewrite_lan_beacon_endpoint;
use super::tap::{arp_reply_for_local, mac_address};
use super::{
    DirectGuest, HOST_ADDRESS_BYTES, JoinRoomRequest, JoinedRoom, PACKET_PUMP_INTERVAL, RoomClient,
    SESSION_HEARTBEAT_INTERVAL, TapSession, TunnelStats, record_received_udp, record_sent_udp,
};

pub struct GuestSession {
    pub joined: JoinedRoom,
    pub stats: Arc<TunnelStats>,
    stop: Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for GuestSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GuestSession")
            .finish_non_exhaustive()
    }
}

impl GuestSession {
    pub fn start(
        joined: JoinedRoom,
        identity: JoinRoomRequest,
        tunnel: TapSession,
    ) -> Result<Self, String> {
        let room = &joined.room;
        let host: SocketAddr = format!("{}:{}", room.endpoint.host, room.endpoint.port)
            .parse()
            .map_err(|_| "invalid host endpoint".to_string())?;
        let local_mac = mac_address()?;
        let local_ip = joined.assigned_ip.clone();
        let mut udp = DirectGuest::new(
            host,
            room.pin.clone(),
            room.join_token.clone(),
            local_mac,
            local_ip.clone(),
        )?;
        udp.begin()?;
        let stats = udp.stats();
        let worker_stats = stats.clone();
        let (stop, rx) = mpsc::channel();
        let heartbeat_pin = room.pin.clone();
        let worker = thread::spawn(move || {
            let client = RoomClient::new("https://api.hebnix.com");
            let mut next_heartbeat = std::time::Instant::now() + SESSION_HEARTBEAT_INTERVAL;
            loop {
                if rx.try_recv().is_ok() {
                    break;
                }
                if let Some(packet) = tunnel.try_receive() {
                    record_sent_udp(&worker_stats, &packet);
                    let _ = udp.send_packet(&packet);
                }
                if let Ok(Some(packet)) = udp.poll() {
                    let packet = rewrite_lan_beacon_endpoint(packet, HOST_ADDRESS_BYTES);
                    record_received_udp(&worker_stats, &packet);
                    match arp_reply_for_local(&packet, &local_ip) {
                        Ok(Some(reply)) => {
                            let _ = udp.send_packet(&reply);
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
                    let _ = client.join_room(&heartbeat_pin, &identity);
                    next_heartbeat += SESSION_HEARTBEAT_INTERVAL;
                }
                thread::sleep(PACKET_PUMP_INTERVAL);
            }
        });
        Ok(Self {
            joined,
            stats,
            stop,
            worker: Some(worker),
        })
    }
    pub fn stop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }

    pub fn leave(&mut self) -> Result<(), String> {
        self.stop();
        RoomClient::new("https://api.hebnix.com")
            .leave_room(&self.joined.room.pin, &self.joined.leave_token)
    }
}
impl Drop for GuestSession {
    fn drop(&mut self) {
        self.stop();
    }
}
