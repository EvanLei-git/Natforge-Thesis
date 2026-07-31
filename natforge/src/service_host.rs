//! Service Host mode.
//!
//! Reserves a (possibly multi-route) tunnel from the control plane, then maintains
//! a persistent yamux session to the core proxy. The agent is the yamux *server*:
//! it accepts one inbound stream per public connection, reads the per-stream
//! preamble to learn which route the stream belongs to, dials the matching local
//! port, replays any peeked bytes, and copies bidirectionally - all in memory.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use futures_util::future::poll_fn;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, copy_bidirectional, split};
use tokio::net::{TcpStream, UdpSocket};
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use tracing::{error, info, warn};
use yamux::{Config as YamuxConfig, Connection, Mode};

// Handshake + preamble wire contract lives in the shared `natforge-proto` crate so
// the agent and core can never drift; the agent only *reads* preambles (the core
// writes them) and exchanges two length-prefixed JSON frames before the yamux upgrade.
use natforge_proto::{
    AgentHello, AgentRouteBinding, CoreReply, RouteMode, read_datagram, read_preamble,
    write_datagram,
};

const MAX_FRAME: u32 = 1 << 20;

async fn read_frame<S: AsyncRead + Unpin>(stream: &mut S) -> anyhow::Result<Vec<u8>> {
    let len = stream.read_u32().await?;
    if len > MAX_FRAME {
        anyhow::bail!("frame too large ({len} bytes)");
    }
    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf).await?;
    Ok(buf)
}

async fn write_frame<S: AsyncWrite + Unpin>(stream: &mut S, data: &[u8]) -> anyhow::Result<()> {
    stream.write_u32(data.len() as u32).await?;
    stream.write_all(data).await?;
    stream.flush().await?;
    Ok(())
}

/// A route the user asked this agent to expose.
#[derive(Debug, Clone)]
pub struct RouteSpec {
    pub mode: RouteMode,
    pub local_port: u16,
}

#[derive(Serialize)]
struct RequestedRoute {
    mode: RouteMode,
    local_port: u16,
}

#[derive(Serialize)]
struct RequestTunnelReq {
    routes: Vec<RequestedRoute>,
    /// Optional region/node id the user picked (`--region`); server default otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    node_id: Option<String>,
}

#[derive(Deserialize, Clone)]
#[allow(dead_code)] // public_endpoint is part of the wire shape, shown by the server
struct ReservedRoute {
    route_id: u16,
    mode: RouteMode,
    local_port: u16,
    public_endpoint: String,
}

#[derive(Deserialize, Clone)]
#[allow(dead_code)] // node_id is part of the wire shape, surfaced for diagnostics
struct Reservation {
    tunnel_id: i64,
    subdomain: String,
    full_host: String,
    tunnel_token: String,
    routes: Vec<ReservedRoute>,
    /// host:port of the node hosting this tunnel - where the agent connects.
    control_endpoint: String,
    region: Option<String>,
    node_id: String,
    /// SHA-256 fingerprint of the node's TLS control cert, pinned by the agent.
    control_cert_fingerprint: Option<String>,
}

async fn reserve(
    control_plane: &str,
    session_token: &str,
    specs: &[RouteSpec],
    node_id: Option<&str>,
) -> Result<Reservation> {
    let body = RequestTunnelReq {
        routes: specs
            .iter()
            .map(|s| RequestedRoute {
                mode: s.mode,
                local_port: s.local_port,
            })
            .collect(),
        node_id: node_id.map(|s| s.to_string()),
    };
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{control_plane}/api/tunnels/request"))
        .bearer_auth(session_token)
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let code = resp.status();
        let msg = resp.text().await.unwrap_or_default();
        return Err(anyhow!("tunnel request failed: {code} {msg}"));
    }
    Ok(resp.json().await?)
}

pub async fn run(
    control_plane: &str,
    tunnel_server: Option<&str>,
    region: Option<&str>,
    specs: Vec<RouteSpec>,
    session_token: &str,
) -> Result<()> {
    let mut reservation = reserve(control_plane, session_token, &specs, region).await?;
    info!(
        "reserved tunnel {} '{}' ({}) on region '{}' with {} route(s)",
        reservation.tunnel_id,
        reservation.subdomain,
        reservation.full_host,
        reservation
            .region
            .as_deref()
            .unwrap_or(&reservation.node_id),
        reservation.routes.len()
    );

    loop {
        // Connect to the node hosting the tunnel; --tunnel-server overrides (dev).
        let target = tunnel_server
            .unwrap_or(reservation.control_endpoint.as_str())
            .to_string();
        match connect_and_serve(&target, &reservation).await {
            Ok(()) => warn!("tunnel session ended; reconnecting in 3s…"),
            Err(e) => warn!("tunnel error ({e}); reconnecting in 3s…"),
        }
        tokio::time::sleep(Duration::from_secs(3)).await;

        // Refresh the reservation before every reconnect so control-plane edits -
        // a new subdomain, route labels, or a re-minted token - take effect on a
        // live tunnel. reserve() is idempotent by route signature: it returns the
        // SAME tunnel (same dedicated ports) with the current subdomain and a fresh
        // token, or leaves us on the existing reservation if the control plane is
        // momentarily unreachable.
        match reserve(control_plane, session_token, &specs, region).await {
            Ok(r) => {
                if r.subdomain != reservation.subdomain {
                    info!("subdomain changed to '{}' ({})", r.subdomain, r.full_host);
                }
                reservation = r;
            }
            Err(e) => warn!("re-reserve failed ({e}); retrying with the existing reservation"),
        }
    }
}

