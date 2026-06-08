//! Service-host tunnel reservation, listing, and teardown.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::db::connection::SharedState;
use crate::db::queries::{self, ReserveError};
use crate::jwt::{issue_tunnel_token, AuthUser};
use crate::models::user::{RequestedRoute, TunnelView};
use natforge_proto::{RouteClaim, RouteMode};

#[derive(Deserialize)]
pub struct RequestTunnelReq {
    pub routes: Vec<RequestedRoute>,
}

#[derive(Serialize)]
pub struct ReservedRoute {
    pub route_id: u16,
    pub mode: String,
    pub local_port: u16,
    pub public_endpoint: String,
}

#[derive(Serialize)]
pub struct TunnelRequestRes {
    pub tunnel_id: i64,
    pub subdomain: String,
    pub full_host: String,
    pub tunnel_token: String,
    pub routes: Vec<ReservedRoute>,
    pub status: String,
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
        RouteMode::Tcp => format!("{}:{}", domain, public_port.unwrap_or(0)),
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

    let requested: Vec<(RouteMode, u16)> = req.routes.iter().map(|r| (r.mode, r.local_port)).collect();

    let reserved = queries::reserve_tunnel(
        &state.db.pg,
        user.user_id,
        &state.config.node_id,
        &state.config.domain,
        &requested,
        2, // max tunnels per user
    )
    .await
    .map_err(|e| match e {
        ReserveError::LimitReached(n) => (StatusCode::FORBIDDEN, format!("tunnel limit reached ({n})")),
        ReserveError::PortExhausted => (StatusCode::SERVICE_UNAVAILABLE, "public TCP port pool exhausted".into()),
        ReserveError::BlockedPort(p) => (StatusCode::FORBIDDEN, format!("port {p} is blocked")),
        ReserveError::Db(err) => (StatusCode::INTERNAL_SERVER_ERROR, format!("database error: {err}")),
    })?;

    // Build token claims + the response view from the persisted routes.
    let mut claims = Vec::with_capacity(reserved.routes.len());
    let mut views = Vec::with_capacity(reserved.routes.len());
    for r in &reserved.routes {
        let mode = parse_kind(&r.kind);
        let host = if mode.is_host_routed() {
            Some(format!("{}.{}", reserved.subdomain, state.config.domain))
        } else {
            None
        };
        let public_port = if mode == RouteMode::Tcp {
            r.public_port.map(|p| p as u16)
        } else {
            None
        };
        claims.push(RouteClaim { route_id: r.route_id as u16, mode, host, public_port });
        views.push(ReservedRoute {
            route_id: r.route_id as u16,
            mode: r.kind.clone(),
            local_port: r.local_port as u16,
            public_endpoint: endpoint(mode, &reserved.subdomain, &state.config.domain, r.public_port),
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
        full_host: format!("{}.{}", reserved.subdomain, state.config.domain),
        tunnel_id: reserved.tunnel_id,
        subdomain: reserved.subdomain,
        tunnel_token: token,
        routes: views,
        status: if reserved.reused { "reused" } else { "reserved" }.into(),
    }))
}

pub async fn get_tunnels(State(state): State<SharedState>, user: AuthUser) -> Json<Vec<TunnelView>> {
    let tunnels = queries::tunnels_for_owner(&state.db.pg, user.user_id, &state.config.domain)
        .await
        .unwrap_or_default();
    Json(tunnels)
}

pub async fn stop_tunnel(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(tunnel_id): Path<i64>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let owner = queries::tunnel_owner_subdomain(&state.db.pg, tunnel_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let (owner_id, subdomain) = match owner {
        Some(v) => v,
        None => return Err((StatusCode::NOT_FOUND, "no such tunnel".into())),
    };
    if owner_id != user.user_id && user.role != "admin" {
        return Err((StatusCode::FORBIDDEN, "not your tunnel".into()));
    }

    // Best-effort: tell the data plane to drop the live session.
    let url = format!("{}/internal/tunnels/{}/stop", state.config.core_url, subdomain);
    let _ = state.http.post(&url).send().await;

    queries::delete_tunnel(&state.db.pg, tunnel_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "status": "tunnel_closed", "tunnel_id": tunnel_id })))
}
