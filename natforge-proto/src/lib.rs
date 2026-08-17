//! NatForge shared wire protocol.
//!
//! This crate is the single source of truth for the contract between the data
//! plane (`natforge-node`) and the agent (`natforge`), plus the tunnel-token
//! claims the control plane (`natforge-backend`) mints and the data plane verifies.
//! Putting all of it here guarantees the three components cannot drift.
//!
//! Three things live here:
//!   1. `RouteMode` + the control-plane handshake structs (`AgentHello`/`CoreReply`).
//!   2. The signed `TunnelClaims` (and `RouteClaim`) embedded in the tunnel JWT.
//!   3. The per-stream binary preamble codec (`encode_preamble`/`read_preamble`)
//!      written by the core at the start of every yamux stream so the agent knows
//!      which route the stream is for and can replay any peeked bytes verbatim.

use std::net::{IpAddr, SocketAddr};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

// ---------------------------------------------------------------------------
// Route mode
// ---------------------------------------------------------------------------

/// How a route's public traffic is matched and delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteMode {
    /// HTTP, matched by the `Host` header on the shared :80 listener.
    Http,
    /// HTTPS, matched by TLS SNI on the shared :443 listener (passthrough, no termination).
    Https,
    /// Raw TCP, matched by a dedicated public port from the pool.
    Tcp,
    /// Raw UDP, matched by a dedicated public port from the pool. Each client flow
    /// rides a per-flow yamux stream, datagrams length-prefixed (see
    /// `write_datagram` / `read_datagram`).
    Udp,
    /// Raw TCP **and** UDP on one dedicated public port (for game servers that use the
    /// same port for both). This is a reservation/request-level mode only: the control
    /// plane expands it into a `Tcp` claim and a `Udp` claim sharing the one pooled
    /// port, so it never appears in a `RouteClaim` or on the wire, and the data plane
    /// and agent handle the two halves exactly like independent tcp/udp routes.
    Both,
}

impl RouteMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            RouteMode::Http => "http",
            RouteMode::Https => "https",
            RouteMode::Tcp => "tcp",
            RouteMode::Udp => "udp",
            RouteMode::Both => "both",
        }
    }
    /// http/https share one subdomain and need no dedicated port.
    pub fn is_host_routed(&self) -> bool {
        matches!(self, RouteMode::Http | RouteMode::Https)
    }
}

// ---------------------------------------------------------------------------
// Control-plane handshake (length-prefixed JSON, one frame each direction)
// ---------------------------------------------------------------------------

/// One route the agent wants to serve: maps a signed `route_id` to a local port.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRouteBinding {
    pub route_id: u16,
    pub local_port: u16,
}

/// The only role an agent announces today: a persistent host serving one or more
/// local services. Kept as a shared constant so the agent (which sends it) and the
/// node (which checks it) can never drift apart on the spelling.
pub const ROLE_SERVICE_HOST: &str = "service_host";

/// Frame 1: agent -> core, immediately after connecting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHello {
    pub tunnel_token: String,
    pub role: String, // ROLE_SERVICE_HOST
    pub routes: Vec<AgentRouteBinding>,
}

/// One route as confirmed by the core, with the public endpoint to advertise.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteResult {
    pub route_id: u16,
    pub mode: RouteMode,
    pub public_endpoint: String,
}

/// Frame 2: core -> agent, before the socket is upgraded to yamux.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CoreReply {
    Ok {
        tunnel_id: i64,
        subdomain: String,
        routes: Vec<RouteResult>,
    },
    Error {
        message: String,
    },
}

// ---------------------------------------------------------------------------
// Signed tunnel-token claims (HS256). Minted by the control plane, verified by
// the data plane - both use these exact structs, so they cannot disagree.
// ---------------------------------------------------------------------------

