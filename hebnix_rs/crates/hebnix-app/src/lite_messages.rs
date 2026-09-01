use hebnix_sdk::save_file::WindowMode;
use hebnix_sdk::stats::{StatsEvent, websocket::WsCommand};
use serde_json::Value;

#[derive(Debug)]
pub enum AppMsg {
    Log(String),
    GameEvent(StatsEvent),
    RlStatus {
        rl_open: bool,
        api_open: bool,
        root_dir: Option<String>,
    },
    StatsApiInitialised,
    WindowMode(WindowMode),
    ToggleVisibility,
    TrayVisibility(bool),
    TrayQuit,
    HotkeyCaptured(Option<String>),
    Topmost(bool),
    OverlayPost {
        slug: String,
        data: serde_json::Value,
    },
    PluginHttpRes {
        slug: String,
        req_id: String,
        status: u16,
        body: String,
    },
    PluginHttpDownloadRes {
        slug: String,
        req_id: String,
        status: u16,
        body: Vec<u8>,
    },
    PluginHttpRedirectRes {
        slug: String,
        req_id: String,
        status: u16,
        location: String,
    },
    // result of http_multipart_post_async, lands in on_http_upload_response
    PluginHttpUploadRes {
        slug: String,
        req_id: String,
        status: u16,
        body: String,
    },
    PluginFetch {
        result: Result<Value, String>,
    },
    PluginImage {
        key: String,
        bytes: Vec<u8>,
    },
    PluginDownloadDone {
        result: Result<String, String>,
    },
    AppUpdateFetched {
        result: Result<Option<crate::update::UpdateInfo>, String>,
    },
    ChangelogFetched {
        result: Result<Option<crate::update::ChangelogEntry>, String>,
    },
    AppUpdateFailed {
        error: String,
    },
    PluginUpdatesFound {
        updates: Result<Vec<Value>, String>,
    },
    PluginAutoUpdateDone {
        slug: String,
        was_enabled: bool,
        result: Result<String, String>,
    },
    SendWsCommand(WsCommand),
}
