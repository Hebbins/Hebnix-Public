// crates/hebnix-app/src/spoofer/proxy.rs
//! http proxy. mitms only the hosts a rule asks for and blind tunnels the rest.
//! thread per connection, no async, same shape as monitor.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};

use crossbeam_channel::Sender;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::{ServerConfig, ServerConnection, StreamOwned};

use crate::messages::AppMsg;
use crate::spoofer::ca::Ca;
use crate::spoofer::rules::{Body, Rule};

pub fn serve_one(
    mut tls: StreamOwned<ServerConnection, TcpStream>,
    upstream: &ureq::Agent,
    rules: &Arc<Vec<Box<dyn Rule>>>,
    tx: &Sender<AppMsg>,
    host: &str,
) -> Result<(), String> {
    let req = read_http_request(&mut tls)?;
    let matching: Vec<&Box<dyn Rule>> = rules.iter().filter(|r| r.matches_host(host)).collect();
    let upstream_host = matching
        .iter()
        .find_map(|rule| rule.upstream_host(host, &req.path))
        .unwrap_or(host);
    // Requests received by the HTTPS proxy are normally origin-form, but some
    // Rocket League clients send an absolute URI.  Always rebuild that URI with
    // the selected upstream host: config.psynet.gg is redirected locally, while
    // its /rpc and /Services calls must be released to api.rlpp.psynet.gg.
    let path_and_query = absolute_uri_path(&req.path);
    let url = format!("https://{upstream_host}{path_and_query}");
    let mut call = upstream.request(&req.method, &url);
    for (name, value) in &req.headers {
        let low = name.to_ascii_lowercase();
        if SKIP_REQ_HEADERS.contains(&low.as_str()) {
            continue;
        }
        if matching
            .iter()
            .any(|r| r.strip_request_headers().contains(&low.as_str()))
        {
            continue;
        }
        call = call.set(name, value);
    }

    let res = if req.body.is_empty() {
        call.call()
    } else {
        call.send_bytes(&req.body)
    };

    let (status, status_text, mut headers, mut bytes, ctype) = match res {
        Ok(r) => read_ureq_response(r),
        Err(ureq::Error::Status(code, r)) => {
            let text = r.status_text().to_string();
            let (_, _, h, b, ct) = read_ureq_response(r);
            (code, text, h, b, ct)
        }
        Err(e) => return Err(format!("upstream {url}: {e}")),
    };

    let mut body = Body::new(&ctype, std::mem::take(&mut bytes));
    body.request_path = Some(&req.path);
    body.response_headers = headers.clone();
    for r in &matching {
        if r.rewrite(&mut body) {
            tracing::debug!("rewrote {host}{}", req.path);
            if let Some(msg) = r.announce() {
                let _ = tx.send(AppMsg::Log(format!("[Spoofer] {msg}")));
            }
        }
    }

    headers.retain(|(n, _)| {
        let l = n.to_ascii_lowercase();
        l != "content-length"
            && l != "transfer-encoding"
            && l != "content-encoding"
            && l != "connection"
    });

    for (name, value) in &body.set_headers {
        let low = name.to_ascii_lowercase();
        headers.retain(|(n, _)| n.to_ascii_lowercase() != low);
        headers.push((name.clone(), value.clone()));
    }

    let mut out = format!("HTTP/1.1 {status} {status_text}\r\n");
    for (n, v) in &headers {
        out.push_str(&format!("{n}: {v}\r\n"));
    }
    out.push_str(&format!("Content-Length: {}\r\n", body.bytes.len()));
    out.push_str("Connection: close\r\n\r\n");

    tls.write_all(out.as_bytes())
        .map_err(|e| format!("write head: {e}"))?;
    tls.write_all(&body.bytes)
        .map_err(|e| format!("write body: {e}"))?;
    let _ = tls.flush();
    Ok(())
}

fn absolute_uri_path(target: &str) -> &str {
    for scheme in ["https://", "http://"] {
        if let Some(authority_and_path) = target.strip_prefix(scheme) {
            return authority_and_path
                .find('/')
                .map(|index| &authority_and_path[index..])
                .unwrap_or("/");
        }
    }

    if target.starts_with('/') { target } else { "/" }
}

#[cfg(test)]
mod tests {
    use super::absolute_uri_path;

    #[test]
    fn absolute_request_target_keeps_only_path_and_query() {
        assert_eq!(
            absolute_uri_path("https://config.psynet.gg/rpc/Player/GetPlayerSkills?x=1"),
            "/rpc/Player/GetPlayerSkills?x=1"
        );
    }

