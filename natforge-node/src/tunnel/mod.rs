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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, copy_bidirectional_with_sizes, split};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use tracing::{info, warn};
use yamux::{Config as YamuxConfig, Connection, Mode};

use crate::dns::CloudflareManager;
use crate::jwt::verify_tunnel_token;
use crate::reporter;
use crate::state::{ActiveTunnel, CoreState, OpenStream, RouteHandle, TunnelStats};
use natforge_proto::{
    AgentHello, CoreReply, ROLE_SERVICE_HOST, RouteMode, RouteResult, encode_preamble,
    read_datagram, read_frame, write_datagram, write_frame,
};

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
        // TCP keepalive so an ungracefully-dropped agent connection (crash, kill, sleep,
        // network loss) is detected in ~50s and the yamux driver ends, letting the tunnel
        // tear down and free its public ports. Without it the socket (and its bound
        // tcp/udp ports) can leak until the core restarts.
        {
            let ka = socket2::TcpKeepalive::new()
                .with_time(std::time::Duration::from_secs(20))
                .with_interval(std::time::Duration::from_secs(10))
                .with_retries(3);
            let _ = socket2::SockRef::from(&socket).set_tcp_keepalive(&ka);
        }
        // TCP_NODELAY: this socket carries the whole yamux/TLS session. Disable
        // send-side buffering so small relayed frames flush immediately, keeping
        // interactive (low-concurrency) latency low.
        let _ = socket.set_nodelay(true);
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
    srv_service: Option<String>, // opt-in SRV label (tcp/udp routes only)
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
    if hello.role != ROLE_SERVICE_HOST {
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
    let mut granted: HashMap<u16, &natforge_proto::RouteClaim> = HashMap::new();
    for r in &claims.routes {
        granted.insert(r.route_id, r);
    }
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
    let custom_domain = claims.custom_domain.clone();
    let cfg = &state.config;

    // 2. Ownership pre-checks (reject before committing anything).
    {
        // Don't let a stale/leaked token for a different tunnel hijack a live host.
        let http = state.http_routes.read().await;
        let https = state.https_routes.read().await;
        let free_or_ours =
            |existing: Option<i64>| existing.is_none() || existing == Some(tunnel_id);
        let http_existing = match http.get(&subdomain) {
            Some(h) => Some(h.tunnel_id),
            None => None,
        };
        let https_existing = match https.get(&subdomain) {
            Some(h) => Some(h.tunnel_id),
            None => None,
        };
        let may_proceed = free_or_ours(http_existing) && free_or_ours(https_existing);
        if !may_proceed {
            drop((http, https));
            reject(&mut socket, format!("subdomain {subdomain} already in use")).await?;
            anyhow::bail!("subdomain {subdomain} conflict");
        }
    }

    // 3. Bind dedicated TCP/UDP ports up front (so we can fail cleanly).
    let mut tcp_listeners: Vec<(u16, u16, TcpListener)> = Vec::new(); // (route_id, port, listener)
    let mut udp_sockets: Vec<(u16, u16, UdpSocket)> = Vec::new(); // (route_id, port, socket)
    let mut planned: Vec<PlannedRoute> = Vec::new();
    for r in &claims.routes {
        let endpoint = match r.mode {
            RouteMode::Http => format!("{}.{}:{}", subdomain, cfg.public_host, cfg.http_port),
            RouteMode::Https => format!("{}.{}:{}", subdomain, cfg.public_host, cfg.https_port),
            RouteMode::Tcp => {
                let port = match r.public_port {
                    Some(p) => p,
                    None => 0,
                };
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
            RouteMode::Udp => {
                let port = match r.public_port {
                    Some(p) => p,
                    None => 0,
                };
                match UdpSocket::bind(format!("0.0.0.0:{port}")).await {
                    Ok(s) => udp_sockets.push((r.route_id, port, s)),
                    Err(e) => {
                        reject(
                            &mut socket,
                            format!("failed to bind public udp port {port}: {e}"),
                        )
                        .await?;
                        anyhow::bail!("bind udp {port}: {e}");
                    }
                }
                format!("{}:{}", cfg.public_host, port)
            }
            // The control plane expands `both` into a tcp claim + a udp claim before
            // minting the token, so a raw `Both` never reaches the data plane; reject
            // it cleanly rather than binding nothing.
            RouteMode::Both => {
                reject(&mut socket, "unexpected 'both' route in token".into()).await?;
                anyhow::bail!("unexpected Both route reached the data plane");
            }
        };
        planned.push(PlannedRoute {
            route_id: r.route_id,
            mode: r.mode,
            public_port: r.public_port,
            public_endpoint: endpoint,
            srv_service: r.srv_service.clone(),
        });
    }

    // 4. Acknowledge, then upgrade to yamux.
    let mut reply_routes = Vec::new();
    for p in &planned {
        reply_routes.push(RouteResult {
            route_id: p.route_id,
            mode: p.mode,
            public_endpoint: p.public_endpoint.clone(),
        });
    }
    let reply = CoreReply::Ok {
        tunnel_id,
        subdomain: subdomain.clone(),
        routes: reply_routes,
    };
    write_frame(&mut socket, &serde_json::to_vec(&reply)?).await?;

    info!(
        "tunnel UP: id={tunnel_id} sub={subdomain} routes={}",
        planned.len()
    );

    let (open_tx, open_rx) = mpsc::channel::<OpenStream>(256);
    // Larger yamux send frames cut per-frame overhead on bulk transfers (throughput tuning).
    let mut ycfg = YamuxConfig::default();
    ycfg.set_split_send_size(64 * 1024);
    let conn = Connection::new(socket.compat(), ycfg, Mode::Client);
    let driver = tokio::spawn(mux::run_client_driver(conn, open_rx));
    let driver_abort = driver.abort_handle();

    let stats = Arc::new(TunnelStats::default());
    let mut public_ports = Vec::new();
    let mut udp_ports = Vec::new();
    let mut has_http = false;
    let mut has_https = false;
    let mut listener_handles = Vec::new();
    let mut srv_records: Vec<(String, String)> = Vec::new(); // (service, proto) for teardown

    // 5. Register routes + spawn tcp/udp listeners.
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
                if let Some(cd) = &custom_domain {
                    state
                        .custom_http
                        .write()
                        .await
                        .insert(cd.clone(), handle.clone());
                }
                state
                    .http_routes
                    .write()
                    .await
                    .insert(subdomain.clone(), handle);
            }
            RouteMode::Https => {
                has_https = true;
                if let Some(cd) = &custom_domain {
                    state
                        .custom_https
                        .write()
                        .await
                        .insert(cd.clone(), handle.clone());
                }
                state
                    .https_routes
                    .write()
                    .await
                    .insert(subdomain.clone(), handle);
            }
            RouteMode::Tcp => {
                let port = match p.public_port {
                    Some(pp) => pp,
                    None => 0,
                };
                public_ports.push(port);
                state.port_routes.write().await.insert(port, handle.clone());
                // find the pre-bound listener for this route
                let mut found = None;
                let mut i = 0;
                for (rid, _, _) in &tcp_listeners {
                    if *rid == p.route_id {
                        found = Some(i);
                        break;
                    }
                    i += 1;
                }
                if let Some(idx) = found {
                    let (_, _, listener) = tcp_listeners.remove(idx);
                    let st = state.clone();
                    let h = handle.clone();
                    let jh = tokio::spawn(async move { tcp_listener_loop(st, listener, h).await });
                    listener_handles.push(jh);
                }
            }
            RouteMode::Udp => {
                let port = match p.public_port {
                    Some(pp) => pp,
                    None => 0,
                };
                udp_ports.push(port);
                state.udp_routes.write().await.insert(port, handle.clone());
                let mut found = None;
                let mut i = 0;
                for (rid, _, _) in &udp_sockets {
                    if *rid == p.route_id {
                        found = Some(i);
                        break;
                    }
                    i += 1;
                }
                if let Some(idx) = found {
                    let (_, _, sock) = udp_sockets.remove(idx);
                    let st = state.clone();
                    let h = handle.clone();
                    let jh = tokio::spawn(async move { udp_listener_loop(st, sock, h).await });
                    listener_handles.push(jh);
                }
            }
            // Unreachable: `both` is expanded into tcp+udp claims upstream (rejected in
            // the bind pass above), so no PlannedRoute is ever Both.
            RouteMode::Both => {}
        }
        // Opt-in SRV: only routes the owner labelled get a `_<service>._<proto>.<sub>`
        // record (proto from the transport), so SRV-aware clients (Minecraft `_minecraft`,
        // Mindustry `_mindustry`, ...) can connect by hostname. Unlabelled routes get none.
        if let (Some(service), Some(port)) = (p.srv_service.as_deref(), p.public_port) {
            let proto = if p.mode == RouteMode::Udp {
                "udp"
            } else {
                "tcp"
            };
            if let Err(e) = CloudflareManager::from_config(cfg)
                .provision_srv(
                    &state.http,
                    service,
                    proto,
                    &subdomain,
                    &cfg.public_host,
                    port,
                )
                .await
            {
                warn!("SRV provision failed for _{service}._{proto}.{subdomain}: {e}");
            }
            srv_records.push((service.to_string(), proto.to_string()));
        }
    }

    // For a custom-domain http route with no https route, obtain a per-domain cert
    // so https://<custom> works (issued in the background; http keeps working).
    if let Some(cd) = &custom_domain
        && has_http
        && !has_https
    {
        state.acme.ensure_cert(cd.clone());
    }

    state.tunnels.write().await.insert(
        subdomain.clone(),
        ActiveTunnel {
            tunnel_id,
            subdomain: subdomain.clone(),
            owner_id,
            public_ports: public_ports.clone(),
            udp_ports: udp_ports.clone(),
            custom_domain: custom_domain.clone(),
            srv_records: srv_records.clone(),
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
    teardown(
        &state,
        tunnel_id,
        &subdomain,
        &public_ports,
        &udp_ports,
        custom_domain.as_deref(),
    )
    .await;
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
    udp_ports: &[u16],
    custom_domain: Option<&str>,
) {
    let srv_records = if let Some(t) = state.tunnels.write().await.remove(subdomain) {
        for jh in t.listener_handles {
            jh.abort();
        }
        t.srv_records
    } else {
        Vec::new()
    };
    {
        let mut http = state.http_routes.write().await;
        let existing = match http.get(subdomain) {
            Some(h) => Some(h.tunnel_id),
            None => None,
        };
        if existing == Some(tunnel_id) {
            http.remove(subdomain);
        }
    }
    {
        let mut https = state.https_routes.write().await;
        let existing = match https.get(subdomain) {
            Some(h) => Some(h.tunnel_id),
            None => None,
        };
        if existing == Some(tunnel_id) {
            https.remove(subdomain);
        }
    }
    {
        let mut pr = state.port_routes.write().await;
        for port in ports {
            let existing = match pr.get(port) {
                Some(h) => Some(h.tunnel_id),
                None => None,
            };
            if existing == Some(tunnel_id) {
                pr.remove(port);
            }
        }
    }
    {
        let mut ur = state.udp_routes.write().await;
        for port in udp_ports {
            let existing = match ur.get(port) {
                Some(h) => Some(h.tunnel_id),
                None => None,
            };
            if existing == Some(tunnel_id) {
                ur.remove(port);
            }
        }
    }
    if let Some(cd) = custom_domain {
        let mut ch = state.custom_http.write().await;
        let existing = match ch.get(cd) {
            Some(h) => Some(h.tunnel_id),
            None => None,
        };
        if existing == Some(tunnel_id) {
            ch.remove(cd);
        }
        drop(ch);
        let mut cs = state.custom_https.write().await;
        let existing = match cs.get(cd) {
            Some(h) => Some(h.tunnel_id),
            None => None,
        };
        if existing == Some(tunnel_id) {
            cs.remove(cd);
        }
    }
    // Remove the opt-in SRV records this tunnel provisioned.
    for (service, proto) in &srv_records {
        if let Err(e) = CloudflareManager::from_config(&state.config)
            .remove_srv(
                &state.http,
                service,
                proto,
                subdomain,
                &state.config.public_host,
            )
            .await
        {
            warn!("SRV remove failed for _{service}._{proto}.{subdomain}: {e}");
        }
    }
}

/// Accept loop for a dedicated tcp route's public port.
async fn tcp_listener_loop(state: Arc<CoreState>, listener: TcpListener, handle: RouteHandle) {
    loop {
        let (inbound, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let _ = inbound.set_nodelay(true);
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
    let preamble = encode_preamble(handle.route_id, Some(peer), &[]);
    if outbound.write_all(&preamble).await.is_err() {
        return;
    }
    let start = std::time::Instant::now();
    match copy_bidirectional_with_sizes(&mut inbound, &mut outbound, 65536, 65536).await {
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

// --------------------------------------------------------------------------
// UDP routes
// --------------------------------------------------------------------------

/// How long a udp flow may go without a client datagram before it is reaped.
const UDP_FLOW_IDLE: Duration = Duration::from_secs(60);
/// Cap on concurrent udp flows per route (bounds memory against spoofed sources).
const UDP_MAX_FLOWS: usize = 4096;

/// Accept loop for a dedicated udp route's public port. UDP is connectionless, so
/// a flow is keyed on the client's source address: the first datagram from a new
/// source opens one yamux stream (carrying the routing preamble), and later
/// datagrams from that source ride it. Flows expire on idle, and each flow task
/// signals its close so the table stays bounded.
async fn udp_listener_loop(state: Arc<CoreState>, socket: UdpSocket, handle: RouteHandle) {
    let socket = Arc::new(socket);
    let mut flows: HashMap<std::net::SocketAddr, mpsc::Sender<Vec<u8>>> = HashMap::new();
    let (closed_tx, mut closed_rx) = mpsc::channel::<std::net::SocketAddr>(64);
    let mut buf = vec![0u8; 65535];
    loop {
        tokio::select! {
            Some(peer) = closed_rx.recv() => {
                flows.remove(&peer);
            }
            r = socket.recv_from(&mut buf) => {
                let (n, peer) = match r {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let mut data = buf[..n].to_vec();
                if let Some(tx) = flows.get(&peer) {
                    match tx.try_send(data) {
                        Ok(()) => continue,
                        // Congested: drop this datagram, keep the flow (UDP semantics).
                        Err(mpsc::error::TrySendError::Full(_)) => continue,
                        // Flow task ended: drop it and re-open below with this datagram.
                        Err(mpsc::error::TrySendError::Closed(d)) => {
                            flows.remove(&peer);
                            data = d;
                        }
                    }
                }
                if flows.len() >= UDP_MAX_FLOWS {
                    continue;
                }
                let peer_ip = peer.ip().to_string();
                let country = state.geo.country(peer.ip());
                if state
                    .is_country_blocked(handle.tunnel_id, country.as_deref())
                    .await
                {
                    reporter::report_conn_log(
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
                    continue;
                }
                // Open the per-flow stream to the agent.
                let (reply_tx, reply_rx) = oneshot::channel();
                if handle
                    .open_tx
                    .send(OpenStream {
                        reply: reply_tx,
                    })
                    .await
                    .is_err()
                {
                    return; // tunnel gone
                }
                let stream = match reply_rx.await {
                    Ok(Ok(s)) => s,
                    _ => continue,
                };
                let (tx, rx) = mpsc::channel::<Vec<u8>>(256);
                let _ = tx.try_send(data); // deliver the datagram that opened the flow
                flows.insert(peer, tx);
                let st = state.clone();
                let h = handle.clone();
                let sock = socket.clone();
                let ctx = closed_tx.clone();
                tokio::spawn(async move {
                    udp_flow(st, h, sock, stream, peer, country, rx, ctx).await;
                });
            }
        }
    }
}

/// Relay one udp flow to the agent: client datagrams (fed over `rx`) go out framed
/// on the stream; datagrams the agent returns are sent back to the client. The flow
/// ends on idle, on stream close, or when the tunnel drops, and is logged like a
/// tcp connection.
#[allow(clippy::too_many_arguments)]
async fn udp_flow(
    state: Arc<CoreState>,
    handle: RouteHandle,
    socket: Arc<UdpSocket>,
    stream: yamux::Stream,
    peer: std::net::SocketAddr,
    country: Option<String>,
    mut rx: mpsc::Receiver<Vec<u8>>,
    closed_tx: mpsc::Sender<std::net::SocketAddr>,
) {
    let mut s = stream.compat();
    if s.write_all(&encode_preamble(handle.route_id, Some(peer), &[]))
        .await
        .is_err()
    {
        let _ = closed_tx.send(peer).await;
        return;
    }
    let (mut sr, mut sw) = split(s);
    let bytes_in = Arc::new(AtomicU64::new(0));
    let bytes_out = Arc::new(AtomicU64::new(0));
    let start = std::time::Instant::now();

    // client -> agent (framed onto the stream); idle with no client datagram ends it.
    let bi = bytes_in.clone();
    let downstream = async move {
        // Ends on idle timeout (Err) or channel close (Ok(None)).
        while let Ok(Some(d)) = tokio::time::timeout(UDP_FLOW_IDLE, rx.recv()).await {
            bi.fetch_add(d.len() as u64, Ordering::Relaxed);
            if write_datagram(&mut sw, &d).await.is_err() {
                break;
            }
        }
    };
    // agent -> client.
    let bo = bytes_out.clone();
    let sock = socket.clone();
    let upstream = async move {
        while let Ok(Some(d)) = read_datagram(&mut sr).await {
            bo.fetch_add(d.len() as u64, Ordering::Relaxed);
            let _ = sock.send_to(&d, peer).await;
        }
    };
    tokio::select! {
        _ = downstream => {}
        _ = upstream => {}
    }

    let bin = bytes_in.load(Ordering::Relaxed);
    let bout = bytes_out.load(Ordering::Relaxed);
    handle.stats.bytes_in.fetch_add(bin, Ordering::Relaxed);
    handle.stats.bytes_out.fetch_add(bout, Ordering::Relaxed);
    reporter::report_conn_log(
        &state,
        &handle,
        &peer.ip().to_string(),
        country.as_deref(),
        bin,
        bout,
        start.elapsed().as_millis(),
        false,
    )
    .await;
    let _ = closed_tx.send(peer).await;
}