async fn connect_and_serve(tunnel_server: &str, reservation: &Reservation) -> Result<()> {
    info!("connecting to core proxy control plane at {tunnel_server} (TLS)");
    let tcp = TcpStream::connect(tunnel_server).await?;
    let fingerprint = reservation
        .control_cert_fingerprint
        .as_deref()
        .ok_or_else(|| anyhow!("reservation did not include a control certificate fingerprint"))?;
    let mut socket = crate::tls::connect(tcp, fingerprint).await?;

    // route_id -> (mode, local_port), learned from the reservation. The mode tells
    // handle_stream whether to bridge a local TCP connection or relay UDP datagrams.
    let routes: Arc<HashMap<u16, (RouteMode, u16)>> = Arc::new(
        reservation
            .routes
            .iter()
            .map(|r| (r.route_id, (r.mode, r.local_port)))
            .collect(),
    );
    let bindings: Vec<AgentRouteBinding> = reservation
        .routes
        .iter()
        .map(|r| AgentRouteBinding {
            route_id: r.route_id,
            local_port: r.local_port,
        })
        .collect();

    let hello = AgentHello {
        tunnel_token: reservation.tunnel_token.clone(),
        role: "service_host".to_string(),
        routes: bindings,
    };
    write_frame(&mut socket, &serde_json::to_vec(&hello)?).await?;

    let reply: CoreReply = serde_json::from_slice(&read_frame(&mut socket).await?)?;
    match reply {
        CoreReply::Ok {
            tunnel_id,
            subdomain,
            routes: confirmed,
        } => {
            info!("════════════════════════════════════════════════════");
            info!(" Tunnel LIVE  (tunnel {tunnel_id}, subdomain {subdomain})");
            for r in &confirmed {
                let local = routes.get(&r.route_id).map(|(_, p)| *p).unwrap_or(0);
                info!(
                    "   route {} [{:?}]  {}  ->  127.0.0.1:{}",
                    r.route_id, r.mode, r.public_endpoint, local
                );
            }
            info!("════════════════════════════════════════════════════");
        }
        CoreReply::Error { message } => {
            return Err(anyhow!("core proxy rejected tunnel: {message}"));
        }
    }

    // Upgrade to a yamux server session.
    let mut conn = Connection::new(socket.compat(), YamuxConfig::default(), Mode::Server);
    loop {
        match poll_fn(|cx| conn.poll_next_inbound(cx)).await {
            Some(Ok(stream)) => {
                tokio::spawn(handle_stream(stream, routes.clone()));
            }
            Some(Err(e)) => {
                warn!("yamux session error: {e}");
                return Ok(());
            }
            None => return Ok(()),
        }
    }
}

/// Bridge one inbound multiplexed stream to its local service, dispatching by the
/// route_id carried in the per-stream preamble. tcp/http/https bridge a local TCP
/// connection; udp relays datagrams to a local UDP socket.
async fn handle_stream(stream: yamux::Stream, routes: Arc<HashMap<u16, (RouteMode, u16)>>) {
    let mut remote = stream.compat();
    let (route_id, _peer, replay) = match read_preamble(&mut remote).await {
        Ok(v) => v,
        Err(e) => {
            warn!("bad stream preamble: {e}");
            return;
        }
    };
    let Some(&(mode, local_port)) = routes.get(&route_id) else {
        warn!("stream for unknown route_id {route_id}");
        return;
    };
    if mode == RouteMode::Udp {
        udp_relay(remote, local_port).await;
        return;
    }
    let mut local = match TcpStream::connect(("127.0.0.1", local_port)).await {
        Ok(s) => s,
        Err(e) => {
            error!("cannot reach local service on 127.0.0.1:{local_port}: {e}");
            return;
        }
    };
    // Replay the bytes the core peeked for routing (HTTP request / TLS ClientHello).
    if !replay.is_empty()
        && let Err(e) = local.write_all(&replay).await
    {
        warn!("failed writing replay to local service: {e}");
        return;
    }
    if let Err(e) = copy_bidirectional(&mut remote, &mut local).await {
        warn!("local relay closed: {e}");
    }
}

/// Relay one udp flow. The core's per-flow stream carries the client's datagrams
/// (length-prefixed) inbound; datagrams the local service replies with go back the
/// same way. A fresh ephemeral local socket per flow preserves the local service's
/// per-client source separation. The flow ends when the core closes the stream.
async fn udp_relay<S: AsyncRead + AsyncWrite + Unpin>(remote: S, local_port: u16) {
    let sock = match UdpSocket::bind("127.0.0.1:0").await {
        Ok(s) => s,
        Err(e) => {
            error!("cannot bind local udp socket: {e}");
            return;
        }
    };
    if let Err(e) = sock.connect(("127.0.0.1", local_port)).await {
        error!("cannot reach local udp service on 127.0.0.1:{local_port}: {e}");
        return;
    }
    let sock = Arc::new(sock);
    let (mut sr, mut sw) = split(remote);

    // core stream -> local service
    let s_in = sock.clone();
    let downstream = async move {
        while let Ok(Some(d)) = read_datagram(&mut sr).await {
            if s_in.send(&d).await.is_err() {
                break;
            }
        }
    };
    // local service -> core stream
    let upstream = async move {
        let mut buf = vec![0u8; 65535];
        while let Ok(n) = sock.recv(&mut buf).await {
            if write_datagram(&mut sw, &buf[..n]).await.is_err() {
                break;
            }
        }
    };
    tokio::select! {
        _ = downstream => {}
        _ = upstream => {}
    }
}
