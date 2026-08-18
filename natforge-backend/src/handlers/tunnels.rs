//! Service-host tunnel reservation, listing, and teardown.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::db::connection::SharedState;
use crate::db::queries::{self, ReserveError};
use crate::handlers::err as db_err;
use crate::jwt::{AuthUser, DeviceAuth, issue_tunnel_token};
use crate::models::{Node, RequestedRoute, RouteRow, TunnelRow, TunnelView};
use natforge_proto::{RouteClaim, RouteMode};

#[derive(Deserialize)]
pub struct RequestTunnelReq {
    pub routes: Vec<RequestedRoute>,
    /// Optional user-chosen subdomain; a random one is used when omitted.
    #[serde(default)]
    pub subdomain: Option<String>,
    /// Optional node/region to host the tunnel on; the default node is used if omitted.
    #[serde(default)]
    pub node_id: Option<String>,
    /// Optional device to attach this tunnel to as a service (must be caller-owned).
    #[serde(default)]
    pub device_id: Option<i64>,
    /// When true, an existing tunnel with the same route set is a conflict rather than
    /// being reused. The dashboard "create a new service host" flow sets this; a
    /// reconnecting CLI agent leaves it false so it keeps its subdomain and ports.
    #[serde(default)]
    pub create_new: bool,
}

#[derive(Serialize)]
pub struct ReservedRoute {
    pub route_id: u16,
    pub mode: String,
    pub local_port: u16,
    pub public_endpoint: String,
    pub label: Option<String>,
}

#[derive(Serialize)]
pub struct TunnelRequestRes {
    pub tunnel_id: i64,
    pub subdomain: String,
    pub full_host: String,
    pub tunnel_token: String,
    pub routes: Vec<ReservedRoute>,
    pub status: String,
    /// Where the agent should connect (the chosen node's control endpoint).
    pub control_endpoint: String,
    pub region: Option<String>,
    pub node_id: String,
    /// SHA-256 fingerprint of the node's TLS control cert, for the agent to pin.
    pub control_cert_fingerprint: Option<String>,
    /// User-owned hostname fronting this tunnel, if set.
    pub custom_domain: Option<String>,
}

fn parse_kind(kind: &str) -> RouteMode {
    match kind {
        "http" => RouteMode::Http,
        "https" => RouteMode::Https,
        "udp" => RouteMode::Udp,
        "both" => RouteMode::Both,
        _ => RouteMode::Tcp,
    }
}

/// Per-tunnel route policy shared by fresh reservation and in-place route edits:
/// at most 1 http, 1 https, 2 tcp and 2 udp (a `both` route counts as one tcp AND one
/// udp), and each dedicated (non-host-routed) local port may be used by at most one
/// route, so exposing the same port over TCP and UDP must be a single `both` route
/// rather than two.
fn check_route_policy(routes: &[RequestedRoute]) -> Result<(), String> {
    let (mut http, mut https, mut tcp, mut udp) = (0, 0, 0, 0);
    let mut dedicated_ports: Vec<u16> = Vec::with_capacity(routes.len());
    for r in routes {
        if !r.mode.is_host_routed() {
            if dedicated_ports.contains(&r.local_port) {
                return Err(format!(
                    "local port {} is used by more than one route; pick Both to expose one port over TCP and UDP",
                    r.local_port
                ));
            }
            dedicated_ports.push(r.local_port);
        }
        match r.mode {
            RouteMode::Http => http += 1,
            RouteMode::Https => https += 1,
            RouteMode::Tcp => tcp += 1,
            RouteMode::Udp => udp += 1,
            RouteMode::Both => {
                tcp += 1;
                udp += 1;
            }
        }
    }
    if http > 1 || https > 1 || tcp > 2 || udp > 2 {
        return Err("at most 1 http, 1 https, 2 tcp and 2 udp routes per service".into());
    }
    Ok(())
}

fn endpoint(mode: RouteMode, subdomain: &str, domain: &str, public_port: Option<i32>) -> String {
    match mode {
        RouteMode::Http => format!("http://{subdomain}.{domain}"),
        RouteMode::Https => format!("https://{subdomain}.{domain}"),
        RouteMode::Tcp | RouteMode::Udp | RouteMode::Both => {
            format!("{subdomain}.{domain}:{}", public_port.unwrap_or(0))
        }
    }
}

/// route_id offset for the udp half of a `both` route on the wire, kept well above the
/// small dense route ids so the two halves of one `both` never collide.
const BOTH_UDP_ROUTE_OFFSET: u16 = 10000;

/// The transports a route occupies on its local port, as a bitset (bit0 = tcp, bit1 =
/// udp). `both` occupies both; http/https are host-routed and hold no dedicated port.
/// Two routes on the same local port clash iff their transport sets intersect, so
/// tcp:N and udp:N coexist but tcp:N and both:N do not.
fn transport_bits(kind: &str) -> u8 {
    match kind {
        "tcp" => 0b01,
        "udp" => 0b10,
        "both" => 0b11,
        _ => 0,
    }
}

