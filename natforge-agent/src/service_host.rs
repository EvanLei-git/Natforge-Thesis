//! Service Host mode.
//!
//! Reserves a (possibly multi-route) tunnel from the control plane, then maintains
//! a persistent yamux session to the core proxy. The agent is the yamux *server*:
//! it accepts one inbound stream per public connection, reads the per-stream
//! preamble to learn which route the stream belongs to, dials the matching local
//! port, replays any peeked bytes, and copies bidirectionally - all in memory.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use futures_util::future::poll_fn;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, copy_bidirectional, split};
use tokio::net::{TcpStream, UdpSocket};
use tokio::task::JoinHandle;
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use tracing::{error, info, warn};
use yamux::{Config as YamuxConfig, Connection, Mode};

// Handshake + preamble wire contract lives in the shared `natforge-proto` crate so
// the agent and core can never drift; the agent only *reads* preambles (the core
// writes them) and exchanges two length-prefixed JSON frames before the yamux upgrade.
use natforge_proto::{
    AgentHello, AgentRouteBinding, CoreReply, ROLE_SERVICE_HOST, RouteMode, read_datagram,
    read_frame, read_preamble, write_datagram, write_frame,
};

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
    let mut routes = Vec::new();
    for s in specs {
        routes.push(RequestedRoute {
            mode: s.mode,
            local_port: s.local_port,
        });
    }
    let node_id = match node_id {
        Some(s) => Some(s.to_string()),
        None => None,
    };
    let body = RequestTunnelReq { routes, node_id };
    let client = reqwest::Client::new();
    let request = client.post(format!("{control_plane}/api/tunnels/request"));
    let request = request.bearer_auth(session_token);
    let request = request.json(&body);
    let resp = request.send().await?;
    if !resp.status().is_success() {
        let code = resp.status();
        let msg = match resp.text().await {
            Ok(v) => v,
            Err(_) => String::new(),
        };
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
        let endpoint = match tunnel_server {
            Some(v) => v,
            None => reservation.control_endpoint.as_str(),
        };
        let target = endpoint.to_string();
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

/// Run mode for an enrolled device: pull the device's services from the control
/// plane (config-driven, no `--route` flags) and serve **all** of them at once.
///
/// The device is a supervisor: it maintains one serve task per service (keyed by
/// tunnel id), each holding its own control connection to its node. On a short poll
/// it re-fetches the config and reconciles: services newly added get a task, removed
/// services have theirs aborted, and a task whose session has ended is respawned with
/// a fresh reservation. Because a live route edit drops the session on the node, that
/// service's task ends and is respawned with the new port set within a poll cycle, so
/// dashboard edits take effect with no CLI. Serving several services from one machine
/// over a *single* shared control connection is a further optimisation; here each
/// service rides its own connection, which the core already handles.
pub async fn run_device(
    control_plane: &str,
    device_token: &str,
    tunnel_server: Option<&str>,
) -> Result<()> {
    let mut tasks: HashMap<i64, JoinHandle<()>> = HashMap::new();
    let mut was_empty = false;
    loop {
        let services = match fetch_config(control_plane, device_token).await {
            Ok(s) => s,
            Err(e) => {
                warn!("config fetch failed ({e}); retrying in 5s…");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let mut want: HashSet<i64> = HashSet::new();
        for r in &services {
            want.insert(r.tunnel_id);
        }

        // Stop tasks for services that were removed; drop finished tasks so the spawn
        // pass below respawns them with the freshly fetched reservation.
        tasks.retain(|tid, h| {
            if !want.contains(tid) {
                h.abort();
                info!("service {tid} removed; stopped serving it");
                false
            } else {
                !h.is_finished()
            }
        });

        // Spawn a serve task for every service that does not already have a live one.
        for reservation in services {
            if tasks.contains_key(&reservation.tunnel_id) {
                continue;
            }
            let endpoint = match tunnel_server {
                Some(v) => v,
                None => reservation.control_endpoint.as_str(),
            };
            let target = endpoint.to_string();
            info!(
                "serving '{}' ({}) with {} route(s)",
                reservation.subdomain,
                reservation.full_host,
                reservation.routes.len()
            );
            let tid = reservation.tunnel_id;
            let handle = tokio::spawn(async move {
                if let Err(e) = connect_and_serve(&target, &reservation).await {
                    warn!("service '{}' error: {e}", reservation.subdomain);
                }
            });
            tasks.insert(tid, handle);
        }

        if want.is_empty() {
            if !was_empty {
                info!("device has no services yet; add one from the dashboard.");
            }
            was_empty = true;
        } else {
            was_empty = false;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

async fn fetch_config(control_plane: &str, device_token: &str) -> Result<Vec<Reservation>> {
    let client = reqwest::Client::new();
    let request = client.get(format!("{control_plane}/api/devices/me/config"));
    let request = request.bearer_auth(device_token);
    let resp = request.send().await?;
    if !resp.status().is_success() {
        let code = resp.status();
        let msg = match resp.text().await {
            Ok(v) => v,
            Err(_) => String::new(),
        };
        return Err(anyhow!("config fetch failed: {code} {msg}"));
    }
    Ok(resp.json().await?)
}

async fn connect_and_serve(tunnel_server: &str, reservation: &Reservation) -> Result<()> {
    info!("connecting to core proxy control plane at {tunnel_server} (TLS)");
    let tcp = TcpStream::connect(tunnel_server).await?;
    // Keep the control channel liveness-checked. A network or interface change (a cable
    // swap, WiFi drop, roaming) can leave this socket wedged in ESTABLISHED on a source
    // address that no longer exists, stalling the tunnel without ever erroring, so the
    // reconnect loop never fires. TCP keepalive forces the dead path to surface as an
    // error within ~50s, and the outer loop re-reserves and reconnects from the current
    // interface. Mirrors the keepalive the core sets on the accepted side.
    {
        let ka = socket2::TcpKeepalive::new();
        let ka = ka.with_time(std::time::Duration::from_secs(20));
        let ka = ka.with_interval(std::time::Duration::from_secs(10));
        let ka = ka.with_retries(3);
        let _ = socket2::SockRef::from(&tcp).set_tcp_keepalive(&ka);
    }
    // TCP_NODELAY on the control channel, mirroring the core side: flush small yamux
    // frames immediately so interactive traffic stays low-latency.
    let _ = tcp.set_nodelay(true);
    let fingerprint = match reservation.control_cert_fingerprint.as_deref() {
        Some(v) => v,
        None => {
            return Err(anyhow!(
                "reservation did not include a control certificate fingerprint"
            ));
        }
    };
    let mut socket = crate::tls::connect(tcp, fingerprint).await?;

    // route_id -> (mode, local_port), learned from the reservation. The mode tells
    // handle_stream whether to bridge a local TCP connection or relay UDP datagrams.
    let mut route_map: HashMap<u16, (RouteMode, u16)> = HashMap::new();
    for r in &reservation.routes {
        route_map.insert(r.route_id, (r.mode, r.local_port));
    }
    let routes: Arc<HashMap<u16, (RouteMode, u16)>> = Arc::new(route_map);
    let mut bindings: Vec<AgentRouteBinding> = Vec::new();
    for r in &reservation.routes {
        bindings.push(AgentRouteBinding {
            route_id: r.route_id,
            local_port: r.local_port,
        });
    }

    let hello = AgentHello {
        tunnel_token: reservation.tunnel_token.clone(),
        role: ROLE_SERVICE_HOST.to_string(),
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
                let local = match routes.get(&r.route_id) {
                    Some((_, p)) => *p,
                    None => 0,
                };
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
        Ok(s) => {
            let _ = s.set_nodelay(true);
            s
        }
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
