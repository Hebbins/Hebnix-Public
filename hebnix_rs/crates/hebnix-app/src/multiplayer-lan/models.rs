use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HostEndpoint {
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MapDescriptor {
    pub id: String,
    pub name: String,
    pub sha256: String,
    pub download_url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreateRoomRequest {
    pub host_name: String,
    pub port: u16,
    pub map: MapDescriptor,
    pub protocol_version: u16,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomCredentials {
    pub pin: String,
    pub host_secret: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JoinRoomRequest {
    pub player_token: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpdatePlayerRequest {
    pub player_token: String,
    pub platform_id: String,
    pub platform: String,
    pub display_name: String,
    /// speculative: not yet accepted by the live backend. Once Harry adds
    /// tailnet-ip tracking to the room API, this tells the host where to
    /// unicast the rewritten LAN beacon for this player.
    pub tailnet_ip: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JoinedRoom {
    pub room: Room,
    /// historically a TAP-subnet slot address the server assigned; with
    /// tailnet IPs assigned by headscale per-device instead, this field is
    /// mostly vestigial now -- guests use their own sidecar's tailnet IP,
    /// not a server-assigned one. Kept for backward compatibility with the
    /// current live API shape until Harry updates it.
    #[serde(default)]
    pub assigned_ip: String,
    pub leave_token: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LeaveRoomRequest {
    pub leave_token: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct PlayerInfo {
    pub platform_id: String,
    pub display_name: String,
    /// speculative: see UpdatePlayerRequest.tailnet_ip
    #[serde(default)]
    pub tailnet_ip: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Room {
    pub pin: String,
    pub host_name: String,
    pub endpoint: HostEndpoint,
    pub map: MapDescriptor,
    pub join_token: String,
    pub expires_at: String,
    #[serde(default)]
    pub protocol_version: u16,
    /// speculative: not returned by the live backend yet. Once it is, the
    /// host uses each player's tailnet_ip to unicast the rewritten LAN
    /// beacon to them (see hosting.rs).
    #[serde(default)]
    pub players: Vec<PlayerInfo>,
}

/// speculative: the response shape for the not-yet-built
/// `?request=tsnet/authkey` endpoint (see the tsnet-multiplayer rework
/// plan). `control_url` is included per-response rather than hardcoded so
/// the backend can move headscale without a client update.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TsnetAuthKey {
    pub auth_key: String,
    pub control_url: String,
    pub expires_at: String,
}