/// Expand a persisted route into the wire claims + agent views. A `both` route becomes
/// a tcp half (route_id R) and a udp half (route_id R + offset) sharing the one public
/// port, so the data plane binds a TCP listener and a UDP socket on that number and the
/// agent relays each half like an ordinary tcp/udp route. Every other mode yields a
/// single entry. This is the only place the one-row `both` abstraction is unfolded.
fn expand_route(
    r: &RouteRow,
    subdomain: &str,
    host: &str,
) -> (Vec<RouteClaim>, Vec<ReservedRoute>) {
    if parse_kind(&r.kind) == RouteMode::Both {
        let port_u16 = match r.public_port {
            Some(p) => Some(p as u16),
            None => None,
        };
        let ep = endpoint(RouteMode::Tcp, subdomain, host, r.public_port);
        let tcp_id = r.route_id as u16;
        let udp_id = tcp_id.wrapping_add(BOTH_UDP_ROUTE_OFFSET);
        let claim = |route_id, mode, srv: Option<String>| RouteClaim {
            route_id,
            mode,
            host: None,
            public_port: port_u16,
            srv_service: srv,
        };
        let view = |route_id, mode: &str| ReservedRoute {
            route_id,
            mode: mode.to_string(),
            local_port: r.local_port as u16,
            public_endpoint: ep.clone(),
            label: r.label.clone(),
        };
        // Only the tcp half carries the SRV label, so a `both` route yields one
        // `_<service>._tcp` record (the form the SRV-aware game clients query).
        return (
            vec![
                claim(tcp_id, RouteMode::Tcp, r.srv_service.clone()),
                claim(udp_id, RouteMode::Udp, None),
            ],
            vec![view(tcp_id, "tcp"), view(udp_id, "udp")],
        );
    }
    let mode = parse_kind(&r.kind);
    let route_host = if mode.is_host_routed() {
        Some(format!("{subdomain}.{host}"))
    } else {
        None
    };
    let public_port = if mode.is_host_routed() {
        None
    } else {
        match r.public_port {
            Some(p) => Some(p as u16),
            None => None,
        }
    };
    (
        vec![RouteClaim {
            route_id: r.route_id as u16,
            mode,
            host: route_host,
            public_port,
            // SRV only applies to dedicated-port (tcp/udp) routes, not host-routed http/https.
            srv_service: if mode.is_host_routed() {
                None
            } else {
                r.srv_service.clone()
            },
        }],
        vec![ReservedRoute {
            route_id: r.route_id as u16,
            mode: r.kind.clone(),
            local_port: r.local_port as u16,
            public_endpoint: endpoint(mode, subdomain, host, r.public_port),
            label: r.label.clone(),
        }],
    )
}

