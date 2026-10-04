//! tiny http listener on the crl port, hands out the empty crl. RL asks for it
//! mid handshake, without it the handshake just dies.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::Sender;

use crate::messages::AppMsg;
use crate::spoofer::ca::CRL_PORT;

pub struct CrlServer {
    running: Arc<AtomicBool>,
    port: u16,
}

impl CrlServer {
    pub fn start(crl_der: Vec<u8>, tx: Sender<AppMsg>) -> Result<Self, String> {
        Self::start_on(&format!("127.0.0.1:{CRL_PORT}"), crl_der, tx).map(|(s, _)| s)
    }

    /// port 0 picks a free one, thats what the tests use
    fn start_on(addr: &str, crl_der: Vec<u8>, tx: Sender<AppMsg>) -> Result<(Self, u16), String> {
        let listener =
            TcpListener::bind(addr).map_err(|e| format!("cant bind crl port {addr}: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let running = Arc::new(AtomicBool::new(true));
        let crl = Arc::new(crl_der);

        {
            let running = Arc::clone(&running);
            std::thread::Builder::new()
                .name("spoofer-crl".into())
                .spawn(move || {
                    for stream in listener.incoming() {
                        if !running.load(Ordering::Relaxed) {
                            break;
                        }
                        if let Ok(s) = stream {
                            handle(s, &crl, &tx);
                        }
                    }
                })
                .map_err(|e| format!("cant spawn crl thread: {e}"))?;
        }

        Ok((Self { running, port }, port))
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        let _ = TcpStream::connect(format!("127.0.0.1:{}", self.port));
    }
}

fn handle(mut stream: TcpStream, crl: &[u8], tx: &Sender<AppMsg>) {
    let mut buf = [0u8; 1024];
    let _ = stream.read(&mut buf);
    let _ = tx.send(AppMsg::Log(format!(
        "[Spoofer] served CRL ({} bytes) for revocation check",
        crl.len()
    )));

    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/pkix-crl\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        crl.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(crl);
    let _ = stream.flush();
}
