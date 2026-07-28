//! Control plane + data plane for the Core Proxy.
//!
//! An agent connects to the control port and, after a length-prefixed JSON
//! handshake, the socket is upgraded to yamux (core = client, agent = server).
//! The signed tunnel token authorizes a set of routes: http/https routes register
//! under the tunnel's subdomain (served by the shared :80/:443 routers in
//! `shared.rs`), and tcp routes each bind a dedicated public port here. Every
//! public connection becomes one yamux stream carrying a routing preamble.

pub mod mux;
pub mod shared;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, copy_bidirectional};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use tracing::{info, warn};
use yamux::{Config as YamuxConfig, Connection, Mode};

use crate::dns::CloudflareManager;
use crate::jwt::verify_tunnel_token;
use crate::reporter;
use crate::state::{ActiveTunnel, CoreState, OpenStream, RouteHandle, TunnelStats};
use natforge_proto::{AgentHello, CoreReply, RouteMode, RouteResult, encode_preamble};

const MAX_FRAME: u32 = 1 << 20;

async fn read_frame<S: AsyncRead + Unpin>(stream: &mut S) -> anyhow::Result<Vec<u8>> {
    let len = stream.read_u32().await?;
    if len > MAX_FRAME {
        anyhow::bail!("handshake frame too large ({len} bytes)");
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

#[derive(Serialize)]
struct ErrReply<'a> {
    status: &'a str,
    message: String,
}

async fn reject<S: AsyncWrite + Unpin>(socket: &mut S, message: String) -> anyhow::Result<()> {
    let body = serde_json::to_vec(&ErrReply {
        status: "error",
        message,
    })?;
    let _ = write_frame(socket, &body).await;
    Ok(())
}

pub async fn run_control_plane(state: Arc<CoreState>) -> anyhow::Result<()> {
    let addr = format!("0.0.0.0:{}", state.config.control_port);
    let listener = TcpListener::bind(&addr).await?;
    info!("agent control plane listening on {addr} (yamux over TLS)");
    loop {
        let (socket, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                warn!("control accept error: {e}");
                continue;
            }
        };
        let st = state.clone();
        tokio::spawn(async move {
            // Establish TLS before anything else: the whole yamux session (and every
            // multiplexed user connection inside it) rides this encrypted channel.
            let tls = match st.tls.accept(socket).await {
                Ok(s) => s,
                Err(e) => {
                    warn!("agent {peer} TLS handshake failed: {e}");
                    return;
                }
            };
            if let Err(e) = handle_agent(st, tls, peer).await {
                warn!("agent {peer} session ended: {e}");
            }
        });
    }
}

/// Resolved per-route plan derived from the (authoritative) token claims.
struct PlannedRoute {
    route_id: u16,
    mode: RouteMode,
    public_port: Option<u16>, // tcp only
    public_endpoint: String,
}

