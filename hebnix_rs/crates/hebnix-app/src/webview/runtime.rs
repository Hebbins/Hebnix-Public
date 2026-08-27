//! WebView2 runtime check + install.
//!
//! no runtime means no overlays at all. it ships on win11 and reaches win10
//! through edge, but debloated images and LTSC strip it, so the bootstrapper
//! is pulled from microsoft on demand.

use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use webview2_com::Microsoft::Web::WebView2::Win32::GetAvailableCoreWebView2BrowserVersionString;
use webview2_com::take_pwstr;
use windows::core::PCWSTR;

const BOOTSTRAPPER: &str = "MicrosoftEdgeWebview2Setup.exe";

/// in case WebView2 is missing we need to install it, i have not  tested this.
const BOOTSTRAPPER_URL: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";

/// silent still raises uac, the exe asks for it itself
const INSTALL_ARGS: &str = "/silent /install";

static AVAILABLE: AtomicBool = AtomicBool::new(false);

/// set by ensure_present, read before building the overlay browser
#[allow(dead_code)] // wired up when pages land
pub fn is_available() -> bool {
    AVAILABLE.load(Ordering::Relaxed)
}

pub fn installed_version() -> Option<String> {
    unsafe {
        let mut version = windows::core::PWSTR::null();
        GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut version).ok()?;
        let version = take_pwstr(version);
        (!version.is_empty()).then_some(version)
    }
}

// not spoofer::is_admin, hebnix-lite has no spoofer module
fn is_elevated() -> bool {
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn IsUserAnAdmin() -> i32;
    }
    unsafe { IsUserAnAdmin() != 0 }
}

/// into temp, nothing is kept
fn download_bootstrapper() -> Result<PathBuf, String> {
    let response = ureq::get(BOOTSTRAPPER_URL)
        .timeout(std::time::Duration::from_secs(60))
        .call()
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() < 100_000 {
        return Err(format!("the download was only {} bytes", bytes.len()));
    }
    let path = std::env::temp_dir().join(BOOTSTRAPPER);
    std::fs::write(&path, bytes).map_err(|error| error.to_string())?;
    Ok(path)
}

/// once at startup, after the instance guard so only one copy prompts
pub fn ensure_present() -> bool {
    if let Some(version) = installed_version() {
        tracing::info!("WebView2 runtime {version}");
        AVAILABLE.store(true, Ordering::Relaxed);
        return true;
    }

    let prompt = if is_elevated() {
        "Hebnix is missing one required component: the Microsoft Edge WebView2 runtime.\n\n\
         Plugin overlays cannot run without it. Hebnix will download the installer from \
         Microsoft (about 2 MB) and run it.\n\nDownload and install it now?"
    } else {
        "Hebnix is missing one required component: the Microsoft Edge WebView2 runtime.\n\n\
         Plugin overlays cannot run without it. Hebnix will download the installer from \
         Microsoft (about 2 MB) and run it. Windows will ask you to allow the installer, \
         press Yes when it does.\n\nDownload and install it now?"
    };
    let accepted = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Info)
        .set_title("Hebnix")
        .set_description(prompt)
        .set_buttons(rfd::MessageButtons::OkCancel)
        .show();
    if accepted != rfd::MessageDialogResult::Ok {
        tracing::warn!("user declined the WebView2 runtime install, overlays are off");
        return false;
    }

    let installed = download_bootstrapper().and_then(|installer| {
        let result = run_installer(&installer);
        let _ = std::fs::remove_file(&installer);
        result
    });
    match installed {
        Ok(()) => {}
        Err(error) => {
            tracing::warn!("WebView2 install failed: {error}");
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("Hebnix")
                .set_description(format!(
                    "The WebView2 runtime install did not finish.\n\n{error}\n\n\
                     Plugin overlays stay disabled until it is installed."
                ))
                .set_buttons(rfd::MessageButtons::Ok)
                .show();
            return false;
        }
    }

    match installed_version() {
        Some(version) => {
            tracing::info!("WebView2 runtime {version} installed");
            AVAILABLE.store(true, Ordering::Relaxed);
            true
        }
        None => {
            tracing::warn!("bootstrapper returned success but no runtime is present");
            false
        }
    }
}

/// not Command: CreateProcess fails with ERROR_ELEVATION_REQUIRED instead of
/// raising the uac prompt
fn run_installer(installer: &std::path::Path) -> Result<(), String> {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{INFINITE, WaitForSingleObject};
    use windows::Win32::UI::Shell::{
        SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let verb: Vec<u16> = "runas\0".encode_utf16().collect();
    let file: Vec<u16> = format!("{}\0", installer.display())
        .encode_utf16()
        .collect();
    let args: Vec<u16> = format!("{INSTALL_ARGS}\0").encode_utf16().collect();

    unsafe {
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(args.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        ShellExecuteExW(&mut info).map_err(|error| error.message())?;
        if info.hProcess.is_invalid() {
            return Err("the installer did not start".to_string());
        }
        let waited = WaitForSingleObject(info.hProcess, INFINITE);
        let _ = CloseHandle(info.hProcess);
        if waited != WAIT_OBJECT_0 {
            return Err("gave up waiting for the installer".to_string());
        }
    }
    Ok(())
}