pub async fn request_tunnel(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(req): Json<RequestTunnelReq>,
) -> Result<Json<TunnelRequestRes>, (StatusCode, String)> {
    let banned = match queries::is_user_banned(&state.db.pg, user.user_id).await {
        Ok(v) => v,
        Err(_) => false,
    };
    if banned {
        return Err((StatusCode::FORBIDDEN, "this account is banned".into()));
    }
    if req.routes.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "at least one route is required".into(),
        ));
    }
    if let Err(e) = check_route_policy(&req.routes) {
        return Err((StatusCode::BAD_REQUEST, e));
    }

    let mut requested: Vec<(RouteMode, u16, Option<String>)> = Vec::new();
    for r in &req.routes {
        requested.push((r.mode, r.local_port, r.label.clone()));
    }
    let custom_deref = req.subdomain.as_deref();
    let custom_trimmed = custom_deref.map(str::trim);
    let custom = match custom_trimmed {
        Some(s) => {
            if !s.is_empty() {
                Some(s)
            } else {
                None
            }
        }
        None => None,
    };

    // Pick the node/region: the requested one (must exist + be active), else the default.
    let node = match &req.node_id {
        Some(nid) => {
            let node_opt = queries::get_node(&state.db.pg, nid).await.map_err(db_err)?;
            let node_active = match node_opt {
                Some(n) => {
                    if n.active {
                        Some(n)
                    } else {
                        None
                    }
                }
                None => None,
            };
            match node_active {
                Some(n) => n,
                None => {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "unknown or inactive region".to_string(),
                    ));
                }
            }
        }
        None => {
            let node_opt = queries::default_node(&state.db.pg).await.map_err(db_err)?;
            match node_opt {
                Some(n) => n,
                None => {
                    return Err((
                        StatusCode::SERVICE_UNAVAILABLE,
                        "no region/node is available yet".to_string(),
                    ));
                }
            }
        }
    };

    // If attaching to a device, it must be caller-owned, and none of the requested
    // (protocol, local port) endpoints may already be served by the device's OTHER
    // services (tcp:N and udp:N are distinct, so only exact protocol+port clashes).
    if let Some(did) = req.device_id {
        let dev_opt = queries::device_by_id(&state.db.pg, did)
            .await
            .map_err(db_err)?;
        let owner_opt = match dev_opt {
            Some(d) => Some(d.owner_id),
            None => None,
        };
        let owns = owner_opt == Some(user.user_id);
        if !owns {
            return Err((StatusCode::FORBIDDEN, "not your device".into()));
        }
        let taken = queries::device_routes_excluding(&state.db.pg, did, 0)
            .await
            .map_err(db_err)?;
        let mut conflict: Option<&RequestedRoute> = None;
        for r in &req.routes {
            let mut clash = false;
            for (k, p) in &taken {
                if *p == r.local_port as i32
                    && transport_bits(k) & transport_bits(r.mode.as_str()) != 0
                {
                    clash = true;
                    break;
                }
            }
            if clash {
                conflict = Some(r);
                break;
            }
        }
        if let Some(r) = conflict {
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "{}:{} is already used by another service on this device",
                    r.mode.as_str(),
                    r.local_port
                ),
            ));
        }
    }

    let max_tunnels = queries::user_max_tunnels(&state.db.pg, user.user_id)
        .await
        .map_err(db_err)?;
    let reserved = match queries::reserve_tunnel(
        &state.db.pg,
        user.user_id,
        &node.node_id,
        &node.public_host,
        &requested,
        custom,
        max_tunnels, // per-user cap (users.max_tunnels)
        !req.create_new,
        req.device_id,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            return Err(match e {
                ReserveError::LimitReached(n) => {
                    (StatusCode::FORBIDDEN, format!("tunnel limit reached ({n})"))
                }
                ReserveError::PortExhausted => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "public TCP port pool exhausted".into(),
                ),
                ReserveError::BadSubdomain => (
                    StatusCode::BAD_REQUEST,
                    "invalid subdomain: use 3–30 chars of a–z, 0–9 and '-'".into(),
                ),
                ReserveError::SubdomainTaken(s) => (
                    StatusCode::CONFLICT,
                    format!("subdomain '{s}' is already taken"),
                ),
                ReserveError::RouteSetExists(sub) => (
                    StatusCode::CONFLICT,
                    format!("a service host already exposes those exact ports ({sub})"),
                ),
                ReserveError::Db(err) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("database error: {err}"),
                ),
            });
        }
    };

    // The tunnel's actual node (reuse may keep an existing node); fall back to chosen.
    let host_node_res = queries::get_node(&state.db.pg, &reserved.node_id).await;
    let host_node_opt = match host_node_res {
        Ok(inner) => inner,
        Err(_) => None,
    };
    let host_node = match host_node_opt {
        Some(n) => n,
        None => node,
    };
    let host = &reserved.public_host;

    // Build token claims + the response view from the persisted routes (a `both` route
    // unfolds into its tcp + udp halves here).
    let mut claims = Vec::new();
    let mut views = Vec::new();
    for r in &reserved.routes {
        let (c, v) = expand_route(r, &reserved.subdomain, host);
        claims.extend(c);
        views.extend(v);
    }

    let token = issue_tunnel_token(
        &state.config.jwt_secret,
        user.user_id,
        reserved.tunnel_id,
        &reserved.subdomain,
        claims,
        reserved.custom_domain.clone(),
    );

    tracing::info!(
        "user {} reserved tunnel {} ({}) {} routes (reused={})",
        user.user_id,
        reserved.tunnel_id,
        reserved.subdomain,
        reserved.routes.len(),
        reserved.reused
    );

    Ok(Json(TunnelRequestRes {
        full_host: format!("{}.{}", reserved.subdomain, host),
        tunnel_id: reserved.tunnel_id,
        subdomain: reserved.subdomain,
        tunnel_token: token,
        routes: views,
        status: if reserved.reused {
            "reused"
        } else {
            "reserved"
        }
        .into(),
        control_endpoint: host_node.control_endpoint,
        region: host_node.region,
        node_id: host_node.node_id,
        control_cert_fingerprint: host_node.control_cert_fp,
        custom_domain: reserved.custom_domain,
    }))
}

/// Build the connect-ready reservation the agent understands (a fresh tunnel token,
/// the node's control endpoint + pinned cert, and the routes) from a persisted tunnel.
fn build_reservation(
    secret: &str,
    owner_id: i32,
    t: &TunnelRow,
    routes: &[RouteRow],
    node: &Node,
    custom_domain: Option<String>,
) -> TunnelRequestRes {
    let host = &t.public_host;
    let mut claims = Vec::new();
    let mut views = Vec::new();
    for r in routes {
        let (c, v) = expand_route(r, &t.subdomain, host);
        claims.extend(c);
        views.extend(v);
    }
    // The custom domain rides the signed token so the node registers it and issues its
    // cert; omitting it (the old bug) left device service hosts unreachable by custom name.
    let token = issue_tunnel_token(
        secret,
        owner_id,
        t.id,
        &t.subdomain,
        claims,
        custom_domain.clone(),
    );
    TunnelRequestRes {
        full_host: format!("{}.{}", t.subdomain, host),
        tunnel_id: t.id,
        subdomain: t.subdomain.clone(),
        tunnel_token: token,
        routes: views,
        status: "reserved".into(),
        control_endpoint: node.control_endpoint.clone(),
        region: node.region.clone(),
        node_id: node.node_id.clone(),
        control_cert_fingerprint: node.control_cert_fp.clone(),
        custom_domain,
    }
}