async fn handle_agent<S>(
    state: Arc<CoreState>,
    mut socket: S,
    peer: std::net::SocketAddr,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // 1. Handshake + token verification.
    let hello: AgentHello = serde_json::from_slice(&read_frame(&mut socket).await?)?;
    if hello.role != "service_host" {
        reject(&mut socket, format!("unsupported role '{}'", hello.role)).await?;
        return Ok(());
    }
    let claims = match verify_tunnel_token(&hello.tunnel_token, &state.config.jwt_secret) {
        Ok(c) => c,
        Err(e) => {
            reject(&mut socket, e.clone()).await?;
            anyhow::bail!(e);
        }
    };

    // Every binding must correspond to a granted route.
    let granted: HashMap<u16, &natforge_proto::RouteClaim> =
        claims.routes.iter().map(|r| (r.route_id, r)).collect();
    for b in &hello.routes {
        if !granted.contains_key(&b.route_id) {
            reject(
                &mut socket,
                format!("route_id {} not authorized", b.route_id),
            )
            .await?;
            anyhow::bail!("unauthorized route {}", b.route_id);
        }
    }

    let subdomain = claims.subdomain.clone();
    let tunnel_id = claims.tunnel_id;
    let owner_id = claims.sub;
    let cfg = &state.config;

    // 2. Policy + ownership pre-checks (reject before committing anything).
    for r in &claims.routes {
        if let Some(port) = r.public_port
            && state.blocked_ports.read().await.contains(&port)
        {
            reject(&mut socket, format!("port {port} is globally blocked")).await?;
            anyhow::bail!("blocked port {port}");
        }
    }
    {
        // Don't let a stale/leaked token for a different tunnel hijack a live host.
        let http = state.http_routes.read().await;
        let https = state.https_routes.read().await;
        let free_or_ours =
            |existing: Option<i64>| existing.is_none() || existing == Some(tunnel_id);
        let may_proceed = free_or_ours(http.get(&subdomain).map(|h| h.tunnel_id))
            && free_or_ours(https.get(&subdomain).map(|h| h.tunnel_id));
        if !may_proceed {
            drop((http, https));
            reject(&mut socket, format!("subdomain {subdomain} already in use")).await?;
            anyhow::bail!("subdomain {subdomain} conflict");
        }
    }

    // 3. Bind dedicated TCP ports up front (so we can fail cleanly).
    let mut tcp_listeners: Vec<(u16, u16, TcpListener)> = Vec::new(); // (route_id, port, listener)
    let mut planned: Vec<PlannedRoute> = Vec::new();
    for r in &claims.routes {
        let endpoint = match r.mode {
            RouteMode::Http => format!("{}.{}:{}", subdomain, cfg.public_host, cfg.http_port),
            RouteMode::Https => format!("{}.{}:{}", subdomain, cfg.public_host, cfg.https_port),
            RouteMode::Tcp => {
                let port = r.public_port.unwrap_or(0);
                match TcpListener::bind(format!("0.0.0.0:{port}")).await {
                    Ok(l) => tcp_listeners.push((r.route_id, port, l)),
                    Err(e) => {
                        reject(
                            &mut socket,
                            format!("failed to bind public port {port}: {e}"),
                        )
                        .await?;
                        anyhow::bail!("bind {port}: {e}");
                    }
                }
                format!("{}:{}", cfg.public_host, port)
            }
        };
        planned.push(PlannedRoute {
            route_id: r.route_id,
            mode: r.mode,
            public_port: r.public_port,
            public_endpoint: endpoint,
        });
    }

    // 4. Acknowledge, then upgrade to yamux.
    let reply = CoreReply::Ok {
        tunnel_id,
        subdomain: subdomain.clone(),
        routes: planned
            .iter()
            .map(|p| RouteResult {
                route_id: p.route_id,
                mode: p.mode,
                public_endpoint: p.public_endpoint.clone(),
            })
            .collect(),
    };
    write_frame(&mut socket, &serde_json::to_vec(&reply)?).await?;

    info!(
        "tunnel UP: id={tunnel_id} sub={subdomain} routes={}",
        planned.len()
    );

    let (open_tx, open_rx) = mpsc::channel::<OpenStream>(256);
    let conn = Connection::new(socket.compat(), YamuxConfig::default(), Mode::Client);
    let driver = tokio::spawn(mux::run_client_driver(conn, open_rx));
    let driver_abort = driver.abort_handle();

    let stats = Arc::new(TunnelStats::default());
    let mut public_ports = Vec::new();
    let mut has_http = false;
    let mut has_https = false;
    let mut listener_handles = Vec::new();

    // 5. Register routes + spawn tcp listeners.
    for p in &planned {
        let handle = RouteHandle {
            tunnel_id,
            owner_id,
            route_id: p.route_id,
            mode: p.mode,
            open_tx: open_tx.clone(),
            stats: stats.clone(),
        };
        match p.mode {
            RouteMode::Http => {
                has_http = true;
                state
                    .http_routes
                    .write()
                    .await
                    .insert(subdomain.clone(), handle);
            }
            RouteMode::Https => {
                has_https = true;
                state
                    .https_routes
                    .write()
                    .await
                    .insert(subdomain.clone(), handle);
            }
            RouteMode::Tcp => {
                let port = p.public_port.unwrap_or(0);
                public_ports.push(port);
                state.port_routes.write().await.insert(port, handle.clone());
                // find the pre-bound listener for this route
                if let Some(idx) = tcp_listeners
                    .iter()
                    .position(|(rid, _, _)| *rid == p.route_id)
                {
                    let (_, _, listener) = tcp_listeners.remove(idx);
                    let st = state.clone();
                    let h = handle.clone();
                    let jh = tokio::spawn(async move { tcp_listener_loop(st, listener, h).await });
                    listener_handles.push(jh);
                }
            }
        }
        // Per-tunnel DNS only for tcp routes: provision an SRV so players can use
        // <sub>.<domain> instead of host:port. http/https are covered by the wildcard A.
        if let Some(port) = p.public_port {
            CloudflareManager::from_config(cfg)
                .provision_srv(&state.http, &subdomain, &cfg.public_host, port)
                .await
                .ok();
        }
    }

    state.tunnels.write().await.insert(
        subdomain.clone(),
        ActiveTunnel {
            tunnel_id,
            subdomain: subdomain.clone(),
            owner_id,
            public_ports: public_ports.clone(),
            has_http,
            has_https,
            stats: stats.clone(),
            listener_handles,
            driver_abort,
        },
    );

    // 6. Notify website + Redis mirror + periodic bandwidth reporting.
    reporter::tunnel_up(&state, tunnel_id, &subdomain, &peer.ip().to_string()).await;
    reporter::redis_mirror_up(
        &state,
        tunnel_id,
        &subdomain,
        has_http,
        has_https,
        &public_ports,
    )
    .await;

    let report_state = state.clone();
    let report_sub = subdomain.clone();
    let report_stats = stats.clone();
    let report_ports = public_ports.clone();
    let reporter_handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            ticker.tick().await;
            let bin = report_stats.bytes_in.load(Ordering::Relaxed);
            let bout = report_stats.bytes_out.load(Ordering::Relaxed);
            reporter::report_bandwidth(&report_state, tunnel_id, owner_id, bin, bout).await;
            // Refresh the Redis liveness mirror (idempotent SET..EX) before it expires.
            reporter::redis_mirror_up(
                &report_state,
                tunnel_id,
                &report_sub,
                has_http,
                has_https,
                &report_ports,
            )
            .await;
        }
    });

    // 7. Block until the agent's yamux session ends, then tear down.
    let _ = driver.await;
    reporter_handle.abort();
    teardown(&state, tunnel_id, &subdomain, &public_ports).await;
    reporter::tunnel_down(&state, tunnel_id, &subdomain).await;
    info!("tunnel DOWN: id={tunnel_id} sub={subdomain}");
    Ok(())
}

