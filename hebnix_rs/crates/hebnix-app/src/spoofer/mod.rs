// crates/hebnix-app/src/spoofer/mod.rs
//! name spoofer. our own CA (ca), our own mitm proxy (proxy), spoofs live in rules.

pub mod ca;
pub mod crl;
pub mod dns;
pub mod hosts;
pub mod proxy;
pub mod rules;
pub mod skill_bridge;
pub mod socket;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crossbeam_channel::Sender;

use crate::messages::AppMsg;
use crate::spoofer::proxy::Proxy;
use crate::spoofer::rules::{
    NameRule, OwnedProductsRule, Rule, TITLE_HOST, TitleRule, TitleSettings,
};
use crate::spoofer::skill_bridge::SkillBridge;
use crate::spoofer::socket::SocketProxy;

pub const PROXY_HOST: &str = "127.0.0.1";
pub const PROXY_PORT: u16 = 8080;
pub const MAX_NAME_LENGTH: usize = 32;

const INET_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

pub fn is_admin() -> bool {
    type BOOL = i32;
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn IsUserAnAdmin() -> BOOL;
    }
    unsafe { IsUserAnAdmin() != 0 }
}

pub const SKIP_ELEVATE_ARG: &str = "--no-elevate";

pub fn spawn_elevated_relaunch() -> bool {
    use std::os::windows::process::CommandExt;

    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let exe = exe.to_string_lossy().replace('\'', "''"); // ps single quote escape
    let pid = std::process::id();
    let script = format!(
        "Wait-Process -Id {pid} -ErrorAction SilentlyContinue; \
         try {{ Start-Process -FilePath '{exe}' -Verb RunAs -ErrorAction Stop }} \
         catch {{ Start-Process -FilePath '{exe}' -ArgumentList '{SKIP_ELEVATE_ARG}' }}"
    );
    std::process::Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .spawn()
        .is_ok()
}

fn marker_path(base_dir: &Path) -> PathBuf {
    ca::dir(base_dir).join("proxy_backup.json")
}

pub const PROXY_BYPASS: &str = "<local>;*.epicgames.com;*.psyonix.com;*.live.psynet.gg";

#[derive(serde::Serialize, serde::Deserialize)]
struct ProxyBackup {
    proxy_enable: Option<u32>,
    proxy_server: Option<String>,
    #[serde(default)]
    proxy_override: Option<String>,
}

fn read_current_proxy() -> ProxyBackup {
    use winreg::RegKey;
    let hkcu = RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    match hkcu.open_subkey(INET_KEY) {
        Ok(k) => ProxyBackup {
            proxy_enable: k.get_value("ProxyEnable").ok(),
            proxy_server: k.get_value("ProxyServer").ok(),
            proxy_override: k.get_value("ProxyOverride").ok(),
        },
        Err(_) => ProxyBackup {
            proxy_enable: None,
            proxy_server: None,
            proxy_override: None,
        },
    }
}

fn apply_proxy(state: &ProxyBackup) -> Result<(), String> {
    use winreg::RegKey;
    use winreg::enums::*;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (settings, _) = hkcu
        .create_subkey(INET_KEY)
        .map_err(|e| format!("open inet key: {e}"))?;

    settings
        .set_value("ProxyEnable", &state.proxy_enable.unwrap_or(0))
        .map_err(|e| format!("set ProxyEnable: {e}"))?;

    for (name, value) in [
        ("ProxyServer", &state.proxy_server),
        ("ProxyOverride", &state.proxy_override),
    ] {
        match value {
            Some(v) => settings
                .set_value(name, v)
                .map_err(|e| format!("set {name}: {e}"))?,
            None => {
                let _ = settings.delete_value(name);
            }
        }
    }

    refresh_wininet();
    Ok(())
}

pub fn enable_proxy(base_dir: &Path) -> Result<(), String> {
    let marker = marker_path(base_dir);
    if !marker.is_file() {
        let backup = read_current_proxy();
        if let Ok(json) = serde_json::to_vec_pretty(&backup) {
            let _ = std::fs::create_dir_all(ca::dir(base_dir));
            let _ = std::fs::write(&marker, json);
        }
    }
    apply_proxy(&ProxyBackup {
        proxy_enable: Some(1),
        proxy_server: Some(format!("{PROXY_HOST}:{PROXY_PORT}")),
        proxy_override: Some(PROXY_BYPASS.to_string()),
    })
}

pub fn disable_proxy(base_dir: &Path) {
    let marker = marker_path(base_dir);
    match std::fs::read(&marker)
        .ok()
        .and_then(|b| serde_json::from_slice::<ProxyBackup>(&b).ok())
    {
        Some(backup) => {
            let _ = apply_proxy(&backup);
        }
        None => {
            let current = read_current_proxy();
            let ours = format!("{PROXY_HOST}:{PROXY_PORT}");
            let _ = apply_proxy(&ProxyBackup {
                proxy_enable: Some(0),
                proxy_server: current.proxy_server.filter(|s| s != &ours),
                proxy_override: current.proxy_override.filter(|s| s != PROXY_BYPASS),
            });
        }
    }
    let _ = std::fs::remove_file(&marker);
}

