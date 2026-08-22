use hebnix_sdk::save_file::WindowMode;
use hebnix_sdk::stats::StatsEvent;
use serde_json::Value;

#[derive(Debug)]
pub enum AppMsg {
    Log(String),
    GameEvent(StatsEvent),
    // periodic RL monitor result. root_dir is the game install folder resolved
    // from the running process, used to auto-fill the configured paths.
    RlStatus {
        rl_open: bool,
        api_open: bool,
        root_dir: Option<String>,
    },
    // monitor set PacketSendRate, game needs a restart
    StatsApiInitialised,
    // only sent when it changes
    WindowMode(WindowMode),
    ToggleVisibility,
    TrayOpen,
    TrayQuit,
    HotkeyCaptured(Option<String>),
    // should the main window be topmost (RL or hebnix focused)
    Topmost(bool),
    WorkshopCatalog(Result<Vec<Value>, String>),
    WorkshopImage {
        key: String,
        bytes: Vec<u8>,
    },
    WorkshopOpDone {
        message: String,
    },
    WorkshopMultiplayerProgress(String),
    WorkshopMultiplayerPrepared {
        result: Result<
            (
                crate::multiplayer_lan::TapSession,
                Option<crate::multiplayer_lan::JoinedRoom>,
            ),
            String,
        >,
    },
    WorkshopHostStarted {
        result: Result<crate::multiplayer_lan::HostSession, String>,
    },
    WorkshopGuestJoined {
        result: Result<crate::multiplayer_lan::GuestSession, String>,
    },
    WorkshopPlayerUpdated {
        result: Result<(), String>,
    },
    WorkshopHostSessionCheck {
        result: Result<crate::multiplayer_lan::Room, String>,
    },
    WorkshopWizardCheck {
        rl_open: bool,
        tap_ready: bool,
        launch_ready: bool,
        detected_map: Option<String>,
    },
    // "install from hebnix" plugin metadata fetch done
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
    // http result, slug picks the plugin that asked
    PluginHttpRes {
        slug: String,
        req_id: String,
        status: u16,
        body: String,
    },
    // byte-safe variant of PluginHttpRes for http_download_async — body is
    // raw bytes, not decoded as UTF-8 text, so binary responses (e.g.
    // avatar images) survive intact.
    PluginHttpDownloadRes {
        slug: String,
        req_id: String,
        status: u16,
        body: Vec<u8>,
    },
    // result of http_get_no_redirect_async — location is the response's
    // Location header (empty if none/not a redirect). Used for OAuth flows
    // that put their payload in a 302's Location header (e.g. PSN's NPSSO
    // exchange) instead of a followable body.
    PluginHttpRedirectRes {
        slug: String,
        req_id: String,
        status: u16,
        location: String,
    },
    AppUpdateFetched {
        result: Result<Option<crate::update::UpdateInfo>, String>,
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
    SendWsCommand(hebnix_sdk::stats::websocket::WsCommand),
}
