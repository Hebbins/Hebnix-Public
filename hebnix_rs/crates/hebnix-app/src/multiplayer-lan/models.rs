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
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JoinedRoom {
    pub room: Room,
    pub assigned_ip: String,
    pub leave_token: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LeaveRoomRequest {
    pub leave_token: String,
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
}