    #[test]
    fn origin_form_target_is_unchanged() {
        assert_eq!(
            absolute_uri_path("/Services/v1/config"),
            "/Services/v1/config"
        );
    }
}

const SKIP_REQ_HEADERS: [&str; 5] = [
    "host",
    "content-length",
    "connection",
    "proxy-connection",
    "accept-encoding",
];

pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub fn read_http_request<S: Read>(stream: &mut S) -> Result<HttpRequest, String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        let n = stream
            .read(&mut tmp)
            .map_err(|e| format!("read req: {e}"))?;
        if n == 0 {
            return Err("client closed before headers".into());
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > 65536 {
            return Err("request headers too big".into());
        }
    };

    let mut header_storage = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut header_storage);
    let parsed = req
        .parse(&buf[..header_end + 4])
        .map_err(|e| format!("parse req: {e}"))?;
    if parsed.is_partial() {
        return Err("incomplete request".into());
    }

    let method = req.method.unwrap_or("GET").to_string();
    let path = req.path.unwrap_or("/").to_string();
    let mut headers = Vec::new();
    let mut content_length = 0usize;
    for h in req.headers.iter() {
        let value = String::from_utf8_lossy(h.value).to_string();
        if h.name.eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse().unwrap_or(0);
        }
        headers.push((h.name.to_string(), value));
    }

    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_length {
        let n = stream
            .read(&mut tmp)
            .map_err(|e| format!("read body: {e}"))?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }

    Ok(HttpRequest {
        method,
        path,
        headers,
        body,
    })
}

pub fn read_ureq_response(
    r: ureq::Response,
) -> (u16, String, Vec<(String, String)>, Vec<u8>, String) {
    let status = r.status();
    let status_text = r.status_text().to_string();
    let ctype = r.content_type().to_string();
    let names = r.headers_names();
    let mut headers = Vec::new();
    for name in names {
        if let Some(v) = r.header(&name) {
            headers.push((name.clone(), v.to_string()));
        }
    }
    let mut bytes = Vec::new();
    let _ = r
        .into_reader()
        .take(32 * 1024 * 1024)
        .read_to_end(&mut bytes);
    (status, status_text, headers, bytes, ctype)
}

pub fn build_server_config(resolver: Arc<CertResolver>) -> Result<Arc<ServerConfig>, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("tls versions: {e}"))?
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    Ok(Arc::new(config))
}

pub fn build_upstream_agent_pins(pins: HashMap<String, std::net::Ipv4Addr>) -> ureq::Agent {
    let builder = ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .timeout(std::time::Duration::from_secs(15));
    if pins.is_empty() {
        return builder.build();
    }
    builder
        .resolver(move |netloc: &str| {
            let (name, port) = netloc.rsplit_once(':').unwrap_or((netloc, "443"));
            if let Some(ip) = pins
                .iter()
                .find_map(|(host, ip)| name.eq_ignore_ascii_case(host).then_some(*ip))
            {
                let port: u16 = port.parse().unwrap_or(443);
                return Ok(vec![std::net::SocketAddr::from((ip, port))]);
            }
            use std::net::ToSocketAddrs;
            netloc.to_socket_addrs().map(|i| i.collect())
        })
        .build()
}

pub struct CertResolver {
    ca: Arc<Ca>,
    cache: Mutex<HashMap<String, Arc<CertifiedKey>>>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl std::fmt::Debug for CertResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CertResolver").finish_non_exhaustive()
    }
}

impl CertResolver {
    pub fn new(ca: Arc<Ca>) -> Self {
        Self {
            ca,
            cache: Mutex::new(HashMap::new()),
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        }
    }

    fn leaf_for(&self, host: &str) -> Option<Arc<CertifiedKey>> {
        if let Some(hit) = self.cache.lock().ok()?.get(host) {
            return Some(Arc::clone(hit));
        }
        let leaf = self.ca.sign_leaf(host).ok()?;
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf.key_der));
        let signing_key = self.provider.key_provider.load_private_key(key_der).ok()?;
        let certified = Arc::new(CertifiedKey::new(
            vec![CertificateDer::from(leaf.cert_der)],
            signing_key,
        ));
        self.cache
            .lock()
            .ok()?
            .insert(host.to_string(), Arc::clone(&certified));
        Some(certified)
    }
}

impl ResolvesServerCert for CertResolver {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.leaf_for(hello.server_name()?)
    }
}