pub fn restore_if_crashed(base_dir: &Path) {
    if marker_path(base_dir).is_file() {
        tracing::warn!("stale spoofer proxy marker, restoring system proxy");
        disable_proxy(base_dir);
    }
    if hosts::is_redirected(TITLE_HOST) {
        tracing::warn!("stale hosts redirect, clearing it");
        let _ = hosts::clear();
    }
}

fn refresh_wininet() {
    use windows::Win32::Networking::WinInet::{
        INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED, InternetSetOptionW,
    };
    unsafe {
        let _ = InternetSetOptionW(None, INTERNET_OPTION_SETTINGS_CHANGED, None, 0);
        let _ = InternetSetOptionW(None, INTERNET_OPTION_REFRESH, None, 0);
    }
}

pub struct SpooferManager {
    base_dir: PathBuf,
    tx: Sender<AppMsg>,
    spoofed_name: Arc<Mutex<String>>,
    pub spoofed_friends: Arc<Mutex<HashMap<String, String>>>,
    pub discovered_friends: Arc<Mutex<HashMap<String, String>>>,
    pub spoofed_ranks: Arc<Mutex<HashMap<i32, (i32, f64)>>>,
    owned_products: Arc<Mutex<HashSet<i64>>>,
    proxy: Mutex<Option<Proxy>>,
    title_settings: Arc<Mutex<TitleSettings>>,
    title_proxy: Mutex<Option<SocketProxy>>,
    skill_bridge: Mutex<Option<SkillBridge>>,
    crl: Mutex<Option<crl::CrlServer>>,
}

impl SpooferManager {
    fn ensure_crl(&self, ca: &ca::Ca) {
        let mut slot = match self.crl.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        if slot.is_some() {
            return;
        }
        let der = match ca.crl_der() {
            Ok(d) => d,
            Err(e) => {
                let _ = self.tx.send(AppMsg::Log(format!("[Spoofer] crl gen: {e}")));
                return;
            }
        };
        match crl::CrlServer::start(der, self.tx.clone()) {
            Ok(s) => *slot = Some(s),
            Err(e) => {
                let _ = self
                    .tx
                    .send(AppMsg::Log(format!("[Spoofer] crl server: {e}")));
            }
        }
    }

    fn maybe_stop_crl(&self) {
        let name_up = self.proxy.lock().map(|p| p.is_some()).unwrap_or(false);
        let title_up = self
            .title_proxy
            .lock()
            .map(|p| p.is_some())
            .unwrap_or(false);
        if name_up || title_up {
            return;
        }
        if let Ok(mut slot) = self.crl.lock() {
            if let Some(s) = slot.take() {
                s.stop();
            }
        }
    }

