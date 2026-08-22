// crates/hebnix-app/src/spoofer/socket.rs
//! socket proxy. reverse is 443 after a hosts redirect

use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::Sender;
use rustls::{ServerConfig, ServerConnection, StreamOwned};

use crate::messages::AppMsg;
use crate::spoofer::ca::Ca;
use crate::spoofer::proxy::{
    CertResolver, build_server_config, build_upstream_agent_pins, serve_one,
};
use crate::spoofer::rules::Rule;

// The hosts redirect is explicitly 127.0.0.1, so match the C# relay and do
// not expose the interception endpoint on the local network.
pub const REVERSE_ADDR: &str = "127.0.0.1:443";

pub struct SocketProxy {
    running: Arc<AtomicBool>,
    addr: String,
}

impl SocketProxy {
    pub fn start(
        ca: Arc<Ca>,
        rules: Arc<Vec<Box<dyn Rule>>>,
        tx: Sender<AppMsg>,
        real_ips: std::collections::HashMap<String, std::net::Ipv4Addr>,
    ) -> Result<Self, String> {
        let addr = REVERSE_ADDR.to_string();
        let listener = TcpListener::bind(&addr).map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied
                || e.kind() == std::io::ErrorKind::AddrInUse
            {
                format!("cant bind {addr}, something else is using it: {e}")
            } else {
                format!("cant bind {addr}: {e}")
            }
        })?;
        let running = Arc::new(AtomicBool::new(true));

        let resolver = Arc::new(CertResolver::new(ca));
        let server_config = build_server_config(resolver)?;
        let upstream = build_upstream_agent_pins(real_ips);

        {
            let running = Arc::clone(&running);
            let addr_clone = addr.clone();
            std::thread::Builder::new()
                .name("spoofer-socket".into())
                .spawn(move || {
                    accept_loop(
                        listener,
                        &running,
                        &server_config,
                        &upstream,
                        &rules,
                        &tx,
                        &addr_clone,
                    )
                })
                .map_err(|e| format!("cant spawn socket proxy thread: {e}"))?;
        }

        Ok(Self { running, addr })
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        let _ = TcpStream::connect(&self.addr);
    }
}

fn accept_loop(
    listener: TcpListener,
    running: &AtomicBool,
    server_config: &Arc<ServerConfig>,
    upstream: &ureq::Agent,
    rules: &Arc<Vec<Box<dyn Rule>>>,
    tx: &Sender<AppMsg>,
    addr: &str,
) {
    tracing::info!("spoofer socket proxy listening on {addr}");
    for stream in listener.incoming() {
        if !running.load(Ordering::Relaxed) {
            break;
        }
        let Ok(stream) = stream else { continue };

        let server_config = Arc::clone(server_config);
        let upstream = upstream.clone();
        let rules = Arc::clone(rules);
        let tx = tx.clone();
        std::thread::spawn(move || {
            if let Err(e) = handle_reverse(stream, &server_config, &upstream, &rules, &tx) {
                tracing::debug!("socket proxy conn ended: {e}");
            }
        });
    }
    tracing::info!("spoofer socket proxy stopped");
}

fn handle_reverse(
    client: TcpStream,
    server_config: &Arc<ServerConfig>,
    upstream: &ureq::Agent,
    rules: &Arc<Vec<Box<dyn Rule>>>,
    tx: &Sender<AppMsg>,
) -> Result<(), String> {
    let mut conn =
        ServerConnection::new(Arc::clone(server_config)).map_err(|e| format!("tls conn: {e}"))?;
    let mut sock = client;
    while conn.is_handshaking() {
        conn.complete_io(&mut sock)
            .map_err(|e| format!("tls handshake: {e}"))?;
    }
    let host = conn
        .server_name()
        .map(str::to_string)
        .ok_or("no sni on the reverse connection")?;
    tracing::debug!("reverse {host}");

    let tls = StreamOwned::new(conn, sock);
    serve_one(tls, upstream, rules, tx, &host)
}