/// A device agent (device-token authed) pulls the connect-ready reservations for all
/// of its services here. This is what makes `natforge run` config-driven instead of
/// `--route`-flag driven.
pub async fn device_config(
    State(state): State<SharedState>,
    dev: DeviceAuth,
) -> Result<Json<Vec<TunnelRequestRes>>, (StatusCode, String)> {
    // The running agent polls this endpoint; treat each poll as a liveness heartbeat.
    let _ = queries::touch_device_online(&state.db.pg, dev.device_id).await;
    let tunnels = queries::device_service_tunnels(&state.db.pg, dev.device_id)
        .await
        .map_err(db_err)?;
    let mut out = Vec::with_capacity(tunnels.len());
    for t in &tunnels {
        // A paused service host (owner pressed Stop) is withheld from the agent, which
        // then tears it down; pressing Start restores it to the config and it resumes.
        if t.status == "stopped" {
            continue;
        }
        let Some(node_id) = t.node_id.as_deref() else {
            continue;
        };
        let Some(node) = queries::get_node(&state.db.pg, node_id)
            .await
            .map_err(db_err)?
        else {
            continue;
        };
        let routes = queries::routes_for_tunnel(&state.db.pg, t.id)
            .await
            .map_err(db_err)?;
        // A parked (0-port) service host has nothing to serve; don't hand it to the agent.
        if routes.is_empty() {
            continue;
        }
        let custom_domain = queries::tunnel_custom_domain(&state.db.pg, t.id)
            .await
            .map_err(db_err)?;
        out.push(build_reservation(
            &state.config.jwt_secret,
            dev.owner_id,
            t,
            &routes,
            &node,
            custom_domain,
        ));
    }
    Ok(Json(out))
}

pub async fn get_tunnels(
    State(state): State<SharedState>,
    user: AuthUser,
) -> Json<Vec<TunnelView>> {
    let tunnels = match queries::tunnels_for_owner(&state.db.pg, user.user_id).await {
        Ok(v) => v,
        Err(_) => Vec::new(),
    };
    Json(tunnels)
}

/// Authorize the caller to read/modify a tunnel: must own it (or be admin).
/// Returns the tunnel's owner_id on success.
async fn authorize_tunnel(
    state: &SharedState,
    user: &AuthUser,
    tunnel_id: i64,
) -> Result<i32, (StatusCode, String)> {
    let owner_opt = queries::tunnel_owner(&state.db.pg, tunnel_id)
        .await
        .map_err(db_err)?;
    let owner = match owner_opt {
        Some(o) => o,
        None => return Err((StatusCode::NOT_FOUND, "no such tunnel".to_string())),
    };
    if owner != user.user_id && user.role != "admin" {
        return Err((StatusCode::FORBIDDEN, "not your tunnel".into()));
    }
    Ok(owner)
}

/// GET /api/tunnels/{id}/bandwidth - current totals + a recent cumulative series.
pub async fn tunnel_bandwidth(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(tunnel_id): Path<i64>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    authorize_tunnel(&state, &user, tunnel_id).await?;
    let mut series = queries::bandwidth_series(&state.db.pg, tunnel_id, 100)
        .await
        .map_err(db_err)?;
    let first_sample = series.first();
    let (bytes_in, bytes_out) = match first_sample {
        Some(s) => (s.bytes_in, s.bytes_out),
        None => (0, 0),
    };
    series.reverse(); // chronological for charting
    Ok(Json(json!({
        "tunnel_id": tunnel_id,
        "bytes_in": bytes_in,
        "bytes_out": bytes_out,
        "total": bytes_in + bytes_out,
        "series": series,
    })))
}

/// GET /api/tunnels/{id}/logs - recent per-connection log entries.
pub async fn tunnel_logs(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(tunnel_id): Path<i64>,
) -> Result<Json<Vec<crate::models::ConnLog>>, (StatusCode, String)> {
    authorize_tunnel(&state, &user, tunnel_id).await?;
    let logs = queries::recent_conn_logs(&state.db.pg, tunnel_id, 20)
        .await
        .map_err(db_err)?;
    Ok(Json(logs))
}

/// GET /api/tunnels/{id}/region_blocks - countries this tunnel refuses (alpha-2).
pub async fn get_tunnel_region_blocks(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(tunnel_id): Path<i64>,
) -> Result<Json<Vec<String>>, (StatusCode, String)> {
    authorize_tunnel(&state, &user, tunnel_id).await?;
    let codes = queries::tunnel_region_blocks(&state.db.pg, tunnel_id)
        .await
        .map_err(db_err)?;
    Ok(Json(codes))
}

#[derive(Deserialize)]
pub struct SetRegionBlocksReq {
    pub country_codes: Vec<String>,
}

/// PUT /api/tunnels/{id}/region_blocks - replace this tunnel's blocked-country list.
pub async fn set_tunnel_region_blocks(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(tunnel_id): Path<i64>,
    Json(req): Json<SetRegionBlocksReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    authorize_tunnel(&state, &user, tunnel_id).await?;
    // Normalise to distinct, valid 2-letter uppercase codes.
    let mut codes: Vec<String> = Vec::new();
    for c in &req.country_codes {
        let upper = c.trim().to_uppercase();
        let mut all_alpha = true;
        for ch in upper.chars() {
            if !ch.is_ascii_alphabetic() {
                all_alpha = false;
                break;
            }
        }
        if upper.len() == 2 && all_alpha {
            codes.push(upper);
        }
    }
    codes.sort();
    codes.dedup();
    if codes.len() > 100 {
        return Err((
            StatusCode::BAD_REQUEST,
            "too many countries (max 100)".into(),
        ));
    }
    queries::set_tunnel_region_blocks(&state.db.pg, tunnel_id, &codes)
        .await
        .map_err(db_err)?;
    Ok(Json(json!({ "status": "updated", "country_codes": codes })))
}