/// One authorized route inside the tunnel token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteClaim {
    pub route_id: u16,
    pub mode: RouteMode,
    /// Some("sub.natforge.com") for http/https; None for tcp.
    pub host: Option<String>,
    /// Some(pool port) for tcp; None for http/https.
    pub public_port: Option<u16>,
    /// Optional SRV service label (e.g. "minecraft"); when set, the data plane
    /// provisions `_<service>._<proto>.<subdomain>`. `#[serde(default)]` keeps older
    /// tokens (issued before this field) deserializing as `None`.
    #[serde(default)]
    pub srv_service: Option<String>,
}

/// The full set of claims carried by a tunnel token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelClaims {
    /// Schema version (== 1).
    pub v: u8,
    /// Owning user id (kept named `sub` for JWT convention).
    pub sub: i32,
    /// Durable tunnel primary key.
    pub tunnel_id: i64,
    /// Reserved subdomain (always allocated, even for tcp-only tunnels).
    pub subdomain: String,
    /// Token purpose discriminator ("tunnel").
    pub purpose: String,
    pub routes: Vec<RouteClaim>,
    /// Optional user-owned hostname fronting this tunnel (e.g. play.mygame.com),
    /// in addition to the assigned subdomain. Absent in older tokens (serde default).
    #[serde(default)]
    pub custom_domain: Option<String>,
    /// Expiry (unix seconds).
    pub exp: usize,
}

