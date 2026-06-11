//! Service-host tunnel reservation, listing, and teardown.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::db::connection::SharedState;
use crate::db::queries::{self, ReserveError};
use crate::jwt::{issue_tunnel_token, AuthUser};
use crate::models::{RequestedRoute, TunnelView};
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
}

fn parse_kind(kind: &str) -> RouteMode {
    match kind {
        "http" => RouteMode::Http,
        "https" => RouteMode::Https,
        _ => RouteMode::Tcp,
    }
}

fn endpoint(mode: RouteMode, subdomain: &str, domain: &str, public_port: Option<i32>) -> String {
    match mode {
        RouteMode::Http => format!("http://{subdomain}.{domain}"),
        RouteMode::Https => format!("https://{subdomain}.{domain}"),
        RouteMode::Tcp => format!("{subdomain}.{domain}:{}", public_port.unwrap_or(0)),
    }
}

pub async fn request_tunnel(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(req): Json<RequestTunnelReq>,
) -> Result<Json<TunnelRequestRes>, (StatusCode, String)> {
    if req.routes.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "at least one route is required".into()));
    }
    // Per-tunnel route policy: <= 1 http, <= 1 https, <= 2 tcp.
    let (mut http, mut https, mut tcp) = (0, 0, 0);
    for r in &req.routes {
        match r.mode {
            RouteMode::Http => http += 1,
            RouteMode::Https => https += 1,
            RouteMode::Tcp => tcp += 1,
        }
    }
    if http > 1 || https > 1 || tcp > 2 {
        return Err((StatusCode::BAD_REQUEST, "at most 1 http, 1 https and 2 tcp routes per tunnel".into()));
    }
    // Reject globally blocked local ports up front.
    for r in &req.routes {
        if queries::is_port_blocked(&state.db.pg, r.local_port)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        {
            return Err((StatusCode::FORBIDDEN, format!("local port {} is globally blocked", r.local_port)));
        }
    }

    let requested: Vec<(RouteMode, u16, Option<String>)> =
        req.routes.iter().map(|r| (r.mode, r.local_port, r.label.clone())).collect();
    let custom = req.subdomain.as_deref().map(str::trim).filter(|s| !s.is_empty());

    // Pick the node/region: the requested one (must exist + be active), else the default.
    let db_err = |e: anyhow::Error| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    let node = match &req.node_id {
        Some(nid) => queries::get_node(&state.db.pg, nid).await.map_err(db_err)?
            .filter(|n| n.active)
            .ok_or((StatusCode::BAD_REQUEST, "unknown or inactive region".to_string()))?,
        None => queries::default_node(&state.db.pg).await.map_err(db_err)?
            .ok_or((StatusCode::SERVICE_UNAVAILABLE, "no region/node is available yet".to_string()))?,
    };

    let reserved = queries::reserve_tunnel(
        &state.db.pg,
        user.user_id,
        &node.node_id,
        &node.public_host,
        &requested,
        custom,
        2, // max tunnels per user
    )
    .await
    .map_err(|e| match e {
        ReserveError::LimitReached(n) => (StatusCode::FORBIDDEN, format!("tunnel limit reached ({n})")),
        ReserveError::PortExhausted => (StatusCode::SERVICE_UNAVAILABLE, "public TCP port pool exhausted".into()),
        ReserveError::BlockedPort(p) => (StatusCode::FORBIDDEN, format!("port {p} is blocked")),
        ReserveError::BadSubdomain => (StatusCode::BAD_REQUEST, "invalid subdomain: use 3–30 chars of a–z, 0–9 and '-'".into()),
        ReserveError::SubdomainTaken(s) => (StatusCode::CONFLICT, format!("subdomain '{s}' is already taken")),
        ReserveError::Db(err) => (StatusCode::INTERNAL_SERVER_ERROR, format!("database error: {err}")),
    })?;

    // The tunnel's actual node (reuse may keep an existing node); fall back to chosen.
    let host_node = queries::get_node(&state.db.pg, &reserved.node_id)
        .await
        .ok()
        .flatten()
        .unwrap_or(node);
    let host = &reserved.public_host;

    // Build token claims + the response view from the persisted routes.
    let mut claims = Vec::with_capacity(reserved.routes.len());
    let mut views = Vec::with_capacity(reserved.routes.len());
    for r in &reserved.routes {
        let mode = parse_kind(&r.kind);
        let route_host = if mode.is_host_routed() {
            Some(format!("{}.{}", reserved.subdomain, host))
        } else {
            None
        };
        let public_port = if mode == RouteMode::Tcp {
            r.public_port.map(|p| p as u16)
        } else {
            None
        };
        claims.push(RouteClaim { route_id: r.route_id as u16, mode, host: route_host, public_port });
        views.push(ReservedRoute {
            route_id: r.route_id as u16,
            mode: r.kind.clone(),
            local_port: r.local_port as u16,
            public_endpoint: endpoint(mode, &reserved.subdomain, host, r.public_port),
            label: r.label.clone(),
        });
    }

    let token = issue_tunnel_token(
        &state.config.jwt_secret,
        user.user_id,
        reserved.tunnel_id,
        &reserved.subdomain,
        claims,
    );

    tracing::info!(
        "user {} reserved tunnel {} ({}) {} routes (reused={})",
        user.user_id, reserved.tunnel_id, reserved.subdomain, reserved.routes.len(), reserved.reused
    );

    Ok(Json(TunnelRequestRes {
        full_host: format!("{}.{}", reserved.subdomain, host),
        tunnel_id: reserved.tunnel_id,
        subdomain: reserved.subdomain,
        tunnel_token: token,
        routes: views,
        status: if reserved.reused { "reused" } else { "reserved" }.into(),
        control_endpoint: host_node.control_endpoint,
        region: host_node.region,
        node_id: host_node.node_id,
        control_cert_fingerprint: host_node.control_cert_fp,
    }))
}