#[derive(Deserialize)]
pub struct SetCustomDomainReq {
    pub domain: String,
}

/// Shape check for a user-owned FQDN: labels + dots, not under our apex, no
/// scheme/port. Ownership itself is proven by the domain resolving to the node (and,
/// with ACME, by answering the HTTP-01 challenge), not by this check.
fn valid_custom_domain(d: &str, apex: &str) -> bool {
    if !(4..=253).contains(&d.len()) {
        return false;
    }
    if d.contains('/') || d.contains(':') || d.contains(' ') || !d.contains('.') {
        return false;
    }
    if d == apex || d.ends_with(&format!(".{apex}")) {
        return false; // must be the user's own domain, not a natforge.com name
    }
    let mut all_labels_ok = true;
    for l in d.split('.') {
        let mut bytes_ok = true;
        for c in l.bytes() {
            if !(c.is_ascii_alphanumeric() || c == b'-') {
                bytes_ok = false;
                break;
            }
        }
        let label_ok =
            !l.is_empty() && l.len() <= 63 && bytes_ok && !l.starts_with('-') && !l.ends_with('-');
        if !label_ok {
            all_labels_ok = false;
            break;
        }
    }
    all_labels_ok
}

/// PUT /api/tunnels/{id}/custom_domain - front the tunnel with a user-owned hostname.
/// Takes effect on the running agent's next reconnect (the new token carries it).
pub async fn set_custom_domain(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(tunnel_id): Path<i64>,
    Json(req): Json<SetCustomDomainReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (subdomain, node_id) = authorize_tunnel_owner(&state, &user, tunnel_id).await?;
    let domain = req.domain.trim().trim_end_matches('.').to_lowercase();
    if !valid_custom_domain(&domain, &state.config.domain) {
        return Err((
            StatusCode::BAD_REQUEST,
            "invalid domain: use a fully-qualified hostname you own, not a natforge.com name"
                .into(),
        ));
    }
    let set_result = queries::set_custom_domain(&state.db.pg, tunnel_id, Some(&domain)).await;
    if let Err(e) = set_result {
        return Err(match &e {
            sqlx::Error::Database(db) if db.constraint() == Some("tunnels_custom_domain_uq") => (
                StatusCode::CONFLICT,
                "that domain is already claimed".to_string(),
            ),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        });
    }
    // Reconnect a live tunnel so the core learns the domain from a fresh token.
    let status_res = queries::tunnel_status(&state.db.pg, tunnel_id).await;
    let status_opt = match status_res {
        Ok(inner) => inner,
        Err(_) => None,
    };
    if status_opt.as_deref() == Some("online") {
        signal_node_stop(&state, &subdomain, &node_id).await;
    }
    // The CNAME target is region-specific: edge.<this node's public host>, so a tunnel
    // on Switzerland tells the user to point at edge.swiss.natforge.com, not the apex
    // edge. (The operator provisions one grey `edge.<region>` record per region node.)
    let edge_host = match &node_id {
        Some(nid) => {
            let node_res = queries::get_node(&state.db.pg, nid).await;
            let node_opt = match node_res {
                Ok(inner) => inner,
                Err(_) => None,
            };
            match node_opt {
                Some(n) => n.public_host,
                None => state.config.domain.clone(),
            }
        }
        None => state.config.domain.clone(),
    };
    Ok(Json(json!({
        "status": "custom_domain_set",
        "domain": domain,
        "cname_target": format!("edge.{edge_host}"),
    })))
}

/// DELETE /api/tunnels/{id}/custom_domain - stop fronting the tunnel with a custom host.
pub async fn clear_custom_domain(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(tunnel_id): Path<i64>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (subdomain, node_id) = authorize_tunnel_owner(&state, &user, tunnel_id).await?;
    queries::set_custom_domain(&state.db.pg, tunnel_id, None)
        .await
        .map_err(db_err)?;
    let status_res = queries::tunnel_status(&state.db.pg, tunnel_id).await;
    let status_opt = match status_res {
        Ok(inner) => inner,
        Err(_) => None,
    };
    if status_opt.as_deref() == Some("online") {
        signal_node_stop(&state, &subdomain, &node_id).await;
    }
    Ok(Json(json!({ "status": "custom_domain_cleared" })))
}

#[derive(Deserialize)]
pub struct MigrateReq {
    pub node_id: String,
}