impl TunnelClaims {
    /// Structural validation independent of signature: version, purpose, and the
    /// per-mode invariants (http/https carry a host and no port; tcp the reverse),
    /// plus route_id uniqueness.
    pub fn validate_shape(&self) -> Result<(), String> {
        if self.v != 1 {
            return Err(format!("unsupported token version {}", self.v));
        }
        if self.purpose != "tunnel" {
            return Err("token is not a tunnel token".into());
        }
        let mut seen = std::collections::HashSet::new();
        for r in &self.routes {
            if !seen.insert(r.route_id) {
                return Err(format!("duplicate route_id {}", r.route_id));
            }
            match r.mode {
                RouteMode::Http | RouteMode::Https => {
                    if r.host.is_none() || r.public_port.is_some() {
                        return Err(format!(
                            "route {} (http/https) must carry host and no port",
                            r.route_id
                        ));
                    }
                }
                // Both is expanded into tcp+udp claims before a token is minted, so it
                // should never reach here; treat it like tcp/udp defensively.
                RouteMode::Tcp | RouteMode::Udp | RouteMode::Both => {
                    if r.public_port.is_none() || r.host.is_some() {
                        return Err(format!(
                            "route {} (tcp/udp) must carry port and no host",
                            r.route_id
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Per-stream binary preamble
// ---------------------------------------------------------------------------
//
// Layout (all multi-byte integers big-endian):
//   off  size  field
//   0    4     magic = b"NFS1"
//   4    1     version = 1
//   5    2     route_id : u16
//   7    1     client_addr_kind : 0=none, 4=IPv4, 6=IPv6
//   8    L     client_ip bytes (L = 0 / 4 / 16)
//   8+L  2     client_port : u16
//   10+L 2     replay_len : u16
//   12+L R     replay bytes (the peeked HTTP request / TLS ClientHello; 0 for tcp)
//   then       live bidirectional traffic

pub const STREAM_MAGIC: &[u8; 4] = b"NFS1";
pub const STREAM_VERSION: u8 = 1;
/// Replay is bounded by the peek caps (<= 16 KiB), always within u16.
pub const MAX_REPLAY: usize = u16::MAX as usize;

/// Encode the per-stream preamble the core writes before any payload.
pub fn encode_preamble(route_id: u16, peer: Option<SocketAddr>, replay: &[u8]) -> Vec<u8> {
    debug_assert!(replay.len() <= MAX_REPLAY, "replay exceeds u16");
    let mut b = Vec::with_capacity(16 + replay.len());
    b.extend_from_slice(STREAM_MAGIC);
    b.push(STREAM_VERSION);
    b.extend_from_slice(&route_id.to_be_bytes());
    match peer.map(|p| p.ip()) {
        Some(IpAddr::V4(v4)) => {
            b.push(4);
            b.extend_from_slice(&v4.octets());
        }
        Some(IpAddr::V6(v6)) => {
            b.push(6);
            b.extend_from_slice(&v6.octets());
        }
        None => b.push(0),
    }
    b.extend_from_slice(&peer.map(|p| p.port()).unwrap_or(0).to_be_bytes());
    let rl = replay.len().min(MAX_REPLAY) as u16;
    b.extend_from_slice(&rl.to_be_bytes());
    b.extend_from_slice(&replay[..rl as usize]);
    b
}

/// Read and validate the per-stream preamble. Returns the route id, the original
/// public peer (if known), and any replay bytes to be written to the local service
/// before bidirectional copying begins.
pub async fn read_preamble<R: AsyncRead + Unpin>(
    r: &mut R,
) -> anyhow::Result<(u16, Option<SocketAddr>, Vec<u8>)> {
    let mut hdr = [0u8; 7];
    r.read_exact(&mut hdr).await?;
    anyhow::ensure!(&hdr[0..4] == STREAM_MAGIC, "bad stream magic");
    anyhow::ensure!(
        hdr[4] == STREAM_VERSION,
        "unsupported stream version {}",
        hdr[4]
    );
    let route_id = u16::from_be_bytes([hdr[5], hdr[6]]);

    let mut kind = [0u8; 1];
    r.read_exact(&mut kind).await?;
    let ip = match kind[0] {
        4 => {
            let mut o = [0u8; 4];
            r.read_exact(&mut o).await?;
            Some(IpAddr::from(o))
        }
        6 => {
            let mut o = [0u8; 16];
            r.read_exact(&mut o).await?;
            Some(IpAddr::from(o))
        }
        0 => None,
        other => anyhow::bail!("bad client_addr_kind {other}"),
    };
    let mut p = [0u8; 2];
    r.read_exact(&mut p).await?;
    let port = u16::from_be_bytes(p);

    let mut rl = [0u8; 2];
    r.read_exact(&mut rl).await?;
    let replay_len = u16::from_be_bytes(rl) as usize;
    // Bounded by u16 on the wire; assert the explicit cap defensively before allocating.
    anyhow::ensure!(
        replay_len <= MAX_REPLAY,
        "replay_len {replay_len} exceeds cap"
    );
    let mut replay = vec![0u8; replay_len];
    if replay_len > 0 {
        r.read_exact(&mut replay).await?;
    }
    let peer = ip.map(|i| SocketAddr::new(i, port));
    Ok((route_id, peer, replay))
}

// ---------------------------------------------------------------------------
// UDP datagram framing
// ---------------------------------------------------------------------------
//
// A udp route carries each datagram over its per-flow yamux stream (after the
// preamble), length-prefixed so datagram boundaries survive the reliable byte
// stream: u16 big-endian length, then the payload. UDP payloads are <= 65507
// bytes, always within u16.

/// Largest datagram we frame (bounded by the u16 length prefix).
pub const MAX_DATAGRAM: usize = u16::MAX as usize;

/// Write one length-prefixed datagram, then flush (datagrams are latency-sensitive
/// and must not be held back by the stream's write buffer).
pub async fn write_datagram<W: AsyncWrite + Unpin>(w: &mut W, data: &[u8]) -> std::io::Result<()> {
    let len = data.len().min(MAX_DATAGRAM) as u16;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(&data[..len as usize]).await?;
    w.flush().await?;
    Ok(())
}

/// Read one length-prefixed datagram. Returns `Ok(None)` on a clean end of stream.
pub async fn read_datagram<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    let mut lb = [0u8; 2];
    match r.read_exact(&mut lb).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u16::from_be_bytes(lb) as usize;
    let mut buf = vec![0u8; len];
    if len > 0 {
        r.read_exact(&mut buf).await?;
    }
    Ok(Some(buf))
}

/// Largest handshake/control frame we accept (1 MiB). These frames carry JSON
/// control messages (the agent hello, the core reply), so this is a generous
/// ceiling that still rejects a peer that announces an absurd length.
pub const MAX_FRAME: u32 = 1 << 20;

/// Read one length-prefixed control frame: a big-endian `u32` length followed by
/// that many bytes. Used for the pre-yamux handshake on the control channel.
pub async fn read_frame<S: AsyncRead + Unpin>(stream: &mut S) -> anyhow::Result<Vec<u8>> {
    let len = stream.read_u32().await?;
    if len > MAX_FRAME {
        anyhow::bail!("frame too large ({len} bytes)");
    }
    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Write one length-prefixed control frame, then flush so the peer sees it promptly.
pub async fn write_frame<S: AsyncWrite + Unpin>(stream: &mut S, data: &[u8]) -> anyhow::Result<()> {
    stream.write_u32(data.len() as u32).await?;
    stream.write_all(data).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    async fn roundtrip(route_id: u16, peer: Option<SocketAddr>, replay: &[u8]) {
        let enc = encode_preamble(route_id, peer, replay);
        let mut cur = std::io::Cursor::new(enc);
        let (rid, p, rep) = read_preamble(&mut cur).await.unwrap();
        assert_eq!(rid, route_id);
        assert_eq!(p, peer);
        assert_eq!(rep, replay);
    }

    #[tokio::test]
    async fn preamble_ipv4_with_replay() {
        let peer = Some(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5)),
            51234,
        ));
        roundtrip(
            7,
            peer,
            b"GET / HTTP/1.1\r\nHost: duck-a1b2.natforge.com\r\n\r\n",
        )
        .await;
    }

    #[tokio::test]
    async fn preamble_ipv6_no_replay() {
        let peer = Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443));
        roundtrip(1, peer, b"").await;
    }

    #[tokio::test]
    async fn preamble_no_peer() {
        roundtrip(65535, None, b"abc").await;
    }

    #[tokio::test]
    async fn rejects_bad_magic() {
        let mut cur = std::io::Cursor::new(b"XXXX\x01\x00\x01\x00\x00\x00\x00\x00".to_vec());
        assert!(read_preamble(&mut cur).await.is_err());
    }

    #[tokio::test]
    async fn datagram_roundtrip() {
        for payload in [&b""[..], &b"hello"[..], &vec![0xABu8; 1400][..]] {
            let mut buf: Vec<u8> = Vec::new();
            write_datagram(&mut buf, payload).await.unwrap();
            let mut cur = std::io::Cursor::new(buf);
            let got = read_datagram(&mut cur).await.unwrap();
            assert_eq!(got.as_deref(), Some(payload));
            // clean EOF after the framed datagram
            assert!(read_datagram(&mut cur).await.unwrap().is_none());
        }
    }

    #[test]
    fn claims_shape_validation() {
        let ok = TunnelClaims {
            v: 1,
            sub: 1,
            tunnel_id: 1,
            subdomain: "x".into(),
            purpose: "tunnel".into(),
            routes: vec![
                RouteClaim {
                    route_id: 1,
                    mode: RouteMode::Http,
                    host: Some("x.n.com".into()),
                    public_port: None,
                    srv_service: None,
                },
                RouteClaim {
                    route_id: 2,
                    mode: RouteMode::Tcp,
                    host: None,
                    public_port: Some(20001),
                    srv_service: None,
                },
            ],
            custom_domain: None,
            exp: 9999999999,
        };
        assert!(ok.validate_shape().is_ok());

        let bad = TunnelClaims {
            v: 1,
            sub: 1,
            tunnel_id: 1,
            subdomain: "x".into(),
            purpose: "tunnel".into(),
            routes: vec![RouteClaim {
                route_id: 1,
                mode: RouteMode::Tcp,
                host: Some("x".into()),
                public_port: None,
                srv_service: None,
            }],
            custom_domain: None,
            exp: 9999999999,
        };
        assert!(bad.validate_shape().is_err());
    }
}
