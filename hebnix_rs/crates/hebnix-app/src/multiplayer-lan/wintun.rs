use std::ffi::c_void;
use std::path::PathBuf;
use std::process::Command;

use libloading::Library;

const RING_CAPACITY: u32 = 0x400000;

type CreateAdapter = unsafe extern "system" fn(*const u16, *const u16, *const u8) -> *mut c_void;
type CloseAdapter = unsafe extern "system" fn(*mut c_void);
type StartSession = unsafe extern "system" fn(*mut c_void, u32) -> *mut c_void;
type EndSession = unsafe extern "system" fn(*mut c_void);
type ReceivePacket = unsafe extern "system" fn(*mut c_void, *mut u32) -> *mut u8;
type ReleaseReceivePacket = unsafe extern "system" fn(*mut c_void, *mut u8);
type AllocateSendPacket = unsafe extern "system" fn(*mut c_void, u32) -> *mut u8;
type SendPacket = unsafe extern "system" fn(*mut c_void, *mut u8);

pub struct WintunSession {
    _library: Library,
    adapter: *mut c_void,
    session: *mut c_void,
    close_adapter: CloseAdapter,
    end_session: EndSession,
    receive_packet: ReceivePacket,
    release_receive_packet: ReleaseReceivePacket,
    allocate_send_packet: AllocateSendPacket,
    send_packet: SendPacket,
}

unsafe impl Send for WintunSession {}

impl WintunSession {
    pub fn create(name: &str, address: &str) -> Result<Self, String> {
        let path = wintun_path()?;
        let library = unsafe { Library::new(&path) }
            .map_err(|error| format!("could not load {}: {error}", path.display()))?;
        unsafe {
            let create: CreateAdapter =
                *library.get(b"WintunCreateAdapter\0").map_err(load_error)?;
            let close_adapter: CloseAdapter =
                *library.get(b"WintunCloseAdapter\0").map_err(load_error)?;
            let start_session: StartSession =
                *library.get(b"WintunStartSession\0").map_err(load_error)?;
            let end_session: EndSession =
                *library.get(b"WintunEndSession\0").map_err(load_error)?;
            let receive_packet: ReceivePacket =
                *library.get(b"WintunReceivePacket\0").map_err(load_error)?;
            let release_receive_packet: ReleaseReceivePacket = *library
                .get(b"WintunReleaseReceivePacket\0")
                .map_err(load_error)?;
            let allocate_send_packet: AllocateSendPacket = *library
                .get(b"WintunAllocateSendPacket\0")
                .map_err(load_error)?;
            let send_packet: SendPacket =
                *library.get(b"WintunSendPacket\0").map_err(load_error)?;
            let adapter_name = name;
            let name = wide(adapter_name);
            let kind = wide("Hebnix");
            let adapter = create(name.as_ptr(), kind.as_ptr(), std::ptr::null());
            if adapter.is_null() {
                return Err("could not create the Hebnix LAN adapter".to_string());
            }
            let session = start_session(adapter, RING_CAPACITY);
            if session.is_null() {
                close_adapter(adapter);
                return Err("could not start the Hebnix LAN adapter session".to_string());
            }
            let instance = Self {
                _library: library,
                adapter,
                session,
                close_adapter,
                end_session,
                receive_packet,
                release_receive_packet,
                allocate_send_packet,
                send_packet,
            };
            if let Err(error) = configure_address(adapter_name, address) {
                drop(instance);
                return Err(error);
            }
            Ok(instance)
        }
    }

    pub fn try_receive(&self) -> Option<Vec<u8>> {
        unsafe {
            let mut size = 0;
            let packet = (self.receive_packet)(self.session, &mut size);
            if packet.is_null() || size == 0 {
                return None;
            }
            let bytes = std::slice::from_raw_parts(packet, size as usize).to_vec();
            (self.release_receive_packet)(self.session, packet);
            Some(bytes)
        }
    }

    pub fn send(&self, packet: &[u8]) -> Result<(), String> {
        if packet.is_empty() || packet.len() > u32::MAX as usize {
            return Err("invalid LAN packet size".to_string());
        }
        unsafe {
            let destination = (self.allocate_send_packet)(self.session, packet.len() as u32);
            if destination.is_null() {
                return Err("Wintun send ring is full".to_string());
            }
            std::ptr::copy_nonoverlapping(packet.as_ptr(), destination, packet.len());
            (self.send_packet)(self.session, destination);
        }
        Ok(())
    }
}

fn configure_address(name: &str, address: &str) -> Result<(), String> {
    let output = Command::new("netsh")
        .args([
            "interface",
            "ipv4",
            "set",
            "address",
            &format!("name={name}"),
            "static",
            address,
            "255.255.255.0",
            "none",
        ])
        .output()
        .map_err(|error| format!("could not configure the Hebnix LAN adapter: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

impl Drop for WintunSession {
    fn drop(&mut self) {
        unsafe {
            (self.end_session)(self.session);
            (self.close_adapter)(self.adapter);
        }
    }
}

fn wintun_path() -> Result<PathBuf, String> {
    super::tap::embedded_wintun_path()
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
fn load_error(error: libloading::Error) -> String {
    format!("missing Wintun API: {error}")
}