/// POST /api/tunnels/{id}/migrate - move a tunnel to another region/node. It keeps
/// its subdomain and identity; its wildcard host and any pooled ports move to the
/// target node. A live tunnel is dropped on its old node so the running agent
/// re-reserves and reconnects onto the new one (~3s).
pub async fn migrate_tunnel(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(tunnel_id): Path<i64>,
    Json(req): Json<MigrateReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (subdomain, old_node) = authorize_tunnel_owner(&state, &user, tunnel_id).await?;
    let target_opt = queries::get_node(&state.db.pg, req.node_id.trim())
        .await
        .map_err(db_err)?;
    let target_active = match target_opt {
        Some(n) => {
            if n.active {
                Some(n)
            } else {
                None
            }
        }
        None => None,
    };
    let target = match target_active {
        Some(n) => n,
        None => {
            return Err((
                StatusCode::BAD_REQUEST,
                "unknown or inactive region".to_string(),
            ));
        }
    };
    if old_node.as_deref() == Some(target.node_id.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            "tunnel is already on that region".into(),
        ));
    }
    let migrate_res = queries::migrate_tunnel(
        &state.db.pg,
        tunnel_id,
        &target.node_id,
        &target.public_host,
    )
    .await;
    if let Err(e) = migrate_res {
        return Err(match e {
            ReserveError::PortExhausted => (
                StatusCode::SERVICE_UNAVAILABLE,
                "the target region's port pool is full".into(),
            ),
            ReserveError::Db(err) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("database error: {err}"),
            ),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "migration failed".into()),
        });
    }
    // Drop the live session on the OLD node so the agent re-reserves onto the new one.
    signal_node_stop(&state, &subdomain, &old_node).await;
    Ok(Json(json!({
        "status": "migrated",
        "tunnel_id": tunnel_id,
        "node_id": target.node_id,
        "region": target.region,
    })))
}

/// A region as offered to the user when picking where a tunnel should live.
#[derive(Serialize)]
pub struct RegionOption {
    pub node_id: String,
    pub name: String,
    pub region: Option<String>,
}

/// Public list of active regions for the tunnel-request dropdown (auth required).
pub async fn list_regions(
    State(state): State<SharedState>,
    _user: AuthUser,
) -> Json<Vec<RegionOption>> {
    let nodes = match queries::list_nodes(&state.db.pg, true).await {
        Ok(v) => v,
        Err(_) => Vec::new(),
    };
    let mut options: Vec<RegionOption> = Vec::new();
    for n in nodes {
        options.push(RegionOption {
            node_id: n.node_id,
            name: n.name,
            region: n.region,
        });
    }
    Json(options)
}

/// Best-effort: tell the node hosting a subdomain to drop its live yamux session
/// (internal-secret guarded). Routed to the node's internal URL, falling back to
/// the configured `core_url`. Reused by stop/delete and by admin ban/delete-user.
pub(crate) async fn signal_node_stop(
    state: &SharedState,
    subdomain: &str,
    node_id: &Option<String>,
) {
    let base = match node_id {
        Some(nid) => {
            let node_res = queries::get_node(&state.db.pg, nid).await;
            let node_opt = match node_res {
                Ok(inner) => inner,
                Err(_) => None,
            };
            match node_opt {
                Some(n) => n.internal_url,
                None => state.config.core_url.clone(),
            }
        }
        None => state.config.core_url.clone(),
    };
    let url = format!("{}/internal/tunnels/{}/stop", base, subdomain);
    let _ = state
        .http
        .post(&url)
        .header("x-internal-secret", &state.config.internal_secret)
        .send()
        .await;
}

/// Ownership/admin check; returns (subdomain, node_id) for the tunnel.
async fn authorize_tunnel_owner(
    state: &SharedState,
    user: &AuthUser,
    tunnel_id: i64,
) -> Result<(String, Option<String>), (StatusCode, String)> {
    let owner_opt = queries::tunnel_owner_subdomain(&state.db.pg, tunnel_id)
        .await
        .map_err(db_err)?;
    let owner = match owner_opt {
        Some(o) => o,
        None => return Err((StatusCode::NOT_FOUND, "no such tunnel".to_string())),
    };
    let (owner_id, subdomain, node_id) = owner;
    if owner_id != user.user_id && user.role != "admin" {
        return Err((StatusCode::FORBIDDEN, "not your tunnel".into()));
    }
    Ok((subdomain, node_id))
}

/// DELETE /api/tunnels/{id} - remove the tunnel and free its ports.
pub async fn delete_tunnel(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(tunnel_id): Path<i64>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (subdomain, node_id) = authorize_tunnel_owner(&state, &user, tunnel_id).await?;
    signal_node_stop(&state, &subdomain, &node_id).await;
    let deleted = queries::delete_tunnel(&state.db.pg, tunnel_id)
        .await
        .map_err(db_err)?;
    if deleted == 0 {
        return Err((StatusCode::GONE, "tunnel already removed".into()));
    }
    Ok(Json(
        json!({ "status": "tunnel_deleted", "tunnel_id": tunnel_id }),
    ))
}

#[derive(Deserialize)]
pub struct SetRouteSrvReq {
    /// SRV service label (e.g. "minecraft"); empty/null clears it (no record).
    #[serde(default)]
    pub service: Option<String>,
}

