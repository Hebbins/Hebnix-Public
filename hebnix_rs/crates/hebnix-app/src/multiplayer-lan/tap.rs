use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crossbeam_channel::{Receiver, unbounded};

pub const ADAPTER_NAME: &str = "Hebnix TAP";
const WINTUN_DLL: &[u8] = include_bytes!("wintun.dll");
#[cfg(target_arch = "x86_64")]
const TAP_DRIVER_ARCH: &str = "amd64";
#[cfg(target_arch = "x86_64")]
const TAP_DEVCON: &[u8] = include_bytes!("tap-driver/dist.win10/amd64/devcon.exe");
#[cfg(target_arch = "x86_64")]
const TAP_INF: &[u8] = include_bytes!("tap-driver/dist.win10/amd64/OemVista.inf");
#[cfg(target_arch = "x86_64")]
const TAP_CAT: &[u8] = include_bytes!("tap-driver/dist.win10/amd64/tap0901.cat");
#[cfg(target_arch = "x86_64")]
const TAP_SYS: &[u8] = include_bytes!("tap-driver/dist.win10/amd64/tap0901.sys");
const MASK: &str = "255.255.255.0";
const INVALID_HANDLE: isize = -1;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const OPEN_EXISTING: u32 = 3;
const FILE_ATTRIBUTE_SYSTEM: u32 = 4;
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
const TAP_SET_MEDIA_STATUS: u32 = 2_228_248;
const FRAME_SIZE: usize = 2_048;
const ERROR_IO_PENDING: u32 = 997;
const WAIT_OBJECT_0: u32 = 0;
const INFINITE: u32 = u32::MAX;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

type CreateFile =
    unsafe extern "system" fn(*const u16, u32, u32, *const (), u32, u32, *const ()) -> isize;
type DeviceIoControl =
    unsafe extern "system" fn(isize, u32, *mut u8, u32, *mut u8, u32, *mut u32, *const ()) -> i32;
type ReadFile = unsafe extern "system" fn(isize, *mut u8, u32, *mut u32, *const ()) -> i32;
type WriteFile = unsafe extern "system" fn(isize, *const u8, u32, *mut u32, *const ()) -> i32;
type CancelIoEx = unsafe extern "system" fn(isize, *const ()) -> i32;
type CloseHandle = unsafe extern "system" fn(isize) -> i32;
type CreateEventW = unsafe extern "system" fn(*const (), i32, i32, *const u16) -> isize;
type WaitForSingleObject = unsafe extern "system" fn(isize, u32) -> u32;
type GetOverlappedResult = unsafe extern "system" fn(isize, *mut Overlapped, *mut u32, i32) -> i32;
type GetLastError = unsafe extern "system" fn() -> u32;

#[repr(C)]
#[derive(Default)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    event: isize,
}

struct TapApi {
    _library: libloading::Library,
    read_file: ReadFile,
    write_file: WriteFile,
    cancel_io_ex: CancelIoEx,
    close_handle: CloseHandle,
    create_event_w: CreateEventW,
    wait_for_single_object: WaitForSingleObject,
    get_overlapped_result: GetOverlappedResult,
    get_last_error: GetLastError,
}

unsafe impl Send for TapApi {}
unsafe impl Sync for TapApi {}

pub struct TapSession {
    api: Arc<TapApi>,
    handle: Mutex<isize>,
    frames: Receiver<Vec<u8>>,
    reader: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for TapSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("TapSession").finish_non_exhaustive()
    }
}

unsafe impl Send for TapSession {}
unsafe impl Sync for TapSession {}

