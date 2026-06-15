//! Shared-port subdomain routing: HTTP `Host`-header routing on :80 and HTTPS
//! TLS-SNI passthrough routing on :443. Both peek the connection's opening bytes
//! to pick a subdomain, then open a yamux stream to that tunnel's agent and carry
//! the peeked bytes verbatim inside the per-stream preamble's replay field, so the
//! origin service receives a byte-exact request / ClientHello. No TLS is
//! terminated — :443 is pure L4 passthrough (the core never sees plaintext).
//!
//! Routing is per-connection (first Host/SNI wins). HTTP/1.1 keep-alive across
//! different subdomains on one connection is out of scope (matches the L4 model).

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{copy_bidirectional, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_util::compat::FuturesAsyncReadCompatExt;
use tracing::{info, warn};

use crate::state::{CoreState, OpenStream};
use natforge_proto::{encode_preamble, RouteMode};

const PEEK_TIMEOUT: Duration = Duration::from_secs(5);

/// Extract the routing subdomain (leftmost label) from a hostname, stripping any
/// port and trailing dot. "duck-a1b2.natforge.com:8080" -> "duck-a1b2".
fn subdomain_of(host: &str) -> String {
    let host = host.split(':').next().unwrap_or(host).trim().trim_end_matches('.');
    host.split('.').next().unwrap_or(host).to_ascii_lowercase()
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

// --------------------------------------------------------------------------
// HTTP Host routing
// --------------------------------------------------------------------------

pub async fn run_http(state: Arc<CoreState>) -> anyhow::Result<()> {
    let addr = format!("0.0.0.0:{}", state.config.http_port);
    let listener = TcpListener::bind(&addr).await?;
    info!("HTTP subdomain router listening on {addr}");
    loop {
        match listener.accept().await {
            Ok((sock, peer)) => {
                let st = state.clone();
                tokio::spawn(async move { serve_http(st, sock, peer).await });
            }
            Err(e) => warn!("http accept error: {e}"),
        }
    }
}

async fn serve_http(state: Arc<CoreState>, mut inbound: TcpStream, peer: SocketAddr) {
    let mut buf = Vec::with_capacity(2048);
    let mut tmp = [0u8; 2048];
    let host = loop {
        let n = match tokio::time::timeout(PEEK_TIMEOUT, inbound.read(&mut tmp)).await {
            Ok(Ok(0)) | Err(_) => return,
            Ok(Ok(n)) => n,
            Ok(Err(_)) => return,
        };
        buf.extend_from_slice(&tmp[..n]);
        if let Some(h) = parse_host(&buf) {
            break h;
        }
        if find_subsequence(&buf, b"\r\n\r\n").is_some() || buf.len() >= state.config.max_header_bytes {
            // Headers complete (or too big) with no usable Host.
            let _ = inbound
                .write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 11\r\n\r\nbad request")
                .await;
            return;
        }
    };
    let sub = subdomain_of(&host);
    route_and_splice(state, inbound, peer, buf, sub, RouteMode::Http).await;
}

/// Find the `Host:` header value in raw request bytes (case-insensitive).
fn parse_host(buf: &[u8]) -> Option<String> {
    for line in buf.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.len() >= 5 && line[..5].eq_ignore_ascii_case(b"host:") {
            let val = std::str::from_utf8(&line[5..]).ok()?.trim();
            if val.is_empty() {
                return None;
            }
            return Some(val.to_string());
        }
    }
    None
}

// --------------------------------------------------------------------------
// HTTPS SNI passthrough routing
// --------------------------------------------------------------------------

enum Sni {
    Found(String),
    NeedMore,
    None,
    NotTls,
}

pub async fn run_https(state: Arc<CoreState>) -> anyhow::Result<()> {
    let addr = format!("0.0.0.0:{}", state.config.https_port);
    let listener = TcpListener::bind(&addr).await?;
    info!("HTTPS (SNI passthrough) router listening on {addr}");
    loop {
        match listener.accept().await {
            Ok((sock, peer)) => {
                let st = state.clone();
                tokio::spawn(async move { serve_https(st, sock, peer).await });
            }
            Err(e) => warn!("https accept error: {e}"),
        }
    }
}

async fn serve_https(state: Arc<CoreState>, mut inbound: TcpStream, peer: SocketAddr) {
    let cap = state.config.max_header_bytes.max(16384);
    let mut buf = Vec::with_capacity(2048);
    let mut tmp = [0u8; 4096];
    let sni = loop {
        let n = match tokio::time::timeout(PEEK_TIMEOUT, inbound.read(&mut tmp)).await {
            Ok(Ok(0)) | Err(_) => return,
            Ok(Ok(n)) => n,
            Ok(Err(_)) => return,
        };
        buf.extend_from_slice(&tmp[..n]);
        match parse_sni(&buf) {
            Sni::Found(name) => break name,
            Sni::NeedMore if buf.len() < cap => continue,
            _ => return, // NotTls / None / over cap -> close (cannot 404 under TLS)
        }
    };
    let sub = subdomain_of(&sni);
    route_and_splice(state, inbound, peer, buf, sub, RouteMode::Https).await;
}

/// Bounds-checked TLS ClientHello SNI extractor (single-record ClientHello, which
/// covers curl/openssl/browsers). Returns NeedMore until enough bytes are present.
fn parse_sni(b: &[u8]) -> Sni {
    if b.len() < 5 {
        return Sni::NeedMore;
    }
    if b[0] != 0x16 || b[1] != 0x03 {
        return Sni::NotTls;
    }
    let rec_len = u16::from_be_bytes([b[3], b[4]]) as usize;
    if b.len() < 5 + rec_len {
        return Sni::NeedMore;
    }
    let hs = &b[5..5 + rec_len];
    // Handshake: type(1)=ClientHello(0x01) + length(3) + body
    if hs.len() < 4 || hs[0] != 0x01 {
        return Sni::None;
    }
    let body = &hs[4..];
    let mut p = 0usize;
    // client_version(2) + random(32)
    let need = |p: usize, n: usize, len: usize| p + n <= len;
    if !need(p, 34, body.len()) {
        return Sni::NeedMore;
    }
    p += 34;
    // session_id
    if !need(p, 1, body.len()) {
        return Sni::NeedMore;
    }
    let sid = body[p] as usize;
    p += 1 + sid;
    // cipher_suites
    if !need(p, 2, body.len()) {
        return Sni::NeedMore;
    }
    let cs = u16::from_be_bytes([body[p], body[p + 1]]) as usize;
    p += 2 + cs;
    // compression_methods
    if !need(p, 1, body.len()) {
        return Sni::NeedMore;
    }
    let cm = body[p] as usize;
    p += 1 + cm;
    // extensions
    if !need(p, 2, body.len()) {
        return Sni::None; // no extensions => no SNI
    }
    let ext_total = u16::from_be_bytes([body[p], body[p + 1]]) as usize;
    p += 2;
    // The claimed extension block must actually be present; if not, read more.
    if p + ext_total > body.len() {
        return Sni::NeedMore;
    }
    let ext_end = p + ext_total;
    while p + 4 <= ext_end {
        let etype = u16::from_be_bytes([body[p], body[p + 1]]);
        let elen = u16::from_be_bytes([body[p + 2], body[p + 3]]) as usize;
        p += 4;
        if p + elen > body.len() {
            return Sni::NeedMore;
        }
        if etype == 0x0000 {
            // server_name extension: list_len(2), name_type(1), name_len(2), name
            let ext = &body[p..p + elen];
            if ext.len() < 5 || ext[2] != 0x00 {
                return Sni::None;
            }
            let name_len = u16::from_be_bytes([ext[3], ext[4]]) as usize;
            if 5 + name_len > ext.len() {
                return Sni::None;
            }
            return match std::str::from_utf8(&ext[5..5 + name_len]) {
                Ok(s) => Sni::Found(s.to_string()),
                Err(_) => Sni::None,
            };
        }
        p += elen;
    }
    Sni::None
}

// --------------------------------------------------------------------------
// Shared splice
// --------------------------------------------------------------------------

async fn route_and_splice(
    state: Arc<CoreState>,
    mut inbound: TcpStream,
    peer: SocketAddr,
    peeked: Vec<u8>,
    subdomain: String,
    mode: RouteMode,
) {
    let handle = match state.host_route(&subdomain, mode).await {
        Some(h) => h,
        None => {
            if mode == RouteMode::Http {
                let _ = inbound
                    .write_all(b"HTTP/1.1 404 Not Found\r\nConnection: close\r\nContent-Length: 18\r\n\r\nunknown tunnel host")
                    .await;
            }
            return;
        }
    };

    let peer_ip = peer.ip().to_string();
    if !state.ddos.analyze_connection(&peer_ip).await {
        return;
    }

    let country = state.geo.country(peer.ip());
    // Geo-policy: drop (and log) connections from blocked countries. Under TLS we
    // cannot send a friendly error, so we simply close — matching the L4 model.
    if state.is_country_blocked(handle.tunnel_id, country.as_deref()).await {
        if mode == RouteMode::Http {
            let _ = inbound
                .write_all(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 16\r\n\r\nregion is blocked")
                .await;
        }
        crate::reporter::report_conn_log(&state, &handle, &peer_ip, country.as_deref(), 0, 0, 0, true).await;
        return;
    }

    let (reply_tx, reply_rx) = oneshot::channel();
    if handle.open_tx.send(OpenStream { route_id: handle.route_id, reply: reply_tx }).await.is_err() {
        return;
    }
    let stream = match reply_rx.await {
        Ok(Ok(s)) => s,
        _ => return,
    };
    let mut outbound = stream.compat();
    let preamble = encode_preamble(handle.route_id, Some(peer), &peeked);
    if outbound.write_all(&preamble).await.is_err() {
        return;
    }
    let start = std::time::Instant::now();
    match copy_bidirectional(&mut inbound, &mut outbound).await {
        Ok((to_agent, from_agent)) => {
            // `to_agent` already excludes the peeked bytes (they were consumed before
            // copy_bidirectional); count them once here, not twice.
            let bytes_in = to_agent + peeked.len() as u64;
            handle.stats.bytes_in.fetch_add(bytes_in, Ordering::Relaxed);
            handle.stats.bytes_out.fetch_add(from_agent, Ordering::Relaxed);
            crate::reporter::report_conn_log(
                &state, &handle, &peer_ip, country.as_deref(),
                bytes_in, from_agent, start.elapsed().as_millis(), false,
            )
            .await;
        }
        Err(e) => warn!("relay closed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client_hello(server_name: &str) -> Vec<u8> {
        let name = server_name.as_bytes();
        let mut sni = Vec::new();
        sni.extend_from_slice(&((1 + 2 + name.len()) as u16).to_be_bytes()); // list_len
        sni.push(0x00); // host_name
        sni.extend_from_slice(&(name.len() as u16).to_be_bytes());
        sni.extend_from_slice(name);
        let mut ext = Vec::new();
        ext.extend_from_slice(&0x0000u16.to_be_bytes());
        ext.extend_from_slice(&(sni.len() as u16).to_be_bytes());
        ext.extend_from_slice(&sni);
        let mut body = Vec::new();
        body.extend_from_slice(&[0x03, 0x03]);
        body.extend_from_slice(&[0u8; 32]);
        body.push(0);
        body.extend_from_slice(&2u16.to_be_bytes());
        body.extend_from_slice(&[0x00, 0x2f]);
        body.push(1);
        body.push(0x00);
        body.extend_from_slice(&(ext.len() as u16).to_be_bytes());
        body.extend_from_slice(&ext);
        let mut hs = vec![0x01];
        hs.extend_from_slice(&[(body.len() >> 16) as u8, (body.len() >> 8) as u8, body.len() as u8]);
        hs.extend_from_slice(&body);
        let mut rec = vec![0x16, 0x03, 0x01];
        rec.extend_from_slice(&(hs.len() as u16).to_be_bytes());
        rec.extend_from_slice(&hs);
        rec
    }

    #[test]
    fn sni_found() {
        let h = client_hello("duck-a1b2.natforge.com");
        match parse_sni(&h) {
            Sni::Found(s) => assert_eq!(s, "duck-a1b2.natforge.com"),
            _ => panic!("expected Found"),
        }
    }

    #[test]
    fn sni_truncated_is_need_more() {
        let h = client_hello("x.natforge.com");
        assert!(matches!(parse_sni(&h[..8]), Sni::NeedMore));
    }

    #[test]
    fn sni_non_tls() {
        assert!(matches!(parse_sni(b"GET / HTTP/1.1\r\n"), Sni::NotTls));
    }

    #[test]
    fn host_parsing() {
        assert_eq!(parse_host(b"GET / HTTP/1.1\r\nHost: a.b.com\r\n\r\n").as_deref(), Some("a.b.com"));
        assert_eq!(parse_host(b"GET / HTTP/1.1\r\nhOsT:  x.y.z:8080  \r\n").as_deref(), Some("x.y.z:8080"));
        assert_eq!(parse_host(b"GET / HTTP/1.1\r\nUser-Agent: c\r\n\r\n"), None);
    }

    #[test]
    fn subdomain_extraction() {
        assert_eq!(subdomain_of("duck-a1b2.natforge.com:8080"), "duck-a1b2");
        assert_eq!(subdomain_of("X.Y.Z"), "x");
        assert_eq!(subdomain_of("solo"), "solo");
    }
}