/// POST /api/tunnels/{id}/routes/{route_id}/srv - set/clear a route's SRV service label.
/// The data plane provisions `_<service>._<proto>.<subdomain>` on the agent's next
/// reconnect (triggered here); an empty service removes it.
pub async fn set_route_srv(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((tunnel_id, route_id)): Path<(i64, i32)>,
    Json(req): Json<SetRouteSrvReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (subdomain, node_id) = authorize_tunnel_owner(&state, &user, tunnel_id).await?;
    // Normalize: lowercase DNS-safe label (letters/digits/hyphen), no leading underscore,
    // max 30 chars; empty clears it. The '_' prefix and proto are added at provision time.
    let service_deref = req.service.as_deref();
    let service_trimmed = service_deref.map(str::trim);
    let service: Option<String> = match service_trimmed {
        Some(s) => {
            let lowered = s.trim_start_matches('_').to_ascii_lowercase();
            let mut normalized = String::new();
            let mut kept = 0;
            for c in lowered.chars() {
                if kept >= 30 {
                    break;
                }
                if c.is_ascii_alphanumeric() || c == '-' {
                    normalized.push(c);
                    kept += 1;
                }
            }
            if normalized.is_empty() {
                None
            } else {
                Some(normalized)
            }
        }
        None => None,
    };
    let n = queries::set_route_srv(&state.db.pg, tunnel_id, route_id as i16, service.as_deref())
        .await
        .map_err(db_err)?;
    if n == 0 {
        return Err((StatusCode::NOT_FOUND, "route not found".into()));
    }
    // Re-handshake the live tunnel so the node re-reads claims and (re)provisions DNS.
    let status_res = queries::tunnel_status(&state.db.pg, tunnel_id).await;
    let status_opt = match status_res {
        Ok(inner) => inner,
        Err(_) => None,
    };
    if status_opt.as_deref() == Some("online") {
        signal_node_stop(&state, &subdomain, &node_id).await;
    }
    Ok(Json(
        json!({ "status": "route_srv_set", "tunnel_id": tunnel_id, "route_id": route_id, "service": service }),
    ))
}

/// POST /api/tunnels/{id}/stop - pause the service host: drop the live session and mark
/// it `stopped` so the agent stops serving it, KEEPING the tunnel (same subdomain/ports).
/// Resume with `/start`.
pub async fn stop_tunnel(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(tunnel_id): Path<i64>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (subdomain, node_id) = authorize_tunnel_owner(&state, &user, tunnel_id).await?;
    signal_node_stop(&state, &subdomain, &node_id).await;
    let n = queries::stop_tunnel_keep(&state.db.pg, tunnel_id)
        .await
        .map_err(db_err)?;
    if n == 0 {
        return Err((StatusCode::GONE, "tunnel already removed".into()));
    }
    Ok(Json(
        json!({ "status": "tunnel_stopped", "tunnel_id": tunnel_id }),
    ))
}

/// POST /api/tunnels/{id}/start - resume a paused service host. Clears the `stopped`
/// status so the agent picks it back up on its next config poll (no agent restart).
pub async fn start_tunnel(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(tunnel_id): Path<i64>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    authorize_tunnel_owner(&state, &user, tunnel_id).await?;
    let n = queries::start_tunnel_keep(&state.db.pg, tunnel_id)
        .await
        .map_err(db_err)?;
    if n == 0 {
        return Err((StatusCode::CONFLICT, "service host is not paused".into()));
    }
    Ok(Json(
        json!({ "status": "tunnel_started", "tunnel_id": tunnel_id }),
    ))
}

#[derive(Deserialize)]
pub struct RouteLabelEdit {
    pub route_id: i32,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Deserialize)]
pub struct EditTunnelReq {
    /// Present => set/clear the display name. Absent => leave unchanged.
    #[serde(default)]
    pub name: Option<String>,
    /// Present => change the subdomain (the public address). Absent => leave.
    #[serde(default)]
    pub subdomain: Option<String>,
    /// Per-route label edits (each scoped by route_id within this tunnel).
    #[serde(default)]
    pub route_labels: Vec<RouteLabelEdit>,
}

/// PATCH /api/tunnels/{id} - edit a tunnel's display name, subdomain (its public
/// address), and per-route labels (owner or admin). The local port is the agent's
/// OWN machine port (set via `--route` and part of the idempotency key), so it is
/// deliberately not editable here. A live subdomain change drops the session so the
/// running agent re-reserves onto the new host (~3s).
pub async fn edit_tunnel(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(tunnel_id): Path<i64>,
    Json(req): Json<EditTunnelReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (current_sub, node_id) = authorize_tunnel_owner(&state, &user, tunnel_id).await?;

    // 1) Display name (present => set/clear).
    if let Some(raw) = req.name.as_ref() {
        let trimmed = raw.trim();
        if trimmed.chars().count() > 60 {
            return Err((
                StatusCode::BAD_REQUEST,
                "name too long (max 60 chars)".into(),
            ));
        }
        let name = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        };
        queries::rename_tunnel(&state.db.pg, tunnel_id, name)
            .await
            .map_err(db_err)?;
    }

    // 2) Per-route labels.
    for rl in &req.route_labels {
        let label_deref = rl.label.as_deref();
        let label_trimmed = label_deref.map(str::trim);
        let label = match label_trimmed {
            Some(s) => {
                if !s.is_empty() {
                    Some(s)
                } else {
                    None
                }
            }
            None => None,
        };
        let label_too_long = match label {
            Some(s) => s.chars().count() > 40,
            None => false,
        };
        if label_too_long {
            return Err((
                StatusCode::BAD_REQUEST,
                "route label too long (max 40 chars)".into(),
            ));
        }
        queries::update_route_label(&state.db.pg, tunnel_id, rl.route_id, label)
            .await
            .map_err(db_err)?;
    }

    // 3) Subdomain - the actual address. Validate, ensure free, apply.
    let mut subdomain_changed = false;
    if let Some(raw) = req.subdomain.as_ref() {
        let want = raw.trim().to_lowercase();
        if want != current_sub {
            if !queries::valid_subdomain(&want) {
                return Err((StatusCode::BAD_REQUEST,
                    "invalid subdomain (3–30 chars, lowercase a–z/0–9/-, must start & end alphanumeric)".into()));
            }
            if queries::is_reserved_subdomain(&state.db.pg, &want)
                .await
                .map_err(db_err)?
            {
                return Err((StatusCode::CONFLICT, "that subdomain is reserved".into()));
            }
            if queries::subdomain_in_use(&state.db.pg, &want, tunnel_id)
                .await
                .map_err(db_err)?
            {
                return Err((
                    StatusCode::CONFLICT,
                    "that subdomain is already taken".into(),
                ));
            }
            let set_sub_res = queries::set_tunnel_subdomain(&state.db.pg, tunnel_id, &want).await;
            if set_sub_res.is_err() {
                return Err((
                    StatusCode::CONFLICT,
                    "that subdomain is already taken".into(),
                ));
            }
            subdomain_changed = true;

            // If the tunnel is live, drop the session on the OLD subdomain so the
            // running agent re-reserves and reconnects on the new host.
            let status = queries::tunnel_status(&state.db.pg, tunnel_id)
                .await
                .map_err(db_err)?;
            if status.as_deref() == Some("online") {
                signal_node_stop(&state, &current_sub, &node_id).await;
            }
        }
    }

    Ok(Json(json!({
        "status": "tunnel_updated",
        "tunnel_id": tunnel_id,
        "subdomain_changed": subdomain_changed,
    })))
}