pub fn ensure_adapter(address: &str) -> Result<(), String> {
    let _ = embedded_wintun_path()?;
    if adapter_exists()? {
        return configure(address);
    }
    if let Some(name) = find_tap_adapter()? {
        rename(&name)?;
        return configure(address);
    }
    let driver = driver_dir()?;
    let inf = driver.join("OemVista.inf");
    let devcon = driver.join("devcon.exe");
    if !inf.is_file() || !devcon.is_file() {
        return Err("the bundled TAP driver is incomplete".to_string());
    }
    run_pnputil(&inf)?;
    run(
        &devcon.to_string_lossy(),
        &["install", &inf.to_string_lossy(), "tap0901"],
    )?;
    for _ in 0..30 {
        if let Some(name) = find_tap_adapter()? {
            rename(&name)?;
            return configure(address);
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    Err("the TAP driver installed but its adapter did not appear".to_string())
}

pub fn embedded_wintun_path() -> Result<PathBuf, String> {
    let path = embedded_runtime_dir()?.join("wintun.dll");
    write_embedded(&path, WINTUN_DLL)?;
    Ok(path)
}

fn embedded_runtime_dir() -> Result<PathBuf, String> {
    let root = dirs::data_dir()
        .ok_or_else(|| "could not find AppData".to_string())?
        .join("Hebnix")
        .join("multiplayer-lan");
    let driver = root
        .join("tap-driver")
        .join("dist.win10")
        .join(TAP_DRIVER_ARCH);
    write_embedded(&driver.join("devcon.exe"), TAP_DEVCON)?;
    write_embedded(&driver.join("OemVista.inf"), TAP_INF)?;
    write_embedded(&driver.join("tap0901.cat"), TAP_CAT)?;
    write_embedded(&driver.join("tap0901.sys"), TAP_SYS)?;
    Ok(root)
}

fn write_embedded(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    if path.is_file()
        && std::fs::metadata(path).map(|meta| meta.len()).ok() == Some(bytes.len() as u64)
    {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(path, bytes).map_err(|error| error.to_string())
}

pub fn configure_existing(address: &str) -> Result<(), String> {
    if !adapter_exists()? {
        return Err("Hebnix TAP is not installed; use the optional setup button first".to_string());
    }
    if adapter_has_address(address)? {
        return ensure_peer_routes(address);
    }
    configure(address)
}

pub fn is_configured(address: &str) -> Result<bool, String> {
    Ok(adapter_exists()? && adapter_has_address(address)?)
}

pub fn clear_configuration() -> Result<(), String> {
    if !adapter_exists()? {
        return Ok(());
    }
    run_powershell(
        "$adapter = Get-NetAdapter -Name 'Hebnix TAP' -ErrorAction SilentlyContinue; if ($null -ne $adapter) { Get-NetRoute -InterfaceIndex $adapter.ifIndex -AddressFamily IPv4 -ErrorAction SilentlyContinue | Where-Object { $_.DestinationPrefix -like '10.242.77.*' -or $_.DestinationPrefix -like '192.10.192.*' } | Remove-NetRoute -Confirm:$false -ErrorAction SilentlyContinue; Get-NetNeighbor -InterfaceIndex $adapter.ifIndex -AddressFamily IPv4 -ErrorAction SilentlyContinue | Where-Object { $_.IPAddress -like '10.242.77.*' -or $_.IPAddress -like '192.10.192.*' } | Remove-NetNeighbor -Confirm:$false -ErrorAction SilentlyContinue; Get-NetIPAddress -InterfaceIndex $adapter.ifIndex -AddressFamily IPv4 -ErrorAction SilentlyContinue | Where-Object { $_.IPAddress -like '10.242.77.*' -or $_.IPAddress -like '192.10.192.*' } | Remove-NetIPAddress -Confirm:$false -ErrorAction SilentlyContinue }",
    )?;
    Ok(())
}

pub fn mac_address() -> Result<String, String> {
    let mac = run_powershell("(Get-NetAdapter -Name 'Hebnix TAP' -ErrorAction Stop).MacAddress")?;
    let mac = mac.trim().replace('-', "").replace(':', "");
    if mac.len() == 12 && mac.chars().all(|value| value.is_ascii_hexdigit()) {
        Ok(mac)
    } else {
        Err("could not read the Hebnix TAP MAC address".to_string())
    }
}

pub fn add_neighbor(address: &str, mac: &str) -> Result<(), String> {
    let normalized = format_mac(mac).ok_or_else(|| "invalid TAP MAC address".to_string())?;
    let _ = run(
        "netsh",
        &[
            "interface",
            "ipv4",
            "delete",
            "neighbors",
            ADAPTER_NAME,
            address,
        ],
    );
    run(
        "netsh",
        &[
            "interface",
            "ipv4",
            "add",
            "neighbors",
            ADAPTER_NAME,
            address,
            &normalized,
            "store=active",
        ],
    )
}

pub fn arp_announcement(address: &str) -> Result<Vec<u8>, String> {
    let mac = parse_mac(&mac_address()?).ok_or_else(|| "invalid TAP MAC address".to_string())?;
    let ip = parse_ip(address)?;
    let mut frame = vec![0; 42];
    frame[..6].fill(0xff);
    frame[6..12].copy_from_slice(&mac);
    frame[12..14].copy_from_slice(&0x0806u16.to_be_bytes());
    frame[14..16].copy_from_slice(&1u16.to_be_bytes());
    frame[16..18].copy_from_slice(&0x0800u16.to_be_bytes());
    frame[18] = 6;
    frame[19] = 4;
    frame[20..22].copy_from_slice(&1u16.to_be_bytes());
    frame[22..28].copy_from_slice(&mac);
    frame[28..32].copy_from_slice(&ip);
    frame[38..42].copy_from_slice(&ip);
    Ok(frame)
}

pub fn arp_reply_for_local(frame: &[u8], local_address: &str) -> Result<Option<Vec<u8>>, String> {
    if frame.len() < 42
        || frame[12..14] != 0x0806u16.to_be_bytes()
        || frame[20..22] != 1u16.to_be_bytes()
    {
        return Ok(None);
    }
    let local_ip = parse_ip(local_address)?;
    if frame[38..42] != local_ip {
        return Ok(None);
    }
    let local_mac =
        parse_mac(&mac_address()?).ok_or_else(|| "invalid TAP MAC address".to_string())?;
    let mut reply = vec![0; 42];
    reply[..6].copy_from_slice(&frame[22..28]);
    reply[6..12].copy_from_slice(&local_mac);
    reply[12..14].copy_from_slice(&0x0806u16.to_be_bytes());
    reply[14..16].copy_from_slice(&1u16.to_be_bytes());
    reply[16..18].copy_from_slice(&0x0800u16.to_be_bytes());
    reply[18] = 6;
    reply[19] = 4;
    reply[20..22].copy_from_slice(&2u16.to_be_bytes());
    reply[22..28].copy_from_slice(&local_mac);
    reply[28..32].copy_from_slice(&local_ip);
    reply[32..38].copy_from_slice(&frame[22..28]);
    reply[38..42].copy_from_slice(&frame[28..32]);
    Ok(Some(reply))
}

impl TapSession {
    pub fn open() -> Result<Self, String> {
        let guid =
            run_powershell("(Get-NetAdapter -Name 'Hebnix TAP' -ErrorAction Stop).InterfaceGuid")?;
        let guid = guid.trim().trim_matches('{').trim_matches('}');
        if guid.is_empty() {
            return Err("could not locate the Hebnix TAP adapter".to_string());
        }
        let library = unsafe { libloading::Library::new("kernel32.dll") }
            .map_err(|error| error.to_string())?;
        unsafe {
            let create_file: CreateFile = *library.get(b"CreateFileW\0").map_err(load_error)?;
            let device_io_control: DeviceIoControl =
                *library.get(b"DeviceIoControl\0").map_err(load_error)?;
            let read_file: ReadFile = *library.get(b"ReadFile\0").map_err(load_error)?;
            let write_file: WriteFile = *library.get(b"WriteFile\0").map_err(load_error)?;
            let cancel_io_ex: CancelIoEx = *library.get(b"CancelIoEx\0").map_err(load_error)?;
            let close_handle: CloseHandle = *library.get(b"CloseHandle\0").map_err(load_error)?;
            let create_event_w: CreateEventW =
                *library.get(b"CreateEventW\0").map_err(load_error)?;
            let wait_for_single_object: WaitForSingleObject =
                *library.get(b"WaitForSingleObject\0").map_err(load_error)?;
            let get_overlapped_result: GetOverlappedResult =
                *library.get(b"GetOverlappedResult\0").map_err(load_error)?;
            let get_last_error: GetLastError =
                *library.get(b"GetLastError\0").map_err(load_error)?;
            let path = wide(&format!(r"\\.\Global\{{{guid}}}.tap"));
            let handle = create_file(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_SYSTEM | FILE_FLAG_OVERLAPPED,
                std::ptr::null(),
            );
            if handle == INVALID_HANDLE {
                return Err("could not open the Hebnix TAP adapter".to_string());
            }
            let mut status = 1u32;
            let mut returned = 0u32;
            if device_io_control(
                handle,
                TAP_SET_MEDIA_STATUS,
                (&mut status as *mut u32).cast(),
                4,
                (&mut status as *mut u32).cast(),
                4,
                &mut returned,
                std::ptr::null(),
            ) == 0
            {
                close_handle(handle);
                return Err("could not activate the Hebnix TAP adapter".to_string());
            }
            let api = Arc::new(TapApi {
                _library: library,
                read_file,
                write_file,
                cancel_io_ex,
                close_handle,
                create_event_w,
                wait_for_single_object,
                get_overlapped_result,
                get_last_error,
            });
            let (sender, frames) = unbounded();
            let reader_api = api.clone();
            let reader = thread::spawn(move || {
                loop {
                    let mut frame = vec![0; FRAME_SIZE];
                    let length = match read_overlapped(&reader_api, handle, &mut frame) {
                        Ok(length) => length,
                        Err(_) => break,
                    };
                    frame.truncate(length as usize);
                    if sender.send(frame).is_err() {
                        break;
                    }
                }
            });
            Ok(Self {
                api,
                handle: Mutex::new(handle),
                frames,
                reader: Mutex::new(Some(reader)),
            })
        }
    }

    pub fn try_receive(&self) -> Option<Vec<u8>> {
        self.frames.try_recv().ok()
    }

    pub fn send(&self, frame: &[u8]) -> Result<(), String> {
        let handle = *self
            .handle
            .lock()
            .map_err(|_| "TAP adapter lock failed".to_string())?;
        if handle == INVALID_HANDLE {
            return Err("TAP adapter is closed".to_string());
        }
        let written = write_overlapped(&self.api, handle, frame)?;
        if written as usize != frame.len() {
            return Err("TAP adapter wrote an incomplete Ethernet frame".to_string());
        }
        Ok(())
    }

    pub fn stop(&self) {
        if let Ok(mut handle) = self.handle.lock() {
            if *handle != INVALID_HANDLE {
                unsafe {
                    (self.api.cancel_io_ex)(*handle, std::ptr::null());
                }
                if let Ok(mut reader) = self.reader.lock() {
                    if let Some(reader) = reader.take() {
                        let _ = reader.join();
                    }
                }
                unsafe {
                    (self.api.close_handle)(*handle);
                }
                *handle = INVALID_HANDLE;
            }
        }
    }
}

impl Drop for TapSession {
    fn drop(&mut self) {
        self.stop();
    }
}

fn read_overlapped(api: &TapApi, handle: isize, frame: &mut [u8]) -> Result<u32, String> {
    let event = unsafe { (api.create_event_w)(std::ptr::null(), 0, 0, std::ptr::null()) };
    if event == 0 {
        return Err(format!("could not create TAP read event ({})", unsafe {
            (api.get_last_error)()
        }));
    }
    let mut operation = Overlapped {
        event,
        ..Default::default()
    };
    let mut length = 0u32;
    let started = unsafe {
        (api.read_file)(
            handle,
            frame.as_mut_ptr(),
            frame.len() as u32,
            &mut length,
            (&mut operation as *mut Overlapped).cast(),
        )
    };
    let result = if started != 0 {
        Ok(length)
    } else if unsafe { (api.get_last_error)() } == ERROR_IO_PENDING {
        wait_overlapped(api, handle, &mut operation, &mut length)
    } else {
        Err(format!("TAP read failed ({})", unsafe {
            (api.get_last_error)()
        }))
    };
    unsafe {
        (api.close_handle)(event);
    }
    result
}

fn write_overlapped(api: &TapApi, handle: isize, frame: &[u8]) -> Result<u32, String> {
    let event = unsafe { (api.create_event_w)(std::ptr::null(), 0, 0, std::ptr::null()) };
    if event == 0 {
        return Err(format!("could not create TAP write event ({})", unsafe {
            (api.get_last_error)()
        }));
    }
    let mut operation = Overlapped {
        event,
        ..Default::default()
    };
    let mut written = 0u32;
    let started = unsafe {
        (api.write_file)(
            handle,
            frame.as_ptr(),
            frame.len() as u32,
            &mut written,
            (&mut operation as *mut Overlapped).cast(),
        )
    };
    let result = if started != 0 {
        Ok(written)
    } else if unsafe { (api.get_last_error)() } == ERROR_IO_PENDING {
        wait_overlapped(api, handle, &mut operation, &mut written)
    } else {
        Err(format!("TAP write failed ({})", unsafe {
            (api.get_last_error)()
        }))
    };
    unsafe {
        (api.close_handle)(event);
    }
    result
}

fn wait_overlapped(
    api: &TapApi,
    handle: isize,
    operation: &mut Overlapped,
    length: &mut u32,
) -> Result<u32, String> {
    if unsafe { (api.wait_for_single_object)(operation.event, INFINITE) } != WAIT_OBJECT_0 {
        return Err(format!("TAP I/O wait failed ({})", unsafe {
            (api.get_last_error)()
        }));
    }
    if unsafe { (api.get_overlapped_result)(handle, operation, length, 0) } == 0 {
        return Err(format!("TAP I/O failed ({})", unsafe {
            (api.get_last_error)()
        }));
    }
    Ok(*length)
}

pub fn driver_dir() -> Result<PathBuf, String> {
    let root = embedded_runtime_dir()?
        .join("tap-driver")
        .join("dist.win10");
    Ok(root.join(TAP_DRIVER_ARCH))
}

fn configure(address: &str) -> Result<(), String> {
    run(
        "netsh",
        &[
            "interface",
            "ipv4",
            "set",
            "address",
            &format!("name={ADAPTER_NAME}"),
            "static",
            address,
            MASK,
            "none",
        ],
    )?;
    for _ in 0..20 {
        if adapter_has_address(address)? {
            return ensure_peer_routes(address);
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    Err(format!(
        "Hebnix TAP did not receive its Workshop LAN address ({address})"
    ))
}

fn ensure_peer_routes(local_address: &str) -> Result<(), String> {
    let script = format!(
        "$adapter = Get-NetAdapter -Name 'Hebnix TAP' -ErrorAction Stop; Set-NetIPInterface -InterfaceIndex $adapter.ifIndex -AddressFamily IPv4 -InterfaceMetric 1 -ErrorAction Stop; 1..8 | ForEach-Object {{ $address = '{}.' + $_; if ($address -ne '{local_address}') {{ $prefix = $address + '/32'; Get-NetRoute -InterfaceIndex $adapter.ifIndex -DestinationPrefix $prefix -AddressFamily IPv4 -ErrorAction SilentlyContinue | Remove-NetRoute -Confirm:$false -ErrorAction SilentlyContinue; New-NetRoute -DestinationPrefix $prefix -InterfaceIndex $adapter.ifIndex -NextHop '0.0.0.0' -RouteMetric 1 -PolicyStore ActiveStore -ErrorAction Stop | Out-Null }} }}",
        super::VPN_SUBNET,
    );
    run_powershell(&script).map(|_| ())
}

fn adapter_exists() -> Result<bool, String> {
    Ok(run_powershell(
        "(Get-NetAdapter -Name 'Hebnix TAP' -ErrorAction SilentlyContinue) -ne $null",
    )?
    .trim()
    .eq_ignore_ascii_case("true"))
}

fn adapter_has_address(address: &str) -> Result<bool, String> {
    Ok(run_powershell(&format!(
        "(Get-NetIPAddress -InterfaceAlias 'Hebnix TAP' -AddressFamily IPv4 -ErrorAction SilentlyContinue | Where-Object {{ $_.IPAddress -eq '{address}' }}) -ne $null"
    ))?
    .trim()
    .eq_ignore_ascii_case("true"))
}

fn find_tap_adapter() -> Result<Option<String>, String> {
    let output = run_powershell(
        "Get-NetAdapter -ErrorAction SilentlyContinue | Where-Object { ($_.InterfaceDescription -like '*TAP-Windows*' -or $_.InterfaceDescription -like '*OpenVPN TAP*') -and $_.Name -ne 'BakkboardTAP' } | Sort-Object ifIndex -Descending | Select-Object -First 1 -ExpandProperty Name",
    )?;
    let name = output.trim();
    Ok((!name.is_empty()).then(|| name.to_string()))
}

fn rename(current: &str) -> Result<(), String> {
    if current.eq_ignore_ascii_case(ADAPTER_NAME) {
        return Ok(());
    }
    run(
        "netsh",
        &[
            "interface",
            "set",
            "interface",
            &format!("name={current}"),
            &format!("newname={ADAPTER_NAME}"),
        ],
    )
}

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(format!(
        "{program} {} failed with {}. {} {}",
        args.join(" "),
        output.status.code().map_or_else(
            || "an unknown exit code".to_string(),
            |code| format!("exit code {code}")
        ),
        stdout,
        stderr,
    )
    .trim()
    .to_string())
}

fn run_pnputil(inf: &std::path::Path) -> Result<(), String> {
    let output = Command::new("pnputil")
        .args(["/add-driver", &inf.to_string_lossy(), "/install"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| error.to_string())?;
    if matches!(output.status.code(), Some(0 | 5 | 259)) {
        return Ok(());
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(format!("pnputil driver install failed. {stdout} {stderr}")
        .trim()
        .to_string())
}

fn run_powershell(script: &str) -> Result<String, String> {
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
fn load_error(error: libloading::Error) -> String {
    format!("missing Windows TAP API: {error}")
}

fn parse_ip(address: &str) -> Result<[u8; 4], String> {
    address
        .parse::<std::net::Ipv4Addr>()
        .map(|value| value.octets())
        .map_err(|_| format!("invalid virtual IP address: {address}"))
}

fn parse_mac(value: &str) -> Option<[u8; 6]> {
    let hex: String = value
        .chars()
        .filter(|value| value.is_ascii_hexdigit())
        .collect();
    if hex.len() != 12 {
        return None;
    }
    let mut mac = [0; 6];
    for (index, byte) in mac.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(mac)
}

fn format_mac(value: &str) -> Option<String> {
    let mac = parse_mac(value)?;
    Some(
        mac.iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join("-"),
    )
}
