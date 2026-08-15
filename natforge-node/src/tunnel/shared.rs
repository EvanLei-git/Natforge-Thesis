//! Shared-port subdomain routing: HTTP `Host`-header routing on :80 and HTTPS
//! TLS-SNI passthrough routing on :443. Both peek the connection's opening bytes
//! to pick a subdomain, then open a yamux stream to that tunnel's agent and carry
//! the peeked bytes verbatim inside the per-stream preamble's replay field, so the
//! origin service receives a byte-exact request / ClientHello. No TLS is
//! terminated - :443 is pure L4 passthrough (the core never sees plaintext).
//!
//! Routing is per-connection (first Host/SNI wins). HTTP/1.1 keep-alive across
//! different subdomains on one connection is out of scope (matches the L4 model).

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf, copy_bidirectional};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_rustls::TlsAcceptor;
use tokio_util::compat::FuturesAsyncReadCompatExt;
use tracing::{info, warn};

use crate::state::{CoreState, OpenStream, RouteHandle};
use natforge_proto::{RouteMode, encode_preamble};

const PEEK_TIMEOUT: Duration = Duration::from_secs(5);

/// Extract the routing subdomain (leftmost label) from a hostname, stripping any
/// port and trailing dot. "duck-a1b2.natforge.com:8080" -> "duck-a1b2".
fn subdomain_of(host: &str) -> String {
    let host = host
        .split(':')
        .next()
        .unwrap_or(host)
        .trim()
        .trim_end_matches('.');
    host.split('.').next().unwrap_or(host).to_ascii_lowercase()
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A stream that first replays a `prefix` (bytes already read off `inner`) and then
/// delegates to `inner`. Used to hand a consumed TLS ClientHello back to a
/// `TlsAcceptor` when terminating HTTPS for an `http`-mode subdomain, so the peeked
/// bytes are not lost.
struct PrefixedStream<S> {
    prefix: Vec<u8>,
    pos: usize,
    inner: S,
}

impl<S> PrefixedStream<S> {
    fn new(prefix: Vec<u8>, inner: S) -> Self {
        Self {
            prefix,
            pos: 0,
            inner,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for PrefixedStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.pos < this.prefix.len() {
            let remaining = &this.prefix[this.pos..];
            let n = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..n]);
            this.pos += n;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for PrefixedStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
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
                let _ = sock.set_nodelay(true);
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
        // ACME HTTP-01 validation hits `/.well-known/acme-challenge/<token>` on the
        // custom domain (which points at us); answer it before subdomain routing.
        if let Some(token) = acme_challenge_token(&buf) {
            serve_acme_challenge(&state, &mut inbound, &token).await;
            return;
        }
        if let Some(h) = parse_host(&buf) {
            break h;
        }
        if find_subsequence(&buf, b"\r\n\r\n").is_some()
            || buf.len() >= state.config.max_header_bytes
        {
            // Headers complete (or too big) with no usable Host.
            let _ = inbound
                .write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 11\r\n\r\nbad request")
                .await;
            return;
        }
    };
    // The bare apex (and `www.`) is the control-plane dashboard, not a tunnel:
    // forward those to the website backend instead of subdomain-routing them.
    let hostname = host
        .split(':')
        .next()
        .unwrap_or(&host)
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let apex = state.config.public_host.to_ascii_lowercase();
    if hostname == apex || hostname == format!("www.{apex}") {
        proxy_to_dashboard(inbound, buf, &state.config.dashboard_addr).await;
        return;
    }
    // A registered custom hostname wins; otherwise route by the *.apex subdomain.
    let handle = match state.custom_host_route(&hostname, RouteMode::Http).await {
        Some(h) => Some(h),
        None => {
            state
                .host_route(&subdomain_of(&host), RouteMode::Http)
                .await
        }
    };
    match handle {
        Some(h) => {
            let head = with_forwarded_headers(
                &mut inbound,
                buf,
                peer,
                &hostname,
                "http",
                state.config.max_header_bytes,
            )
            .await;
            splice_to_route(state, inbound, peer, head, h, RouteMode::Http).await
        }
        None => {
            let _ = inbound
                .write_all(b"HTTP/1.1 404 Not Found\r\nConnection: close\r\nContent-Length: 18\r\n\r\nunknown tunnel host")
                .await;
        }
    }
}

/// Add `X-Forwarded-{For,Host,Proto}` to the first HTTP request head so the origin app
/// sees the real client IP and the scheme it was reached by (e.g. generates https URLs).
/// Reads the rest of the head from `stream` into `head`, strips any client-supplied
/// `X-Forwarded-*` (they'd be spoofed - the client dials us directly), inserts ours
/// before the blank line, and returns the rewritten head to replay to the origin.
/// Best-effort: on any incomplete/non-UTF8 head it returns the bytes unchanged, so a
/// request is never corrupted. Only the first request on a connection is rewritten.
async fn with_forwarded_headers<S: AsyncRead + Unpin>(
    stream: &mut S,
    mut head: Vec<u8>,
    peer: SocketAddr,
    host: &str,
    scheme: &str,
    max_bytes: usize,
) -> Vec<u8> {
    let mut tmp = [0u8; 2048];
    while find_subsequence(&head, b"\r\n\r\n").is_none() && head.len() < max_bytes {
        match stream.read(&mut tmp).await {
            Ok(0) | Err(_) => return head,
            Ok(n) => head.extend_from_slice(&tmp[..n]),
        }
    }
    let Some(end) = find_subsequence(&head, b"\r\n\r\n") else {
        return head;
    };
    let Ok(text) = std::str::from_utf8(&head[..end]) else {
        return head;
    };
    let mut out = Vec::with_capacity(head.len() + 160);
    for line in text
        .split("\r\n")
        .filter(|l| !l.to_ascii_lowercase().starts_with("x-forwarded-"))
    {
        out.extend_from_slice(line.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(
        format!(
            "X-Forwarded-For: {}\r\nX-Forwarded-Proto: {}\r\nX-Forwarded-Host: {}\r\n",
            peer.ip(),
            scheme,
            host
        )
        .as_bytes(),
    );
    out.extend_from_slice(b"\r\n"); // end of headers
    out.extend_from_slice(&head[end + 4..]); // any body bytes already read
    out
}

/// Forward a plain-HTTP connection (the apex / www host) to the local dashboard
/// (`natforge-backend`). The bytes already peeked for the Host header are replayed
/// first, then the streams are spliced - a simple L4 HTTP proxy, no preamble/yamux.
async fn proxy_to_dashboard(mut inbound: TcpStream, peeked: Vec<u8>, upstream: &str) {
    let mut up = match TcpStream::connect(upstream).await {
        Ok(s) => s,
        Err(e) => {
            warn!("dashboard upstream {upstream} unreachable: {e}");
            let _ = inbound
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\nContent-Length: 17\r\n\r\ndashboard offline")
                .await;
            return;
        }
    };
    if up.write_all(&peeked).await.is_err() {
        return;
    }
    let _ = copy_bidirectional(&mut inbound, &mut up).await;
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

/// If the buffered request line is `GET /.well-known/acme-challenge/<token> ...`,
/// return the token (once the request line is complete).
fn acme_challenge_token(buf: &[u8]) -> Option<String> {
    const PREFIX: &[u8] = b"/.well-known/acme-challenge/";
    let line_end = find_subsequence(buf, b"\r\n")?;
    let mut parts = buf[..line_end].split(|&b| b == b' ');
    let _method = parts.next()?;
    let path = parts.next()?;
    let token = path.strip_prefix(PREFIX)?;
    std::str::from_utf8(token).ok().map(str::to_string)
}

/// Serve an ACME HTTP-01 key authorization (or 404 if the token is unknown).
async fn serve_acme_challenge(state: &Arc<CoreState>, inbound: &mut TcpStream, token: &str) {
    match state.acme.challenge_response(token).await {
        Some(resp) => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                resp.len()
            );
            let _ = inbound.write_all(head.as_bytes()).await;
            let _ = inbound.write_all(resp.as_bytes()).await;
        }
        None => {
            let _ = inbound
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                )
                .await;
        }
    }
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
                let _ = sock.set_nodelay(true);
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
    let sni_host = sni
        .split(':')
        .next()
        .unwrap_or(&sni)
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let sub = subdomain_of(&sni);
    // An explicit `https` route means the user terminates TLS themselves: pass the
    // encrypted stream through untouched (a custom hostname wins over the subdomain).
    let passthrough = match state.custom_host_route(&sni_host, RouteMode::Https).await {
        Some(h) => Some(h),
        None => state.host_route(&sub, RouteMode::Https).await,
    };
    if let Some(h) = passthrough {
        splice_to_route(state, inbound, peer, buf, h, RouteMode::Https).await;
        return;
    }
    // Otherwise terminate TLS and forward plain HTTP to the agent, if we hold a cert
    // for this name: the `*.<apex>` wildcard for a subdomain http route, or a
    // per-domain ACME cert for a custom-domain http route. Decide first so the
    // buffered ClientHello + socket are moved exactly once.
    let terminate: Option<(TlsAcceptor, RouteHandle)> = if let (Some(acc), Some(h)) = (
        state.wildcard_acceptor().await,
        state.host_route(&sub, RouteMode::Http).await,
    ) {
        Some((acc, h))
    } else if state.acme.has_cert(&sni_host) {
        state
            .custom_host_route(&sni_host, RouteMode::Http)
            .await
            .map(|h| (state.acme.acceptor(), h))
    } else {
        None
    };
    if let Some((acceptor, h)) = terminate {
        let prefixed = PrefixedStream::new(buf, inbound);
        match acceptor.accept(prefixed).await {
            Ok(mut tls) => {
                let head = with_forwarded_headers(
                    &mut tls,
                    Vec::new(),
                    peer,
                    &sni_host,
                    "https",
                    state.config.max_header_bytes,
                )
                .await;
                splice_to_route(state, tls, peer, head, h, RouteMode::Http).await
            }
            Err(e) => warn!("TLS termination failed for '{sni_host}': {e}"),
        }
    }
    // No matching route or cert: close. We cannot send an application error before
    // completing the TLS handshake, so a silent close is the only option.
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

async fn splice_to_route<S>(
    state: Arc<CoreState>,
    mut inbound: S,
    peer: SocketAddr,
    peeked: Vec<u8>,
    handle: RouteHandle,
    mode: RouteMode,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let peer_ip = peer.ip().to_string();
    let country = state.geo.country(peer.ip());
    // Geo-policy: drop (and log) connections from blocked countries. Under TLS we
    // cannot send a friendly error, so we simply close - matching the L4 model.
    if state
        .is_country_blocked(handle.tunnel_id, country.as_deref())
        .await
    {
        if mode == RouteMode::Http {
            let _ = inbound
                .write_all(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 16\r\n\r\nregion is blocked")
                .await;
        }
        crate::reporter::report_conn_log(
            &state,
            &handle,
            &peer_ip,
            country.as_deref(),
            0,
            0,
            0,
            true,
        )
        .await;
        return;
    }

    let (reply_tx, reply_rx) = oneshot::channel();
    if handle
        .open_tx
        .send(OpenStream { reply: reply_tx })
        .await
        .is_err()
    {
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
            handle
                .stats
                .bytes_out
                .fetch_add(from_agent, Ordering::Relaxed);
            crate::reporter::report_conn_log(
                &state,
                &handle,
                &peer_ip,
                country.as_deref(),
                bytes_in,
                from_agent,
                start.elapsed().as_millis(),
                false,
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
        hs.extend_from_slice(&[
            (body.len() >> 16) as u8,
            (body.len() >> 8) as u8,
            body.len() as u8,
        ]);
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
        assert_eq!(
            parse_host(b"GET / HTTP/1.1\r\nHost: a.b.com\r\n\r\n").as_deref(),
            Some("a.b.com")
        );
        assert_eq!(
            parse_host(b"GET / HTTP/1.1\r\nhOsT:  x.y.z:8080  \r\n").as_deref(),
            Some("x.y.z:8080")
        );
        assert_eq!(parse_host(b"GET / HTTP/1.1\r\nUser-Agent: c\r\n\r\n"), None);
    }

    #[test]
    fn subdomain_extraction() {
        assert_eq!(subdomain_of("duck-a1b2.natforge.com:8080"), "duck-a1b2");
        assert_eq!(subdomain_of("X.Y.Z"), "x");
        assert_eq!(subdomain_of("solo"), "solo");
    }

    #[test]
    fn acme_challenge_token_parse() {
        assert_eq!(
            acme_challenge_token(
                b"GET /.well-known/acme-challenge/tok123 HTTP/1.1\r\nHost: play.example.com\r\n"
            )
            .as_deref(),
            Some("tok123")
        );
        assert_eq!(acme_challenge_token(b"GET / HTTP/1.1\r\n"), None);
        // request line not yet complete (no CRLF)
        assert_eq!(
            acme_challenge_token(b"GET /.well-known/acme-challenge/ab"),
            None
        );
    }
}