    pub fn new(base_dir: PathBuf, tx: Sender<AppMsg>) -> Self {
        let owned_products = std::fs::read(base_dir.join("owned_products.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<i64>>(&bytes).ok())
            .unwrap_or_default()
            .into_iter()
            .collect();
        Self {
            base_dir,
            tx,
            spoofed_name: Arc::new(Mutex::new(String::new())),
            spoofed_friends: Arc::new(Mutex::new(HashMap::new())),
            discovered_friends: Arc::new(Mutex::new(HashMap::new())),
            spoofed_ranks: Arc::new(Mutex::new(HashMap::new())),
            owned_products: Arc::new(Mutex::new(owned_products)),
            proxy: Mutex::new(None),
            title_settings: Arc::new(Mutex::new(TitleSettings::default())),
            title_proxy: Mutex::new(None),
            skill_bridge: Mutex::new(None),
            crl: Mutex::new(None),
        }
    }

    pub fn owned_product_ids(&self) -> HashSet<i64> {
        self.owned_products
            .lock()
            .map(|owned| owned.clone())
            .unwrap_or_default()
    }

    pub fn set_username(&self, name: &str) {
        if let Ok(mut guard) = self.spoofed_name.lock() {
            let truncated: String = name.chars().take(MAX_NAME_LENGTH).collect();
            *guard = truncated;
        }
    }

    pub fn update_friends(&self, spoofs: HashMap<String, String>) {
        if let Ok(mut guard) = self.spoofed_friends.lock() {
            *guard = spoofs;
        }
    }

    pub fn update_ranks(&self, ranks: HashMap<i32, (i32, f64)>) {
        if let Ok(mut guard) = self.spoofed_ranks.lock() {
            *guard = ranks;
        }
        if self
            .spoofed_ranks
            .lock()
            .map(|ranks| !ranks.is_empty())
            .unwrap_or(false)
        {
            if let Err(error) = self.start_skill_bridge() {
                let detail = format!("[Spoofer] Rank bridge failed to start: {error}");
                let _ = std::fs::write(self.base_dir.join("rank_spoofer_status.log"), &detail);
                let _ = self.tx.send(AppMsg::Log(detail));
            }
        } else {
            self.stop_skill_bridge();
        }
    }

    fn start_skill_bridge(&self) -> Result<(), String> {
        let mut slot = self
            .skill_bridge
            .lock()
            .map_err(|_| "rank bridge lock poisoned")?;
        if slot.is_none() {
            *slot = Some(SkillBridge::start(
                Arc::clone(&self.spoofed_ranks),
                self.tx.clone(),
                self.base_dir.join("rank_spoofer_frames.log"),
            )?);
        }
        Ok(())
    }

    fn stop_skill_bridge(&self) {
        if let Ok(mut slot) = self.skill_bridge.lock() {
            if let Some(bridge) = slot.take() {
                bridge.stop();
            }
        }
    }

    pub fn http_running(&self) -> bool {
        self.proxy.lock().map(|p| p.is_some()).unwrap_or(false)
    }

    pub fn socket_running(&self) -> bool {
        self.title_proxy
            .lock()
            .map(|p| p.is_some())
            .unwrap_or(false)
    }

    pub fn start_http(&self) -> Result<(), String> {
        let mut slot = self.proxy.lock().map_err(|_| "proxy lock poisoned")?;
        if slot.is_some() {
            return Ok(());
        }

        let ca = Arc::new(ca::ensure(&self.base_dir)?);
        if !ca::is_current_installed(&self.base_dir) {
            return Err(
                "certificate not installed. open Spoofer settings and click Install Certificate"
                    .into(),
            );
        }

        let rules: Arc<Vec<Box<dyn Rule>>> = Arc::new(vec![
            Box::new(NameRule::new(Arc::clone(&self.spoofed_name))),
            Box::new(crate::spoofer::rules::FriendsRule::new(
                Arc::clone(&self.spoofed_friends),
                Arc::clone(&self.discovered_friends),
            )),
            Box::new(OwnedProductsRule::new(
                Arc::clone(&self.owned_products),
                self.base_dir.join("owned_products.json"),
            )),
            Box::new(crate::spoofer::rules::RankRule::new(Arc::clone(
                &self.spoofed_ranks,
            ))),
        ]);

        self.ensure_crl(&ca);
        let proxy = Proxy::start(ca, rules, self.tx.clone())?;
        enable_proxy(&self.base_dir)?;
        *slot = Some(proxy);
        Ok(())
    }

    pub fn stop_http(&self) {
        disable_proxy(&self.base_dir);
        if let Ok(mut slot) = self.proxy.lock() {
            if let Some(proxy) = slot.take() {
                proxy.stop();
            }
        }
        self.maybe_stop_crl();
    }

    pub fn set_title(&self, text: &str) {
        if let Ok(mut settings) = self.title_settings.lock() {
            settings.text = text.chars().take(64).collect();
        }
    }

    pub fn set_title_enabled(&self, enabled: bool) {
        if let Ok(mut settings) = self.title_settings.lock() {
            settings.enabled = enabled;
        }
    }

    pub fn set_title_options(&self, color: String, glow: bool, target_id: Option<String>) {
        if let Ok(mut settings) = self.title_settings.lock() {
            settings.color = color;
            settings.glow = glow;
            settings.target_id = target_id;
        }
    }

    pub fn start_socket(&self) -> Result<(), String> {
        let mut slot = self.title_proxy.lock().map_err(|_| "title lock poisoned")?;
        if slot.is_some() {
            return Ok(());
        }

        let ca = Arc::new(ca::ensure(&self.base_dir)?);
        if !ca::is_current_installed(&self.base_dir) {
            return Err(
                "Certificate not installed. open Spoofer settings and click Install Certificate"
                    .into(),
            );
        }
        if !hosts::is_writable() {
            return Err("The hosts file needs administrator, restart Hebnix as admin".into());
        }

        let ip = dns::resolve_a(TITLE_HOST)?;
        let rules: Arc<Vec<Box<dyn Rule>>> = Arc::new(vec![
            Box::new(TitleRule::new(Arc::clone(&self.title_settings))),
            Box::new(crate::spoofer::rules::RankRule::new(Arc::clone(
                &self.spoofed_ranks,
            ))),
        ]);

        self.ensure_crl(&ca);
        let proxy = SocketProxy::start(ca, rules, self.tx.clone(), TITLE_HOST.to_string(), ip)?;

        if let Err(e) = hosts::redirect(TITLE_HOST) {
            proxy.stop();
            return Err(e);
        }

        *slot = Some(proxy);
        Ok(())
    }

    pub fn stop_socket(&self) {
        // Remove the redirect before stopping the listener so Rocket League
        // immediately resumes resolving config.psynet.gg normally.
        let _ = hosts::clear();
        if let Ok(mut slot) = self.title_proxy.lock() {
            if let Some(proxy) = slot.take() {
                proxy.stop();
            }
        }
        self.stop_skill_bridge();
        self.maybe_stop_crl();
    }

    /// Stops only runtime interception. It deliberately does not modify saved
    /// spoof settings, so the user's enabled toggles survive the next launch.
    pub fn shutdown(&self) {
        self.stop_socket();
        self.stop_http();
        // Clear a redirect even if the socket failed to start or its state was
        // lost, and restore a proxy marker left by a partial startup.
        let _ = hosts::clear();
        disable_proxy(&self.base_dir);
    }
}