#[derive(Deserialize)]
pub struct SetRoutesReq {
    pub routes: Vec<RequestedRoute>,
}

/// PUT /api/tunnels/{id}/routes - reconcile a service's exposed ports (owner or admin)
/// WITHOUT tearing the tunnel down. Add a port, drop a port, or change the set: kept
/// ports keep their public address, new tcp/udp ports get a fresh pooled port, dropped
/// ports free theirs. This is how the dashboard grows a service in place instead of
/// forcing a brand-new tunnel. On a live tunnel the node's session is dropped so the
/// running agent re-pulls its config and serves the new set (~3s), no CLI needed.
pub async fn set_service_routes(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(tunnel_id): Path<i64>,
    Json(req): Json<SetRoutesReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (subdomain, node_id) = authorize_tunnel_owner(&state, &user, tunnel_id).await?;
    let Some(node_id) = node_id else {
        return Err((
            StatusCode::CONFLICT,
            "this tunnel has no host node yet".into(),
        ));
    };
    // An empty route set is allowed: it "parks" the service host at 0 ports (its dedicated
    // public ports are freed) while keeping the row and its name. The owner can add ports
    // back later. This is also what the idle-reclamation sweep produces.
    // Same per-tunnel policy as a fresh reservation: <=1 http, <=1 https, <=2 tcp, <=2 udp.
    if let Err(e) = check_route_policy(&req.routes) {
        return Err((StatusCode::BAD_REQUEST, e));
    }
    // Per-device port uniqueness, keyed by (protocol, local port): two services on one
    // device must not both claim the same local endpoint. tcp:N and udp:N do NOT clash
    // (distinct local sockets); tcp:N twice, or udp:N twice, do.
    if let Some(device_id) = queries::tunnel_device_id(&state.db.pg, tunnel_id)
        .await
        .map_err(db_err)?
    {
        let taken = queries::device_routes_excluding(&state.db.pg, device_id, tunnel_id)
            .await
            .map_err(db_err)?;
        let mut conflict: Option<&RequestedRoute> = None;
        for r in &req.routes {
            let mut clash = false;
            for (k, p) in &taken {
                if *p == r.local_port as i32
                    && transport_bits(k) & transport_bits(r.mode.as_str()) != 0
                {
                    clash = true;
                    break;
                }
            }
            if clash {
                conflict = Some(r);
                break;
            }
        }
        if let Some(r) = conflict {
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "{}:{} is already used by another service on this device",
                    r.mode.as_str(),
                    r.local_port
                ),
            ));
        }
    }

    let mut requested: Vec<(RouteMode, u16, Option<String>)> = Vec::new();
    for r in &req.routes {
        requested.push((r.mode, r.local_port, r.label.clone()));
    }
    let routes_res =
        queries::set_service_routes(&state.db.pg, tunnel_id, &node_id, &requested).await;
    let routes = match routes_res {
        Ok(v) => v,
        Err(e) => {
            return Err(match e {
                ReserveError::PortExhausted => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "the region's public port pool is full".into(),
                ),
                ReserveError::RouteSetExists(_) => (
                    StatusCode::CONFLICT,
                    "you already have a service with those exact ports".into(),
                ),
                ReserveError::Db(err) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("database error: {err}"),
                ),
                _ => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "route update failed".into(),
                ),
            });
        }
    };

    // Live tunnel: drop the session so the running agent re-pulls and serves the new set.
    let status_res = queries::tunnel_status(&state.db.pg, tunnel_id).await;
    let status_opt = match status_res {
        Ok(inner) => inner,
        Err(_) => None,
    };
    if status_opt.as_deref() == Some("online") {
        signal_node_stop(&state, &subdomain, &Some(node_id)).await;
    }
    Ok(Json(json!({
        "status": "routes_updated",
        "tunnel_id": tunnel_id,
        "route_count": routes.len(),
    })))
}
