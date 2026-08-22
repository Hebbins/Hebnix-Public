use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{
    CreateRoomRequest, JoinRoomRequest, JoinedRoom, LeaveRoomRequest, Room, RoomCredentials,
    UpdatePlayerRequest,
};

#[derive(Clone)]
pub struct RoomClient {
    base_url: String,
    agent: ureq::Agent,
}

impl RoomClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            agent: ureq::AgentBuilder::new().try_proxy_from_env(false).build(),
        }
    }

    pub fn create_room(
        &self,
        request: &CreateRoomRequest,
    ) -> Result<(Room, RoomCredentials), String> {
        #[derive(serde::Deserialize)]
        struct Response {
            room: Room,
            credentials: RoomCredentials,
        }

        let response: Response = self.post("/?request=multiplayer/create", request)?;
        Ok((response.room, response.credentials))
    }

    pub fn get_room(&self, pin: &str) -> Result<Room, String> {
        self.get(&format!("/?request=multiplayer/join/{}", urlencoding(pin)))
    }

    pub fn join_room(&self, pin: &str, request: &JoinRoomRequest) -> Result<JoinedRoom, String> {
        let joined: JoinedRoom = self.post(
            &format!("/?request=multiplayer/join/{}", urlencoding(pin)),
            request,
        )?;
        if joined.room.protocol_version != 2 {
            return Err(
                "The host is using an incompatible Workshop multiplayer version".to_string(),
            );
        }
        Ok(joined)
    }

    pub fn leave_room(&self, pin: &str, leave_token: &str) -> Result<(), String> {
        let request = LeaveRoomRequest {
            leave_token: leave_token.to_string(),
        };
        let _: serde_json::Value = self.post(
            &format!("/?request=multiplayer/leave/{}", urlencoding(pin)),
            &request,
        )?;
        Ok(())
    }

    pub fn update_player(&self, pin: &str, request: &UpdatePlayerRequest) -> Result<(), String> {
        let _: serde_json::Value = self.post(
            &format!("/?request=multiplayer/player/{}", urlencoding(pin)),
            request,
        )?;
        Ok(())
    }

    pub fn heartbeat(&self, pin: &str, host_secret: &str) -> Result<Room, String> {
        self.host_post(
            &format!("/?request=multiplayer/update/{}", urlencoding(pin)),
            host_secret,
        )
    }

    pub fn close_room(&self, pin: &str, host_secret: &str) -> Result<(), String> {
        let url = format!(
            "{}/?request=multiplayer/close/{}",
            self.base_url,
            urlencoding(pin)
        );
        self.agent
            .post(&url)
            .set("x-hebnix-host-secret", host_secret)
            .send_string("")
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let url = format!("{}{}", self.base_url, path);
        self.agent
            .get(&url)
            .call()
            .map_err(|error| error.to_string())?
            .into_json()
            .map_err(|error| error.to_string())
    }

    fn post<T: Serialize, R: DeserializeOwned>(&self, path: &str, body: &T) -> Result<R, String> {
        let url = format!("{}{}", self.base_url, path);
        self.agent
            .post(&url)
            .send_json(body)
            .map_err(api_error)?
            .into_json()
            .map_err(|error| error.to_string())
    }

    fn host_post<T: DeserializeOwned>(&self, path: &str, host_secret: &str) -> Result<T, String> {
        let url = format!("{}{}", self.base_url, path);
        self.agent
            .post(&url)
            .set("x-hebnix-host-secret", host_secret)
            .send_string("")
            .map_err(|error| error.to_string())?
            .into_json()
            .map_err(|error| error.to_string())
    }
}

fn api_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(_, response) => response
            .into_json::<serde_json::Value>()
            .ok()
            .and_then(|body| {
                body.get("error")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "the multiplayer API rejected the request".to_string()),
        error => error.to_string(),
    }
}

fn urlencoding(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => vec![byte as char],
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}