pub async fn get_tunnels(State(state): State<SharedState>, user: AuthUser) -> Json<Vec<TunnelView>> {
    let tunnels = queries::tunnels_for_owner(&state.db.pg, user.user_id)
        .await
        .unwrap_or_default();
    Json(tunnels)
}

/// Authorize the caller to read/modify a tunnel: must own it (or be admin).
/// Returns the tunnel's owner_id on success.
async fn authorize_tunnel(
    state: &SharedState,
    user: &AuthUser,
    tunnel_id: i64,
) -> Result<i32, (StatusCode, String)> {
    let owner = queries::tunnel_owner(&state.db.pg, tunnel_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "no such tunnel".to_string()))?;
    if owner != user.user_id && user.role != "admin" {
        return Err((StatusCode::FORBIDDEN, "not your tunnel".into()));
    }
    Ok(owner)
}

/// GET /api/tunnels/{id}/bandwidth — current totals + a recent cumulative series.
pub async fn tunnel_bandwidth(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(tunnel_id): Path<i64>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    authorize_tunnel(&state, &user, tunnel_id).await?;
    let mut series = queries::bandwidth_series(&state.db.pg, tunnel_id, 100)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let (bytes_in, bytes_out) = series.first().map(|s| (s.bytes_in, s.bytes_out)).unwrap_or((0, 0));
    series.reverse(); // chronological for charting
    Ok(Json(json!({
        "tunnel_id": tunnel_id,
        "bytes_in": bytes_in,
        "bytes_out": bytes_out,
        "total": bytes_in + bytes_out,
        "series": series,
    })))
}

/// GET /api/tunnels/{id}/logs — recent per-connection log entries.
pub async fn tunnel_logs(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(tunnel_id): Path<i64>,
) -> Result<Json<Vec<crate::models::ConnLog>>, (StatusCode, String)> {
    authorize_tunnel(&state, &user, tunnel_id).await?;
    let logs = queries::recent_conn_logs(&state.db.pg, tunnel_id, 200)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(logs))
}

/// GET /api/tunnels/{id}/region_blocks — countries this tunnel refuses (alpha-2).
pub async fn get_tunnel_region_blocks(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(tunnel_id): Path<i64>,
) -> Result<Json<Vec<String>>, (StatusCode, String)> {
    authorize_tunnel(&state, &user, tunnel_id).await?;
    let codes = queries::tunnel_region_blocks(&state.db.pg, tunnel_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(codes))
}

#[derive(Deserialize)]
pub struct SetRegionBlocksReq {
    pub country_codes: Vec<String>,
}

/// PUT /api/tunnels/{id}/region_blocks — replace this tunnel's blocked-country list.
pub async fn set_tunnel_region_blocks(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(tunnel_id): Path<i64>,
    Json(req): Json<SetRegionBlocksReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    authorize_tunnel(&state, &user, tunnel_id).await?;
    // Normalise to distinct, valid 2-letter uppercase codes.
    let mut codes: Vec<String> = req
        .country_codes
        .iter()
        .map(|c| c.trim().to_uppercase())
        .filter(|c| c.len() == 2 && c.chars().all(|ch| ch.is_ascii_alphabetic()))
        .collect();
    codes.sort();
    codes.dedup();
    if codes.len() > 100 {
        return Err((StatusCode::BAD_REQUEST, "too many countries (max 100)".into()));
    }
    queries::set_tunnel_region_blocks(&state.db.pg, tunnel_id, &codes)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "status": "updated", "country_codes": codes })))
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
    let nodes = queries::list_nodes(&state.db.pg, true).await.unwrap_or_default();
    Json(
        nodes
            .into_iter()
            .map(|n| RegionOption { node_id: n.node_id, name: n.name, region: n.region })
            .collect(),
    )
}

pub async fn stop_tunnel(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(tunnel_id): Path<i64>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let owner = queries::tunnel_owner_subdomain(&state.db.pg, tunnel_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let (owner_id, subdomain, node_id) = match owner {
        Some(v) => v,
        None => return Err((StatusCode::NOT_FOUND, "no such tunnel".into())),
    };
    if owner_id != user.user_id && user.role != "admin" {
        return Err((StatusCode::FORBIDDEN, "not your tunnel".into()));
    }

    // Route the stop signal to the node hosting the tunnel (fall back to core_url).
    let base = match node_id {
        Some(nid) => queries::get_node(&state.db.pg, &nid)
            .await
            .ok()
            .flatten()
            .map(|n| n.internal_url)
            .unwrap_or_else(|| state.config.core_url.clone()),
        None => state.config.core_url.clone(),
    };
    // Best-effort: tell the data plane to drop the live session (internal-secret guarded).
    let url = format!("{}/internal/tunnels/{}/stop", base, subdomain);
    let _ = state
        .http
        .post(&url)
        .header("x-internal-secret", &state.config.internal_secret)
        .send()
        .await;

    let deleted = queries::delete_tunnel(&state.db.pg, tunnel_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if deleted == 0 {
        return Err((StatusCode::GONE, "tunnel already removed".into()));
    }
    Ok(Json(json!({ "status": "tunnel_closed", "tunnel_id": tunnel_id })))
}