/// Remove this tunnel's registry entries (only if still owned by this tunnel_id)
/// and abort its tcp listeners.
pub(crate) async fn teardown(
    state: &Arc<CoreState>,
    tunnel_id: i64,
    subdomain: &str,
    ports: &[u16],
) {
    if let Some(t) = state.tunnels.write().await.remove(subdomain) {
        for jh in t.listener_handles {
            jh.abort();
        }
    }
    {
        let mut http = state.http_routes.write().await;
        if http.get(subdomain).map(|h| h.tunnel_id) == Some(tunnel_id) {
            http.remove(subdomain);
        }
    }
    {
        let mut https = state.https_routes.write().await;
        if https.get(subdomain).map(|h| h.tunnel_id) == Some(tunnel_id) {
            https.remove(subdomain);
        }
    }
    {
        let mut pr = state.port_routes.write().await;
        for port in ports {
            if pr.get(port).map(|h| h.tunnel_id) == Some(tunnel_id) {
                pr.remove(port);
            }
        }
    }
    // Remove the per-tunnel SRV record(s) if this tunnel had any tcp routes.
    if !ports.is_empty() {
        CloudflareManager::from_config(&state.config)
            .remove_srv(&state.http, subdomain, &state.config.public_host)
            .await
            .ok();
    }
}

/// Accept loop for a dedicated tcp route's public port.
async fn tcp_listener_loop(state: Arc<CoreState>, listener: TcpListener, handle: RouteHandle) {
    loop {
        let (inbound, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => continue,
        };
        if !state.ddos.analyze_connection(&peer.ip().to_string()).await {
            continue;
        }
        let h = handle.clone();
        let st = state.clone();
        tokio::spawn(async move {
            bridge_public_connection(st, inbound, peer, h).await;
        });
    }
}

/// Bridge a raw-TCP public connection to a fresh yamux stream (preamble carries the
/// route id and peer, with no replay bytes). Geo-policy is enforced first, and the
/// connection is recorded in the per-tunnel log on close.
async fn bridge_public_connection(
    state: Arc<CoreState>,
    mut inbound: TcpStream,
    peer: std::net::SocketAddr,
    handle: RouteHandle,
) {
    let peer_ip = peer.ip().to_string();
    let country = state.geo.country(peer.ip());
    // Geo-policy: drop (and log) connections from blocked countries.
    if state
        .is_country_blocked(handle.tunnel_id, country.as_deref())
        .await
    {
        reporter::report_conn_log(&state, &handle, &peer_ip, country.as_deref(), 0, 0, 0, true)
            .await;
        return;
    }

    let (reply_tx, reply_rx) = oneshot::channel();
    if handle
        .open_tx
        .send(OpenStream {
            route_id: handle.route_id,
            reply: reply_tx,
        })
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
    let preamble = encode_preamble(handle.route_id, Some(peer), &[]);
    if outbound.write_all(&preamble).await.is_err() {
        return;
    }
    let start = std::time::Instant::now();
    match copy_bidirectional(&mut inbound, &mut outbound).await {
        Ok((to_agent, from_agent)) => {
            handle.stats.bytes_in.fetch_add(to_agent, Ordering::Relaxed);
            handle
                .stats
                .bytes_out
                .fetch_add(from_agent, Ordering::Relaxed);
            reporter::report_conn_log(
                &state,
                &handle,
                &peer_ip,
                country.as_deref(),
                to_agent,
                from_agent,
                start.elapsed().as_millis(),
                false,
            )
            .await;
        }
        Err(e) => warn!("tcp relay closed: {e}"),
    }
}
